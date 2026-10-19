//! Splitting a mesh into its separate parts.

use crate::mesh::Mesh;
use crate::topo::{components_by_vertex, half_edges, nesting, signed_volume_of, submesh};

/// Splits `mesh` into connected parts (shells connected through shared
/// vertices). Closed shells that are fully enclosed by another shell (cavities
/// of hollowed models, or bodies inside such a cavity) stay together with their
/// outermost enclosing shell, so no part ends up floating inside another one.
/// Parts are sorted by descending absolute volume. Invalid triangles are dropped.
pub fn split_shells(mesh: &Mesh) -> Vec<Mesh> {
    split_groups(mesh)
        .into_iter()
        .map(|tris| submesh(&mesh.positions, tris))
        .collect()
}

/// Number of parts [`split_shells`] would produce.
pub fn part_count(mesh: &Mesh) -> usize {
    split_groups(mesh).len()
}

fn split_groups(mesh: &Mesh) -> Vec<Vec<[u32; 3]>> {
    let finite = mesh.positions.iter().all(|p| p.is_finite());
    let (comp, count) = components_by_vertex(mesh);
    if count == 0 {
        return Vec::new();
    }
    let mut groups: Vec<Vec<[u32; 3]>> = vec![Vec::new(); count];
    for (t, &c) in mesh.triangles.iter().zip(&comp) {
        if c != u32::MAX {
            groups[c as usize].push(*t);
        }
    }
    if count == 1 {
        return groups;
    }
    // a shell is closed when every edge is shared by exactly two of its triangles
    let mut closed = vec![finite; count];
    if finite {
        let valid: Vec<[u32; 3]> = groups.iter().flatten().copied().collect();
        let owner: Vec<u32> = groups
            .iter()
            .enumerate()
            .flat_map(|(i, g)| std::iter::repeat_n(i as u32, g.len()))
            .collect();
        let he = half_edges(&valid);
        for g in he.chunk_by(|a, b| a.0 == b.0) {
            if g.len() != 2 {
                closed[owner[(g[0].1 / 3) as usize] as usize] = false;
            }
        }
    }
    let nest = nesting(&mesh.positions, &groups, &closed);
    let root = |mut i: usize| {
        let mut guard = 0;
        while let Some(p) = nest[i].parent {
            i = p;
            guard += 1;
            if guard > count {
                break;
            }
        }
        i
    };
    let mut merged: Vec<Vec<[u32; 3]>> = vec![Vec::new(); count];
    for (i, g) in groups.iter_mut().enumerate() {
        merged[root(i)].extend(std::mem::take(g));
    }
    let mut parts: Vec<(f64, Vec<[u32; 3]>)> = merged
        .into_iter()
        .filter(|g| !g.is_empty())
        .map(|g| {
            let v = if finite {
                signed_volume_of(&mesh.positions, &g).abs()
            } else {
                0.0
            };
            (v, g)
        })
        .collect();
    parts.sort_by(|a, b| b.0.total_cmp(&a.0));
    parts.into_iter().map(|p| p.1).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::{cuboid, frustum, sphere};
    use glam::Vec3;

    #[test]
    fn separate_parts_and_cavities() {
        let mut m = cuboid(Vec3::ZERO, Vec3::splat(20.0));
        let mut cavity = sphere(Vec3::ZERO, 5.0, 16, 8);
        cavity.flip();
        m.append(&cavity);
        // a loose ball inside the cavity stays with its container
        m.append(&sphere(Vec3::ZERO, 2.0, 12, 6));
        m.append(&sphere(Vec3::new(40.0, 0.0, 0.0), 3.0, 16, 8));
        m.append(&frustum(
            Vec3::new(-40.0, 0.0, 0.0),
            Vec3::new(-40.0, 0.0, 5.0),
            2.0,
            1.0,
            12,
        ));
        let parts = split_shells(&m);
        assert_eq!(parts.len(), 3);
        assert_eq!(part_count(&m), 3);
        let first = &parts[0];
        let expected = 8000.0 - cavity.signed_volume().abs() + sphere(Vec3::ZERO, 2.0, 12, 6).signed_volume();
        assert!((first.signed_volume() - expected).abs() < 1e-2);
        for p in &parts {
            assert_eq!(p.open_edge_count(), 0);
            assert!(p.signed_volume() > 0.0);
        }
        assert!(parts[1].signed_volume() > parts[2].signed_volume());
        let total: usize = parts.iter().map(|p| p.triangle_count()).sum();
        assert_eq!(total, m.triangle_count());
    }

    #[test]
    fn overlapping_shells_are_separate_parts() {
        let mut m = cuboid(Vec3::ZERO, Vec3::splat(10.0));
        m.append(&cuboid(Vec3::new(5.0, 0.0, 0.0), Vec3::splat(8.0)));
        assert_eq!(split_shells(&m).len(), 2);
    }

    #[test]
    fn trivial_inputs() {
        assert!(split_shells(&Mesh::default()).is_empty());
        let c = cuboid(Vec3::ZERO, Vec3::ONE);
        let p = split_shells(&c);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].triangle_count(), 12);
        let bad = Mesh::new(vec![Vec3::ZERO, Vec3::X], vec![[0, 1, 5], [0, 0, 1]]);
        assert!(split_shells(&bad).is_empty());
    }
}
