//! Hollowing and drain holes, automatic orientation, text labels and base
//! plates (pads), checked on the geometry and on the sliced result.

mod common;

use std::f64::consts::PI;
use std::sync::Arc;

use common::*;
use openresin_core::hollow::{drain_hole, hollow, hollow_with_info, HollowOptions};
use openresin_core::label::{place_label, surface_frame, text_mesh, text_size, LabelMode, LabelOptions};
use openresin_core::mesh::Mesh;
use openresin_core::orient::{auto_orient, rank_orientations, OrientGoal, OrientOptions};
use openresin_core::primitives::{base_plate, base_plate_under, cuboid, frustum, BasePlateOptions};
use openresin_core::repair::analyze;
use openresin_core::scene::Scene;
use openresin_core::slicer::Slicer;
use openresin_core::{Mat4, Quat, Vec2, Vec3};

fn boxed(x0: f32, x1: f32, y0: f32, y1: f32, z0: f32, z1: f32) -> Mesh {
    cuboid(
        Vec3::new((x0 + x1) * 0.5, (y0 + y1) * 0.5, (z0 + z1) * 0.5),
        Vec3::new(x1 - x0, y1 - y0, z1 - z0),
    )
}

#[track_caller]
fn assert_watertight(m: &Mesh, what: &str) {
    let r = analyze(m);
    assert!(r.watertight && r.non_manifold_edges == 0, "{what}: {r:?}");
    assert!(m.signed_volume() > 0.0, "{what}: inside out");
}

// ---------------------------------------------------------------- hollow

#[test]
fn hollow_box_walls_have_the_requested_thickness() {
    let m = boxed(-15.0, 15.0, -10.0, 10.0, 0.0, 16.0);
    for wall in [1.5f32, 3.0] {
        let r = hollow_with_info(
            &m,
            &HollowOptions {
                wall_thickness: wall,
                ..Default::default()
            },
        )
        .unwrap();
        assert_watertight(&r.cavity, "cavity");
        let b = r.cavity.bounds();
        let (lo, hi) = (Vec3::new(-15.0, -10.0, 0.0) + wall, Vec3::new(15.0, 10.0, 16.0) - wall);
        for k in 0..3 {
            assert!(
                (b.min[k] - lo[k]).abs() < 0.25 && (b.max[k] - hi[k]).abs() < 0.25,
                "wall {wall}: {b:?}"
            );
        }
        let inner = ((30.0 - 2.0 * wall) * (20.0 - 2.0 * wall) * (16.0 - 2.0 * wall)) as f64;
        assert_rel(r.cavity_volume, inner, 0.05, &format!("cavity volume, wall {wall}"));
        assert_close(r.cavity.signed_volume(), r.cavity_volume, 1e-6, "reported volume");
        // shell material = model - cavity
        let shell = m.signed_volume() - r.cavity_volume;
        assert_rel(shell, 9600.0 - inner, 0.1, &format!("shell volume, wall {wall}"));
        assert!(r.voxel_size <= wall / 5.0 + 1e-6);
        // sliced: a ring of the wall thickness
        let o = raster(400, 400, 80.0, 80.0, 4);
        let s = Slicer::with_negatives(&[&m], &[&r.cavity], 0.25, o);
        let mid = lit_mm2(&s.rasterize(32), &o);
        let ring = 600.0 - ((30.0 - 2.0 * wall) * (20.0 - 2.0 * wall)) as f64;
        // the cavity outline is within ~0.1 mm of the ideal inner offset
        let inner_perimeter = 2.0 * ((30.0 - 2.0 * wall) + (20.0 - 2.0 * wall)) as f64;
        assert_close(
            mid,
            ring,
            inner_perimeter * 0.1 + 1.0,
            &format!("ring area, wall {wall}"),
        );
        assert_close(lit_mm2(&s.rasterize(0), &o), 600.0, 0.01, "solid floor");
    }
}

#[test]
fn hollow_cylinder_volume() {
    let cyl = frustum(Vec3::ZERO, Vec3::new(0.0, 0.0, 20.0), 10.0, 10.0, 96);
    let cav = hollow(
        &cyl,
        &HollowOptions {
            wall_thickness: 2.0,
            ..Default::default()
        },
    )
    .unwrap();
    assert_watertight(&cav, "cylinder cavity");
    assert_rel(cav.signed_volume(), PI * 64.0 * 16.0, 0.05, "cavity volume");
    let b = cav.bounds();
    assert!((b.min.z - 2.0).abs() < 0.25 && (b.max.z - 18.0).abs() < 0.25, "{b:?}");
    assert!((b.size().x - 16.0).abs() < 0.4, "{b:?}");
}

#[test]
fn drain_hole_geometry_and_sliced_connection() {
    let (d, depth) = (3.0f32, 4.0f32);
    let hole = drain_hole(Vec3::new(10.0, 0.0, 8.0), -Vec3::X, d, depth);
    assert_watertight(&hole, "drain hole");
    let segs = ((d * std::f32::consts::PI / 0.25).ceil() as u32).clamp(16, 96);
    let r = d as f64 * 0.5;
    assert_rel(
        hole.signed_volume(),
        ngon_area(segs, r) * (depth as f64 + r),
        1e-4,
        "hole volume",
    );
    let b = hole.bounds();
    // from r outside the surface to `depth` inside
    assert!((b.max.x - 11.5).abs() < 1e-4 && (b.min.x - 6.0).abs() < 1e-4, "{b:?}");
    assert!((b.center().y).abs() < 1e-4 && (b.center().z - 8.0).abs() < 1e-4);
    for bad in [
        drain_hole(Vec3::ZERO, Vec3::ZERO, 2.0, 2.0),
        drain_hole(Vec3::ZERO, Vec3::Z, 0.0, 2.0),
        drain_hole(Vec3::ZERO, Vec3::Z, 2.0, f32::NAN),
        drain_hole(Vec3::splat(f32::INFINITY), Vec3::Z, 2.0, 2.0),
    ] {
        assert!(bad.is_empty());
    }

    // a hollowed box with a side drain hole, as scene cutouts
    let mut scene = Scene::default();
    let id = scene.add("box", boxed(-10.0, 10.0, -10.0, 10.0, 0.0, 16.0));
    {
        let o = scene.get_mut(id).unwrap();
        let cav = hollow(
            &o.mesh,
            &HollowOptions {
                wall_thickness: 2.0,
                ..Default::default()
            },
        )
        .unwrap();
        // local coordinates: the box is centred, z in -8..8, so world z 8 is local 0
        let hole = drain_hole(Vec3::new(10.0, 0.0, 0.0), -Vec3::X, d, depth);
        o.cutouts.push(Arc::new(cav));
        o.cutouts.push(Arc::new(hole));
    }
    let (pos, neg) = scene.slice_volumes();
    assert_eq!((pos.len(), neg.len()), (1, 2));
    let o = raster(400, 400, 80.0, 80.0, 1);
    let posr: Vec<&Mesh> = pos.iter().collect();
    let negr: Vec<&Mesh> = neg.iter().collect();
    let s = Slicer::with_negatives(&posr, &negr, 0.1, o);
    let g = s.rasterize(80).to_gray(); // z = 8.05, through the hole
    let at = |x: f32, y: f32| {
        let (px_, py_) = o.to_pixel(x, y);
        px(&g, 400, px_ as u32, py_ as u32)
    };
    assert_eq!(at(9.0, 0.0), 0, "hole through the +x wall");
    assert_eq!(at(9.0, 4.0), 255, "wall next to the hole");
    assert_eq!(at(-9.0, 0.0), 255, "opposite wall intact");
    assert_eq!(at(0.0, 0.0), 0, "cavity");
    let below = s.rasterize(40).to_gray(); // z = 4.05: wall all around
    let (hx, hy) = o.to_pixel(9.0, 0.0);
    assert_eq!(px(&below, 400, hx as u32, hy as u32), 255);
}

#[test]
fn too_thin_or_invalid_models_are_not_hollowed() {
    let plate = boxed(-20.0, 20.0, -20.0, 20.0, 0.0, 3.5);
    assert!(hollow(
        &plate,
        &HollowOptions {
            wall_thickness: 2.0,
            ..Default::default()
        }
    )
    .unwrap()
    .is_empty());
    assert!(hollow(
        &plate,
        &HollowOptions {
            wall_thickness: -1.0,
            ..Default::default()
        }
    )
    .is_err());
    assert!(hollow(
        &plate,
        &HollowOptions {
            wall_thickness: f32::NAN,
            ..Default::default()
        }
    )
    .is_err());
    let mut bad = plate.clone();
    bad.triangles.push([0, 1, 99]);
    assert!(hollow(&bad, &HollowOptions::default()).is_err());
}

// ---------------------------------------------------------------- orient

fn rotated(m: &Mesh, q: Quat) -> Mesh {
    m.transformed(&Mat4::from_quat(q))
}

#[test]
fn lowest_height_minimises_the_print_height() {
    let rod = cuboid(Vec3::ZERO, Vec3::new(40.0, 4.0, 6.0));
    let start = Quat::from_euler(glam::EulerRot::XYZ, 0.9, -0.3, 0.5);
    let m = rotated(&rod, start);
    let o = OrientOptions {
        goal: OrientGoal::LowestHeight,
        ..Default::default()
    };
    let ranked = rank_orientations(&m, &o);
    let best = ranked[0];
    let min_height = ranked.iter().map(|r| r.height).fold(f32::MAX, f32::min);
    assert!(best.height <= min_height + 0.05, "{} vs {min_height}", best.height);
    let q = auto_orient(&m, &o);
    assert!(
        (rotated(&m, q).bounds().size().z - 4.0).abs() < 0.05,
        "{}",
        rotated(&m, q).bounds().size().z
    );
    // the reported rotation maps `down` to -Z and the reported height matches
    assert!((best.rotation * best.down + Vec3::Z).length() < 1e-4);
    assert!((rotated(&m, best.rotation).bounds().size().z - best.height).abs() < 1e-3);
}

#[test]
fn fewest_supports_maximises_flat_contact() {
    let slab = cuboid(Vec3::ZERO, Vec3::new(30.0, 20.0, 5.0));
    let m = rotated(&slab, Quat::from_euler(glam::EulerRot::XYZ, 0.4, 0.7, 0.2));
    let o = OrientOptions {
        goal: OrientGoal::FewestSupports,
        ..Default::default()
    };
    let best = rank_orientations(&m, &o)[0];
    assert!(best.supports < 0.5, "{best:?}");
    assert!((best.contact_area - 600.0).abs() < 6.0, "{best:?}");
    let r = rotated(&m, best.rotation);
    let b = r.bounds();
    assert!((b.size().z - 5.0).abs() < 0.05);
    assert!((area_at_z(&r, b.min.z, 0.05) - 600.0).abs() < 6.0);
    // an L bracket rests on its long leg (largest face), not on its side
    let l = l_bracket();
    assert!(analyze(&l).is_clean());
    assert_close(l.signed_volume(), 156.0 * 20.0, 1e-3, "L volume");
    let lm = rotated(&l, Quat::from_euler(glam::EulerRot::XYZ, 2.0, 0.3, -1.0));
    let best = rank_orientations(&lm, &o)[0];
    assert!(
        best.supports < 0.5 && (best.contact_area - 800.0).abs() < 8.0,
        "{best:?}"
    );
    assert!((best.height - 15.0).abs() < 0.05, "{best:?}");
}

/// A single closed shell: L profile (40 x 3 leg, 3 x 15 upright) in the XZ
/// plane, extruded 20 mm along Y.
fn l_bracket() -> Mesh {
    let prof = [
        (0.0f32, 0.0f32),
        (40.0, 0.0),
        (40.0, 3.0),
        (3.0, 3.0),
        (3.0, 15.0),
        (0.0, 15.0),
    ];
    let n = prof.len() as u32;
    let mut m = Mesh::default();
    for y in [0.0f32, 20.0] {
        m.positions.extend(prof.iter().map(|&(x, z)| Vec3::new(x, y, z)));
    }
    // caps: the profile is star shaped around its first vertex
    for k in 1..n - 1 {
        m.triangles.push([0, k, k + 1]);
        m.triangles.push([n, n + k + 1, n + k]);
    }
    for k in 0..n {
        let j = (k + 1) % n;
        m.triangles.push([k, n + j, j]);
        m.triangles.push([k, n + k, n + j]);
    }
    m
}

#[test]
#[ignore = "known limitation: orientation scoring counts internal faces of touching/overlapping shells as overhangs"]
fn fewest_supports_ignores_internal_faces_of_touching_shells() {
    // the same L built from two touching boxes: the upright's bottom face is internal
    let mut l = boxed(0.0, 40.0, 0.0, 20.0, 0.0, 3.0);
    l.append(&boxed(0.0, 3.0, 0.0, 20.0, 3.0, 15.0));
    let lm = rotated(&l, Quat::from_euler(glam::EulerRot::XYZ, 2.0, 0.3, -1.0));
    let o = OrientOptions {
        goal: OrientGoal::FewestSupports,
        ..Default::default()
    };
    let best = rank_orientations(&lm, &o)[0];
    assert!(
        best.supports < 0.5 && (best.contact_area - 800.0).abs() < 8.0,
        "{best:?}"
    );
}

#[test]
fn balanced_tilts_flat_surfaces_off_the_plate() {
    let slab = cuboid(Vec3::ZERO, Vec3::new(30.0, 20.0, 5.0));
    let o = OrientOptions::default();
    assert_eq!(o.goal, OrientGoal::Balanced);
    let best = rank_orientations(&slab, &o)[0];
    // no large face parallel to the plate
    assert!(best.flat_area < 0.05 * 600.0, "{best:?}");
    let r = rotated(&slab, best.rotation);
    for i in 0..r.triangle_count() {
        let tilt = r.face_normal(i).z.abs().acos().to_degrees();
        assert!(tilt > 10.0, "face {i} only {tilt} deg from horizontal");
    }
    assert!(rank_orientations(&Mesh::default(), &o).is_empty());
}

// ---------------------------------------------------------------- labels

/// Pixels of "HI" in the built-in 5x7 font: H = 17, I = 11.
const HI_PIXELS: f64 = 28.0;

#[test]
fn label_meshes_are_closed_and_placed_on_the_surface() {
    let o = LabelOptions {
        height: 7.0,
        ..Default::default()
    };
    let s = o.height / 7.0;
    let size = text_size("HI", o.height);
    assert_eq!(size, Vec2::new(11.0, 7.0) * s);
    let flat = text_mesh("HI", o.height, 1.0);
    assert_close(flat.signed_volume(), HI_PIXELS, 1e-4, "text volume");
    // on a tilted surface: every vertex within [-overlap, depth] of the surface plane
    let n = Vec3::new(1.0, 0.0, 1.0).normalize();
    let p = Vec3::new(3.0, -2.0, 7.0);
    for (mode, lo, hi) in [
        (LabelMode::Emboss, -o.overlap, o.depth),
        (LabelMode::Engrave, -o.depth, o.overlap),
    ] {
        let m = place_label("HI", &LabelOptions { mode, ..o.clone() }, p, n, Vec3::Z);
        assert!(analyze(&m).is_clean(), "{mode:?}");
        assert_close(
            m.signed_volume(),
            HI_PIXELS * (o.depth + o.overlap) as f64,
            1e-3,
            "label volume",
        );
        let ds: Vec<f32> = m.positions.iter().map(|v| (*v - p).dot(n)).collect();
        let (dmin, dmax) = ds.iter().fold((f32::MAX, f32::MIN), |a, &d| (a.0.min(d), a.1.max(d)));
        assert!(
            (dmin - lo).abs() < 1e-4 && (dmax - hi).abs() < 1e-4,
            "{mode:?}: {dmin}..{dmax}"
        );
        // centred on the clicked point: in label space the text block spans
        // x -5.5..5.5 (the last column of "I" is empty) and y -3.5..3.5
        let inv = surface_frame(p, n, Vec3::Z).inverse();
        let local = m.transformed(&inv).bounds();
        for (got, want) in [
            (local.min.x, -5.5),
            (local.max.x, 4.5),
            (local.min.y, -3.5),
            (local.max.y, 3.5),
        ] {
            assert!((got - want).abs() < 1e-4, "{mode:?}: {local:?}");
        }
    }
    // the frame is a proper rotation with +Z along the normal
    let f = surface_frame(p, n, Vec3::Z);
    assert!((f.determinant() - 1.0).abs() < 1e-5);
    assert!((f.transform_vector3(Vec3::Z) - n).length() < 1e-6);
    assert!((f.transform_point3(Vec3::ZERO) - p).length() < 1e-6);
    // text "up" is the projection of the requested up direction
    let up = f.transform_vector3(Vec3::Y);
    assert!((up - (Vec3::Z - n * n.dot(Vec3::Z)).normalize()).length() < 1e-5);
}

#[test]
fn engraved_label_is_removed_from_the_sliced_layers() {
    let mut scene = Scene::default();
    let id = scene.add("cube", boxed(-10.0, 10.0, -10.0, 10.0, 0.0, 10.0));
    let opts = LabelOptions {
        height: 7.0,
        depth: 0.8,
        overlap: 0.4,
        mode: LabelMode::Engrave,
    };
    {
        let o = scene.get_mut(id).unwrap();
        // local coordinates: the top face is at z = 5
        let label = place_label("HI", &opts, Vec3::new(0.0, 0.0, 5.0), Vec3::Z, Vec3::Y);
        o.cutouts.push(Arc::new(label));
    }
    let (pos, neg) = scene.slice_volumes();
    let o = raster(600, 600, 60.0, 60.0, 4); // 0.1 mm pixels, text pixel edges on pixel boundaries
    let posr: Vec<&Mesh> = pos.iter().collect();
    let negr: Vec<&Mesh> = neg.iter().collect();
    let s = Slicer::with_negatives(&posr, &negr, 0.1, o);
    assert_eq!(s.layer_count(), 100);
    assert_close(lit_mm2(&s.rasterize(95), &o), 400.0 - HI_PIXELS, 1e-3, "engraved layer"); // z = 9.55
    assert_close(lit_mm2(&s.rasterize(90), &o), 400.0, 1e-3, "below the engraving"); // z = 9.05
                                                                                     // embossed on top and on a side face
    let cube = boxed(-10.0, 10.0, -10.0, 10.0, 0.0, 10.0);
    let top = place_label(
        "HI",
        &LabelOptions {
            mode: LabelMode::Emboss,
            ..opts.clone()
        },
        Vec3::new(0.0, 0.0, 10.0),
        Vec3::Z,
        Vec3::Y,
    );
    let side = place_label(
        "HI",
        &LabelOptions {
            mode: LabelMode::Emboss,
            ..opts.clone()
        },
        Vec3::new(10.0, 0.0, 5.0),
        Vec3::X,
        Vec3::Z,
    );
    let s = Slicer::new(&[&cube, &top, &side], 0.1, o);
    assert_eq!(s.layer_count(), 108);
    assert_close(
        lit_mm2(&s.rasterize(103), &o),
        HI_PIXELS,
        1e-3,
        "embossed text above the top",
    ); // z = 10.35
    assert_close(
        lit_mm2(&s.rasterize(98), &o),
        400.0,
        1e-3,
        "overlap does not double count",
    ); // z = 9.85
       // z = 5.05 crosses the middle glyph row: H "11111" + I "00100" = 6 pixels, 0.8 mm proud
    assert_close(lit_mm2(&s.rasterize(50), &o), 400.0 + 6.0 * 0.8, 1e-3, "side label row");
}

#[test]
fn label_edge_cases() {
    let o = LabelOptions::default();
    assert!(place_label("", &o, Vec3::ZERO, Vec3::Z, Vec3::Y).is_empty());
    assert!(place_label(
        "A",
        &LabelOptions {
            depth: 0.0,
            overlap: 0.0,
            ..o.clone()
        },
        Vec3::ZERO,
        Vec3::Z,
        Vec3::Y
    )
    .is_empty());
    // multi-line text: lines are centred, height = 2 lines + gap
    let two = text_size("AB\nC", 7.0);
    assert_eq!(two, Vec2::new(11.0, 16.0));
    let m = text_mesh("AB\nC", 7.0, 1.0);
    assert!(analyze(&m).is_clean());
    let b = m.bounds();
    assert!(
        (b.size().y - 16.0).abs() < 1e-4 && (b.size().x - 11.0).abs() < 1e-4,
        "{b:?}"
    );
    // a zero normal falls back to +Z
    let z = place_label("A", &o, Vec3::ZERO, Vec3::ZERO, Vec3::Y);
    assert!((z.bounds().max.z - o.depth).abs() < 1e-4);
}

// ---------------------------------------------------------------- base plate

#[test]
fn base_plate_covers_the_footprint_with_margin() {
    let o = BasePlateOptions {
        margin: 2.0,
        thickness: 1.5,
        chamfer: 1.0,
        footprint_height: None,
    };
    // a cube rotated 45 deg about Z: diamond footprint
    let cube = cuboid(Vec3::new(5.0, -3.0, 6.5), Vec3::splat(10.0)).transformed(&Mat4::from_rotation_translation(
        Quat::from_rotation_z(std::f32::consts::FRAC_PI_4),
        Vec3::ZERO,
    ));
    let pad = base_plate_under(&cube, &o);
    assert_watertight(&pad, "pad");
    let b = pad.bounds();
    assert_eq!((b.min.z, b.max.z), (0.0, 1.5));
    // the top outline (z = thickness) is the footprint hull grown by the margin
    let top: Vec<Vec2> = pad
        .positions
        .iter()
        .filter(|p| p.z == o.thickness)
        .map(|p| p.truncate())
        .collect();
    let bottom: Vec<Vec2> = pad
        .positions
        .iter()
        .filter(|p| p.z == 0.0)
        .map(|p| p.truncate())
        .collect();
    assert_eq!(top.len(), bottom.len());
    let dist_inside = |poly: &[Vec2], q: Vec2| -> f32 {
        (0..poly.len())
            .map(|i| {
                let (a, c) = (poly[i], poly[(i + 1) % poly.len()]);
                openresin_core::geometry::cross2(c - a, q - a) / (c - a).length()
            })
            .fold(f32::MAX, f32::min)
    };
    let min_top = cube
        .positions
        .iter()
        .map(|p| dist_inside(&top, p.truncate()))
        .fold(f32::MAX, f32::min);
    let min_bottom = cube
        .positions
        .iter()
        .map(|p| dist_inside(&bottom, p.truncate()))
        .fold(f32::MAX, f32::min);
    assert!((min_top - o.margin).abs() < 1e-3, "top margin {min_top}");
    assert!(
        (min_bottom - (o.margin + o.chamfer)).abs() < 1e-3,
        "bottom margin {min_bottom}"
    );
    // frustum of similar squares: side 10 + 2 * 2 on top, + 2 * 3 at the bottom
    let (at, ab) = (14.0f64 * 14.0, 16.0f64 * 16.0);
    assert_close(
        pad.signed_volume(),
        1.5 / 3.0 * (at + ab + (at * ab).sqrt()),
        1e-2,
        "pad volume",
    );
    // a thicker pad with no chamfer is a prism
    let prism = base_plate_under(
        &cube,
        &BasePlateOptions {
            chamfer: 0.0,
            thickness: 3.0,
            ..o.clone()
        },
    );
    assert_close(prism.signed_volume(), 196.0 * 3.0, 1e-2, "prism pad");
}

#[test]
fn base_plate_footprint_height_limits_the_outline() {
    // a cone standing on its tip: only the lowest millimetre defines the pad
    let cone = frustum(Vec3::new(0.0, 0.0, 2.0), Vec3::new(0.0, 0.0, 12.0), 0.0, 10.0, 48);
    let o = BasePlateOptions {
        footprint_height: Some(1.0),
        ..Default::default()
    };
    let pad = base_plate_under(&cone, &o);
    assert_watertight(&pad, "pad");
    // the tip is a single point: a square of +-(margin + chamfer) around it
    let s = pad.bounds().size();
    assert!((s.x - 2.0 * (o.margin + o.chamfer)).abs() < 1e-4, "{s:?}");
    let full = base_plate_under(&cone, &BasePlateOptions::default());
    assert!((full.bounds().size().x - (20.0 + 2.0 * 3.0)).abs() < 0.1);
    assert!(base_plate(&[Vec2::new(f32::NAN, 1.0)], &o).is_empty());
}
