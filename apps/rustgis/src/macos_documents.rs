//! Finder and Dock document requests, using Apple's documented event-manager API.
//! See https://github.com/opengeos/RustGIS/issues/1706 for the scoped native-code exception.
//! Remove this adapter when the windowing stack provides native document delivery. It never
//! replaces winit's application delegate, intercepts Quit, or opens a project in a callback.
//!
//! The desktop executable owns `aevt`/`odoc` until its one event loop returns. The registration
//! is retained through that run and removed before its target is released; it does not restore
//! AppKit's default document handler. Shared crates and every other desktop module deny unsafe.
//!
//! Native reference resolution is synchronous (without UI or mounting); project/data reads
//! happen later through the existing user-interface path, with its normal progress and Cancel.
//! https://developer.apple.com/library/archive/documentation/Cocoa/Conceptual/ScriptableCocoaApplications/SApps_handle_AEs/SAppsHandleAEs.html
#![allow(unsafe_code)]

use objc2::rc::Retained;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_foundation::{
    NSAppleEventDescriptor, NSAppleEventManager, NSNotification, NSNotificationCenter, NSObject, NSObjectProtocol, NSURL,
    NSURLBookmarkResolutionOptions, ns_string,
};

use crate::open_documents::{Batch, Inbox, MAX_FILES};

const CORE: u32 = u32::from_be_bytes(*b"aevt");
const OPEN: u32 = u32::from_be_bytes(*b"odoc");
const DIRECT: u32 = u32::from_be_bytes(*b"----");
const LIST: u32 = u32::from_be_bytes(*b"list");
const BOOKMARK: u32 = u32::from_be_bytes(*b"bmrk");

define_class!(
    // SAFETY: NSObject imposes no subclass invariants. Instances, ivars and all callbacks
    // are main-thread-only; Registration retains the instance until callbacks are removed.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "RustGISDocumentHandler"]
    #[ivars = Inbox]
    struct Handler;

    // SAFETY: the superclass implements NSObjectProtocol and no requirement is overridden.
    unsafe impl NSObjectProtocol for Handler {}

    impl Handler {
        // SAFETY: notification selectors take one NSNotification and return void.
        #[unsafe(method(willFinishLaunching:))]
        fn will_finish(&self, _notification: &NSNotification) {
            self.register();
        }

        // SAFETY: Apple's event handler selector takes event and reply descriptors and
        // returns void. Reference decoding is caught before paths enter the inbox.
        #[unsafe(method(openDocuments:withReplyEvent:))]
        fn open(&self, event: &NSAppleEventDescriptor, reply: &NSAppleEventDescriptor) {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| decode(event)));
            let batch = result.unwrap_or_else(|_| Batch { unreadable: 1, ..Default::default() });
            if batch.unreadable != 0 || batch.excess != 0 {
                // SAFETY: setParamDescriptor:forKeyword: takes a descriptor and a u32
                // AEKeyword; errn reports coercion failure, not success opening a document.
                unsafe {
                    let _: () = msg_send![reply, setParamDescriptor: &*NSAppleEventDescriptor::descriptorWithInt32(-1700),
                        forKeyword: u32::from_be_bytes(*b"errn")];
                }
            }
            self.ivars().receive(batch);
        }
    }
);

impl Handler {
    /// Install after AppKit sets up its defaults, and immediately for an already-launched app.
    fn register(&self) {
        let manager = NSAppleEventManager::sharedAppleEventManager();
        // SAFETY: self implements the exact two-descriptor selector above and is retained
        // for the registration lifetime. AEEventClass and AEEventID are both u32.
        unsafe {
            let _: () = msg_send![&*manager, setEventHandler: self,
                andSelector: sel!(openDocuments:withReplyEvent:), forEventClass: CORE, andEventID: OPEN];
        }
    }
}

/// The standalone app's one main-thread registration; keep it until run_native returns.
pub struct Registration(Retained<Handler>);

impl Registration {
    /// Observe launch before the event loop exists, buffering its initial document request.
    pub fn install() -> Result<(Self, Inbox), String> {
        let mtm = MainThreadMarker::new().ok_or("macOS document handling must start on the main thread")?;
        let inbox = Inbox::default();
        let allocated = Handler::alloc(mtm).set_ivars(inbox.clone());
        // SAFETY: NSObject's designated init initializes a fresh allocated subclass.
        let handler: Retained<Handler> = unsafe { msg_send![super(allocated), init] };
        // SAFETY: handler is retained for this observer's lifetime, its selector has the
        // required notification signature, and it is registered/removed on the main thread.
        unsafe {
            NSNotificationCenter::defaultCenter().addObserver_selector_name_object(
                &handler,
                sel!(willFinishLaunching:),
                Some(ns_string!("NSApplicationWillFinishLaunchingNotification")),
                None,
            );
        }
        handler.register();
        Ok((Self(handler), inbox))
    }
}

impl Drop for Registration {
    /// Stop both callbacks before releasing their target. This executable never reuses its loop.
    fn drop(&mut self) {
        // SAFETY: the guard is main-thread-only; it owns this standalone executable's sole
        // odoc registration. No callback can run concurrently while its target is released.
        unsafe {
            NSNotificationCenter::defaultCenter().removeObserver(&self.0);
            let manager = NSAppleEventManager::sharedAppleEventManager();
            let _: () = msg_send![&*manager, removeEventHandlerForEventClass: CORE, andEventID: OPEN];
        }
    }
}

/// Decode bounded document references now; nothing borrowed from the callback enters the inbox.
fn decode(event: &NSAppleEventDescriptor) -> Batch {
    // SAFETY: paramDescriptorForKeyword: accepts the u32 AEKeyword keyDirectObject and
    // returns a nullable NSAppleEventDescriptor retained by the typed message send.
    let direct: Option<Retained<NSAppleEventDescriptor>> = unsafe { msg_send![event, paramDescriptorForKeyword: DIRECT] };
    let Some(direct) = direct else { return Batch { unreadable: 1, ..Default::default() } };
    // SAFETY: descriptorType returns the four-byte DescType (u32).
    let kind: u32 = unsafe { msg_send![&*direct, descriptorType] };
    let mut batch = Batch::default();
    if kind == LIST {
        let count = usize::try_from(direct.numberOfItems()).unwrap_or(0);
        batch.excess = count.saturating_sub(MAX_FILES);
        for index in 1..=count.min(MAX_FILES) {
            record(&mut batch, direct.descriptorAtIndex(index as isize).as_deref());
        }
        if count == 0 {
            batch.unreadable = 1;
        }
    } else {
        // Some senders provide one reference directly rather than an AEList.
        record(&mut batch, Some(&direct));
    }
    batch
}

/// Preserve usable UTF-8 paths; the command interface cannot represent other native path bytes.
fn record(batch: &mut Batch, descriptor: Option<&NSAppleEventDescriptor>) {
    let path = descriptor.and_then(file_url).filter(|url| url.isFileURL()).and_then(|url| url.to_file_path());
    match path.filter(|p| p.to_str().is_some_and(|s| !s.is_empty())) {
        Some(path) => batch.paths.push(path),
        None => batch.unreadable = batch.unreadable.saturating_add(1),
    }
}

/// Let Foundation resolve native aliases/bookmarks; never parse its private bookmark format.
fn file_url(descriptor: &NSAppleEventDescriptor) -> Option<Retained<NSURL>> {
    // Native reference blobs are normally a few KiB. Refuse oversized hostile descriptors
    // before asking Foundation to interpret them; the caller counts the unreadable reference.
    if descriptor.data().len() > 1024 * 1024 {
        return None;
    }
    if let Some(url) = descriptor.fileURLValue() {
        return Some(url);
    }
    // SAFETY: descriptorType returns DescType (u32). A bookmark's data is Foundation's
    // documented NSURL bookmark representation, resolved without prompting or mounting.
    let kind: u32 = unsafe { msg_send![descriptor, descriptorType] };
    if kind != BOOKMARK {
        return None;
    }
    // SAFETY: no stale-result pointer is requested; all borrowed objects outlive the call.
    unsafe {
        NSURL::URLByResolvingBookmarkData_options_relativeToURL_bookmarkDataIsStale_error(
            &descriptor.data(),
            NSURLBookmarkResolutionOptions::WithoutUI | NSURLBookmarkResolutionOptions::WithoutMounting,
            None,
            std::ptr::null_mut(),
        )
        .ok()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use objc2::ClassType;
    use objc2_foundation::{NSData, NSString, NSURLBookmarkCreationOptions};
    use std::path::PathBuf;

    /// Construct actual Foundation descriptors, not a Rust stand-in for native path decoding.
    fn descriptor(kind: &[u8; 4], bytes: &[u8]) -> Retained<NSAppleEventDescriptor> {
        // SAFETY: the factory takes DescType/u32 and NSData and returns an optional descriptor.
        let value: Option<Retained<NSAppleEventDescriptor>> = unsafe {
            msg_send![NSAppleEventDescriptor::class(), descriptorWithDescriptorType: u32::from_be_bytes(*kind),
                data: &*NSData::with_bytes(bytes)]
        };
        value.unwrap()
    }

    /// Put a direct parameter into a native open-documents event.
    fn event(direct: &NSAppleEventDescriptor) -> Retained<NSAppleEventDescriptor> {
        // SAFETY: the factory uses u32 codes, a nullable descriptor, i16 return ID and
        // i32 transaction ID, as declared in NSAppleEventDescriptor.h.
        let event: Retained<NSAppleEventDescriptor> = unsafe {
            msg_send![NSAppleEventDescriptor::class(), appleEventWithEventClass: CORE, eventID: OPEN,
                targetDescriptor: None::<&NSAppleEventDescriptor>, returnID: -1i16, transactionID: 0i32]
        };
        // SAFETY: direct is a descriptor and keyDirectObject is a u32 AEKeyword.
        unsafe {
            let _: () = msg_send![&*event, setParamDescriptor: direct, forKeyword: DIRECT];
        }
        event
    }

    #[test]
    fn native_file_urls_keep_spaces_unicode_and_literal_percent() {
        let dir = std::env::temp_dir().join(format!("rustgis-file-urls-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        std::fs::create_dir(dir.join("Área 100%")).unwrap();
        let paths = [dir.join("My project.rgis"), dir.join("Área 100%/sites.csv")];
        for path in &paths {
            std::fs::write(path, b"fixture").unwrap();
        }
        let base = NSURL::from_file_path(&dir).unwrap().absoluteString().unwrap().to_string();
        let list = NSAppleEventDescriptor::listDescriptor();
        for (index, suffix) in ["My%20project.rgis", "%C3%81rea%20100%25/sites.csv"].iter().enumerate() {
            let url = format!("{}/{suffix}", base.trim_end_matches('/'));
            list.insertDescriptor_atIndex(&descriptor(b"furl", url.as_bytes()), index as isize + 1);
        }
        let batch = decode(&event(&list));
        assert_eq!((batch.unreadable, batch.excess), (0, 0));
        assert_eq!(batch.paths.len(), paths.len());
        for (actual, expected) in batch.paths.iter().zip(&paths) {
            // Foundation may decompose accents: verify the file reached, not its Unicode spelling.
            assert_eq!(std::fs::canonicalize(actual).unwrap(), std::fs::canonicalize(expected).unwrap());
        }
        for path in &paths {
            std::fs::remove_file(path).unwrap();
        }
        std::fs::remove_dir(dir.join("Área 100%")).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn native_single_reference_and_malformed_items_are_accounted_for() {
        let file = descriptor(b"furl", b"file:///tmp/project.rgisx");
        let batch = decode(&event(&file));
        assert_eq!(batch.paths, [PathBuf::from("/tmp/project.rgisx")]);
        assert_eq!((batch.unreadable, batch.excess), (0, 0));
        let list = NSAppleEventDescriptor::listDescriptor();
        list.insertDescriptor_atIndex(&file, 1);
        list.insertDescriptor_atIndex(&NSAppleEventDescriptor::nullDescriptor(), 2);
        let batch = decode(&event(&list));
        assert_eq!(batch.paths.len(), 1);
        assert_eq!((batch.unreadable, batch.excess), (1, 0));
        assert_eq!(decode(&event(&NSAppleEventDescriptor::listDescriptor())).unreadable, 1);
    }

    #[test]
    fn native_bookmarks_resolve_a_real_accented_path() {
        let dir = std::env::temp_dir().join(format!("rustgis-documents-{}-Área 100%", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("My project.rgis");
        std::fs::write(&path, b"{}").unwrap();
        let url = NSURL::from_file_path(&path).unwrap();
        let data = url
            .bookmarkDataWithOptions_includingResourceValuesForKeys_relativeToURL_error(NSURLBookmarkCreationOptions::empty(), None, None)
            .unwrap();
        let batch = decode(&event(&descriptor(b"bmrk", &data.to_vec())));
        assert_eq!((batch.unreadable, batch.excess), (0, 0));
        assert_eq!(batch.paths.len(), 1);
        // macOS can spell its temporary directory through /var or /private/var.
        assert_eq!(std::fs::canonicalize(batch.paths.first().unwrap()).unwrap(), std::fs::canonicalize(&path).unwrap());
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn native_non_file_urls_and_oversized_references_are_refused() {
        // SAFETY: URLWithString: takes a valid NSString and returns an optional NSURL.
        let url: Option<Retained<NSURL>> =
            unsafe { msg_send![NSURL::class(), URLWithString: &*NSString::from_str("https://example.com/a.rgis")] };
        assert!(!url.unwrap().isFileURL());
        let batch = decode(&event(&descriptor(b"furl", b"https://example.com/a.rgis")));
        assert!(batch.paths.is_empty());
        assert_eq!(batch.unreadable, 1);
        let batch = decode(&event(&descriptor(b"bmrk", &vec![0; 1024 * 1024 + 1])));
        assert!(batch.paths.is_empty());
        assert_eq!(batch.unreadable, 1);
    }
}
