//! Project files (`.orproj`): a zip containing `project.json` and binary STL meshes.

use std::io::{Read, Seek, Write};
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::io::write_binary_stl;
use crate::profile::PrintSettings;
use crate::scene::{ObjectRole, Scene, SceneObject, Transform};
use crate::support::{SupportPoint, SupportSettings};
use crate::{Error, Result};

pub const PROJECT_EXTENSION: &str = "orproj";
const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct ProjectFile {
    format_version: u32,
    printer_id: Option<String>,
    settings: Option<PrintSettings>,
    objects: Vec<ProjectObject>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ProjectObject {
    id: u64,
    name: String,
    mesh: String,
    transform: Transform,
    supports: Vec<SupportPoint>,
    support_settings: SupportSettings,
    #[serde(default)]
    role: ObjectRole,
    #[serde(default)]
    cutouts: Vec<String>,
}

pub struct Project {
    pub scene: Scene,
    pub printer_id: Option<String>,
    pub settings: Option<PrintSettings>,
}

pub fn save(path: &Path, scene: &Scene, printer_id: Option<&str>, settings: Option<&PrintSettings>) -> Result<()> {
    let file = std::fs::File::create(path)?;
    write(file, scene, printer_id, settings)
}

pub fn write<W: Write + Seek>(
    w: W,
    scene: &Scene,
    printer_id: Option<&str>,
    settings: Option<&PrintSettings>,
) -> Result<()> {
    let mut zip = zip::ZipWriter::new(w);
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut objects = Vec::new();
    for o in &scene.objects {
        let mesh_name = format!("meshes/{}.stl", o.id);
        zip.start_file(&mesh_name, opts)?;
        let mut buf = Vec::new();
        write_binary_stl(&o.mesh, &mut buf)?;
        zip.write_all(&buf)?;
        let mut cutouts = Vec::new();
        for (i, c) in o.cutouts.iter().enumerate() {
            let name = format!("cutouts/{}_{i}.stl", o.id);
            zip.start_file(&name, opts)?;
            let mut buf = Vec::new();
            write_binary_stl(c, &mut buf)?;
            zip.write_all(&buf)?;
            cutouts.push(name);
        }
        objects.push(ProjectObject {
            id: o.id,
            name: o.name.clone(),
            mesh: mesh_name,
            transform: o.transform,
            supports: o.supports.clone(),
            support_settings: o.support_settings.clone(),
            role: o.role,
            cutouts,
        });
    }
    let pf = ProjectFile {
        format_version: FORMAT_VERSION,
        printer_id: printer_id.map(str::to_string),
        settings: settings.cloned(),
        objects,
    };
    zip.start_file("project.json", opts)?;
    zip.write_all(serde_json::to_string_pretty(&pf)?.as_bytes())?;
    zip.finish()?;
    Ok(())
}

pub fn load(path: &Path) -> Result<Project> {
    let file = std::fs::File::open(path)?;
    read(file).map_err(|e| match e {
        Error::Invalid(m) => Error::parse(path, m),
        other => other,
    })
}

pub fn read<R: Read + Seek>(r: R) -> Result<Project> {
    let mut zip = zip::ZipArchive::new(r)?;
    let mut json = String::new();
    zip.by_name("project.json")?.read_to_string(&mut json)?;
    let pf: ProjectFile = serde_json::from_str(&json)?;
    if pf.format_version > FORMAT_VERSION {
        return Err(Error::Invalid(format!(
            "project format version {} is newer than supported ({FORMAT_VERSION})",
            pf.format_version
        )));
    }
    let mut scene = Scene::default();
    for o in pf.objects {
        let mut data = Vec::new();
        zip.by_name(&o.mesh)?.read_to_end(&mut data)?;
        let mesh = crate::io::parse_stl_bytes(&data).map_err(|m| Error::Invalid(format!("{}: {m}", o.mesh)))?;
        let mut cutouts = Vec::new();
        for c in &o.cutouts {
            let mut d = Vec::new();
            zip.by_name(c)?.read_to_end(&mut d)?;
            cutouts.push(Arc::new(
                crate::io::parse_stl_bytes(&d).map_err(|m| Error::Invalid(format!("{c}: {m}")))?,
            ));
        }
        scene.insert(SceneObject {
            id: o.id,
            name: o.name,
            mesh: Arc::new(mesh),
            transform: o.transform,
            supports: o.supports,
            support_settings: o.support_settings,
            role: o.role,
            cutouts,
        });
    }
    Ok(Project {
        scene,
        printer_id: pf.printer_id,
        settings: pf.settings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::cuboid;
    use glam::Vec3;
    use std::io::Cursor;

    #[test]
    fn roundtrip() {
        let mut scene = Scene::default();
        let id = scene.add("cube", cuboid(Vec3::ZERO, Vec3::splat(10.0)));
        scene.get_mut(id).unwrap().transform.translation.x = 12.0;
        let mut buf = Cursor::new(Vec::new());
        write(&mut buf, &scene, Some("generic-4k"), Some(&PrintSettings::default())).unwrap();
        buf.set_position(0);
        let p = read(buf).unwrap();
        assert_eq!(p.printer_id.as_deref(), Some("generic-4k"));
        let o = p.scene.get(id).unwrap();
        assert_eq!(o.transform.translation.x, 12.0);
        assert_eq!(o.mesh.triangle_count(), 12);
    }
}
