//! Consolidated integration tests (one binary per crate to cut link time in CI).

mod date_properties;
mod datum_grids;
#[cfg(not(target_arch = "wasm32"))]
mod extent_query_properties;
#[cfg(not(target_arch = "wasm32"))]
mod planetary_crs;
#[cfg(not(target_arch = "wasm32"))]
mod projection_methods;
mod reprojection_round_trip;
mod shapefile_encoding;
mod utm_vs_proj;
#[cfg(not(target_arch = "wasm32"))]
mod view_memory;
