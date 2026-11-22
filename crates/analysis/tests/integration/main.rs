//! Consolidated integration tests (one binary per crate to cut link time in CI).

mod check_geometry;
mod geometry_conversions;
mod join_field;
mod merge;
mod polar;
mod repair_geometry;
mod spatial_join_merge;
mod summary_fields;
mod tabulate_intersection;
