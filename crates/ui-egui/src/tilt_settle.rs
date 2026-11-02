//! Exact globe, tilted and tiled flat frames, composed off the UI thread. Such a frame takes a few
//! hundred milliseconds for a large view; composed on the UI thread it froze the map just as it came
//! to rest. A copy of the map is drawn on a worker instead (the copy shares its features and
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
}

/// The frame being drawn (at most one at a time), and what it is for.
pub(crate) struct TiltSettle<K> {
    #[cfg(not(target_arch = "wasm32"))]
    job: Option<(K, std::sync::mpsc::Receiver<Done>)>,
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

fn draw(mut map: Map, view: View, backend: Backend) -> Done {
    let t0 = web_time::Instant::now();
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
    Done { rendered, view, map, ms: t0.elapsed().as_millis() as u64, incomplete }
}

impl<K: Copy + PartialEq> TiltSettle<K> {
    /// Whether a frame is being drawn.
    pub fn busy(&self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        return self.job.is_some();
        #[cfg(target_arch = "wasm32")]
        return self.ready.is_some();
    }

    /// The key of the frame being drawn (or drawn and not yet taken).
    pub fn key(&self) -> Option<K> {
        #[cfg(not(target_arch = "wasm32"))]
        return self.job.as_ref().map(|(k, _)| *k);
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
        self.start_with(ctx, key, move || draw(map, view, backend));
    }

    /// Starts `work` drawing the frame keyed `key` (see [`TiltSettle::start`]). A panic in it
    /// gives no frame, and the key isn't tried again.
    fn start_with(&mut self, ctx: &egui::Context, key: K, work: impl FnOnce() -> Done + Send + 'static) {
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
                Ok(_) => self.job = Some((key, rx)),
                Err(_) => self.failed = Some(key),
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
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
            let (_, rx) = self.job.as_ref()?;
            match rx.try_recv() {
                Ok(done) => self.job.take().map(|(k, _)| (k, done)),
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
                // The worker ended without an answer.
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.failed = self.job.take().map(|(k, _)| k);
                    None
                }
            }
        };
        #[cfg(target_arch = "wasm32")]
        let done = self.ready.take();
        if let Some((k, d)) = &done
            && d.rendered.is_none()
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

    #[test]
    fn a_frame_that_cant_be_drawn_isnt_tried_again() {
        let ctx = egui::Context::default();
        let map = Map::new("m");
        let view = View { width: 64.0, height: 48.0, pixels_per_point: 1.0, camera: Camera { pitch: 60.0, ..Camera::world() }, sky: true };
        let mut s = TiltSettle::default();
        s.start_with(&ctx, 1u8, || panic!("the renderer failed"));
        let end = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while s.busy() {
            assert!(s.take().is_none());
            assert!(std::time::Instant::now() < end, "the worker never ended");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
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
        s.start_with(&ctx, 1u8, || panic!("the renderer failed"));
        assert!(wake.recv_timeout(std::time::Duration::from_secs(30)).is_ok(), "never woken");
        assert!(s.take().is_none());
        assert!(!s.busy(), "the woken frame found the job still running");
        drop(open);
    }
}
