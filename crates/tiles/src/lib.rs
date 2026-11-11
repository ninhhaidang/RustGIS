//! L2: web map tiles — raster tiles (XYZ, TMS, WMS), Mapbox Vector Tiles and a subset of
//! MapLibre styles: tile addresses and math, a cache that fetches in the background, and drawing
//! into a pixmap in Web Mercator.
//!
//! Nothing here blocks on the network while drawing: [`draw::draw`] draws what the cache has
//! (standing in for missing tiles with ones it has at other zooms) and says whether more is on
//! its way; [`cache::generation`] tells the app when to draw again.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod address;
pub mod arcgis_auth;
pub mod boundaries;
pub mod cache;
pub mod cog;
pub mod cubes;
mod dem;
pub mod draw;
pub mod earthdata;
pub mod embeddings;
pub mod failure;
pub mod geocode;
pub mod jaxa;
pub mod logins;
pub mod mbtiles;
pub mod mvt;
pub mod nldi;
pub mod overpass;
pub mod overture;
mod picture;
pub mod pmtiles;
pub mod portal;
pub mod remote;
pub mod routing;
pub mod s3;
mod schedule;
pub mod shapes;
pub mod source;
pub mod sourcecoop;
pub mod sprite;
pub mod stac;
pub mod style;
pub mod symbols;
pub mod template;
pub mod usgis;
pub mod vector_export;
pub mod warp;

pub use draw::{Drawn, Frame, PlacedTiles, Placement, draw, place, place_raster};
pub use picture::{Pace, Unfinished, pace, paced, unfinished};
pub use source::{Kind, Spec};
pub use symbols::{Collide, Label, LabelIcon};
