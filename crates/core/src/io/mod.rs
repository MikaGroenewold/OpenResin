//! Model file import/export.

mod obj;
mod stl;
mod threemf;

use std::path::Path;

pub use stl::write_binary_stl;

use crate::{mesh::Mesh, Error, Result};

/// A mesh loaded from disk, with a display name.
#[derive(Debug, Clone)]
pub struct LoadedModel {
    pub name: String,
    pub mesh: Mesh,
}

pub const SUPPORTED_EXTENSIONS: &[&str] = &["stl", "obj", "3mf"];

/// Loads all meshes contained in a model file. STL and OBJ files produce a single
/// model, 3MF files may produce several (one per build item).
pub fn load_models(path: &Path) -> Result<Vec<LoadedModel>> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    let data = std::fs::read(path)?;
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("model").to_string();
    let mut models = match ext.as_str() {
        "stl" => vec![LoadedModel {
            name: stem,
            mesh: stl::parse_stl(&data).map_err(|m| Error::parse(path, m))?,
        }],
        "obj" => vec![LoadedModel {
            name: stem,
            mesh: obj::parse_obj(&data).map_err(|m| Error::parse(path, m))?,
        }],
        "3mf" => threemf::parse_3mf(&data, &stem).map_err(|m| Error::parse(path, m))?,
        other => return Err(Error::UnsupportedFormat(other.to_string())),
    };
    models.retain(|m| !m.mesh.is_empty());
    if models.is_empty() {
        return Err(Error::parse(path, "file contains no triangles"));
    }
    for m in &mut models {
        m.mesh.cleanup();
        m.mesh.orient_outward();
    }
    Ok(models)
}

/// Parses STL data (binary or ASCII) from memory without re-orienting it.
pub fn parse_stl_bytes(data: &[u8]) -> Result<Mesh, String> {
    stl::parse_stl(data)
}

/// Loads a file and merges all contained meshes into one.
pub fn load_mesh(path: &Path) -> Result<Mesh> {
    let models = load_models(path)?;
    let mut mesh = Mesh::default();
    for m in &models {
        mesh.append(&m.mesh);
    }
    Ok(mesh)
}
