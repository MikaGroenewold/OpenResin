use std::io::Write;

use glam::Vec3;

use crate::mesh::Mesh;

pub fn parse_stl(data: &[u8]) -> Result<Mesh, String> {
    if is_binary_stl(data) {
        // a text file without a proper "solid" header still parses as ASCII
        parse_binary(data).or_else(|e| parse_ascii(data).map_err(|_| e))
    } else {
        parse_ascii(data)
    }
}

fn is_binary_stl(data: &[u8]) -> bool {
    if data.len() < 84 {
        return false;
    }
    let n = u32::from_le_bytes(data[80..84].try_into().unwrap()) as usize;
    // Binary files whose header starts with "solid" exist, so the size check wins.
    if 84 + n * 50 == data.len() {
        return true;
    }
    let head = String::from_utf8_lossy(&data[..data.len().min(256)]).to_ascii_lowercase();
    let head = head.trim_start_matches('\u{feff}').trim_start();
    !(head.starts_with("solid") && head.contains("facet"))
}

fn parse_binary(data: &[u8]) -> Result<Mesh, String> {
    let n = u32::from_le_bytes(data[80..84].try_into().unwrap()) as usize;
    let available = (data.len() - 84) / 50;
    if n > available {
        return Err(format!(
            "truncated binary STL: {n} triangles declared, data for {available}"
        ));
    }
    let mut soup = Vec::with_capacity(n);
    for i in 0..n {
        let o = 84 + i * 50 + 12;
        let f = |k: usize| f32::from_le_bytes(data[o + k * 4..o + k * 4 + 4].try_into().unwrap());
        let tri = [
            Vec3::new(f(0), f(1), f(2)),
            Vec3::new(f(3), f(4), f(5)),
            Vec3::new(f(6), f(7), f(8)),
        ];
        if tri.iter().all(|v| v.is_finite()) {
            soup.push(tri);
        }
    }
    Ok(Mesh::from_soup(&soup))
}

fn parse_ascii(data: &[u8]) -> Result<Mesh, String> {
    let text = String::from_utf8_lossy(data);
    let mut soup = Vec::new();
    let mut cur: Vec<Vec3> = Vec::with_capacity(3);
    for line in text.lines() {
        let mut it = line.split_whitespace();
        match it.next() {
            Some("vertex") => {
                let mut v = [0f32; 3];
                for c in &mut v {
                    *c = it
                        .next()
                        .ok_or("truncated vertex")?
                        .parse()
                        .map_err(|e| format!("bad vertex: {e}"))?;
                }
                cur.push(Vec3::from(v));
            }
            Some("endfacet") => {
                if cur.len() >= 3 {
                    // polygons with more than 3 vertices are fanned
                    for k in 1..cur.len() - 1 {
                        soup.push([cur[0], cur[k], cur[k + 1]]);
                    }
                }
                cur.clear();
            }
            _ => {}
        }
    }
    if soup.is_empty() {
        return Err("no facets found".into());
    }
    Ok(Mesh::from_soup(&soup))
}

pub fn write_binary_stl<W: Write>(mesh: &Mesh, mut w: W) -> std::io::Result<()> {
    let mut header = [0u8; 80];
    let tag = b"OpenResin binary STL";
    header[..tag.len()].copy_from_slice(tag);
    w.write_all(&header)?;
    w.write_all(&(mesh.triangles.len() as u32).to_le_bytes())?;
    let mut buf = Vec::with_capacity(mesh.triangles.len() * 50);
    for i in 0..mesh.triangles.len() {
        let n = mesh.face_normal(i);
        let [a, b, c] = mesh.tri(i);
        for v in [n, a, b, c] {
            for k in 0..3 {
                buf.extend_from_slice(&v[k].to_le_bytes());
            }
        }
        buf.extend_from_slice(&[0, 0]);
    }
    w.write_all(&buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::cuboid;

    #[test]
    fn binary_roundtrip() {
        let m = cuboid(Vec3::ZERO, Vec3::new(1.0, 2.0, 3.0));
        let mut buf = Vec::new();
        write_binary_stl(&m, &mut buf).unwrap();
        let back = parse_stl(&buf).unwrap();
        assert_eq!(back.triangles.len(), 12);
        assert!((back.signed_volume() - 6.0).abs() < 1e-4);
    }

    #[test]
    fn ascii() {
        let s = "solid t\nfacet normal 0 0 1\nouter loop\nvertex 0 0 0\nvertex 1 0 0\nvertex 0 1 0\nendloop\nendfacet\nendsolid t\n";
        let m = parse_stl(s.as_bytes()).unwrap();
        assert_eq!(m.triangles.len(), 1);
    }
}
