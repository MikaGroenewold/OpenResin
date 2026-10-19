use glam::{Vec2, Vec3};
use serde::{Deserialize, Serialize};

/// Axis aligned bounding box.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Default for Aabb {
    fn default() -> Self {
        Self::EMPTY
    }
}

impl Aabb {
    pub const EMPTY: Aabb = Aabb {
        min: Vec3::splat(f32::INFINITY),
        max: Vec3::splat(f32::NEG_INFINITY),
    };

    pub fn from_points<'a>(points: impl IntoIterator<Item = &'a Vec3>) -> Self {
        let mut b = Self::EMPTY;
        for p in points {
            b.extend(*p);
        }
        b
    }

    pub fn is_empty(&self) -> bool {
        self.min.x > self.max.x || self.min.y > self.max.y || self.min.z > self.max.z
    }

    pub fn extend(&mut self, p: Vec3) {
        self.min = self.min.min(p);
        self.max = self.max.max(p);
    }

    pub fn union(&self, o: &Aabb) -> Aabb {
        Aabb {
            min: self.min.min(o.min),
            max: self.max.max(o.max),
        }
    }

    pub fn size(&self) -> Vec3 {
        if self.is_empty() {
            Vec3::ZERO
        } else {
            self.max - self.min
        }
    }

    pub fn center(&self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    pub fn intersects_xy(&self, o: &Aabb) -> bool {
        self.min.x <= o.max.x && self.max.x >= o.min.x && self.min.y <= o.max.y && self.max.y >= o.min.y
    }
}

/// 2D cross product (z component of the 3D cross product).
#[inline]
pub fn cross2(a: Vec2, b: Vec2) -> f32 {
    a.x * b.y - a.y * b.x
}

/// Convex hull (Andrew's monotone chain). Returns the hull in counter-clockwise order.
pub fn convex_hull(points: &[Vec2]) -> Vec<Vec2> {
    let mut pts: Vec<Vec2> = points.to_vec();
    pts.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    pts.dedup_by(|a, b| (*a - *b).length_squared() < 1e-12);
    if pts.len() < 3 {
        return pts;
    }
    let mut hull: Vec<Vec2> = Vec::with_capacity(pts.len() * 2);
    // lower hull
    for p in &pts {
        while hull.len() >= 2 && cross2(hull[hull.len() - 1] - hull[hull.len() - 2], *p - hull[hull.len() - 2]) <= 0.0 {
            hull.pop();
        }
        hull.push(*p);
    }
    // upper hull; never pop below the lower hull
    let lower_len = hull.len() + 1;
    for p in pts.iter().rev().skip(1) {
        while hull.len() >= lower_len
            && cross2(hull[hull.len() - 1] - hull[hull.len() - 2], *p - hull[hull.len() - 2]) <= 0.0
        {
            hull.pop();
        }
        hull.push(*p);
    }
    hull.pop();
    hull
}

/// Offsets a convex CCW polygon outwards by `d` (mitre joins limited to 2*d).
pub fn offset_convex(poly: &[Vec2], d: f32) -> Vec<Vec2> {
    let n = poly.len();
    if n < 3 {
        return poly.to_vec();
    }
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let prev = poly[(i + n - 1) % n];
        let cur = poly[i];
        let next = poly[(i + 1) % n];
        let n1 = (cur - prev).perp().normalize_or_zero() * -1.0;
        let n2 = (next - cur).perp().normalize_or_zero() * -1.0;
        let bis = (n1 + n2).normalize_or_zero();
        let cos_half = bis.dot(n1).max(0.5);
        out.push(cur + bis * (d / cos_half));
    }
    out
}

/// Signed area of a polygon (positive for CCW).
pub fn polygon_area(poly: &[Vec2]) -> f32 {
    let n = poly.len();
    let mut a = 0.0;
    for i in 0..n {
        a += cross2(poly[i], poly[(i + 1) % n]);
    }
    a * 0.5
}

/// Möller–Trumbore ray/triangle intersection. Returns distance along `dir`.
#[inline]
pub fn ray_triangle(orig: Vec3, dir: Vec3, a: Vec3, b: Vec3, c: Vec3) -> Option<f32> {
    let e1 = b - a;
    let e2 = c - a;
    let p = dir.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    let t = orig - a;
    let u = t.dot(p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = t.cross(e1);
    let v = dir.dot(q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let dist = e2.dot(q) * inv;
    (dist > 0.0).then_some(dist)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hull_of_square_with_inner_point() {
        let pts = [
            Vec2::new(0.0, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(1.0, 1.0),
            Vec2::new(0.0, 1.0),
            Vec2::new(0.5, 0.5),
        ];
        let h = convex_hull(&pts);
        assert_eq!(h.len(), 4);
        assert!((polygon_area(&h) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn offset_square() {
        let sq = [
            Vec2::new(0.0, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(1.0, 1.0),
            Vec2::new(0.0, 1.0),
        ];
        let o = offset_convex(&sq, 1.0);
        assert!((polygon_area(&o) - 9.0).abs() < 1e-4);
    }

    #[test]
    fn ray_hits_triangle() {
        let d = ray_triangle(Vec3::new(0.2, 0.2, -1.0), Vec3::Z, Vec3::ZERO, Vec3::X, Vec3::Y);
        assert!((d.unwrap() - 1.0).abs() < 1e-6);
    }
}
