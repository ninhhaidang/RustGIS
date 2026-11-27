//! Consolidated integration tests (one binary per crate to cut link time in CI).

mod flag_values;
mod memory_override;
mod reproducible_exports;
mod saved_secrets;
mod strict;
mod threads;
