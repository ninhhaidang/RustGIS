//! RustGIS in the browser.
//!
//! Runs the same [`rustgis_ui_egui::RustGisApp`] as the desktop app through eframe's web runner
//! (WebGPU where available, WebGL2 otherwise). Build with `trunk build --release` from this
//! directory (output in `dist/web`).
//!
//! Differences from the desktop app: Open and Add Data use the browser's file picker (a
//! Shapefile's files are picked or dropped together, or zipped), files can be dropped on the page,
//! and Save and Export download. URL flags: `?webgl` forces WebGL2; `?blank` starts an empty project;
//! `?lang=de` sets the interface language; `?project=<url>` opens a project by its web address;
//! `?embed` runs it inside another page (see `embed.rs`).
//! The GeoLibre tools run in the browser through geolibre-rust's WASI runner, loaded on demand
//! (see `geolibre.rs`).
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

// Pure logic, tested natively; only the browser build uses it.
#[cfg(target_arch = "wasm32")]
mod arcgis;
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
mod embed;
#[cfg(target_arch = "wasm32")]
mod geolibre;
#[cfg(target_arch = "wasm32")]
mod parts;
#[cfg(any(target_arch = "wasm32", test))]
mod request;
#[cfg(target_arch = "wasm32")]
mod web;

#[cfg(target_arch = "wasm32")]
fn main() {
    web::start();
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    eprintln!("rustgis-web only runs in the browser: build it with `trunk build --release` in apps/rustgis-web");
}
