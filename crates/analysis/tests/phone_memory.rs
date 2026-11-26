//! The memory limits of a phone (`rustgis_data::memory::set_phone`), in a test binary of its own:
//! the setting is the whole process's.

use std::sync::atomic::{AtomicU64, Ordering};

use rustgis_analysis::max_buffer_bytes;
use rustgis_analysis::warp::Grid;
use rustgis_data::memory;
use rustgis_data::raster::{self, Crs};

static FREE: AtomicU64 = AtomicU64::new(1 << 30);

#[test]
fn a_phones_limits_follow_what_its_system_allows_and_has_free() {
    let allowed: u64 = 1536 << 20;
    assert!(memory::set_phone(allowed, || Some(FREE.load(Ordering::Relaxed))));
    assert!(!memory::set_phone(8 << 30, || None), "said once");
    assert_eq!((memory::physical(), memory::source()), (Some(allowed), "phone"));

    // 1 GiB free: a quarter of what is allowed, where the desktop's floors would ask for more.
    assert!(raster::SAMPLES_FLOOR as u64 * 8 > allowed / 4);
    assert_eq!(raster::max_samples() as u64 * 8, allowed / 4);
    assert_eq!(max_buffer_bytes() as u64, allowed / 4);
    assert_eq!(memory::undo_history() as u64, allowed / 4);

    // 300 MiB free, asked again: little is left past what is kept for drawing, so the limits are
    // the browser's floors.
    FREE.store(300 << 20, Ordering::Relaxed);
    assert_eq!(memory::available(), Some(300 << 20));
    assert_eq!(raster::max_samples(), raster::BROWSER_SAMPLES);
    assert_eq!(max_buffer_bytes(), 128 << 20);
    assert_eq!(memory::undo_history(), 256 << 20, "Undo follows what is free too");
    assert_eq!(memory::thread_scratch(), memory::SCRATCH_FLOOR);

    // A grid of 64 million cells, within the desktop's floor, is refused with the way out.
    let crs = Crs::from_epsg(3857).unwrap();
    let grid = |cell: f64| Grid::snapped(crs, [0.0, 0.0, 8000.0, 8000.0], (cell, cell), (0.0, 0.0));
    let refused = grid(1.0).unwrap_err();
    assert!(refused.ends_with("more than this computer's 32000000: give a larger cellSize or a smaller extent"), "{refused}");
    assert_eq!(grid(2.0).map(|g| (g.width, g.height)), Ok((4000, 4000)));
}
