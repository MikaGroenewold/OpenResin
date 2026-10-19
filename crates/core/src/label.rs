//! Text labels: a built-in 5x7 pixel font turned into closed, extruded meshes
//! that can be embossed onto (added to) or engraved into (subtracted from) a
//! model surface.

use std::collections::HashMap;

use glam::{Mat4, Vec2, Vec3, Vec4};
use serde::{Deserialize, Serialize};

use crate::mesh::Mesh;

/// Glyph rows (top to bottom), 5 bits each, most significant bit on the left.
type Glyph = [u8; 7];

/// Printable ASCII (32..=126) in a 5x7 grid. Unknown characters render as `?`.
const FONT: [Glyph; 95] = [
    [0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000], // ' '
    [0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00000, 0b00100], // !
    [0b01010, 0b01010, 0b01010, 0b00000, 0b00000, 0b00000, 0b00000], // "
    [0b01010, 0b01010, 0b11111, 0b01010, 0b11111, 0b01010, 0b01010], // #
    [0b00100, 0b01111, 0b10100, 0b01110, 0b00101, 0b11110, 0b00100], // $
    [0b11000, 0b11001, 0b00010, 0b00100, 0b01000, 0b10011, 0b00011], // %
    [0b01100, 0b10010, 0b10100, 0b01000, 0b10101, 0b10010, 0b01101], // &
    [0b01100, 0b00100, 0b01000, 0b00000, 0b00000, 0b00000, 0b00000], // '
    [0b00010, 0b00100, 0b01000, 0b01000, 0b01000, 0b00100, 0b00010], // (
    [0b01000, 0b00100, 0b00010, 0b00010, 0b00010, 0b00100, 0b01000], // )
    [0b00000, 0b00100, 0b10101, 0b01110, 0b10101, 0b00100, 0b00000], // *
    [0b00000, 0b00100, 0b00100, 0b11111, 0b00100, 0b00100, 0b00000], // +
    [0b00000, 0b00000, 0b00000, 0b00000, 0b01100, 0b00100, 0b01000], // ,
    [0b00000, 0b00000, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000], // -
    [0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b01100, 0b01100], // .
    [0b00000, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b00000], // /
    [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110], // 0
    [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110], // 1
    [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111], // 2
    [0b11111, 0b00010, 0b00100, 0b00010, 0b00001, 0b10001, 0b01110], // 3
    [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010], // 4
    [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110], // 5
    [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110], // 6
    [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000], // 7
    [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110], // 8
    [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100], // 9
    [0b00000, 0b01100, 0b01100, 0b00000, 0b01100, 0b01100, 0b00000], // :
    [0b00000, 0b01100, 0b01100, 0b00000, 0b01100, 0b00100, 0b01000], // ;
    [0b00010, 0b00100, 0b01000, 0b10000, 0b01000, 0b00100, 0b00010], // <
    [0b00000, 0b00000, 0b11111, 0b00000, 0b11111, 0b00000, 0b00000], // =
    [0b01000, 0b00100, 0b00010, 0b00001, 0b00010, 0b00100, 0b01000], // >
    [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b00000, 0b00100], // ?
    [0b01110, 0b10001, 0b00001, 0b01101, 0b10101, 0b10101, 0b01110], // @
    [0b01110, 0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001], // A
    [0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110], // B
    [0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110], // C
    [0b11100, 0b10010, 0b10001, 0b10001, 0b10001, 0b10010, 0b11100], // D
    [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111], // E
    [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000], // F
    [0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01111], // G
    [0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001], // H
    [0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110], // I
    [0b00111, 0b00010, 0b00010, 0b00010, 0b00010, 0b10010, 0b01100], // J
    [0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001], // K
    [0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111], // L
    [0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001], // M
    [0b10001, 0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001], // N
    [0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110], // O
    [0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000], // P
    [0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101], // Q
    [0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001], // R
    [0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110], // S
    [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100], // T
    [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110], // U
    [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100], // V
    [0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b10101, 0b01010], // W
    [0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001], // X
    [0b10001, 0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100], // Y
    [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111], // Z
    [0b01110, 0b01000, 0b01000, 0b01000, 0b01000, 0b01000, 0b01110], // [
    [0b00000, 0b10000, 0b01000, 0b00100, 0b00010, 0b00001, 0b00000], // backslash
    [0b01110, 0b00010, 0b00010, 0b00010, 0b00010, 0b00010, 0b01110], // ]
    [0b00100, 0b01010, 0b10001, 0b00000, 0b00000, 0b00000, 0b00000], // ^
    [0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b11111], // _
    [0b01000, 0b00100, 0b00010, 0b00000, 0b00000, 0b00000, 0b00000], // `
    [0b00000, 0b00000, 0b01110, 0b00001, 0b01111, 0b10001, 0b01111], // a
    [0b10000, 0b10000, 0b10110, 0b11001, 0b10001, 0b10001, 0b11110], // b
    [0b00000, 0b00000, 0b01110, 0b10000, 0b10000, 0b10001, 0b01110], // c
    [0b00001, 0b00001, 0b01101, 0b10011, 0b10001, 0b10001, 0b01111], // d
    [0b00000, 0b00000, 0b01110, 0b10001, 0b11111, 0b10000, 0b01110], // e
    [0b00110, 0b01001, 0b01000, 0b11100, 0b01000, 0b01000, 0b01000], // f
    [0b00000, 0b01111, 0b10001, 0b10001, 0b01111, 0b00001, 0b01110], // g
    [0b10000, 0b10000, 0b10110, 0b11001, 0b10001, 0b10001, 0b10001], // h
    [0b00100, 0b00000, 0b01100, 0b00100, 0b00100, 0b00100, 0b01110], // i
    [0b00010, 0b00000, 0b00110, 0b00010, 0b00010, 0b10010, 0b01100], // j
    [0b10000, 0b10000, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010], // k
    [0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110], // l
    [0b00000, 0b00000, 0b11010, 0b10101, 0b10101, 0b10001, 0b10001], // m
    [0b00000, 0b00000, 0b10110, 0b11001, 0b10001, 0b10001, 0b10001], // n
    [0b00000, 0b00000, 0b01110, 0b10001, 0b10001, 0b10001, 0b01110], // o
    [0b00000, 0b00000, 0b11110, 0b10001, 0b11110, 0b10000, 0b10000], // p
    [0b00000, 0b00000, 0b01101, 0b10011, 0b01111, 0b00001, 0b00001], // q
    [0b00000, 0b00000, 0b10110, 0b11001, 0b10000, 0b10000, 0b10000], // r
    [0b00000, 0b00000, 0b01110, 0b10000, 0b01110, 0b00001, 0b11110], // s
    [0b01000, 0b01000, 0b11100, 0b01000, 0b01000, 0b01001, 0b00110], // t
    [0b00000, 0b00000, 0b10001, 0b10001, 0b10001, 0b10011, 0b01101], // u
    [0b00000, 0b00000, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100], // v
    [0b00000, 0b00000, 0b10001, 0b10001, 0b10101, 0b10101, 0b01010], // w
    [0b00000, 0b00000, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001], // x
    [0b00000, 0b00000, 0b10001, 0b10001, 0b01111, 0b00001, 0b01110], // y
    [0b00000, 0b00000, 0b11111, 0b00010, 0b00100, 0b01000, 0b11111], // z
    [0b00010, 0b00100, 0b00100, 0b01000, 0b00100, 0b00100, 0b00010], // {
    [0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100], // |
    [0b01000, 0b00100, 0b00100, 0b00010, 0b00100, 0b00100, 0b01000], // }
    [0b00000, 0b00000, 0b01000, 0b10101, 0b00010, 0b00000, 0b00000], // ~
];

const GLYPH_W: usize = 5;
const GLYPH_H: usize = 7;
const ADVANCE: usize = GLYPH_W + 1;
const LINE_ADVANCE: usize = GLYPH_H + 2;

fn glyph(c: char) -> &'static Glyph {
    let i = c as u32;
    if (32..=126).contains(&i) {
        &FONT[(i - 32) as usize]
    } else {
        &FONT[('?' as u32 - 32) as usize]
    }
}

/// Pixel bitmap of `text` (rows from the bottom), with its size in pixels.
fn bitmap(text: &str) -> (Vec<bool>, usize, usize) {
    let lines: Vec<&str> = text.split('\n').map(|l| l.trim_end_matches('\r')).collect();
    let cols = lines
        .iter()
        .map(|l| (l.chars().count() * ADVANCE).saturating_sub(1))
        .max()
        .unwrap_or(0);
    let rows = (lines.len() * LINE_ADVANCE).saturating_sub(LINE_ADVANCE - GLYPH_H);
    let mut px = vec![false; cols * rows];
    for (li, line) in lines.iter().enumerate() {
        // centre every line horizontally
        let w = (line.chars().count() * ADVANCE).saturating_sub(1);
        let x0 = (cols - w) / 2;
        for (ci, ch) in line.chars().enumerate() {
            let g = glyph(ch);
            for (gr, bits) in g.iter().enumerate() {
                let r = rows - 1 - (li * LINE_ADVANCE + gr);
                for gc in 0..GLYPH_W {
                    if bits & (1 << (GLYPH_W - 1 - gc)) != 0 {
                        px[r * cols + x0 + ci * ADVANCE + gc] = true;
                    }
                }
            }
        }
    }
    (px, cols, rows)
}

/// Size (width, height) in mm of `text` rendered by [`text_mesh`] with capital
/// height `height_mm`.
pub fn text_size(text: &str, height_mm: f32) -> Vec2 {
    let (_, cols, rows) = bitmap(text);
    Vec2::new(cols as f32, rows as f32) * (height_mm / GLYPH_H as f32)
}

/// Extrudes `text` (ASCII; `\n` starts a new line) into a closed, outward
/// oriented mesh. Capital letters are `height_mm` tall; the text lies in the XY
/// plane, reads along +X with +Y up, is centred on the origin and spans
/// `z = 0 ..= depth_mm`. Returns an empty mesh for empty text or invalid sizes.
pub fn text_mesh(text: &str, height_mm: f32, depth_mm: f32) -> Mesh {
    let positive = |x: f32| x.is_finite() && x > 0.0;
    if !positive(height_mm) || !positive(depth_mm) {
        return Mesh::default();
    }
    let (px, cols, rows) = bitmap(text);
    let s = height_mm / GLYPH_H as f32;
    let filled = |x: i64, r: i64| {
        x >= 0 && r >= 0 && (x as usize) < cols && (r as usize) < rows && px[r as usize * cols + x as usize]
    };
    let off = Vec2::new(cols as f32, rows as f32) * s * 0.5;
    let mut mesh = Mesh::default();
    // vertex per (grid x, grid r, group, top?)
    let mut verts: HashMap<(i64, i64, u8, bool), u32> = HashMap::new();
    // Quadrants around a grid point, counter-clockwise: SW, SE, NE, NW.
    // Diagonally touching pixels get separate vertices (keeps the mesh manifold).
    let mut corner = |gx: i64, gr: i64, quadrant: u8, top: bool, mesh: &mut Mesh| -> u32 {
        let q = [
            filled(gx - 1, gr - 1),
            filled(gx, gr - 1),
            filled(gx, gr),
            filled(gx - 1, gr),
        ];
        let diagonal = (q[0] && q[2] && !q[1] && !q[3]) || (q[1] && q[3] && !q[0] && !q[2]);
        let group = if diagonal { quadrant } else { 4 };
        *verts.entry((gx, gr, group, top)).or_insert_with(|| {
            let p = Vec2::new(gx as f32, gr as f32) * s - off;
            mesh.positions.push(p.extend(if top { depth_mm } else { 0.0 }));
            (mesh.positions.len() - 1) as u32
        })
    };
    for r in 0..rows as i64 {
        for x in 0..cols as i64 {
            if !filled(x, r) {
                continue;
            }
            // pixel corners in CCW order with the quadrant the pixel occupies there
            let cs = [(x, r, 2u8), (x + 1, r, 3u8), (x + 1, r + 1, 0u8), (x, r + 1, 1u8)];
            let b: Vec<u32> = cs
                .iter()
                .map(|&(gx, gr, q)| corner(gx, gr, q, false, &mut mesh))
                .collect();
            let t: Vec<u32> = cs
                .iter()
                .map(|&(gx, gr, q)| corner(gx, gr, q, true, &mut mesh))
                .collect();
            mesh.triangles.push([t[0], t[1], t[2]]);
            mesh.triangles.push([t[0], t[2], t[3]]);
            mesh.triangles.push([b[0], b[2], b[1]]);
            mesh.triangles.push([b[0], b[3], b[2]]);
            // walls where the neighbour across the side is empty: S, E, N, W
            let nbs = [(x, r - 1), (x + 1, r), (x, r + 1), (x - 1, r)];
            for (side, nb) in nbs.iter().enumerate() {
                if filled(nb.0, nb.1) {
                    continue;
                }
                let (a, c) = (side, (side + 1) % 4);
                mesh.triangles.push([b[a], b[c], t[c]]);
                mesh.triangles.push([b[a], t[c], t[a]]);
            }
        }
    }
    mesh
}

/// Whether a label is added to or cut into the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LabelMode {
    /// Raised text (add the mesh as a solid part).
    #[default]
    Emboss,
    /// Engraved text (subtract the mesh).
    Engrave,
}

/// Options for [`place_label`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LabelOptions {
    /// Capital letter height (mm).
    pub height: f32,
    /// Emboss height above / engrave depth below the surface (mm).
    pub depth: f32,
    pub mode: LabelMode,
    /// How far the mesh extends to the other side of the surface (mm): into the
    /// model for embossed text, out of it for engraved text. Makes the label
    /// fuse cleanly with slightly curved surfaces.
    pub overlap: f32,
}

impl Default for LabelOptions {
    fn default() -> Self {
        Self {
            height: 5.0,
            depth: 0.8,
            mode: LabelMode::Emboss,
            overlap: 0.4,
        }
    }
}

/// Transform from label space (text in the XY plane, +Y up, +Z out of the
/// surface) to model space: the origin goes to `position`, +Z to `normal` and
/// +Y to `up` projected onto the surface plane (a fallback is used when `up`
/// is parallel to the normal).
pub fn surface_frame(position: Vec3, normal: Vec3, up: Vec3) -> Mat4 {
    let n = normal.normalize_or(Vec3::Z);
    let mut y = up - n * n.dot(up);
    if y.length_squared() < 1e-8 {
        let fallback = if n.z.abs() < 0.9 { Vec3::Z } else { Vec3::Y };
        y = fallback - n * n.dot(fallback);
    }
    let y = y.normalize();
    let x = y.cross(n);
    Mat4::from_cols(
        x.extend(0.0),
        y.extend(0.0),
        n.extend(0.0),
        Vec4::new(position.x, position.y, position.z, 1.0),
    )
}

/// Builds a label for `text` on a model surface at `position` with outward
/// surface `normal`; the text reads upright along `up`. Embossed labels rise
/// `depth` above the surface and reach `overlap` into it; engraved labels go
/// `depth` into the surface and reach `overlap` above it. The result is
/// closed and outward oriented, ready to be added or subtracted.
pub fn place_label(text: &str, opts: &LabelOptions, position: Vec3, normal: Vec3, up: Vec3) -> Mesh {
    let overlap = opts.overlap.max(0.0);
    let total = opts.depth + overlap;
    let mut m = text_mesh(text, opts.height, total);
    if m.is_empty() {
        return m;
    }
    let z0 = match opts.mode {
        LabelMode::Emboss => -overlap,
        LabelMode::Engrave => -opts.depth,
    };
    let local = Mat4::from_translation(Vec3::new(0.0, 0.0, z0));
    m.transform_in_place(&(surface_frame(position, normal, up) * local));
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repair::analyze;

    #[test]
    fn text_is_closed_manifold_with_exact_volume() {
        let text = "Hello, World!\nOpenResin 0.1 %&@ XWvz";
        let m = text_mesh(text, 7.0, 1.5);
        let r = analyze(&m);
        assert!(r.is_clean(), "{r:?}");
        assert_eq!(m.open_edge_count(), 0);
        let (px, _, _) = bitmap(text);
        let lit = px.iter().filter(|&&p| p).count() as f64;
        // pixel size 1 mm
        assert!(
            (m.signed_volume() - lit * 1.5).abs() < 1e-3,
            "{} vs {}",
            m.signed_volume(),
            lit * 1.5
        );
        let b = m.bounds();
        assert!((b.min.z - 0.0).abs() < 1e-6 && (b.max.z - 1.5).abs() < 1e-6);
        assert!(b.center().truncate().length() < 1.0);
        let size = text_size(text, 7.0);
        assert!((size.y - 16.0).abs() < 1e-4);
    }

    #[test]
    fn single_glyph_sizes_and_fallback() {
        let h = text_mesh("H", 5.0, 1.0);
        let s = h.bounds().size();
        assert!((s.x - 5.0 * 5.0 / 7.0).abs() < 1e-4 && (s.y - 5.0).abs() < 1e-4);
        // unknown characters render as '?'
        let q = text_mesh("?", 5.0, 1.0);
        let u = text_mesh("\u{263a}", 5.0, 1.0);
        assert_eq!(q.triangle_count(), u.triangle_count());
        assert!(text_mesh("", 5.0, 1.0).is_empty());
        assert!(text_mesh("   ", 5.0, 1.0).is_empty());
        assert!(text_mesh("A", 0.0, 1.0).is_empty());
        // every glyph on its own is clean
        for c in 33u8..=126 {
            let m = text_mesh(&(c as char).to_string(), 7.0, 1.0);
            assert!(analyze(&m).is_clean(), "glyph {}", c as char);
        }
    }

    #[test]
    fn placement_on_surfaces() {
        let o = LabelOptions::default();
        // on the +X side of a cube, reading upright (+Z)
        let p = Vec3::new(10.0, 0.0, 5.0);
        let m = place_label("ABC", &o, p, Vec3::X, Vec3::Z);
        assert!(analyze(&m).is_clean());
        let b = m.bounds();
        assert!((b.min.x - (10.0 - o.overlap)).abs() < 1e-4 && (b.max.x - (10.0 + o.depth)).abs() < 1e-4);
        assert!((b.size().z - o.height).abs() < 1e-4, "{b:?}");
        // reading direction: a viewer at +X looking at the face (up = +Z) has +Y on the right
        let f = surface_frame(p, Vec3::X, Vec3::Z);
        let right = f.transform_vector3(Vec3::X);
        assert!((right - Vec3::Y).length() < 1e-5);
        assert!(f.determinant() > 0.0);
        let e = place_label(
            "ABC",
            &LabelOptions {
                mode: LabelMode::Engrave,
                ..o.clone()
            },
            p,
            Vec3::X,
            Vec3::Z,
        );
        let b = e.bounds();
        assert!((b.min.x - (10.0 - o.depth)).abs() < 1e-4 && (b.max.x - (10.0 + o.overlap)).abs() < 1e-4);
        assert!(e.signed_volume() > 0.0);
        // degenerate up vector
        let t = place_label("A", &o, Vec3::ZERO, Vec3::Z, Vec3::Z);
        assert!(analyze(&t).is_clean());
        assert!((t.bounds().max.z - o.depth).abs() < 1e-4);
    }
}
