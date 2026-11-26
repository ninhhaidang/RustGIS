//! Consolidated integration tests (one binary per crate to cut link time in CI).

mod antimeridian_edit;
mod cut_reshape_trace;
mod define_projection;
mod geojson_crs;
mod geometry_calculator;
mod kml_duplicate_data;
mod link_choice;
mod measurement_units;
mod move_shared_vertex;
mod never_silent_reading;
mod never_silent_roundtrip;
mod planetary_export;
mod polar_edits;
mod portable_projects;
#[cfg(target_os = "linux")]
mod private_ca;
mod project_save;
mod project_save_roundtrip;
mod raster_crs_notices;
mod read_routes;
mod recovery_paths;
mod snap_boxes;
mod snapping_point_types;
mod vertex_snap_identity;
mod workflows;
