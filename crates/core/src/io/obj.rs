use glam::Vec3;

use crate::mesh::Mesh;

/// Minimal Wavefront OBJ reader: vertices and (polygonal) faces, ignoring
/// texture/normal indices, materials and groups.
pub fn parse_obj(data: &[u8]) -> Result<Mesh, String> {
    let text = String::from_utf8_lossy(data);
    let mut positions: Vec<Vec3> = Vec::new();
    let mut triangles: Vec<[u32; 3]> = Vec::new();
    let mut face: Vec<u32> = Vec::with_capacity(8);
    for (ln, line) in text.lines().enumerate() {
        let line = line.trim();
        let mut it = line.split_whitespace();
        match it.next() {
            Some("v") => {
                let mut v = [0f32; 3];
                for c in &mut v {
                    *c = it
                        .next()
                        .ok_or_else(|| format!("line {}: truncated vertex", ln + 1))?
                        .parse()
                        .map_err(|e| format!("line {}: {e}", ln + 1))?;
                }
                positions.push(Vec3::from(v));
            }
            Some("f") => {
                face.clear();
                for tok in it {
                    let idx_str = tok.split('/').next().unwrap_or("");
                    let idx: i64 = idx_str
                        .parse()
                        .map_err(|e| format!("line {}: bad face index: {e}", ln + 1))?;
                    let resolved = if idx < 0 { positions.len() as i64 + idx } else { idx - 1 };
                    if resolved < 0 || resolved as usize >= positions.len() {
                        return Err(format!("line {}: face index out of range", ln + 1));
                    }
                    face.push(resolved as u32);
                }
                for k in 1..face.len().saturating_sub(1) {
                    triangles.push([face[0], face[k], face[k + 1]]);
                }
            }
            _ => {}
        }
    }
    if triangles.is_empty() {
        return Err("no faces found".into());
    }
    Ok(Mesh::new(positions, triangles))
}

#[cfg(test)]
mod tests {
    #[test]
    fn quad_is_triangulated() {
        let s = "v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nf 1/1/1 2/2/2 3/3/3 4/4/4\n";
        let m = super::parse_obj(s.as_bytes()).unwrap();
        assert_eq!(m.triangles.len(), 2);
    }
}
