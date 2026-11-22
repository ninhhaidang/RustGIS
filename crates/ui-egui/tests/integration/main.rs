//! Consolidated integration tests (one binary per crate to cut link time in CI).

mod autosave;
mod capture_layout;
mod coordinate_readout;
mod define_projection;
mod diagnostics;
mod geolibre_projects;
mod gpu_toggle;
mod layout_favourite;
mod measurement_units;
mod menu_checkboxes;
mod move_shared_vertex;
mod notebook_beside_map;
mod picker_admission;
#[cfg(not(target_arch = "wasm32"))]
mod reload_data;
mod repair_source;
mod shortcut_customization;
mod snapping_point_types;
mod tool_field_lists;
mod vertex_drag_ownership;
mod vertex_snapping;
