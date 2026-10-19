//! Core of OpenResin: mesh I/O, scene handling, slicing/rasterization, supports
//! and print estimation for masked-stereolithography (MSLA) resin printers.
//!
//! Geometry tools: [`repair`] (analysis and repair), [`split`] (separate parts),
//! [`cut`] (plane cuts with capped faces), [`hollow`] (cavities and drain
//! holes, subtracted with [`slicer::Slicer::with_negatives`]), [`orient`]
//! (automatic orientation), [`label`] (text labels),
//! [`primitives::base_plate`] (pads) and [`triangulate`] (polygon
//! triangulation).
//!
//! Coordinate conventions used throughout the crate:
//! * Units are millimetres.
//! * The build plate is the plane `z = 0`, centred on the origin; `+X` points to
//!   the right, `+Y` to the back of the printer and `+Z` up (away from the vat).
//! * Layer images are row-major, row 0 is the back (`+Y`) edge of the plate and
//!   column 0 is the left (`-X`) edge, i.e. a top-down view. Printer specific
//!   mirroring is applied by the rasterizer (see [`slicer::RasterOptions`]).

mod bvh;
pub mod cut;
pub mod error;
pub mod estimate;
pub mod geometry;
pub mod hollow;
pub mod io;
pub mod label;
pub mod mesh;
pub mod orient;
pub mod primitives;
pub mod profile;
pub mod project;
pub mod repair;
#[cfg(test)]
mod robustness_tests;
pub mod scene;
pub mod slicer;
pub mod split;
pub mod support;
mod topo;
pub mod triangulate;

pub use cut::{cut_mesh, cut_z, CutPlane, CutResult};
pub use error::{Error, Result};
pub use glam::{Mat4, Quat, Vec2, Vec3};
pub use hollow::{drain_hole, HollowOptions, HollowResult};
pub use label::{place_label, surface_frame, text_mesh, LabelMode, LabelOptions};
pub use mesh::Mesh;
pub use orient::{auto_orient, OrientGoal, OrientOptions, OrientScore};
pub use primitives::{base_plate, base_plate_under, BasePlateOptions};
pub use repair::{analyze, MeshReport, RepairOptions, RepairReport};
pub use split::split_shells;
