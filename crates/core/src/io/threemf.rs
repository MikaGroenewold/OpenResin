//! 3MF reader (core spec + production extension component paths).

use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read};

use glam::{Mat4, Vec3, Vec4};
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

use super::LoadedModel;
use crate::mesh::Mesh;

#[derive(Default)]
struct Object {
    name: Option<String>,
    mesh: Option<Mesh>,
    components: Vec<Component>,
}

struct Component {
    path: Option<String>,
    object_id: u32,
    transform: Mat4,
}

struct BuildItem {
    path: Option<String>,
    object_id: u32,
    transform: Mat4,
}

#[derive(Default)]
struct ModelFile {
    scale: f32,
    objects: HashMap<u32, Object>,
    build: Vec<BuildItem>,
}

pub fn parse_3mf(data: &[u8], fallback_name: &str) -> Result<Vec<LoadedModel>, String> {
    let mut zip = zip::ZipArchive::new(Cursor::new(data)).map_err(|e| format!("not a 3MF/zip archive: {e}"))?;
    let root_path = find_root_model(&mut zip).unwrap_or_else(|| "/3D/3dmodel.model".to_string());

    let mut files: HashMap<String, ModelFile> = HashMap::new();
    let names: Vec<String> = zip.file_names().map(|s| s.to_string()).collect();
    for name in names.iter().filter(|n| n.to_ascii_lowercase().ends_with(".model")) {
        let mut xml = String::new();
        zip.by_name(name)
            .map_err(|e| e.to_string())?
            .read_to_string(&mut xml)
            .map_err(|e| format!("{name}: {e}"))?;
        files.insert(
            normalize_path(name),
            parse_model_xml(&xml).map_err(|e| format!("{name}: {e}"))?,
        );
    }
    let root_key = normalize_path(&root_path);
    let root = files
        .get(&root_key)
        .ok_or_else(|| format!("root model {root_path} not found"))?;

    let mut out = Vec::new();
    let items: Vec<(Option<String>, u32, Mat4)> = if root.build.is_empty() {
        // No build section: take every object as-is that is not a component
        // of another object (those are included through their parent).
        let used: HashSet<u32> = root
            .objects
            .values()
            .flat_map(|o| o.components.iter())
            .filter(|c| c.path.as_deref().is_none_or(|p| normalize_path(p) == root_key))
            .map(|c| c.object_id)
            .collect();
        let mut ids: Vec<u32> = root.objects.keys().copied().filter(|id| !used.contains(id)).collect();
        ids.sort_unstable();
        ids.into_iter().map(|id| (None, id, Mat4::IDENTITY)).collect()
    } else {
        root.build
            .iter()
            .map(|b| (b.path.clone(), b.object_id, b.transform))
            .collect()
    };
    for (idx, (path, id, transform)) in items.into_iter().enumerate() {
        let file_key = path.map(|p| normalize_path(&p)).unwrap_or_else(|| root_key.clone());
        let mut mesh = Mesh::default();
        let mut name = None;
        collect(&files, &file_key, id, transform, &mut mesh, &mut name, 0)?;
        let scale = files.get(&file_key).map(|f| f.scale).unwrap_or(1.0);
        if scale != 1.0 {
            mesh.transform_in_place(&Mat4::from_scale(Vec3::splat(scale)));
        }
        out.push(LoadedModel {
            name: name.unwrap_or_else(|| format!("{fallback_name}_{}", idx + 1)),
            mesh,
        });
    }
    Ok(out)
}

fn collect(
    files: &HashMap<String, ModelFile>,
    file: &str,
    id: u32,
    transform: Mat4,
    out: &mut Mesh,
    name: &mut Option<String>,
    depth: u32,
) -> Result<(), String> {
    if depth > 32 {
        return Err("component nesting too deep".into());
    }
    let f = files.get(file).ok_or_else(|| format!("missing model part {file}"))?;
    let obj = f
        .objects
        .get(&id)
        .ok_or_else(|| format!("object {id} not found in {file}"))?;
    if name.is_none() {
        name.clone_from(&obj.name);
    }
    if let Some(m) = &obj.mesh {
        out.append(&m.transformed(&transform));
    }
    for c in &obj.components {
        let cf = c
            .path
            .as_deref()
            .map(normalize_path)
            .unwrap_or_else(|| file.to_string());
        collect(files, &cf, c.object_id, transform * c.transform, out, name, depth + 1)?;
    }
    Ok(())
}

fn normalize_path(p: &str) -> String {
    let p = p.replace('\\', "/");
    let p = p.trim_start_matches('/');
    format!("/{}", p).to_ascii_lowercase()
}

fn find_root_model(zip: &mut zip::ZipArchive<Cursor<&[u8]>>) -> Option<String> {
    let mut rels = String::new();
    zip.by_name("_rels/.rels").ok()?.read_to_string(&mut rels).ok()?;
    let mut reader = Reader::from_str(&rels);
    loop {
        match reader.read_event() {
            Ok(Event::Empty(e)) | Ok(Event::Start(e)) if e.local_name().as_ref() == b"Relationship" => {
                let ty = attr(&e, b"Type").unwrap_or_default();
                if ty.ends_with("/3dmodel") {
                    return attr(&e, b"Target");
                }
            }
            Ok(Event::Eof) | Err(_) => return None,
            _ => {}
        }
    }
}

fn attr(e: &BytesStart, key: &[u8]) -> Option<String> {
    e.attributes().flatten().find_map(|a| {
        let k = a.key.local_name();
        (k.as_ref() == key).then(|| a.unescape_value().map(|v| v.into_owned()).unwrap_or_default())
    })
}

fn attr_f32(e: &BytesStart, key: &[u8]) -> f32 {
    attr(e, key).and_then(|v| v.trim().parse().ok()).unwrap_or(0.0)
}

fn parse_transform(s: &str) -> Mat4 {
    let v: Vec<f32> = s.split_whitespace().filter_map(|t| t.parse().ok()).collect();
    if v.len() != 12 {
        return Mat4::IDENTITY;
    }
    // 3MF uses row vectors: [x y z 1] * M, with M given row by row (4 rows of 3).
    Mat4::from_cols(
        Vec4::new(v[0], v[1], v[2], 0.0),
        Vec4::new(v[3], v[4], v[5], 0.0),
        Vec4::new(v[6], v[7], v[8], 0.0),
        Vec4::new(v[9], v[10], v[11], 1.0),
    )
}

fn unit_scale(unit: &str) -> f32 {
    match unit {
        "micron" => 0.001,
        "centimeter" => 10.0,
        "inch" => 25.4,
        "foot" => 304.8,
        "meter" => 1000.0,
        _ => 1.0,
    }
}

fn parse_model_xml(xml: &str) -> Result<ModelFile, String> {
    let mut reader = Reader::from_str(xml);
    let mut file = ModelFile {
        scale: 1.0,
        ..Default::default()
    };
    let mut cur: Option<(u32, Object)> = None;
    let mut positions: Vec<Vec3> = Vec::new();
    let mut triangles: Vec<[u32; 3]> = Vec::new();
    let mut in_mesh = false;
    loop {
        let ev = reader.read_event().map_err(|e| e.to_string())?;
        match ev {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let is_empty = matches!(ev, Event::Empty(_));
                match e.local_name().as_ref() {
                    b"model" => {
                        file.scale = unit_scale(&attr(e, b"unit").unwrap_or_default());
                    }
                    b"object" => {
                        let id = attr(e, b"id").and_then(|v| v.parse().ok()).unwrap_or(0);
                        let obj = Object {
                            name: attr(e, b"name").filter(|n| !n.is_empty()),
                            ..Default::default()
                        };
                        if is_empty {
                            file.objects.insert(id, obj);
                        } else {
                            cur = Some((id, obj));
                        }
                    }
                    b"mesh" => {
                        in_mesh = !is_empty;
                        positions.clear();
                        triangles.clear();
                    }
                    b"vertex" if in_mesh => {
                        positions.push(Vec3::new(attr_f32(e, b"x"), attr_f32(e, b"y"), attr_f32(e, b"z")));
                    }
                    b"triangle" if in_mesh => {
                        let g = |k: &[u8]| attr(e, k).and_then(|v| v.parse::<u32>().ok());
                        if let (Some(a), Some(b), Some(c)) = (g(b"v1"), g(b"v2"), g(b"v3")) {
                            let n = positions.len() as u32;
                            if a < n && b < n && c < n {
                                triangles.push([a, b, c]);
                            }
                        }
                    }
                    b"component" => {
                        if let Some((_, obj)) = cur.as_mut() {
                            obj.components.push(Component {
                                path: attr(e, b"path"),
                                object_id: attr(e, b"objectid").and_then(|v| v.parse().ok()).unwrap_or(0),
                                transform: attr(e, b"transform")
                                    .map(|t| parse_transform(&t))
                                    .unwrap_or(Mat4::IDENTITY),
                            });
                        }
                    }
                    b"item" => {
                        file.build.push(BuildItem {
                            path: attr(e, b"path"),
                            object_id: attr(e, b"objectid").and_then(|v| v.parse().ok()).unwrap_or(0),
                            transform: attr(e, b"transform")
                                .map(|t| parse_transform(&t))
                                .unwrap_or(Mat4::IDENTITY),
                        });
                    }
                    _ => {}
                }
            }
            Event::End(ref e) => match e.local_name().as_ref() {
                b"mesh" => {
                    in_mesh = false;
                    if let Some((_, obj)) = cur.as_mut() {
                        obj.mesh = Some(Mesh::new(
                            std::mem::take(&mut positions),
                            std::mem::take(&mut triangles),
                        ));
                    }
                }
                b"object" => {
                    if let Some((id, obj)) = cur.take() {
                        file.objects.insert(id, obj);
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn minimal_3mf_with_transform() {
        let model = r#"<?xml version="1.0" encoding="UTF-8"?>
<model unit="millimeter" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02">
 <resources>
  <object id="1" type="model" name="tri"><mesh>
   <vertices><vertex x="0" y="0" z="0"/><vertex x="1" y="0" z="0"/><vertex x="0" y="1" z="0"/></vertices>
   <triangles><triangle v1="0" v2="1" v3="2"/></triangles>
  </mesh></object>
  <object id="2" type="model"><components><component objectid="1" transform="1 0 0 0 1 0 0 0 1 10 0 0"/></components></object>
 </resources>
 <build><item objectid="2" transform="1 0 0 0 1 0 0 0 1 0 5 0"/></build>
</model>"#;
        let mut buf = Vec::new();
        {
            let mut w = zip::ZipWriter::new(Cursor::new(&mut buf));
            let opts = zip::write::SimpleFileOptions::default();
            w.start_file("3D/3dmodel.model", opts).unwrap();
            w.write_all(model.as_bytes()).unwrap();
            w.finish().unwrap();
        }
        let models = parse_3mf(&buf, "x").unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].name, "tri");
        let b = models[0].mesh.bounds();
        assert!((b.min.x - 10.0).abs() < 1e-6 && (b.min.y - 5.0).abs() < 1e-6);
    }
}
