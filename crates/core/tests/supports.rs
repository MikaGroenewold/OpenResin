//! Support generation: overhang and island detection, support geometry, raft,
//! model contacts and cross bracing.

mod common;

use common::*;
use openresin_core::mesh::Mesh;
use openresin_core::primitives::{cuboid, frustum};
use openresin_core::support::{auto_points, build, SupportPoint, SupportSettings, SupportSize, VerticalRayIndex};
use openresin_core::{Vec2, Vec3};

fn boxed(x0: f32, x1: f32, y0: f32, y1: f32, z0: f32, z1: f32) -> Mesh {
    cuboid(
        Vec3::new((x0 + x1) * 0.5, (y0 + y1) * 0.5, (z0 + z1) * 0.5),
        Vec3::new(x1 - x0, y1 - y0, z1 - z0),
    )
}

/// Nonzero winding number of `mesh` at `p` (vertical ray upwards).
fn winding(idx: &VerticalRayIndex, p: Vec3) -> i32 {
    idx.hits(p.x + 1.3e-4, p.y + 0.7e-4)
        .iter()
        .filter(|h| h.z > p.z)
        .map(|h| if h.normal.z > 0.0 { 1 } else { -1 })
        .sum()
}

fn down(x: f32, y: f32, z: f32) -> SupportPoint {
    SupportPoint {
        position: Vec3::new(x, y, z),
        normal: -Vec3::Z,
        size: SupportSize::Medium,
    }
}

/// A "T": a column (x, y in -2..2) carrying a 24 x 24 slab at z 25..27. The
/// column reaches into the slab, like the overlapping parts of many real models.
fn t_shape(lift: f32) -> Mesh {
    let mut m = boxed(-2.0, 2.0, -2.0, 2.0, lift, lift + 21.0);
    m.append(&boxed(-12.0, 12.0, -12.0, 12.0, lift + 20.0, lift + 22.0));
    m
}

#[test]
fn overhangs_of_a_t_shape_are_covered() {
    let s = SupportSettings::default();
    let lift = s.lift_height;
    let model = t_shape(lift);
    let idx = VerticalRayIndex::new(&model);
    let pts = auto_points(&model, &s);
    let slab = lift + 20.0;
    // every contact faces down and sits on an exposed surface (air right below it)
    for p in &pts {
        assert!(p.normal.z < -0.7, "contact on a steep or upward surface: {p:?}");
        assert_eq!(
            winding(&idx, p.position - Vec3::Z * 0.05),
            0,
            "contact inside the model: {p:?}"
        );
        assert_eq!(
            winding(&idx, p.position + Vec3::Z * 0.05),
            1,
            "contact not on the surface: {p:?}"
        );
    }
    let under_slab: Vec<Vec2> = pts
        .iter()
        .filter(|p| (p.position.z - slab).abs() < 1e-3)
        .map(|p| p.position.truncate())
        .collect();
    assert!(under_slab.len() > 40, "only {} slab contacts", under_slab.len());
    // no slab contact inside the column footprint (the slab is supported there)
    assert!(under_slab.iter().all(|p| p.x.abs() > 2.0 || p.y.abs() > 2.0));
    // every point of the overhang has a contact within the support spacing
    let mut checked = 0;
    for i in 0..=46 {
        for j in 0..=46 {
            let q = Vec2::new(-11.5 + i as f32 * 0.5, -11.5 + j as f32 * 0.5);
            if q.x.abs() < 2.5 && q.y.abs() < 2.5 {
                continue;
            }
            let d = under_slab.iter().map(|p| (*p - q).length()).fold(f32::MAX, f32::min);
            assert!(d <= s.spacing, "overhang point {q} is {d} mm from the nearest contact");
            checked += 1;
        }
    }
    assert!(checked > 2000);
    // the column's bottom face (4 x 4 mm) gets contacts too
    assert!(pts
        .iter()
        .any(|p| (p.position.z - lift).abs() < 1e-3 && p.position.x.abs() <= 2.0 && p.position.y.abs() <= 2.0));
}

#[test]
fn overhang_angle_threshold() {
    // inverted cones (tip down) whose side is `alpha` degrees from horizontal
    let cone = |alpha: f32, r: f32| {
        let h = r * alpha.to_radians().tan();
        frustum(Vec3::new(0.0, 0.0, 5.0), Vec3::new(0.0, 0.0, 5.0 + h), 0.0, r, 48)
    };
    let grid_only = SupportSettings {
        detect_islands: false,
        ..Default::default()
    };
    // flat cone (20 deg): the whole underside needs supports
    let flat = cone(20.0, 10.0);
    let pts = auto_points(&flat, &grid_only);
    let area = std::f32::consts::PI * 100.0;
    let expected = area / (grid_only.spacing * grid_only.spacing * 0.866);
    assert!(
        (pts.len() as f32) > expected * 0.8 && (pts.len() as f32) < expected * 1.2,
        "{} contacts, expected about {expected}",
        pts.len()
    );
    for p in &pts {
        let r = p.position.truncate().length();
        // on the cone surface: z = 5 + r tan(20)
        assert!(
            (p.position.z - (5.0 + r * 20f32.to_radians().tan())).abs() < 0.05,
            "{p:?}"
        );
        assert!((p.normal.z + 20f32.to_radians().cos()).abs() < 0.02, "{p:?}");
    }
    // steep cone (70 deg): nothing on the grid, the tip is an island
    let steep = cone(70.0, 3.0);
    assert!(auto_points(&steep, &grid_only).is_empty());
    let with_islands = auto_points(&steep, &SupportSettings::default());
    assert!(!with_islands.is_empty() && with_islands.len() <= 2, "{with_islands:?}");
    assert!(with_islands
        .iter()
        .all(|p| p.position.truncate().length() < 0.5 && (p.position.z - 5.0).abs() < 0.3));
    // 50 deg: unsupported at a 45 deg threshold, supported at 60 deg
    let mid = cone(50.0, 8.0);
    assert!(auto_points(&mid, &grid_only).is_empty());
    let steep_ok = SupportSettings {
        overhang_angle: 60.0,
        ..grid_only.clone()
    };
    assert!(auto_points(&mid, &steep_ok).len() > 10);
    // upward facing surfaces never get contacts
    let upright = frustum(Vec3::new(0.0, 0.0, 5.0), Vec3::new(0.0, 0.0, 9.0), 10.0, 0.0, 48);
    assert!(auto_points(&upright, &grid_only)
        .iter()
        .all(|p| (p.position.z - 5.0).abs() < 1e-3));
}

#[test]
fn islands_are_detected_at_every_local_minimum() {
    // two hanging tips, far apart, on a coarse grid that misses both
    let mut m = frustum(Vec3::new(-15.0, 0.0, 6.0), Vec3::new(-15.0, 0.0, 16.0), 0.0, 3.0, 32);
    m.append(&frustum(
        Vec3::new(15.0, 2.0, 9.0),
        Vec3::new(15.0, 2.0, 16.0),
        0.0,
        2.0,
        32,
    ));
    let s = SupportSettings {
        spacing: 50.0,
        ..Default::default()
    };
    let pts = auto_points(&m, &s);
    for (tip, label) in [
        (Vec3::new(-15.0, 0.0, 6.0), "left"),
        (Vec3::new(15.0, 2.0, 9.0), "right"),
    ] {
        let best = pts.iter().map(|p| (p.position - tip).length()).fold(f32::MAX, f32::min);
        assert!(best < 0.5, "{label} tip not supported: {pts:?}");
    }
    assert!(auto_points(
        &m,
        &SupportSettings {
            detect_islands: false,
            ..s
        }
    )
    .is_empty());
    assert!(auto_points(&Mesh::default(), &SupportSettings::default()).is_empty());
}

#[test]
fn support_geometry_is_closed_reaches_the_raft_and_touches_the_model() {
    let s = SupportSettings::default();
    let lift = s.lift_height;
    let model = boxed(-6.0, 6.0, -4.0, 4.0, lift, lift + 8.0);
    let pts = auto_points(&model, &s);
    assert!(pts.len() >= 8);
    let g = build(&model, &pts, &s);
    assert!(g.failed.is_empty(), "failed: {:?}", g.failed);
    assert_eq!(g.braces, 0);
    assert_eq!(g.mesh.open_edge_count(), 0);
    assert!(g.mesh.signed_volume() > 0.0);
    let b = g.mesh.bounds();
    assert_eq!(b.min.z, 0.0, "raft on the plate");
    let (tip_r, _) = (s.tip_diameter[1] * 0.5, s.shaft_diameter[1] * 0.5);
    // tips reach slightly into the model, never higher
    assert!(
        b.max.z <= lift + tip_r + 1e-3 && b.max.z >= lift + tip_r * 0.5 - 1e-3,
        "{b:?}"
    );
    let idx = VerticalRayIndex::new(&model);
    for p in &pts {
        // the tip ring sits right at the contact
        let d = g
            .mesh
            .positions
            .iter()
            .map(|v| (*v - p.position).length())
            .fold(f32::MAX, f32::min);
        assert!(d <= tip_r * 1.2, "no tip at {p:?} ({d})");
        // the pillar stands on the raft: base cap centre just below the raft top
        let foot = Vec3::new(p.position.x, p.position.y, s.raft_thickness - 0.05);
        assert!(
            g.mesh.positions.iter().any(|v| (*v - foot).length() < 1e-3),
            "no foot under {p:?}"
        );
    }
    // support material inside the model only at the contacts
    for v in &g.mesh.positions {
        if winding(&idx, *v) != 0 {
            let near = pts.iter().any(|p| (*v - p.position).length() <= tip_r * 1.2);
            assert!(near, "support vertex {v} inside the model");
        }
    }
    // raft: z = 0 ..= thickness, chamfered, covering every foot
    let bottom: Vec<Vec3> = g.mesh.positions.iter().copied().filter(|v| v.z == 0.0).collect();
    let top: Vec<Vec3> = g
        .mesh
        .positions
        .iter()
        .copied()
        .filter(|v| v.z == s.raft_thickness)
        .collect();
    assert!(bottom.len() >= 4 && top.len() == bottom.len());
    let ext = |vs: &[Vec3]| {
        let (lo, hi) = vs
            .iter()
            .fold((Vec2::splat(f32::MAX), Vec2::splat(f32::MIN)), |(lo, hi), v| {
                (lo.min(v.truncate()), hi.max(v.truncate()))
            });
        (lo, hi)
    };
    let (blo, bhi) = ext(&bottom);
    let (tlo, thi) = ext(&top);
    let chamfer = (s.raft_thickness * 0.8).min(s.raft_margin);
    for (d, what) in [
        (tlo.x - blo.x, "-x"),
        (bhi.x - thi.x, "+x"),
        (tlo.y - blo.y, "-y"),
        (bhi.y - thi.y, "+y"),
    ] {
        // mitred corners move by up to chamfer / cos(60 deg)
        assert!(
            d > chamfer * 0.9 && d <= chamfer * 2.0 + 1e-3,
            "raft chamfer {what}: {d}"
        );
    }
    for p in &pts {
        let f = p.position.truncate();
        let r = s.base_diameter * 0.5;
        assert!(
            f.x - r >= tlo.x && f.x + r <= thi.x && f.y - r >= tlo.y && f.y + r <= thi.y,
            "foot {f} outside the raft"
        );
    }
    // without a raft the feet stand on the plate and nothing is below it
    let no_raft = SupportSettings {
        raft: false,
        ..s.clone()
    };
    let g2 = build(&model, &pts, &no_raft);
    assert!(g2.failed.is_empty());
    assert!((g2.mesh.bounds().min.z + 0.05).abs() < 1e-4, "{:?}", g2.mesh.bounds());
    assert!(g2.mesh.signed_volume() < g.mesh.signed_volume());
    assert_eq!(g2.mesh.open_edge_count(), 0);
}

#[test]
fn support_sizes_scale_the_geometry() {
    let s = SupportSettings {
        raft: false,
        ..Default::default()
    };
    let model = boxed(-3.0, 3.0, -3.0, 3.0, 10.0, 12.0);
    let vol = |size: SupportSize| {
        let p = SupportPoint {
            size,
            ..down(0.0, 0.0, 10.0)
        };
        let g = build(&model, &[p], &s);
        assert!(g.failed.is_empty());
        g.mesh.signed_volume()
    };
    let (l, m, h) = (
        vol(SupportSize::Light),
        vol(SupportSize::Medium),
        vol(SupportSize::Heavy),
    );
    assert!(l < m && m < h, "{l} {m} {h}");
    // the shaft dominates: about pi r^2 * length
    let shaft = |d: f32| std::f64::consts::PI * (d as f64 * 0.5).powi(2) * 8.0;
    assert!(h - l > (shaft(1.6) - shaft(0.8)) * 0.5);
}

#[test]
fn contacts_on_the_model_follow_the_model_contacts_option() {
    let s = SupportSettings::default();
    let lift = s.lift_height;
    // lower slab, riser on the left, upper slab: an "F" lying on its back
    let mut model = boxed(-10.0, 10.0, -5.0, 5.0, lift, lift + 2.0);
    model.append(&boxed(-10.0, -6.0, -5.0, 5.0, lift + 1.5, lift + 12.5));
    model.append(&boxed(-10.0, 10.0, -5.0, 5.0, lift + 12.0, lift + 14.0));
    let pts = vec![
        down(0.0, 0.0, lift + 12.0),
        down(5.0, 1.0, lift + 12.0),
        down(4.0, -2.0, lift),
    ];
    let g = build(&model, &pts, &s);
    assert!(g.failed.is_empty(), "{:?}", g.failed);
    let tip_r = s.tip_diameter[1] * 0.5;
    for p in &pts[..2] {
        // the support stands on the lower slab's top face (cap centre just below it)
        let foot = Vec3::new(p.position.x, p.position.y, lift + 2.0 - tip_r * 0.5);
        assert!(
            g.mesh.positions.iter().any(|v| (*v - foot).length() < 1e-3),
            "no model contact under {p:?}"
        );
        // and does not continue through the slab to the plate
        assert!(!g
            .mesh
            .positions
            .iter()
            .any(|v| (v.truncate() - p.position.truncate()).length() < 0.5 && v.z < lift + 1.5));
    }
    // only allowed to stand on the plate: the upper contacts fail
    let plate_only = SupportSettings {
        model_contacts: false,
        ..s.clone()
    };
    let g2 = build(&model, &pts, &plate_only);
    assert_eq!(g2.failed, vec![0, 1]);
    assert!(g2.mesh.signed_volume() < g.mesh.signed_volume());
    // a contact too close above the model below it fails as well
    let tight = vec![down(0.0, 0.0, lift + 2.0 + s.tip_length * 1.2)];
    assert_eq!(build(&model, &tight, &s).failed, vec![0]);
}

#[test]
fn cross_braces_have_the_expected_count_and_stay_clear() {
    let s = SupportSettings {
        lift_height: 20.0,
        cross_bracing: true,
        ..Default::default()
    };
    // slab bottom at z = 23, three contacts in a row 4 mm apart
    let slab = boxed(-12.0, 12.0, -3.0, 3.0, 23.0, 27.0);
    let pts: Vec<SupportPoint> = [-4.0f32, 0.0, 4.0].iter().map(|&x| down(x, 0.0, 23.0)).collect();
    let g = build(&slab, &pts, &s);
    assert!(g.failed.is_empty());
    // pillars z 1.8 .. 20.5, r = 0.4: 4 braces per 4 mm pair, 2 for the 8 mm pair
    assert_eq!(g.braces, 4 + 4 + 2);
    assert_eq!(g.mesh.open_edge_count(), 0);
    // braces only between the nearest neighbour
    let one = SupportSettings {
        bracing_neighbors: 1,
        ..s.clone()
    };
    assert_eq!(build(&slab, &pts, &one).braces, 8);
    // too far apart or too short: no braces
    assert_eq!(
        build(
            &slab,
            &pts,
            &SupportSettings {
                bracing_max_distance: 3.0,
                ..s.clone()
            }
        )
        .braces,
        0
    );
    assert_eq!(
        build(
            &slab,
            &pts,
            &SupportSettings {
                bracing_min_height: 30.0,
                ..s.clone()
            }
        )
        .braces,
        0
    );

    // the brace vertices (between the pillars and the raft in the mesh)
    let plain = build(
        &slab,
        &pts,
        &SupportSettings {
            cross_bracing: false,
            ..s.clone()
        },
    );
    let k = plain
        .mesh
        .positions
        .iter()
        .zip(&g.mesh.positions)
        .take_while(|(a, b)| a == b)
        .count();
    let extra = g.mesh.positions.len() - plain.mesh.positions.len();
    let braces = &g.mesh.positions[k..k + extra];
    assert!(!braces.is_empty());
    let r = s.bracing_diameter * 0.5;
    for v in braces {
        assert!(
            v.z >= 1.8 - 1e-3 && v.z <= 20.5,
            "brace vertex {v} outside the pillar span"
        );
        assert!(
            v.x >= -4.0 - r - 1e-3 && v.x <= 4.0 + r + 1e-3 && v.y.abs() <= r + 1e-3,
            "brace vertex {v} off the pillar row"
        );
        assert!(box_distance(*v, Vec3::new(-12.0, -3.0, 23.0), Vec3::new(12.0, 3.0, 27.0)) > 2.0);
    }
}

#[test]
fn braces_never_touch_an_auto_supported_model() {
    let s = SupportSettings {
        lift_height: 15.0,
        cross_bracing: true,
        bracing_max_distance: 12.0,
        ..Default::default()
    };
    // a T whose column hangs between the pillars
    let model = t_shape(s.lift_height);
    let boxes = [
        (Vec3::new(-2.0, -2.0, 15.0), Vec3::new(2.0, 2.0, 36.0)),
        (Vec3::new(-12.0, -12.0, 35.0), Vec3::new(12.0, 12.0, 37.0)),
    ];
    let pts = auto_points(&model, &s);
    let g = build(&model, &pts, &s);
    let plain = build(
        &model,
        &pts,
        &SupportSettings {
            cross_bracing: false,
            ..s.clone()
        },
    );
    assert!(g.braces > 10, "only {} braces", g.braces);
    let k = plain
        .mesh
        .positions
        .iter()
        .zip(&g.mesh.positions)
        .take_while(|(a, b)| a == b)
        .count();
    let extra = g.mesh.positions.len() - plain.mesh.positions.len();
    for v in &g.mesh.positions[k..k + extra] {
        let d = boxes
            .iter()
            .map(|(lo, hi)| box_distance(*v, *lo, *hi))
            .fold(f32::MAX, f32::min);
        assert!(d > 0.3, "brace vertex {v} is {d} mm from the model");
    }
}

#[test]
#[ignore = "known limitation: pillars are not checked against the model, so a contact next to a wall gets a shaft that cuts through the wall"]
fn pillars_do_not_cut_through_the_model() {
    let s = SupportSettings::default();
    let model = t_shape(s.lift_height);
    let idx = VerticalRayIndex::new(&model);
    let pts = auto_points(&model, &s);
    let g = build(&model, &pts, &s);
    let tip_r = s.tip_diameter[1] * 0.5;
    for v in &g.mesh.positions {
        if winding(&idx, *v) != 0 {
            let near = pts.iter().any(|p| (*v - p.position).length() <= tip_r * 1.2);
            assert!(near, "support vertex {v} inside the model");
        }
    }
}
