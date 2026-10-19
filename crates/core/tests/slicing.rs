//! Slicer and rasterizer: lit areas, layer counts, unions/holes/negatives,
//! anti-aliasing, mirroring and plate-to-pixel mapping.

mod common;

use std::f64::consts::PI;

use common::*;
use openresin_core::mesh::Mesh;
use openresin_core::primitives::{cuboid, extrude_convex, frustum, sphere};
use openresin_core::profile::{MirrorMode, PrinterProfile};
use openresin_core::slicer::{LayerRaster, Run, Slicer};
use openresin_core::{Vec2, Vec3};

/// Box spanning `[x0,x1] x [y0,y1] x [z0,z1]`.
fn boxed(x0: f32, x1: f32, y0: f32, y1: f32, z0: f32, z1: f32) -> Mesh {
    cuboid(
        Vec3::new((x0 + x1) * 0.5, (y0 + y1) * 0.5, (z0 + z1) * 0.5),
        Vec3::new(x1 - x0, y1 - y0, z1 - z0),
    )
}

#[test]
fn cuboid_layers_have_the_exact_area() {
    // 0.5 mm pixels; edges on pixel boundaries -> exact for every AA level
    let m = boxed(-2.0, 8.0, -5.0, 1.0, 0.0, 4.95);
    for aa in [1u8, 2, 4, 8, 16] {
        let o = raster(200, 200, 100.0, 100.0, aa);
        let s = Slicer::new(&[&m], 0.05, o);
        assert_eq!(s.layer_count(), 99);
        for i in 0..s.layer_count() {
            let r = s.rasterize(i);
            assert_eq!(r.pixel_count(), 200 * 200);
            assert_close(r.lit_area_px(), 240.0, 1e-3, &format!("aa {aa} layer {i}"));
            assert_eq!(r.bbox, Some((96, 98, 116, 110)));
        }
        assert!(s.rasterize(99).is_blank());
    }
}

#[test]
fn off_grid_cuboid_area_is_accurate_with_anti_aliasing() {
    // 10 x 6 mm at fractional pixel positions
    let m = boxed(-1.87, 8.13, -4.37, 1.63, 0.0, 2.0);
    let exact = 10.0 * 6.0 / 0.25; // px
    for aa in [1u8, 2, 4, 8, 16] {
        let o = raster(200, 200, 100.0, 100.0, aa);
        let r = Slicer::new(&[&m], 0.5, o).rasterize(1);
        // vertical edges are exact, horizontal edges are sampled with `aa` sub-rows
        let tol = if aa == 1 {
            2.0 * (20.0 + 12.0) * 0.5
        } else {
            2.0 * 20.0 * 0.5 / aa as f64 + 0.05
        };
        assert_close(r.lit_area_px(), exact, tol, &format!("aa {aa}"));
    }
}

#[test]
fn layer_count_is_ceil_of_height_over_layer_height() {
    for (h, lh, n) in [
        (10.0f32, 0.05f32, 200usize),
        (10.02, 0.05, 201),
        (1.0, 0.03, 34),
        (0.05, 0.05, 1),
        (2.5, 0.1, 25),
        (25.0, 0.025, 1000),
        (0.01, 0.05, 1),
    ] {
        let m = boxed(-1.0, 1.0, -1.0, 1.0, 0.0, h);
        let s = Slicer::new(&[&m], lh, raster(10, 10, 10.0, 10.0, 1));
        assert_eq!(s.layer_count(), n, "height {h} / {lh}");
        assert!((s.layer_position_z(0) - lh).abs() < 1e-7);
        assert!((s.layer_sample_z(0) - lh * 0.5).abs() < 1e-7);
        assert!((s.layer_position_z(n - 1) - lh * n as f32).abs() < 1e-4);
        // whole multiples print every layer
        if (h / lh - (h / lh).round()).abs() < 1e-4 {
            assert!((0..n).all(|i| !s.rasterize(i).is_blank()), "height {h}");
        }
    }
    // geometry below the plate does not add layers
    let below = boxed(-1.0, 1.0, -1.0, 1.0, -5.0, 2.0);
    let s = Slicer::new(&[&below], 0.1, raster(10, 10, 10.0, 10.0, 1));
    assert_eq!(s.layer_count(), 20);
    assert_close(s.rasterize(0).lit_area_px(), 4.0, 1e-6, "layer 0 of a sunk part");
    assert_eq!(Slicer::new(&[], 0.05, raster(10, 10, 10.0, 10.0, 1)).layer_count(), 0);
    let flat = boxed(-1.0, 1.0, -1.0, 1.0, -1.0, 0.0);
    assert_eq!(
        Slicer::new(&[&flat], 0.05, raster(10, 10, 10.0, 10.0, 1)).layer_count(),
        0
    );
}

#[test]
fn cylinder_and_sphere_cross_sections() {
    let o = raster(600, 600, 60.0, 60.0, 4);
    let n = 256;
    let cyl = frustum(Vec3::new(2.3, -1.7, 0.0), Vec3::new(2.3, -1.7, 8.0), 10.0, 10.0, n);
    let s = Slicer::new(&[&cyl], 0.1, o);
    assert_eq!(s.layer_count(), 80);
    let expect = ngon_area(n, 10.0);
    for i in (0..80).step_by(9) {
        assert_rel(
            lit_mm2(&s.rasterize(i), &o),
            expect,
            1e-3,
            &format!("cylinder layer {i}"),
        );
    }
    // sphere: pi (r^2 - d^2) per layer, volume from the layer stack
    let r = 10.0f64;
    let sp = sphere(Vec3::new(-3.0, 4.0, 10.0), r as f32, 128, 64);
    let lh = 0.2f32;
    let s = Slicer::new(&[&sp], lh, o);
    assert_eq!(s.layer_count(), 100);
    let mut vol = 0.0;
    for i in 0..s.layer_count() {
        let a = lit_mm2(&s.rasterize(i), &o);
        vol += a * lh as f64;
        let d = s.layer_sample_z(i) as f64 - 10.0;
        if d.abs() < 8.0 {
            assert_rel(a, PI * (r * r - d * d), 0.02, &format!("sphere layer {i}"));
        }
    }
    assert_rel(vol, 4.0 / 3.0 * PI * r.powi(3), 0.01, "sphere volume from layers");
}

#[test]
fn overlapping_and_nested_meshes_form_a_union() {
    let o = raster(100, 100, 100.0, 100.0, 1);
    let outer = boxed(-10.0, 10.0, -10.0, 10.0, 0.0, 10.0);
    let inner = boxed(-5.0, 5.0, -5.0, 5.0, 2.0, 8.0);
    let other = boxed(5.0, 25.0, -2.0, 8.0, 0.0, 10.0);
    let third = boxed(0.0, 15.0, 0.0, 15.0, 0.0, 10.0);
    // nested solid: no hole, no double counting
    let s = Slicer::new(&[&outer, &inner], 1.0, o);
    let r = s.rasterize(5);
    assert_close(r.lit_area_px(), 400.0, 1e-6, "nested");
    assert_eq!(px(&r.to_gray(), 100, 50, 50), 255);
    // the same mesh twice
    assert_close(
        Slicer::new(&[&outer, &outer], 1.0, o).rasterize(5).lit_area_px(),
        400.0,
        1e-6,
        "duplicate",
    );
    // three overlapping boxes: |A u B u C| by inclusion-exclusion
    let (a, b, c) = (400.0, 200.0, 225.0);
    let ab = 5.0 * 10.0; // x 5..10, y -2..8
    let ac = 10.0 * 10.0; // x 0..10, y 0..10
    let bc = 10.0 * 8.0; // x 5..15, y 0..8
    let abc = 5.0 * 8.0; // x 5..10, y 0..8
    let union = a + b + c - ab - ac - bc + abc;
    let s = Slicer::new(&[&outer, &other, &third], 1.0, o);
    assert_close(s.rasterize(3).lit_area_px(), union, 1e-6, "three boxes");
    // all in one mesh behaves the same
    let mut merged = outer.clone();
    merged.append(&other);
    merged.append(&third);
    assert_eq!(Slicer::new(&[&merged], 1.0, o).rasterize(3), s.rasterize(3));
}

#[test]
fn inverted_inner_shell_makes_a_ring() {
    let o = raster(100, 100, 100.0, 100.0, 1);
    let outer = boxed(-10.0, 10.0, -10.0, 10.0, 0.0, 10.0);
    let mut inner = boxed(-6.0, 4.0, -5.0, 5.0, 2.0, 8.0);
    inner.flip();
    let s = Slicer::new(&[&outer, &inner], 1.0, o);
    let mid = s.rasterize(5);
    assert_close(mid.lit_area_px(), 400.0 - 100.0, 1e-6, "ring");
    let g = mid.to_gray();
    assert_eq!(px(&g, 100, 49, 50), 0, "inside the hole");
    assert_eq!(px(&g, 100, 57, 50), 255, "in the wall");
    // below and above the cavity the layer is solid
    assert_close(s.rasterize(1).lit_area_px(), 400.0, 1e-6, "floor");
    assert_close(s.rasterize(9).lit_area_px(), 400.0, 1e-6, "roof");
}

#[test]
fn negative_volumes_subtract_and_never_add() {
    let o = raster(400, 400, 80.0, 80.0, 4);
    let part = boxed(-10.0, 10.0, -10.0, 10.0, 0.0, 10.0);
    let hole = frustum(Vec3::new(3.0, 2.0, -5.0), Vec3::new(3.0, 2.0, 20.0), 3.0, 3.0, 96);
    let mut hole_inv = hole.clone();
    hole_inv.flip();
    // half inside, half outside the part
    let side = boxed(5.0, 30.0, -12.0, -6.0, 4.0, 6.0);
    for h in [&hole, &hole_inv] {
        let s = Slicer::with_negatives(&[&part], &[h, &side], 0.5, o);
        assert_eq!(s.layer_count(), 20, "negatives do not add layers");
        assert_close(
            lit_mm2(&s.rasterize(2), &o),
            400.0 - ngon_area(96, 3.0),
            0.1,
            "with hole",
        );
        let mid = 400.0 - ngon_area(96, 3.0) - 5.0 * 4.0;
        assert_close(lit_mm2(&s.rasterize(10), &o), mid, 0.1, "with hole and side notch");
        let outline = s.negative_section_mm(10);
        assert!(outline.len() >= 96 + 4);
        assert!(s.has_negatives());
    }
}

#[test]
fn open_and_inconsistent_meshes_still_slice() {
    let o = raster(100, 100, 100.0, 100.0, 1);
    let cube = boxed(-10.0, 10.0, -10.0, 10.0, 0.0, 10.0);
    // missing front face (y = -10): every scanline still crosses two walls
    let mut open = cube.clone();
    open.triangles.drain(4..6);
    assert_eq!(open.open_edge_count(), 4);
    let s = Slicer::new(&[&open], 1.0, o);
    for i in 0..10 {
        assert_close(s.rasterize(i).lit_area_px(), 400.0, 1e-6, &format!("open layer {i}"));
    }
    // left wall flipped: winding does not cancel, even-odd fallback fills the section
    let mut flipped = cube.clone();
    flipped.triangles[10].swap(1, 2);
    flipped.triangles[11].swap(1, 2);
    let s = Slicer::new(&[&flipped], 1.0, o);
    for i in 0..10 {
        assert_close(s.rasterize(i).lit_area_px(), 400.0, 1e-6, &format!("flipped layer {i}"));
    }
}

#[test]
fn anti_aliasing_gives_exact_partial_coverage() {
    // 1 mm pixels: left edge at x = 10.25 px, top edge at y = 29.7 px
    let o1 = raster(100, 100, 100.0, 100.0, 1);
    let m = boxed(-39.75, -20.0, 10.0, 20.3, 0.0, 2.0);
    let g1 = Slicer::new(&[&m], 0.5, o1).rasterize(0).to_gray();
    assert!(g1.iter().all(|&v| v == 0 || v == 255), "no gray without AA");
    for aa in [2u8, 4, 8, 16] {
        let o = raster(100, 100, 100.0, 100.0, aa);
        let r = Slicer::new(&[&m], 0.5, o).rasterize(0);
        let g = r.to_gray();
        // horizontal coverage is exact: 0.75 of pixel 10
        for y in 30..40 {
            assert_eq!(px(&g, 100, 9, y), 0);
            assert_eq!(px(&g, 100, 10, y), 191, "aa {aa} row {y}");
            assert!((11..30).all(|x| px(&g, 100, x, y) == 255));
            assert_eq!(px(&g, 100, 30, y), 0);
        }
        // vertical coverage of row 29 (true 0.3) within one sub-row
        let v = px(&g, 100, 20, 29) as f64 / 255.0;
        assert!((v - 0.3).abs() <= 0.5 / aa as f64 + 1e-2, "aa {aa}: {v}");
        assert_eq!(px(&g, 100, 20, 28), 0);
        // corner pixel combines both
        let corner = px(&g, 100, 10, 29) as f64 / 255.0;
        assert!(
            (corner - 0.75 * v).abs() <= 2.0 / 255.0,
            "aa {aa}: corner {corner} vs {}",
            0.75 * v
        );
        let exact = 19.75 * 10.3;
        assert_close(
            r.lit_area_px(),
            exact,
            19.75 * 0.5 / aa as f64 + 0.05,
            &format!("aa {aa} area"),
        );
    }
}

#[test]
fn anti_aliased_slanted_edge_is_monotone() {
    // triangle prism with a slanted hypotenuse
    let tri = [Vec2::new(-20.0, -15.0), Vec2::new(20.0, -15.0), Vec2::new(-20.0, 15.0)];
    let m = extrude_convex(&tri, 0.0, 1.0, 0.0);
    for aa in [2u8, 4, 8, 16] {
        let o = raster(100, 100, 50.0, 50.0, aa); // 0.5 mm pixels
        let r = Slicer::new(&[&m], 0.5, o).rasterize(0);
        let mut grays = 0;
        r.for_each_row(|y, row| {
            let first = row.iter().position(|&v| v > 0);
            let last = row.iter().rposition(|&v| v > 0);
            let (Some(a), Some(b)) = (first, last) else { return };
            // the section is left aligned: coverage never increases to the right
            // (full pixels first, then a monotone ramp down to 0, no holes)
            for x in a..b {
                assert!(row[x] >= row[x + 1], "aa {aa} row {y}: not monotone at {x}");
            }
            grays += row.iter().filter(|&&v| v > 0 && v < 255).count();
            // row coverage equals the chord length at the row centre (in pixels)
            let yc = 25.0 - (y as f64 + 0.5) * 0.5; // mm
            if yc > -14.0 && yc < 14.0 {
                assert_eq!(row[a], 255, "aa {aa} row {y}");
                let chord_mm = 40.0 * (15.0 - yc) / 30.0;
                let sum: f64 = row.iter().map(|&v| v as f64 / 255.0).sum();
                assert!(
                    (sum - chord_mm / 0.5).abs() < 0.05 + 1.0 / aa as f64,
                    "aa {aa} row {y}: {sum} vs {}",
                    chord_mm / 0.5
                );
            }
        });
        assert!(grays > 50, "aa {aa}: only {grays} gray pixels");
    }
}

fn l_shape() -> Mesh {
    // vertical bar on the left (x -20..-10, y -20..20) and a foot to the right (x -10..10, y -20..-10)
    let mut m = boxed(-20.0, -10.0, -20.0, 20.0, 0.0, 2.0);
    m.append(&boxed(-10.0, 10.0, -20.0, -10.0, 0.0, 2.0));
    m
}

fn test_printer(mirror: MirrorMode) -> PrinterProfile {
    PrinterProfile {
        resolution_x: 100,
        resolution_y: 80,
        size_x: 100.0,
        size_y: 80.0,
        size_z: 100.0,
        mirror,
        ..PrinterProfile::generic()
    }
}

#[test]
fn mirror_modes_flip_the_image() {
    let m = l_shape();
    let img = |mode: MirrorMode| {
        let p = test_printer(mode);
        Slicer::new(&[&m], 0.5, p.raster_options(4)).rasterize(0).to_gray()
    };
    let (none, lcd, mx, my) = (
        img(MirrorMode::None),
        img(MirrorMode::Lcd),
        img(MirrorMode::X),
        img(MirrorMode::Y),
    );
    let (w, h) = (100u32, 80u32);
    // top view: plate (x, y) -> pixel (x + 50, 40 - y)
    assert_eq!(px(&none, w, 35, 25), 255, "bar (-15, 15)");
    assert_eq!(px(&none, w, 55, 55), 255, "foot (5, -15)");
    assert_eq!(px(&none, w, 55, 25), 0, "empty corner (5, 15)");
    assert_eq!(none.iter().filter(|&&v| v == 255).count(), 10 * 40 + 20 * 10);
    for y in 0..h {
        for x in 0..w {
            let v = px(&none, w, x, y);
            assert_eq!(px(&mx, w, w - 1 - x, y), v, "X mirror at {x},{y}");
            assert_eq!(px(&my, w, x, h - 1 - y), v, "Y mirror at {x},{y}");
        }
    }
    assert_eq!(lcd, my, "LCD mirror = rows flipped");
    assert!(!MirrorMode::None.is_mirrored() && MirrorMode::Lcd.is_mirrored() && MirrorMode::X.is_mirrored());
    let o = test_printer(MirrorMode::X).raster_options(8);
    assert!(o.mirror_x && !o.mirror_y && o.anti_aliasing == 8);
}

#[test]
fn plate_positions_map_to_pixels() {
    // square pixels: 0.5 mm
    let o = raster(200, 200, 100.0, 100.0, 1);
    let m = boxed(8.0, 12.0, 18.0, 22.0, 0.0, 1.0);
    assert_eq!(Slicer::new(&[&m], 0.5, o).rasterize(0).bbox, Some((116, 56, 124, 64)));
    // non-square pixels: 0.2 x 0.25 mm
    let o = raster(400, 300, 80.0, 75.0, 1);
    let m = boxed(-12.0, -8.0, 5.0, 10.0, 0.0, 1.0);
    let r = Slicer::new(&[&m], 0.5, o).rasterize(0);
    assert_eq!(r.bbox, Some((140, 110, 160, 130)));
    assert_close(lit_mm2(&r, &o), 20.0, 1e-6, "area in mm2");
    assert_eq!(o.to_pixel(0.0, 0.0), (200.0, 150.0));
    assert_eq!(o.to_pixel(-40.0, 37.5), (0.0, 0.0));
    assert_eq!(o.to_pixel(40.0, -37.5), (400.0, 300.0));
    let mut om = o;
    om.mirror_x = true;
    om.mirror_y = true;
    assert_eq!(om.to_pixel(-40.0, 37.5), (400.0, 300.0));
    // parts outside the exposure area are clipped
    let half_out = boxed(40.0, 60.0, -5.0, 5.0, 0.0, 1.0);
    let o = raster(100, 100, 100.0, 100.0, 4);
    let r = Slicer::new(&[&half_out], 0.5, o).rasterize(0);
    assert_close(r.lit_area_px(), 100.0, 1e-3, "clipped at the right edge");
    assert_eq!(r.pixel_count(), 100 * 100);
    let left = boxed(-70.0, -45.0, -5.0, 5.0, 0.0, 1.0);
    assert_close(
        Slicer::new(&[&left], 0.5, o).rasterize(0).lit_area_px(),
        50.0,
        1e-3,
        "clipped at the left edge",
    );
    let gone = boxed(60.0, 70.0, -5.0, 5.0, 0.0, 1.0);
    assert!(Slicer::new(&[&gone], 0.5, o).rasterize(0).is_blank());
}

#[test]
fn section_outlines_in_millimetres() {
    let m = boxed(-3.0, 5.0, -1.0, 2.0, 0.0, 4.0);
    let s = Slicer::new(&[&m], 0.5, raster(100, 100, 100.0, 100.0, 1));
    let segs = s.section_mm(3);
    let len: f32 = segs.iter().map(|[a, b]| (*b - *a).length()).sum();
    assert!((len - 2.0 * (8.0 + 3.0)).abs() < 1e-4, "perimeter {len}");
    // enclosed area by the shoelace formula over the directed segments
    let area: f32 = segs.iter().map(|[a, b]| a.perp_dot(*b)).sum::<f32>() * 0.5;
    assert!((area - 24.0).abs() < 1e-4, "area {area}");
    // out of range layers are empty, not a panic
    assert!(s.section_mm(s.layer_count()).is_empty());
    assert!(s.section_mm(10_000).is_empty());
    assert!(s.negative_section_mm(10_000).is_empty());
    assert!(Slicer::new(&[], 0.5, raster(10, 10, 10.0, 10.0, 1))
        .section_mm(0)
        .is_empty());
}

#[test]
fn layer_raster_runs_and_helpers() {
    let r = LayerRaster::empty(7, 3);
    assert!(r.is_blank() && r.pixel_count() == 21 && r.lit_area_px() == 0.0);
    // runs are clamped to the image and padded with black
    let long = LayerRaster::from_runs(4, 2, [Run { value: 0, len: 2 }, Run { value: 255, len: 100 }]);
    assert_eq!(long.pixel_count(), 8);
    assert_eq!(long.to_gray(), vec![0, 0, 255, 255, 255, 255, 255, 255]);
    assert_eq!(long.bbox, Some((0, 0, 4, 2)));
    let short = LayerRaster::from_runs(4, 2, [Run { value: 0, len: 5 }, Run { value: 128, len: 2 }]);
    assert_eq!(short.to_gray(), vec![0, 0, 0, 0, 0, 128, 128, 0]);
    assert_eq!(short.bbox, Some((1, 1, 3, 2)));
    assert_close(short.lit_area_px(), 2.0 * 128.0 / 255.0, 1e-9, "partial lit area");
    // downscaling keeps the mean brightness
    let o = raster(200, 200, 100.0, 100.0, 4);
    let m = boxed(-20.3, 13.1, -7.7, 21.9, 0.0, 1.0);
    let full = Slicer::new(&[&m], 0.5, o).rasterize(0);
    let (w, h, small) = full.downscale(50, 50);
    assert_eq!((w, h), (50, 50));
    let mean_small = small.iter().map(|&v| v as f64).sum::<f64>() / (w * h) as f64;
    let mean_full = full.to_gray().iter().map(|&v| v as f64).sum::<f64>() / 40000.0;
    assert!((mean_small - mean_full).abs() < 0.5, "{mean_small} vs {mean_full}");
    // rows
    let mut rows = 0;
    full.for_each_row(|y, row| {
        assert_eq!(row.len(), 200);
        assert_eq!(y, rows);
        rows += 1;
    });
    assert_eq!(rows, 200);
    // from_gray reproduces the slicer output
    let back = LayerRaster::from_gray(200, 200, &full.to_gray());
    assert_eq!(back.runs, full.runs);
    assert_eq!(back.bbox, full.bbox);
    assert_close(back.lit_area_px(), full.lit_area_px(), 1e-6, "lit area");
}

#[test]
fn thin_walls_survive_anti_aliasing() {
    // a 0.2 mm wall on 0.5 mm pixels
    let wall = boxed(0.1, 0.3, -10.0, 10.0, 0.0, 1.0);
    let o = raster(200, 200, 100.0, 100.0, 4);
    let r = Slicer::new(&[&wall], 0.5, o).rasterize(0);
    assert!(!r.is_blank());
    assert_close(lit_mm2(&r, &o), 0.2 * 20.0, 0.01, "thin wall area");
    let g = r.to_gray();
    assert_eq!(px(&g, 200, 100, 100), (0.4f32 * 255.0 + 0.5) as u8);
}
