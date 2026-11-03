//! ArcGIS feature-service answers as protocol buffers (`f=pbf`), from any service.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = rustgis_data::esripbf::is_esripbf(data);
    let _ = rustgis_data::esripbf::read(data, "fuzz");
});
