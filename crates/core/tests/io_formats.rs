//! Model import/export: STL (binary + ASCII), OBJ and 3MF.

mod common;

use std::io::{Cursor, Write};

use common::*;
use openresin_core::io::{load_mesh, load_models, parse_stl_bytes, write_binary_stl, SUPPORTED_EXTENSIONS};
use openresin_core::mesh::Mesh;
use openresin_core::primitives::{cuboid, sphere};
use openresin_core::{Error, Vec3};

fn stl_bytes(m: &Mesh) -> Vec<u8> {
    let mut buf = Vec::new();
    write_binary_stl(m, &mut buf).unwrap();
    buf
}

fn f32_at(data: &[u8], off: usize) -> f32 {
    f32::from_le_bytes(data[off..off + 4].try_into().unwrap())
}

// ---------------------------------------------------------------- STL

#[test]
fn binary_stl_layout_and_roundtrip_preserve_every_triangle() {
    let c = Vec3::new(1.5, -2.25, 3.0);
    let m = sphere(c, 7.0, 24, 12);
    let data = stl_bytes(&m);
    let n = m.triangle_count();
    assert_eq!(data.len(), 84 + 50 * n);
    assert!(data.starts_with(b"OpenResin"));
    assert_eq!(u32::from_le_bytes(data[80..84].try_into().unwrap()) as usize, n);
    // stored normals are unit length and point outward
    for i in 0..n {
        let o = 84 + i * 50;
        let nrm = Vec3::new(f32_at(&data, o), f32_at(&data, o + 4), f32_at(&data, o + 8));
        assert!((nrm.length() - 1.0).abs() < 1e-5, "facet {i} normal {nrm}");
        let [a, b, cc] = m.tri(i);
        let centroid = (a + b + cc) / 3.0;
        assert!(nrm.dot(centroid - c) > 0.0, "facet {i} normal points inward");
        assert_eq!(data[o + 48..o + 50], [0, 0]);
    }
    let back = parse_stl_bytes(&data).unwrap();
    assert_eq!(back.triangle_count(), n);
    // welded back to the shared vertices of the original
    assert_eq!(back.positions.len(), m.positions.len());
    assert_eq!(back.open_edge_count(), 0);
    assert_eq!(canonical_triangles(&back), canonical_triangles(&m));
    assert_close(back.signed_volume(), m.signed_volume(), 1e-9, "volume");
    assert_eq!(back.bounds(), m.bounds());
}

#[test]
fn ascii_stl_matches_binary() {
    let m = cuboid(Vec3::new(0.125, -3.5, 2.0), Vec3::new(4.0, 2.5, 1.25));
    let text = ascii_stl(&m, "part");
    let a = parse_stl_bytes(text.as_bytes()).unwrap();
    let b = parse_stl_bytes(&stl_bytes(&m)).unwrap();
    assert_eq!(a.triangle_count(), 12);
    assert_eq!(a.positions.len(), 8);
    assert_eq!(canonical_triangles(&a), canonical_triangles(&b));
    assert_close(a.signed_volume(), 4.0 * 2.5 * 1.25, 1e-5, "ascii volume");
}

#[test]
fn ascii_stl_with_utf8_bom_is_parsed_as_text() {
    let m = cuboid(Vec3::ZERO, Vec3::splat(2.0));
    let mut data = "\u{feff}".as_bytes().to_vec();
    data.extend_from_slice(ascii_stl(&m, "bom").as_bytes());
    let back = parse_stl_bytes(&data).unwrap();
    assert_eq!(back.triangle_count(), 12);
    assert_close(back.signed_volume(), 8.0, 1e-5, "volume");
}

#[test]
fn ascii_polygon_facets_are_fanned() {
    let s = "solid q\nfacet normal 0 0 1\nouter loop\nvertex 0 0 0\nvertex 2 0 0\nvertex 2 1 0\nvertex 0 1 0\nendloop\nendfacet\nendsolid q\n";
    let m = parse_stl_bytes(s.as_bytes()).unwrap();
    assert_eq!(m.triangle_count(), 2);
    assert_close(m.surface_area(), 2.0, 1e-6, "quad area");
    assert!(m.face_normal(0).z > 0.99 && m.face_normal(1).z > 0.99);
}

#[test]
fn binary_stl_whose_header_starts_with_solid() {
    let m = cuboid(Vec3::ZERO, Vec3::ONE);
    let mut data = stl_bytes(&m);
    let header = b"solid facet endfacet vertex (binary file with a misleading header)";
    data[..header.len()].copy_from_slice(header);
    let back = parse_stl_bytes(&data).unwrap();
    assert_eq!(back.triangle_count(), 12);
    assert_close(back.signed_volume(), 1.0, 1e-6, "volume");
}

#[test]
fn negative_zero_coordinates_are_welded() {
    // the cube spans 0..1, so many coordinates are zero; write some of them as -0.0
    let m = cuboid(Vec3::splat(0.5), Vec3::ONE);
    let mut data = stl_bytes(&m);
    for i in (0..m.triangle_count()).step_by(2) {
        for k in 0..9 {
            let o = 84 + i * 50 + 12 + k * 4;
            if f32_at(&data, o) == 0.0 {
                data[o..o + 4].copy_from_slice(&(-0.0f32).to_le_bytes());
            }
        }
    }
    let back = parse_stl_bytes(&data).unwrap();
    assert_eq!(back.positions.len(), 8, "+0.0 and -0.0 must weld to one vertex");
    assert_eq!(back.open_edge_count(), 0);
}

#[test]
fn non_finite_binary_facets_are_skipped() {
    let m = cuboid(Vec3::ZERO, Vec3::ONE);
    let mut data = stl_bytes(&m);
    // facet 3, vertex 1, x = NaN
    let o = 84 + 3 * 50 + 12 + 12;
    data[o..o + 4].copy_from_slice(&f32::NAN.to_le_bytes());
    let back = parse_stl_bytes(&data).unwrap();
    assert_eq!(back.triangle_count(), 11);
    assert!(back.positions.iter().all(|p| p.is_finite()));
}

#[test]
fn malformed_stl_is_rejected() {
    let cube = stl_bytes(&cuboid(Vec3::ZERO, Vec3::ONE));
    let mut garbage = Vec::with_capacity(1000);
    let mut x = 0x1234_5678u32;
    for _ in 0..1000 {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        garbage.push((x >> 24) as u8);
    }
    garbage[80..84].copy_from_slice(&[0xab; 4]);
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty", Vec::new()),
        ("header only", vec![0u8; 80]),
        ("solid without facets", b"solid x\nendsolid x\n".to_vec()),
        (
            "bad number",
            b"solid x\nfacet normal 0 0 1\nouter loop\nvertex 0 0 zz\n".to_vec(),
        ),
        (
            "truncated vertex",
            b"solid x\nfacet normal 0 0 1\nouter loop\nvertex 1 2\n".to_vec(),
        ),
        ("truncated binary", cube[..84 + 50 * 5 + 20].to_vec()),
        ("garbage", garbage),
    ];
    for (name, data) in cases {
        assert!(parse_stl_bytes(&data).is_err(), "{name} was accepted");
    }
    // a binary file with zero triangles parses, but loading it is an error
    let empty = stl_bytes(&Mesh::default());
    assert_eq!(empty.len(), 84);
    assert!(parse_stl_bytes(&empty).unwrap().is_empty());
    let f = TempFile::with_bytes("empty", "stl", &empty);
    assert!(matches!(load_models(f.path()), Err(Error::Parse { .. })));
    let g = TempFile::with_bytes("garbage", "stl", b"this is not an stl file at all");
    assert!(matches!(load_models(g.path()), Err(Error::Parse { .. })));
}

#[test]
fn load_models_reorients_cleans_and_names() {
    let mut inv = cuboid(Vec3::new(5.0, 5.0, 5.0), Vec3::splat(10.0));
    inv.flip();
    // a zero-area sliver (three collinear, distinct vertices) is dropped
    let mut soupy: Vec<[Vec3; 3]> = (0..inv.triangle_count()).map(|i| inv.tri(i)).collect();
    soupy.push([
        Vec3::new(0.0, 0.0, 0.0),
        Vec3::new(1.0, 0.0, 0.0),
        Vec3::new(2.0, 0.0, 0.0),
    ]);
    let m = Mesh::from_soup(&soupy);
    assert_eq!(m.triangle_count(), 13);
    let f = TempFile::with_bytes("Inverted_Part", "STL", &stl_bytes(&m));
    let models = load_models(f.path()).unwrap();
    assert_eq!(models.len(), 1);
    assert!(models[0].name.ends_with("Inverted_Part"), "{}", models[0].name);
    let mesh = &models[0].mesh;
    assert_eq!(mesh.triangle_count(), 12);
    assert_eq!(mesh.positions.len(), 8, "unreferenced vertices removed");
    assert_close(mesh.signed_volume(), 1000.0, 1e-3, "outward volume");
    assert_eq!(load_mesh(f.path()).unwrap().triangle_count(), 12);
}

#[test]
fn unsupported_and_missing_files() {
    assert_eq!(SUPPORTED_EXTENSIONS, &["stl", "obj", "3mf"]);
    let f = TempFile::with_bytes("model", "ply", b"ply\n");
    match load_models(f.path()) {
        Err(Error::UnsupportedFormat(e)) => assert_eq!(e, "ply"),
        other => panic!("expected UnsupportedFormat, got {other:?}"),
    }
    let missing = TempFile::new("does-not-exist", "stl");
    assert!(matches!(load_models(missing.path()), Err(Error::Io(_))));
}

// ---------------------------------------------------------------- OBJ

const OBJ_CUBE: &str = "# cube made of quads, mixed index styles\r\n\
o cube\r\n\
mtllib none.mtl\r\n\
v 0 0 0\r\nv 2 0 0\r\nv 2 3 0\r\nv 0 3 0\r\n\
v 0 0 4\r\nv 2 0 4\r\nv 2 3 4\r\nv 0 3 4 1.0\r\n\
vt 0 0\r\nvn 0 0 1\r\n\
g sides\r\nusemtl red\r\n\
\r\n\
f 1/1/1 4/1/1 3/1/1 2/1/1\r\n\
f 5//1 6//1 7//1 8//1\r\n\
f -8 -7 -3 -4\r\n\
f 2 3 7 6\r\n\
f 3/1 4/1 8/1 7/1\r\n\
f 4 1 5 8\r\n";

#[test]
fn obj_quads_negative_indices_and_attributes() {
    let f = TempFile::with_bytes("cube", "obj", OBJ_CUBE.as_bytes());
    let models = load_models(f.path()).unwrap();
    assert_eq!(models.len(), 1);
    let m = &models[0].mesh;
    assert_eq!(m.triangle_count(), 12);
    assert_eq!(m.open_edge_count(), 0);
    assert_close(m.signed_volume(), 24.0, 1e-5, "volume");
    assert_close(m.surface_area(), 2.0 * (6.0 + 12.0 + 8.0), 1e-4, "area");
    let b = m.bounds();
    assert_eq!((b.min, b.max), (Vec3::ZERO, Vec3::new(2.0, 3.0, 4.0)));
}

#[test]
fn obj_negative_indices_are_relative_to_the_vertices_read_so_far() {
    let s = "v 0 0 0\nv 1 0 0\nv 0 1 0\nf -3 -2 -1\nv 10 0 0\nv 11 0 0\nv 10 1 0\nf -3 -2 -1\n";
    let f = TempFile::with_bytes("rel", "obj", s.as_bytes());
    let m = load_mesh(f.path()).unwrap();
    assert_eq!(m.triangle_count(), 2);
    let xs: Vec<f32> = (0..2)
        .map(|i| m.tri(i).iter().map(|v| v.x).fold(f32::MAX, f32::min))
        .collect();
    assert!(xs.contains(&0.0) && xs.contains(&10.0), "{xs:?}");
}

#[test]
fn obj_ngon_is_fanned() {
    // regular pentagon
    let mut s = String::new();
    for k in 0..5 {
        let a = k as f32 / 5.0 * std::f32::consts::TAU;
        s += &format!("v {} {} 0\n", a.cos() * 2.0, a.sin() * 2.0);
    }
    s += "f 1 2 3 4 5\n";
    let f = TempFile::with_bytes("pentagon", "obj", s.as_bytes());
    let m = load_mesh(f.path()).unwrap();
    assert_eq!(m.triangle_count(), 3);
    assert_close(m.surface_area(), ngon_area(5, 2.0), 1e-4, "pentagon area");
}

#[test]
fn malformed_obj_is_rejected() {
    let cases = [
        ("index out of range", "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 4\n"),
        ("index zero", "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 0 1 2\n"),
        ("negative out of range", "v 0 0 0\nv 1 0 0\nv 0 1 0\nf -1 -2 -4\n"),
        ("bad index", "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 a 3\n"),
        ("truncated vertex", "v 0 0\nf 1 1 1\n"),
        ("bad coordinate", "v 0 x 0\n"),
        ("no faces", "v 0 0 0\nv 1 0 0\nv 0 1 0\n"),
        ("only a line", "v 0 0 0\nv 1 0 0\nf 1 2\n"),
        ("binary garbage", "\u{0}\u{1}\u{2}\u{ff}garbage"),
    ];
    for (name, text) in cases {
        let f = TempFile::with_bytes("bad", "obj", text.as_bytes());
        assert!(
            matches!(load_models(f.path()), Err(Error::Parse { .. })),
            "{name} was accepted"
        );
    }
}

// ---------------------------------------------------------------- 3MF

fn zip_files(files: &[(&str, &str)]) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut w = zip::ZipWriter::new(Cursor::new(&mut buf));
        let opts = zip::write::SimpleFileOptions::default();
        for (name, content) in files {
            w.start_file(*name, opts).unwrap();
            w.write_all(content.as_bytes()).unwrap();
        }
        w.finish().unwrap();
    }
    buf
}

/// `<mesh>` element of a closed box `[0,sx] x [0,sy] x [0,sz]`.
fn box_mesh_xml(sx: f32, sy: f32, sz: f32) -> String {
    let m = cuboid(Vec3::new(sx, sy, sz) * 0.5, Vec3::new(sx, sy, sz));
    let mut s = String::from("<mesh><vertices>");
    for p in &m.positions {
        s += &format!("<vertex x=\"{}\" y=\"{}\" z=\"{}\"/>", p.x, p.y, p.z);
    }
    s += "</vertices><triangles>";
    for t in &m.triangles {
        s += &format!("<triangle v1=\"{}\" v2=\"{}\" v3=\"{}\"/>", t[0], t[1], t[2]);
    }
    s + "</triangles></mesh>"
}

fn model_xml(unit: &str, resources: &str, build: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<model unit=\"{unit}\" xml:lang=\"en-US\" \
         xmlns=\"http://schemas.microsoft.com/3dmanufacturing/core/2015/02\">\n<resources>{resources}</resources>\n{build}\n</model>"
    )
}

fn load_3mf(data: &[u8]) -> openresin_core::Result<Vec<openresin_core::io::LoadedModel>> {
    let f = TempFile::with_bytes("plate", "3mf", data);
    load_models(f.path())
}

#[test]
fn threemf_single_object_with_rotation_and_translation() {
    // a 10 x 2 x 2 bar, rotated by +90 degrees about Z (x -> y) and moved
    let res = format!(
        "<object id=\"1\" type=\"model\" name=\"bar\">{}</object>",
        box_mesh_xml(10.0, 2.0, 2.0)
    );
    let build = "<build><item objectid=\"1\" transform=\"0 1 0 -1 0 0 0 0 1 5 6 7\"/></build>";
    let data = zip_files(&[("3D/3dmodel.model", &model_xml("millimeter", &res, build))]);
    let models = load_3mf(&data).unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].name, "bar");
    let b = models[0].mesh.bounds();
    // x in [0,10] -> y in [6,16]; y in [0,2] -> x in [3,5]
    for (got, want) in [(b.min, Vec3::new(3.0, 6.0, 7.0)), (b.max, Vec3::new(5.0, 16.0, 9.0))] {
        assert!((got - want).length() < 1e-5, "{b:?}");
    }
    assert_close(models[0].mesh.signed_volume(), 40.0, 1e-4, "volume");
}

#[test]
fn threemf_multiple_objects_instances_and_fallback_names() {
    let res = format!(
        "<object id=\"1\" type=\"model\" name=\"small\">{}</object><object id=\"7\" type=\"model\">{}</object>",
        box_mesh_xml(1.0, 1.0, 1.0),
        box_mesh_xml(2.0, 3.0, 4.0)
    );
    let build = "<build>\
        <item objectid=\"1\"/>\
        <item objectid=\"7\" transform=\"1 0 0 0 1 0 0 0 1 20 0 0\"/>\
        <item objectid=\"1\" transform=\"2 0 0 0 2 0 0 0 2 -20 0 0\"/>\
        </build>";
    let data = zip_files(&[("3D/3dmodel.model", &model_xml("millimeter", &res, build))]);
    let f = TempFile::with_bytes("multi", "3mf", &data);
    let models = load_models(f.path()).unwrap();
    assert_eq!(models.len(), 3);
    let stem = f.path().file_stem().unwrap().to_str().unwrap().to_string();
    assert_eq!(models[0].name, "small");
    assert_eq!(models[1].name, format!("{stem}_2"));
    assert_eq!(models[2].name, "small");
    let vols: Vec<f64> = models.iter().map(|m| m.mesh.signed_volume()).collect();
    for (v, want) in vols.iter().zip([1.0, 24.0, 8.0]) {
        assert_close(*v, want, 1e-4, "instance volume");
    }
    assert_close(models[1].mesh.bounds().min.x as f64, 20.0, 1e-5, "item 2 offset");
    assert_close(models[2].mesh.bounds().min.x as f64, -20.0, 1e-5, "item 3 offset");
    // merged
    let all = load_mesh(f.path()).unwrap();
    assert_eq!(all.triangle_count(), 36);
    assert_close(all.signed_volume(), 33.0, 1e-3, "merged volume");
}

#[test]
fn threemf_units_are_converted_to_millimetres() {
    for (unit, scale) in [
        ("micron", 0.001f64),
        ("millimeter", 1.0),
        ("centimeter", 10.0),
        ("inch", 25.4),
        ("meter", 1000.0),
    ] {
        let res = format!(
            "<object id=\"1\" type=\"model\">{}</object>",
            box_mesh_xml(1.0, 2.0, 3.0)
        );
        let build = "<build><item objectid=\"1\" transform=\"1 0 0 0 1 0 0 0 1 1 0 0\"/></build>";
        let data = zip_files(&[("3D/3dmodel.model", &model_xml(unit, &res, build))]);
        let m = &load_3mf(&data).unwrap()[0].mesh;
        assert_rel(m.signed_volume(), 6.0 * scale.powi(3), 1e-4, unit);
        // the translation is in file units as well
        assert_rel(m.bounds().min.x as f64, scale, 1e-5, unit);
    }
}

#[test]
fn threemf_mirrored_item_is_outward() {
    let res = format!(
        "<object id=\"1\" type=\"model\">{}</object>",
        box_mesh_xml(2.0, 3.0, 4.0)
    );
    let build = "<build><item objectid=\"1\" transform=\"-1 0 0 0 1 0 0 0 1 0 0 0\"/></build>";
    let data = zip_files(&[("3D/3dmodel.model", &model_xml("millimeter", &res, build))]);
    let m = &load_3mf(&data).unwrap()[0].mesh;
    assert_close(m.signed_volume(), 24.0, 1e-4, "volume");
    let b = m.bounds();
    assert_eq!((b.min.x, b.max.x), (-2.0, 0.0));
    // every face normal points away from the centre
    let c = b.center();
    for i in 0..m.triangle_count() {
        let [a, bb, cc] = m.tri(i);
        assert!(m.face_normal(i).dot((a + bb + cc) / 3.0 - c) > 0.0);
    }
}

#[test]
fn threemf_nested_components_and_production_paths() {
    // root model (non-default name, found through _rels/.rels) references an
    // assembly whose parts live in a separate model file
    let rels = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
        <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
        <Relationship Target=\"/3D/main.model\" Id=\"rel0\" Type=\"http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel\"/>\
        </Relationships>";
    let part = model_xml(
        "millimeter",
        &format!(
            "<object id=\"3\" type=\"model\" name=\"leaf\">{}</object>",
            box_mesh_xml(1.0, 1.0, 1.0)
        ),
        "<build/>",
    );
    let root_res = "<object id=\"2\" type=\"model\" name=\"assembly\"><components>\
        <component p:path=\"/3D/Objects/part.model\" objectid=\"3\" transform=\"1 0 0 0 1 0 0 0 1 10 0 0\" xmlns:p=\"http://schemas.microsoft.com/3dmanufacturing/production/2015/06\"/>\
        <component p:path=\"/3D/Objects/part.model\" objectid=\"3\" transform=\"1 0 0 0 1 0 0 0 1 0 10 0\" xmlns:p=\"http://schemas.microsoft.com/3dmanufacturing/production/2015/06\"/>\
        </components></object>\
        <object id=\"1\" type=\"model\" name=\"top\"><components><component objectid=\"2\" transform=\"2 0 0 0 2 0 0 0 2 0 0 0\"/></components></object>";
    let root = model_xml(
        "millimeter",
        root_res,
        "<build><item objectid=\"1\" transform=\"1 0 0 0 1 0 0 0 1 0 0 5\"/></build>",
    );
    let data = zip_files(&[
        ("_rels/.rels", rels),
        ("3D/main.model", &root),
        ("3D/Objects/part.model", &part),
    ]);
    let models = load_3mf(&data).unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].name, "top");
    let m = &models[0].mesh;
    assert_eq!(m.triangle_count(), 24);
    // each leaf is scaled by 2 after its component offset: (10,0,0)*2 and (0,10,0)*2, then lifted by 5
    assert_close(m.signed_volume(), 2.0 * 8.0, 1e-4, "volume");
    let b = m.bounds();
    assert!((b.min - Vec3::new(0.0, 0.0, 5.0)).length() < 1e-5, "{b:?}");
    assert!((b.max - Vec3::new(22.0, 22.0, 7.0)).length() < 1e-5, "{b:?}");
}

#[test]
fn threemf_without_build_section_takes_top_level_objects_once() {
    let res = format!(
        "<object id=\"1\" type=\"model\" name=\"leaf\">{}</object>\
         <object id=\"2\" type=\"model\" name=\"assembly\"><components>\
         <component objectid=\"1\"/><component objectid=\"1\" transform=\"1 0 0 0 1 0 0 0 1 5 0 0\"/>\
         </components></object>",
        box_mesh_xml(1.0, 1.0, 1.0)
    );
    let data = zip_files(&[("3D/3dmodel.model", &model_xml("millimeter", &res, ""))]);
    let models = load_3mf(&data).unwrap();
    // the leaf is only used by the assembly; it must not show up a second time
    assert_eq!(
        models.len(),
        1,
        "{:?}",
        models.iter().map(|m| &m.name).collect::<Vec<_>>()
    );
    assert_eq!(models[0].mesh.triangle_count(), 24);
}

#[test]
fn threemf_invalid_references_and_archives_are_errors() {
    let leaf = box_mesh_xml(1.0, 1.0, 1.0);
    let cycle = format!(
        "<object id=\"1\" type=\"model\">{leaf}</object>\
         <object id=\"2\" type=\"model\"><components><component objectid=\"2\"/><component objectid=\"1\"/></components></object>"
    );
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("not a zip", b"PK\x03\x04 definitely not a zip".to_vec()),
        ("no model", zip_files(&[("readme.txt", "hello")])),
        (
            "missing object",
            zip_files(&[(
                "3D/3dmodel.model",
                &model_xml(
                    "millimeter",
                    &format!("<object id=\"1\">{leaf}</object>"),
                    "<build><item objectid=\"9\"/></build>",
                ),
            )]),
        ),
        (
            "self reference",
            zip_files(&[(
                "3D/3dmodel.model",
                &model_xml("millimeter", &cycle, "<build><item objectid=\"2\"/></build>"),
            )]),
        ),
        (
            "broken xml",
            zip_files(&[("3D/3dmodel.model", "<model><resources><object id=\"1\"></resources>")]),
        ),
    ];
    for (name, data) in cases {
        assert!(load_3mf(&data).is_err(), "{name} was accepted");
    }
}

#[test]
fn threemf_out_of_range_triangles_are_ignored() {
    let res = "<object id=\"1\" type=\"model\"><mesh><vertices>\
        <vertex x=\"0\" y=\"0\" z=\"0\"/><vertex x=\"1\" y=\"0\" z=\"0\"/><vertex x=\"0\" y=\"1\" z=\"0\"/>\
        </vertices><triangles>\
        <triangle v1=\"0\" v2=\"1\" v3=\"2\"/><triangle v1=\"0\" v2=\"1\" v3=\"3\"/><triangle v1=\"0\" v2=\"x\" v3=\"2\"/>\
        </triangles></mesh></object>";
    let data = zip_files(&[(
        "3D/3dmodel.model",
        &model_xml("millimeter", res, "<build><item objectid=\"1\"/></build>"),
    )]);
    let m = &load_3mf(&data).unwrap()[0].mesh;
    assert_eq!(m.triangle_count(), 1);
    assert_close(m.surface_area(), 0.5, 1e-6, "area");
}
