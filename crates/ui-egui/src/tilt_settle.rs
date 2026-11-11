//! Exact globe, tilted and tiled flat frames, and a flat map's frame after a pan or zoom (#1750),
//! composed off the UI thread. Such a frame takes a few hundred milliseconds for a large view;
//! composed on the UI thread it froze the map just as it came to rest. A frame that goes out of
//! date while it is drawn (the map moves again) is stopped ([`TiltSettle::cancel`]). A copy of the map is drawn on a worker instead (the copy shares its features and
//! caches, so it costs pointers), while the drape (see `tilt_drape`) stays on screen until the
//! frame is ready. GPU scenes are composed here too, then uploaded on the UI thread. In the
//! browser there are no threads: the frame is drawn at once, its tile pictures a frame's worth at
//! a time (see `rustgis_tiles::Pace`), the frame drawn again until they all are.

use rustgis_engine::map::Map;
use rustgis_engine::render::{self, Backend, RenderOutput, View};

/// A finished frame.
pub(crate) struct Done {
    /// None when it couldn't be drawn.
    pub rendered: Option<RenderOutput>,
    /// The view it was drawn for.
    pub view: View,
    /// The copy it was drawn from, holding the caches the drawing filled.
    pub map: Map,
    /// How long it took (ms).
    pub ms: u64,
    /// Tile pictures stand in for others, or are still to be drawn: it is drawn again.
    pub incomplete: bool,
    /// It was stopped part-way ([`TiltSettle::cancel`]): no frame, but nothing wrong with it.
    pub cancelled: bool,
    pub archives: render::ArchiveRevision,
}

/// The frame being drawn (at most one at a time), and what it is for.
pub(crate) struct TiltSettle<K> {
    #[cfg(not(target_arch = "wasm32"))]
    job: Option<(K, std::sync::mpsc::Receiver<Done>, Stop)>,
    #[cfg(target_arch = "wasm32")]
    ready: Option<(K, Done)>,
    /// The last frame that couldn't be drawn (the renderer panicked): it isn't tried again.
    failed: Option<K>,
}

impl<K> Default for TiltSettle<K> {
    fn default() -> Self {
        TiltSettle {
            #[cfg(not(target_arch = "wasm32"))]
            job: None,
            #[cfg(target_arch = "wasm32")]
            ready: None,
            failed: None,
        }
    }
}

/// A job's stop flag (see `render::cancel`).
type Stop = std::sync::Arc<std::sync::atomic::AtomicBool>;

fn draw(mut map: Map, view: View, backend: Backend, stop: Stop) -> Done {
    let t0 = web_time::Instant::now();
    let _stop = render::cancel::install(stop.clone());
    let archives = render::archive_revisions([&map]);
    // On the UI thread in the browser, a frame's worth of tile pictures at a time; on a worker
    // natively, all of them.
    #[cfg(target_arch = "wasm32")]
    let (rendered, incomplete) = {
        let (r, lacks) =
            render::rustgis_tiles::paced(render::rustgis_tiles::Pace::Settling, || render::render_backend(&mut map, &view, backend));
        (r, lacks.is_some())
    };
    #[cfg(not(target_arch = "wasm32"))]
    let (rendered, incomplete) = (render::render_backend(&mut map, &view, backend), false);
    // Stopped only if it came back without a picture: a stop after the renderer's last look
    // leaves a finished frame.
    let cancelled = rendered.is_none() && stop.load(std::sync::atomic::Ordering::Relaxed);
    Done { rendered, view, map, ms: t0.elapsed().as_millis() as u64, incomplete, cancelled, archives }
}

impl<K: Copy + PartialEq> TiltSettle<K> {
    /// Whether a frame is being drawn.
    pub fn busy(&self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        return self.job.is_some();
        #[cfg(target_arch = "wasm32")]
        return self.ready.is_some();
    }

    /// Whether the frame keyed `key` couldn't be drawn here (the worker panicked, or the system
    /// refused its thread): it isn't tried again, so the caller draws it another way.
    pub fn failed(&self, key: K) -> bool {
        self.failed == Some(key)
    }

    /// The key of the frame being drawn (or drawn and not yet taken).
    pub fn key(&self) -> Option<K> {
        #[cfg(not(target_arch = "wasm32"))]
        return self.job.as_ref().map(|(k, ..)| *k);
        #[cfg(target_arch = "wasm32")]
        return self.ready.as_ref().map(|(k, _)| *k);
    }

    /// Starts drawing `map` (a copy) for `view`, keyed `key`, waking `ctx` when it is done.
    /// Does nothing while a frame is being drawn (the next starts once that one is taken), or
    /// for a key that couldn't be drawn.
    pub fn start(&mut self, ctx: &egui::Context, key: K, map: Map, view: View) {
        self.start_backend(ctx, key, map, view, Backend::Cpu);
    }

    /// Builds a CPU frame or GPU scene on the worker; GPU resources are uploaded by the UI.
    pub fn start_backend(&mut self, ctx: &egui::Context, key: K, map: Map, view: View, backend: Backend) {
        let stop = Stop::default();
        let flag = stop.clone();
        self.start_with(ctx, key, stop, move || draw(map, view, backend, flag));
    }

    /// Asks the frame being drawn to stop (it is out of date): the renderer looks between layers,
    /// before each chunk of a streamed layer, every 2048 features or 65,536 vertices and between
    /// a tilted view's bands (see `render::cancel`), so
    /// the worker is free again soon. The stopped frame is
    /// still handed back by [`TiltSettle::take`], with no picture and `cancelled` set, and its key
    /// may be drawn again. In the browser frames are drawn at once: there is nothing to stop.
    pub fn cancel(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some((.., stop)) = &self.job {
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Starts `work` drawing the frame keyed `key` (see [`TiltSettle::start`]). A panic in it
    /// gives no frame, and the key isn't tried again.
    fn start_with(&mut self, ctx: &egui::Context, key: K, stop: Stop, work: impl FnOnce() -> Done + Send + 'static) {
        if self.busy() || self.failed == Some(key) {
            return;
        }
        let work = std::panic::AssertUnwindSafe(work);
        #[cfg(not(target_arch = "wasm32"))]
        {
            let (tx, rx) = std::sync::mpsc::channel();
            let ctx = ctx.clone();
            let diagnostics = rustgis_engine::data::diagnostics::Capture::current();
            let spawned = std::thread::Builder::new().name("rustgis-map-frame".into()).spawn(move || {
                let _diagnostics = diagnostics.enter();
                // Without an answer (a panic) `take` finds the channel closed. It is closed before
                // the frame is woken, so the woken frame finds the job over, not still running.
                if let Ok(done) = std::panic::catch_unwind(work) {
                    let _ = tx.send(done);
                }
                drop(tx);
                ctx.request_repaint();
            });
            match spawned {
                Ok(_) => self.job = Some((key, rx, stop)),
                Err(_) => self.failed = Some(key),
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            // Drawn at once: nothing is left to stop.
            let _ = stop;
            match std::panic::catch_unwind(work) {
                Ok(done) => self.ready = Some((key, done)),
                Err(_) => self.failed = Some(key),
            }
            ctx.request_repaint();
        }
    }

    /// The finished frame and its key, once it is ready.
    pub fn take(&mut self) -> Option<(K, Done)> {
        #[cfg(not(target_arch = "wasm32"))]
        let done = {
            let (_, rx, _) = self.job.as_ref()?;
            match rx.try_recv() {
                Ok(done) => self.job.take().map(|(k, ..)| (k, done)),
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
                // The worker ended without an answer.
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.failed = self.job.take().map(|(k, ..)| k);
                    None
                }
            }
        };
        #[cfg(target_arch = "wasm32")]
        let done = self.ready.take();
        if let Some((k, d)) = &done
            && d.rendered.is_none()
            && !d.cancelled
            && d.archives.current([&d.map])
        {
            self.failed = Some(*k);
        }
        done
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_engine::map::Camera;

    #[test]
    fn draws_a_tilted_frame_and_hands_it_back_once() {
        let ctx = egui::Context::default();
        let mut map = Map::new("m");
        map.camera = Camera { pitch: 60.0, ..Camera::world() };
        let view = View { width: 64.0, height: 48.0, pixels_per_point: 1.0, camera: map.camera, sky: true };
        let mut s = TiltSettle::default();
        s.start(&ctx, 7u8, map.clone(), view);
        assert!(s.busy());
        // A second start while busy is ignored.
        s.start(&ctx, 8u8, map, view);
        let end = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let done = loop {
            if let Some(d) = s.take() {
                break d;
            }
            assert!(std::time::Instant::now() < end, "the frame never came");
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert_eq!(done.0, 7);
        let Some(RenderOutput::Cpu(r)) = done.1.rendered else { panic!("a CPU frame") };
        assert_eq!((r.pixmap.width(), r.pixmap.height()), (64, 48));
        assert!(!s.busy());
        assert!(s.take().is_none());
    }

    /// A frame stopped part-way comes back without a picture, isn't taken for one that failed,
    /// and frees the worker for the next.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_stopped_frame_comes_back_cancelled_and_can_be_drawn_again() {
        let ctx = egui::Context::default();
        let map = Map::new("m");
        let view = View { width: 64.0, height: 48.0, pixels_per_point: 1.0, camera: map.camera, sky: true };
        let mut s = TiltSettle::default();
        // The work waits until it is stopped, as a long frame would still be drawing.
        let stop = Stop::default();
        let seen = stop.clone();
        let (m, v) = (map.clone(), view);
        s.start_with(&ctx, 3u8, stop, move || {
            while !seen.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            draw(m, v, Backend::Cpu, seen)
        });
        assert!(s.busy());
        s.cancel();
        let end = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let done = loop {
            if let Some(d) = s.take() {
                break d;
            }
            assert!(std::time::Instant::now() < end, "the stopped frame never came back");
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert_eq!(done.0, 3);
        assert!(done.1.cancelled && done.1.rendered.is_none());
        assert!(!s.busy());
        // The same frame is drawn when asked again.
        s.start(&ctx, 3u8, map, view);
        assert!(s.busy(), "a stopped frame was taken for one that couldn't be drawn");
        s.cancel();
    }

    #[test]
    fn a_frame_that_cant_be_drawn_isnt_tried_again() {
        let ctx = egui::Context::default();
        let map = Map::new("m");
        let view = View { width: 64.0, height: 48.0, pixels_per_point: 1.0, camera: Camera { pitch: 60.0, ..Camera::world() }, sky: true };
        let mut s = TiltSettle::default();
        s.start_with(&ctx, 1u8, Stop::default(), || panic!("the renderer failed"));
        let end = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while s.busy() {
            assert!(s.take().is_none());
            assert!(std::time::Instant::now() < end, "the worker never ended");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(s.failed(1u8) && !s.failed(2u8));
        s.start(&ctx, 1u8, map.clone(), view);
        assert!(!s.busy(), "the same frame was tried again");
        s.start(&ctx, 2u8, map, view);
        assert!(s.busy());
    }

    /// The frame a worker's panic wakes finds the job over: the worker closes the channel before
    /// it wakes the frame, so no wake-up is lost. (The callback holds the worker until the test has
    /// looked.)
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_frame_a_panic_wakes_finds_the_job_over() {
        let ctx = egui::Context::default();
        let (woken, wake) = std::sync::mpsc::channel::<()>();
        let (open, gate) = std::sync::mpsc::channel::<()>();
        let (woken, gate) = (std::sync::Mutex::new(woken), std::sync::Mutex::new(gate));
        ctx.set_request_repaint_callback(move |_| {
            if let Ok(w) = woken.lock() {
                let _ = w.send(());
            }
            if let Ok(g) = gate.lock() {
                let _ = g.recv_timeout(std::time::Duration::from_secs(30));
            }
        });
        let mut s = TiltSettle::default();
        s.start_with(&ctx, 1u8, Stop::default(), || panic!("the renderer failed"));
        assert!(wake.recv_timeout(std::time::Duration::from_secs(30)).is_ok(), "never woken");
        assert!(s.take().is_none());
        assert!(!s.busy(), "the woken frame found the job still running");
        drop(open);
    }
}
