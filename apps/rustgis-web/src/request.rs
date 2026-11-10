//! The browser adapters' owned handoff, preparation and synchronous final-send boundary.
//! No synchronous diagnostic or request context is held while a future is suspended.

use rustgis_ui_egui::rustgis_engine::{data::diagnostics::Capture, render::rustgis_tiles::cache};
use std::future::Future;

/// Captures context at adapter invocation, before `spawn_local` can defer its first poll.
/// Completion re-enters it only synchronously, so callbacks can start follow-on requests.
pub(crate) fn handoff<T, F: Future<Output = Result<T, String>>>(
    work: impl FnOnce(Capture, Option<cache::Still>) -> F,
    done: impl FnOnce(Result<T, String>),
) -> impl Future<Output = ()> {
    let capture = Capture::current();
    let still = cache::current_still();
    async move {
        let result = match cache::check_still(&still) {
            Ok(()) => work(capture.clone(), still.clone()).await,
            Err(e) => Err(e),
        };
        let _diagnostics = capture.enter();
        cache::with_optional_still(still, || done(result));
    }
}

/// The actual adapters prepare lazily, including renewal awaits, then invoke their real fetch
/// synchronously after the final check. The sender must initiate I/O without another handoff.
pub(crate) async fn prepare_and_send<T, R, F: Future<Output = Result<T, String>>>(
    still: &Option<cache::Still>,
    prepare: impl FnOnce() -> F,
    send: impl FnOnce(T) -> R,
) -> Result<R, String> {
    cache::check_still(still)?;
    let prepared = prepare().await?;
    cache::check_still(still)?;
    Ok(send(prepared))
}

/// Formats a fetch failure without exposing the authenticated request's credentials. The
/// address is sanitized on its own first, so a secret query doesn't take the reason with it.
pub(crate) fn fetch_error(url: &str, error: String, authenticated: bool) -> String {
    use rustgis_ui_egui::rustgis_engine::{data::diagnostics::message_with_address, render::rustgis_tiles::logins};
    if authenticated && cache::http_status(&error).is_none() { logins::scrub_error(&message_with_address(url, &error)) } else { error }
}

/// Checks the browser's normalized destination under the caller's inherited ceiling.
/// Normalization may reveal a local host; its original spelling cannot narrow an explicit
/// local choice or widen a public document's permission. The normalizer performs no I/O.
pub(crate) fn destination(
    url: &str,
    ceiling: rustgis_ui_egui::rustgis_engine::render::rustgis_tiles::address::Reach,
    normalize: impl FnOnce(&str) -> Result<String, String>,
) -> Result<String, String> {
    let normalized = normalize(url)?;
    rustgis_ui_egui::rustgis_engine::render::rustgis_tiles::address::check_reach(&normalized, ceiling)?;
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::{Cell, RefCell},
        pin::Pin,
        rc::Rc,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering::SeqCst},
        },
        task::{Context, Poll, Waker},
    };

    /// The browser parser's output is checked at the same preparation boundary used by GET,
    /// HEAD and POST: an explicit local choice survives normalization, but public input does not.
    #[test]
    fn normalized_destinations_keep_the_request_ceiling() {
        use rustgis_ui_egui::rustgis_engine::render::rustgis_tiles::address::Reach;
        for ceiling in [Reach::Local, Reach::Public] {
            for (spelling, normalized, permitted) in [
                ("http://local%68ost:3000/style.json", "http://localhost:3000/style.json", ceiling == Reach::Local),
                ("http://%31%36%39.254.169.254/tile", "http://169.254.169.254/tile", false),
                ("https://public.example/tile", "https://public.example/tile", true),
            ] {
                let result = destination(spelling, ceiling, |_| Ok(normalized.into()));
                assert_eq!(result.is_ok(), permitted, "{spelling} under {ceiling:?}");
                if permitted {
                    assert_eq!(result.unwrap(), normalized);
                }
            }
        }
    }

    /// Status-only authenticated failures retain their machine-readable routine gap/backoff signal.
    #[test]
    fn authenticated_fetch_errors_preserve_http_statuses() {
        for authenticated in [false, true] {
            for error in ["HTTP 404", "HTTP 410", "HTTP 429 (retry after 30 s)"] {
                assert_eq!(fetch_error("https://status.example/tile", error.into(), authenticated), error);
            }
        }
        let error = fetch_error("https://status.example/tile?token=synthetic", "couldn't reach it".into(), true);
        assert!(!error.contains("synthetic"));
        assert!(cache::http_status(&error).is_none());
    }

    fn poll<F: Future + ?Sized>(f: Pin<&mut F>) -> Poll<F::Output> {
        f.poll(&mut Context::from_waker(Waker::noop()))
    }

    struct Held(Rc<Cell<bool>>);
    impl Future for Held {
        type Output = Result<(), String>;
        fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
            if self.0.get() { Poll::Ready(Ok(())) } else { Poll::Pending }
        }
    }

    fn live() -> (Arc<AtomicBool>, cache::Still) {
        let live = Arc::new(AtomicBool::new(true));
        let flag = live.clone();
        (live, Arc::new(move || flag.load(SeqCst)))
    }

    /// The installed GET/HEAD/POST handoff captures before scheduling, not on first poll.
    #[test]
    fn permission_transport_handoff_refuses_before_first_poll() {
        let (live, still) = live();
        let prepared = Cell::new(false);
        let finished = Cell::new(false);
        let mut f = Box::pin(cache::with_still(still, || {
            handoff(
                |_, _| async {
                    prepared.set(true);
                    Ok(())
                },
                |r| {
                    assert!(r.unwrap_err().contains("cancelled"));
                    finished.set(true);
                },
            )
        }));
        assert!(cache::current_still().is_none());
        live.store(false, SeqCst);
        assert!(poll(f.as_mut()).is_ready());
        assert!(!prepared.get() && finished.get());
        assert!(cache::current_still().is_none());
    }

    /// The same production preparation/send function backs GET, HEAD, POST and renewal fetch.
    /// Unchanged account identity sends; replacement behind the same public URL stops a held send.
    #[test]
    fn permission_transport_preparation_await_refuses_final_send() {
        use rustgis_ui_egui::rustgis_engine::render::rustgis_tiles::arcgis_auth as auth;
        let url = "https://browser-identity.example.org/server/rest/services/A/MapServer";
        auth::add_token(url, "synthetic-first").unwrap();
        let identity = auth::account_identity_for(url).unwrap();
        let still: cache::Still = Arc::new(move || identity.is_current());
        let released = Rc::new(Cell::new(false));
        let sends = Cell::new(0);
        let finished = Cell::new(false);
        let owned = Some(still.clone());
        let mut clean = Box::pin(prepare_and_send(&owned, || async { Ok(()) }, |()| sends.set(sends.get() + 1)));
        assert!(matches!(poll(clean.as_mut()), Poll::Ready(Ok(()))));
        drop(clean);
        assert_eq!(sends.get(), 1);
        let mut f = Box::pin(cache::with_still(still, || {
            handoff(
                |_, still| {
                    let (released, sends) = (&released, &sends);
                    async move {
                        prepare_and_send(
                            &still,
                            || async {
                                assert!(cache::current_still().is_none());
                                Held(released.clone()).await
                            },
                            |()| sends.set(sends.get() + 1),
                        )
                        .await
                    }
                },
                |r| {
                    assert!(r.unwrap_err().contains("cancelled"));
                    finished.set(true);
                },
            )
        }));
        assert!(poll(f.as_mut()).is_pending());
        assert!(cache::current_still().is_none(), "no context crosses the await");
        auth::add_token(url, "synthetic-replacement").unwrap();
        released.set(true);
        assert!(poll(f.as_mut()).is_ready());
        assert_eq!(sends.get(), 1, "only the unchanged identity sent");
        assert!(finished.get());
        assert!(cache::current_still().is_none());
        auth::sign_out(Some("https://browser-identity.example.org/server"));
    }

    /// A new service account binding cannot cross a browser preparation await under old consent.
    #[test]
    fn permission_transport_service_binding_refuses_after_preparation() {
        use rustgis_ui_egui::rustgis_engine::render::rustgis_tiles::logins::{self, Method};
        let url = "https://browser-service-identity.example";
        let binding = logins::activate(url, "9150003/connection", Method::Header("X-Key".into()), "synthetic-browser", false).unwrap();
        let identity = logins::account_identity(9_150_003).unwrap();
        let still = Some(Arc::new(move || identity.is_current()) as cache::Still);
        let released = Rc::new(Cell::new(false));
        let sends = Cell::new(0);
        let mut held = Box::pin(prepare_and_send(&still, || Held(released.clone()), |()| sends.set(sends.get() + 1)));
        assert!(poll(held.as_mut()).is_pending());
        let other = logins::activate(url, "91500030/connection", Method::Header("X-Key".into()), "synthetic-other", false).unwrap();
        assert!(cache::check_still(&still).is_ok(), "another session preserves consent");
        assert!(logins::deactivate(&binding));
        let replacement = logins::activate(url, "9150003/connection", Method::Header("X-Key".into()), "synthetic-browser", false).unwrap();
        released.set(true);
        assert!(matches!(poll(held.as_mut()), Poll::Ready(Err(_))));
        assert_eq!(sends.get(), 0);
        assert!(cache::current_still().is_none(), "no context crosses the await");
        assert!(logins::deactivate(&replacement));
        assert!(logins::deactivate(&other));
    }

    /// A callback's follow-on adapter owns the same predicate after the callback returns.
    #[test]
    fn permission_transport_callback_follow_on_keeps_owned_context() {
        let (live, still) = live();
        let expected = still.clone();
        let sends = Rc::new(Cell::new(0));
        let finished = Rc::new(Cell::new(false));
        type Pending = Pin<Box<dyn Future<Output = ()>>>;
        let follow_on: RefCell<Option<Pending>> = RefCell::new(None);
        let mut f = Box::pin(cache::with_still(still, || {
            handoff(
                |_, still| async move { prepare_and_send(&still, || async { Ok(()) }, |()| {}).await },
                |r| {
                    r.unwrap();
                    assert!(Arc::ptr_eq(&cache::current_still().unwrap(), &expected));
                    let (sent, done) = (sends.clone(), finished.clone());
                    *follow_on.borrow_mut() = Some(Box::pin(handoff(
                        move |_, still| async move { prepare_and_send(&still, || async { Ok(()) }, |()| sent.set(sent.get() + 1)).await },
                        move |r| {
                            assert!(r.unwrap_err().contains("cancelled"));
                            done.set(true);
                        },
                    )));
                },
            )
        }));
        assert!(poll(f.as_mut()).is_ready());
        assert!(cache::current_still().is_none());
        live.store(false, SeqCst);
        assert!(poll(follow_on.borrow_mut().as_mut().unwrap().as_mut()).is_ready());
        assert_eq!(sends.get(), 0);
        assert!(finished.get());
        assert!(cache::current_still().is_none());
    }
}
