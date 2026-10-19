//! Thumbnails: storage, conversions and a tiny software renderer.

use image::{imageops::FilterType, RgbaImage};
use openresin_core::{Mat4, Mesh, Vec3};

/// RGBA thumbnail image.
#[derive(Debug, Clone, PartialEq)]
pub struct Thumbnail {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Thumbnail {
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Self {
        assert_eq!(rgba.len(), (width * height * 4) as usize);
        Self { width, height, rgba }
    }

    pub fn from_png(data: &[u8]) -> crate::Result<Self> {
        let img = image::load_from_memory(data)?.to_rgba8();
        Ok(Self::new(img.width(), img.height(), img.into_raw()))
    }

    pub fn to_png(&self) -> crate::Result<Vec<u8>> {
        let img = RgbaImage::from_raw(self.width, self.height, self.rgba.clone()).expect("size checked");
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png)?;
        Ok(out.into_inner())
    }

    /// Scales to fit into `w` x `h` keeping the aspect ratio, centred on a black background.
    pub fn fit(&self, w: u32, h: u32) -> Thumbnail {
        let src = RgbaImage::from_raw(self.width, self.height, self.rgba.clone()).expect("size checked");
        let scale = (w as f32 / self.width as f32).min(h as f32 / self.height as f32);
        let nw = ((self.width as f32 * scale).round() as u32).clamp(1, w);
        let nh = ((self.height as f32 * scale).round() as u32).clamp(1, h);
        let scaled = image::imageops::resize(&src, nw, nh, FilterType::Triangle);
        let mut out = RgbaImage::from_pixel(w, h, image::Rgba([0, 0, 0, 255]));
        image::imageops::overlay(&mut out, &scaled, ((w - nw) / 2) as i64, ((h - nh) / 2) as i64);
        Thumbnail::new(w, h, out.into_raw())
    }

    /// RGB565 pixels (alpha composited on black).
    pub fn to_rgb565(&self) -> Vec<u16> {
        self.rgba
            .chunks_exact(4)
            .map(|p| {
                let a = p[3] as u32;
                let c = |v: u8| (v as u32 * a / 255) as u16;
                ((c(p[0]) >> 3) << 11) | ((c(p[1]) >> 2) << 5) | (c(p[2]) >> 3)
            })
            .collect()
    }

    pub fn from_rgb565(width: u32, height: u32, px: &[u16]) -> Self {
        let mut rgba = Vec::with_capacity(px.len() * 4);
        for &v in px {
            let r = ((v >> 11) & 0x1f) as u8;
            let g = ((v >> 5) & 0x3f) as u8;
            let b = (v & 0x1f) as u8;
            rgba.extend_from_slice(&[(r << 3) | (r >> 2), (g << 2) | (g >> 4), (b << 3) | (b >> 2), 255]);
        }
        Self::new(width, height, rgba)
    }
}

/// Renders a shaded isometric-ish view of the meshes (plate coordinates).
pub fn render_meshes(meshes: &[&Mesh], width: u32, height: u32) -> Thumbnail {
    let mut bounds = openresin_core::geometry::Aabb::EMPTY;
    for m in meshes {
        bounds = bounds.union(&m.bounds());
    }
    let mut rgba = vec![0u8; (width * height * 4) as usize];
    for px in rgba.chunks_exact_mut(4) {
        px.copy_from_slice(&[24, 26, 32, 255]);
    }
    if bounds.is_empty() {
        return Thumbnail::new(width, height, rgba);
    }
    // camera: rotate around Z by -35°, tilt by 55°
    let view = Mat4::from_rotation_x(-55f32.to_radians()) * Mat4::from_rotation_z(-35f32.to_radians());
    let c = bounds.center();
    let mut vb = openresin_core::geometry::Aabb::EMPTY;
    for m in meshes {
        for p in &m.positions {
            vb.extend(view.transform_point3(*p - c));
        }
    }
    let size = vb.size();
    let scale = 0.9 * (width as f32 / size.x.max(1e-3)).min(height as f32 / size.y.max(1e-3));
    let vc = vb.center();
    let project = |p: Vec3| {
        let v = view.transform_point3(p - c) - vc;
        Vec3::new(width as f32 * 0.5 + v.x * scale, height as f32 * 0.5 - v.y * scale, v.z)
    };
    let light = Vec3::new(0.3, -0.5, 0.8).normalize();
    let mut depth = vec![f32::NEG_INFINITY; (width * height) as usize];
    for m in meshes {
        for i in 0..m.triangle_count() {
            let [a, b, cc] = m.tri(i);
            let n = (b - a).cross(cc - a).normalize_or_zero();
            let vn = view.transform_vector3(n);
            if vn.z <= 0.0 {
                continue;
            }
            let shade = 0.35 + 0.65 * vn.dot(light).max(0.0);
            let col = [
                (90.0 + 140.0 * shade) as u8,
                (150.0 + 90.0 * shade) as u8,
                (200.0 + 55.0 * shade) as u8,
            ];
            let (p0, p1, p2) = (project(a), project(b), project(cc));
            let minx = p0.x.min(p1.x).min(p2.x).floor().max(0.0) as i32;
            let maxx = p0.x.max(p1.x).max(p2.x).ceil().min(width as f32 - 1.0) as i32;
            let miny = p0.y.min(p1.y).min(p2.y).floor().max(0.0) as i32;
            let maxy = p0.y.max(p1.y).max(p2.y).ceil().min(height as f32 - 1.0) as i32;
            let area = (p1.x - p0.x) * (p2.y - p0.y) - (p2.x - p0.x) * (p1.y - p0.y);
            if area.abs() < 1e-9 {
                continue;
            }
            for y in miny..=maxy {
                for x in minx..=maxx {
                    let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                    let w0 = ((p1.x - px) * (p2.y - py) - (p2.x - px) * (p1.y - py)) / area;
                    let w1 = ((p2.x - px) * (p0.y - py) - (p0.x - px) * (p2.y - py)) / area;
                    let w2 = 1.0 - w0 - w1;
                    if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                        continue;
                    }
                    let z = w0 * p0.z + w1 * p1.z + w2 * p2.z;
                    let k = (y as u32 * width + x as u32) as usize;
                    if z > depth[k] {
                        depth[k] = z;
                        rgba[k * 4..k * 4 + 3].copy_from_slice(&col);
                    }
                }
            }
        }
    }
    Thumbnail::new(width, height, rgba)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openresin_core::primitives::cuboid;

    #[test]
    fn render_and_convert() {
        let m = cuboid(Vec3::ZERO, Vec3::splat(10.0));
        let t = render_meshes(&[&m], 64, 48);
        let lit = t.rgba.chunks_exact(4).filter(|p| p[2] > 150).count();
        assert!(lit > 200, "{lit}");
        let f = t.fit(116, 116);
        assert_eq!((f.width, f.height), (116, 116));
        let back = Thumbnail::from_rgb565(116, 116, &f.to_rgb565());
        assert_eq!(back.rgba.len(), f.rgba.len());
        let png = t.to_png().unwrap();
        assert_eq!(Thumbnail::from_png(&png).unwrap(), t);
    }
}
