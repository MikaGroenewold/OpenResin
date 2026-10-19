//! Feeds broken meshes (as found in real STL files) through every geometry
//! operation: nothing may panic.

use glam::{Vec2, Vec3};

use crate::cut::{cut, CutPlane};
use crate::hollow::{hollow, HollowOptions};
use crate::label::{place_label, LabelOptions};
use crate::mesh::Mesh;
use crate::orient::{rank_orientations, OrientGoal, OrientOptions};
use crate::primitives::{base_plate, base_plate_under, cuboid, sphere, BasePlateOptions};
use crate::repair::{analyze, repair, RepairOptions};
use crate::slicer::{RasterOptions, Slicer};
use crate::split::split_shells;
use crate::support::{auto_points, build, SupportSettings};

fn nasty_meshes() -> Vec<(&'static str, Mesh)> {
    let cube = cuboid(Vec3::new(0.0, 0.0, 5.0), Vec3::splat(10.0));
    let mut open = cube.clone();
    open.triangles.truncate(7);
    let mut dup = cube.clone();
    dup.triangles.extend(cube.triangles.clone());
    let mut flipped_half = cube.clone();
    for t in flipped_half.triangles.iter_mut().step_by(2) {
        t.swap(1, 2);
    }
    let collinear = Mesh::new(
        (0..6).map(|i| Vec3::new(i as f32, 0.0, 0.0)).collect(),
        vec![[0, 1, 2], [2, 3, 4], [1, 3, 5]],
    );
    let mut nonmanifold = cube.clone();
    nonmanifold.append(&cuboid(Vec3::new(10.0, 10.0, 5.0), Vec3::splat(10.0)));
    let nonmanifold = repair(
        &nonmanifold,
        &RepairOptions {
            fill_holes: false,
            ..Default::default()
        },
    )
    .0;
    let mut fin = cube.clone();
    // a zero-thickness fin sticking out of a face (edge shared by 3 faces)
    let a = fin.positions.len() as u32;
    fin.positions.push(Vec3::new(5.0, 0.0, 15.0));
    fin.triangles.push([6, 7, a]);
    vec![
        ("empty", Mesh::default()),
        (
            "single triangle",
            Mesh::new(vec![Vec3::ZERO, Vec3::X, Vec3::Y], vec![[0, 1, 2]]),
        ),
        (
            "nan vertex",
            Mesh::new(
                vec![Vec3::ZERO, Vec3::new(f32::NAN, 1.0, 0.0), Vec3::Y, Vec3::Z],
                vec![[0, 1, 2], [0, 2, 3]],
            ),
        ),
        (
            "bad index",
            Mesh::new(vec![Vec3::ZERO, Vec3::X, Vec3::Y], vec![[0, 1, 7], [0, 1, 2]]),
        ),
        ("collinear", collinear),
        ("open", open),
        ("duplicated", dup),
        ("inconsistent", flipped_half),
        ("non-manifold", nonmanifold),
        ("fin", fin),
        ("unwelded soup", {
            let s = sphere(Vec3::new(0.0, 0.0, 6.0), 5.0, 12, 6);
            let tris: Vec<[Vec3; 3]> = (0..s.triangle_count()).map(|i| s.tri(i)).collect();
            let mut m = Mesh::default();
            for t in tris {
                let b = m.positions.len() as u32;
                m.positions.extend_from_slice(&t);
                m.triangles.push([b, b + 1, b + 2]);
            }
            m
        }),
    ]
}

#[test]
fn geometry_operations_never_panic() {
    let ropts = RasterOptions {
        width: 64,
        height: 64,
        size_x: 64.0,
        size_y: 64.0,
        mirror_x: false,
        mirror_y: false,
        anti_aliasing: 4,
    };
    for (name, m) in nasty_meshes() {
        let _ = analyze(&m);
        let (fixed, report) = repair(&m, &RepairOptions::default());
        assert!(fixed.positions.iter().all(|p| p.is_finite()), "{name}");
        assert!(
            fixed
                .triangles
                .iter()
                .flatten()
                .all(|&i| (i as usize) < fixed.positions.len()),
            "{name}"
        );
        let _ = serde_json::to_string(&report).unwrap();
        let _ = split_shells(&m);
        let _ = cut(&m, &CutPlane::horizontal(4.0));
        let _ = cut(&m, &CutPlane::new(Vec3::new(0.1, 0.2, 3.0), Vec3::new(1.0, 2.0, 3.0)));
        let _ = hollow(
            &m,
            &HollowOptions {
                wall_thickness: 1.0,
                ..Default::default()
            },
        );
        for goal in [
            OrientGoal::Balanced,
            OrientGoal::FewestSupports,
            OrientGoal::LowestHeight,
        ] {
            let _ = rank_orientations(
                &m,
                &OrientOptions {
                    goal,
                    candidates: 32,
                    ..Default::default()
                },
            );
        }
        let _ = base_plate_under(&m, &BasePlateOptions::default());
        if m.positions.iter().all(|p| p.is_finite())
            && m.triangles.iter().flatten().all(|&i| (i as usize) < m.positions.len())
        {
            let s = SupportSettings {
                cross_bracing: true,
                ..Default::default()
            };
            let pts = auto_points(&m, &s);
            let _ = build(&m, &pts, &s);
            let neg = cuboid(Vec3::new(0.0, 0.0, 5.0), Vec3::splat(4.0));
            let sl = Slicer::with_negatives(&[&m], &[&neg, &m], 0.5, ropts);
            for i in 0..sl.layer_count().min(12) {
                let _ = sl.rasterize(i);
            }
        }
        // repaired output is safe for everything as well
        let _ = cut(&fixed, &CutPlane::horizontal(4.0));
        let _ = hollow(&fixed, &HollowOptions::default());
    }
    let _ = base_plate(&[Vec2::new(f32::NAN, 0.0)], &BasePlateOptions::default());
    let _ = place_label("", &LabelOptions::default(), Vec3::ZERO, Vec3::ZERO, Vec3::ZERO);
    let _ = place_label(
        "x",
        &LabelOptions {
            height: f32::NAN,
            ..Default::default()
        },
        Vec3::ZERO,
        Vec3::Z,
        Vec3::Y,
    );
}

#[test]
fn repaired_open_mesh_can_be_cut_watertight() {
    // end-to-end: broken input -> repair -> cut -> both halves watertight
    let mut m = sphere(Vec3::new(0.0, 0.0, 10.0), 8.0, 32, 16);
    m.triangles.retain(|t| !t.contains(&0)); // hole at the bottom pole
    m.triangles[10].swap(1, 2);
    let (fixed, rep) = repair(&m, &RepairOptions::default());
    assert!(rep.after.is_clean(), "{:?}", rep.after);
    let r = cut(
        &fixed,
        &CutPlane::new(Vec3::new(0.0, 0.0, 9.0), Vec3::new(0.2, 0.1, 1.0)),
    )
    .unwrap();
    for half in [&r.below, &r.above] {
        let a = analyze(half);
        assert!(a.watertight, "{a:?}");
        assert_eq!(a.inverted_shells, 0);
    }
    let total = r.below.signed_volume() + r.above.signed_volume();
    assert!((total - fixed.signed_volume()).abs() < 1e-3 * fixed.signed_volume());
}
