//! One bounded tile download for Test Connection, shared by the engine and the app.

use crate::render::rustgis_tiles::{address, cache, template};
use rustgis_map::tiles::TileKind;
use serde::{Deserialize, Serialize};

pub const NOT_TILE: &str = "The server answered, but the response is not a usable tile.";
pub const UNEXPANDED: &str = "The tile URL has an unsupported placeholder.";
pub const OTHER_ORIGIN: &str = "The sample tile changes the connection's origin. Use a concrete hostname to test with a saved login.";
pub const TEXT_ANSWER: &str = "An XYZ test needs a tile response, not a text answer.";
pub const PATH_KEY_WARNING: &str = "An API key in the URL path is saved as typed, not in the credential store.";

/// An XYZ template the connection test recognises, not just a URL with a subdomain placeholder.
pub fn is_template(url: &str) -> bool {
    template::xyz_sample_zoom(url).is_some()
}

/// A nonempty `;key=…` path parameter, as used by Koordinates. Query keys have their own handling.
pub fn has_path_key(url: &str) -> bool {
    let Some((_, at)) = url.split_once("://") else { return false };
    let Some((_, path)) = at.split(['?', '#']).next().unwrap_or_default().split_once('/') else { return false };
    path.split('/').any(|segment| {
        segment.split(';').skip(1).any(|param| param.split_once('=').is_some_and(|(k, v)| k.eq_ignore_ascii_case("key") && !v.is_empty()))
    })
}

#[derive(Clone, Copy, Deserialize, Serialize)]
pub struct Sample {
    pub zoom: u8,
    pub vector: bool,
}

impl Sample {
    pub(crate) fn for_address(url: &str) -> Option<Self> {
        Some(Self { zoom: template::xyz_sample_zoom(url)?, vector: TileKind::guess(url) == TileKind::Vector })
    }

    pub(crate) fn address(self, url: &str) -> Result<String, String> {
        let template = url.split('#').next().unwrap_or_default();
        let url = template::expand(template, template::TileId { z: self.zoom, x: 0, y: 0 }, 256, false, &[]);
        if url.contains(['{', '}']) {
            return Err(UNEXPANDED.into());
        }
        address::check(&url, Some(template))?;
        Ok(url)
    }

    /// A successful HTTP response is a pass only if the data actually decodes as a tile.
    /// The transport supplies an empty body for 204; neither that nor a 404 is a pass.
    pub fn check(self, response: Result<Vec<u8>, String>) -> Result<Outcome, String> {
        match response {
            Ok(bytes) if bytes.is_empty() => Ok(Outcome::Missing),
            Err(e) if matches!(cache::http_status(&e), Some(204 | 404)) => Ok(Outcome::Missing),
            Ok(bytes) => {
                if bytes.len() > cache::MAX_BYTES {
                    return Err(NOT_TILE.into());
                }
                let decoded = if self.vector {
                    crate::render::rustgis_tiles::mvt::check_sample(&bytes)
                } else {
                    cache::decode_image(&bytes).map(|_| ())
                };
                decoded.map_err(|_| NOT_TILE.to_string())?;
                Ok(Outcome::Received)
            }
            Err(e) => Err(e),
        }
    }
}

#[derive(Clone, Copy)]
pub enum Outcome {
    Received,
    Missing,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vector_sample_requires_name_and_version_but_not_features() {
        let sample = Sample { zoom: 0, vector: true };
        for invalid in [
            &[0x1a, 0][..],            // Empty layer.
            &[0x1a, 2, 0x78, 2],       // Version without name.
            &[0x1a, 3, 0x0a, 1, b'a'], // Name without version.
        ] {
            assert_eq!(sample.check(Ok(invalid.to_vec())).err().as_deref(), Some(NOT_TILE));
        }
        let named_empty_layer = vec![0x1a, 5, 0x0a, 1, b'a', 0x78, 2];
        assert!(matches!(sample.check(Ok(named_empty_layer)), Ok(Outcome::Received)));
    }
}
