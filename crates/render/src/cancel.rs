//! Stopping a frame no longer wanted (#1750). The desktop app draws a settled flat map's frame on a
//! worker; when the map moves again before it is done, the frame is out of date. The worker
//! installs a flag on its thread ([`install`]) and sets it to stop: the renderer looks at it between
//! layers, between a tilted view's bands, before each chunk of a streamed layer and every
//! [`EVERY`] features or [`EVERY_VERTICES`] vertices of a layer, and a frame stopped that way comes
//! back as `None`, without its tiles, labels or effects worked out.
//! With none installed (an export, the CLI, the browser) frames always finish. The flag is the
//! thread's own: threads the renderer starts for a part of the frame (a raster's rows) don't look
//! at it, and finish their part. A long step without a look (projecting or thinning a layer's
//! geometry for the first time, a raster, a single feature's path) runs to its end.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// How many features of a layer are drawn between looks at the flag.
pub(crate) const EVERY: usize = 2048;

/// How many vertices of a layer are drawn between looks at the flag, so a layer of a few heavy
/// features (a detailed coastline) stops part-way too.
pub(crate) const EVERY_VERTICES: usize = 65_536;

thread_local! {
    static FLAG: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
}

/// Makes `flag` stop the frames drawn on this thread until the guard is dropped, which puts back
/// the one before.
pub fn install(flag: Arc<AtomicBool>) -> Installed {
    Installed(FLAG.with(|f| f.replace(Some(flag))))
}

/// Puts back the flag before when dropped (see [`install`]).
pub struct Installed(Option<Arc<AtomicBool>>);

impl Drop for Installed {
    fn drop(&mut self) {
        let before = self.0.take();
        let _ = FLAG.try_with(|f| f.replace(before));
    }
}

/// Whether the frame being drawn on this thread was asked to stop.
pub(crate) fn requested() -> bool {
    // Never during the thread's teardown: no frame is drawn that late.
    FLAG.try_with(|f| f.borrow().as_ref().is_some_and(|flag| flag.load(Ordering::Relaxed))).unwrap_or(false)
}

#[cfg(test)]
thread_local! {
    /// Features drawn on this test thread, to see a stopped frame stop drawing.
    pub(crate) static DRAWN: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flag_stops_frames_on_its_thread_only_while_installed() {
        let flag = Arc::new(AtomicBool::new(false));
        assert!(!requested());
        {
            let _guard = install(flag.clone());
            assert!(!requested());
            flag.store(true, Ordering::Relaxed);
            assert!(requested());
            // Another thread draws on.
            assert!(!std::thread::spawn(requested).join().unwrap_or(true));
        }
        assert!(!requested());
    }

    #[test]
    fn a_stopped_frame_is_no_frame() {
        let mut map = rustgis_map::Map::new("m");
        if let Some(d) = crate::world_dataset("Countries", crate::sample_data::COUNTRIES) {
            map.add_dataset(d);
        }
        let view = crate::View { width: 64.0, height: 48.0, pixels_per_point: 1.0, camera: rustgis_map::Camera::world(), sky: false };
        let flag = Arc::new(AtomicBool::new(false));
        let _guard = install(flag.clone());
        DRAWN.set(0);
        assert!(crate::render(&mut map, &view).is_some());
        assert!(DRAWN.get() > 100, "{} countries drawn", DRAWN.get());
        // Stopped, it draws no feature at all, not just hands back nothing.
        flag.store(true, Ordering::Relaxed);
        DRAWN.set(0);
        assert!(crate::render(&mut map, &view).is_none());
        assert_eq!(DRAWN.get(), 0);
        // So does a tilted one, drawn band by band.
        let tilted = crate::View { camera: rustgis_map::Camera { pitch: 60.0, ..rustgis_map::Camera::world() }, ..view };
        DRAWN.set(0);
        assert!(crate::render(&mut map, &tilted).is_none());
        assert_eq!(DRAWN.get(), 0);
        flag.store(false, Ordering::Relaxed);
        assert!(crate::render(&mut map, &tilted).is_some());
        assert!(DRAWN.get() > 0);
    }
}
