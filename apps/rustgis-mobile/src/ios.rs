//! The iOS app: the shell ([`crate::shell`]) in winit's UIKit window, with UIKit's document
//! picker, Core Location and the camera as its [`Platform`], called through `objc2`.
//!
//! Everything here runs on the main thread, where UIKit calls its delegates: between frames, as
//! the run loop turns. What they report waits in [`Shared`] until the next frame takes it, as the
//! Android activity's events do. Copying picked documents, writing a file to save and writing a
//! photo run on threads of their own.
//!
//! The delegates are Objective-C classes made here (`define_class!`): UIKit keeps only a weak
//! reference to a delegate, so [`Ios`] holds each one for as long as it is wanted.
// UIKit's delegates are Objective-C classes declared here, and a few of its calls are unsafe;
// each block says why it holds.
#![expect(unsafe_code, reason = "Objective-C classes and the few UIKit calls objc2 marks unsafe; see each SAFETY")]

use crate::shell;
use eframe::egui;
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_core_location::{
    CLAuthorizationStatus, CLError, CLLocation, CLLocationManager, CLLocationManagerDelegate, kCLLocationAccuracyBest,
};
use objc2_foundation::{NSArray, NSError, NSHomeDirectory, NSNotification, NSNotificationCenter, NSString, NSURL, NSValue};
use objc2_ui_kit::{
    NSValueUIGeometryExtensions, UIApplication, UIDocumentPickerDelegate, UIDocumentPickerViewController, UIImage, UIImagePickerController,
    UIImagePickerControllerDelegate, UIImagePickerControllerInfoKey, UIImagePickerControllerOriginalImage,
    UIImagePickerControllerSourceType, UIKeyboardFrameEndUserInfoKey, UIKeyboardWillChangeFrameNotification,
    UIKeyboardWillHideNotification, UINavigationControllerDelegate, UIScene, UISceneDelegate, UIViewController, UIWindow,
    UIWindowDidBecomeVisibleNotification, UIWindowScene, UIWindowSceneDelegate,
};
use objc2_uniform_type_identifiers::UTTypeItem;
use rustgis_engine::platform::{Event, Fix, Platform};
use rustgis_ui_egui::i18n;
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// The most positions kept for the front end: two days of one a second out of sight, as on
/// Android. Past it the oldest go, and how many is said ([`crate::Queue`]).
const MOST_POSITIONS: usize = 200_000;

/// What the delegates report, until the next frame takes it.
struct Shared {
    events: Mutex<crate::Queue>,
    /// Pickers (and the camera) on the screen, not yet answered.
    showing: Mutex<usize>,
    /// Work on threads of its own whose answer is still to come: picked documents being
    /// copied, a photo being written. The window looks again while there is any.
    working: Mutex<usize>,
}

impl Default for Shared {
    fn default() -> Shared {
        Shared { events: Mutex::new(crate::Queue::new(MOST_POSITIONS)), showing: Mutex::new(0), working: Mutex::new(0) }
    }
}

/// One more (`up`) or one fewer of a count, never below zero: an answer UIKit gives twice
/// counts once.
fn count(counter: &Mutex<usize>, up: bool) {
    let mut n = counter.lock().unwrap_or_else(|e| e.into_inner());
    *n = if up { n.saturating_add(1) } else { n.saturating_sub(1) };
}

/// Draws the window again: a picker that answers on the main thread, with no work left to do,
/// would otherwise wait for a touch to be taken (the window doesn't look while one is up).
fn wake(mtm: MainThreadMarker) {
    if let Some(view) = key_window(mtm).and_then(|window| window.rootViewController()).and_then(|controller| controller.view()) {
        view.setNeedsDisplay();
    }
}

impl Shared {
    fn add(&self, event: Event) {
        self.events.lock().unwrap_or_else(|e| e.into_inner()).add(event);
    }

    fn failed(&self, text: String) {
        self.add(Event::Failed(text));
    }

    fn showing(&self) -> bool {
        *self.showing.lock().unwrap_or_else(|e| e.into_inner()) > 0
    }

    fn working(&self) -> bool {
        *self.working.lock().unwrap_or_else(|e| e.into_inner()) > 0
    }
}

/// The app's own folders in its sandbox.
#[derive(Clone)]
struct Folders {
    /// `Library/Application Support`: settings, app data, the cache, picked copies and photos.
    support: PathBuf,
    /// `tmp`: files waiting for the export picker.
    temporary: PathBuf,
}

/// What a document picker was shown for.
enum Asked {
    /// Files to open, for the front end's `purpose`.
    Open { purpose: String },
    /// A file to save: offered under `name`, written to `temporary` until the picker has put it
    /// where the user chose.
    Save { name: String, temporary: PathBuf },
}

struct PickerIvars {
    shared: Arc<Shared>,
    folders: Folders,
    asked: RefCell<Option<Asked>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and this class doesn't implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "RustGISPickerDelegate"]
    #[ivars = PickerIvars]
    struct PickerDelegate;

    unsafe impl NSObjectProtocol for PickerDelegate {}

    unsafe impl UIDocumentPickerDelegate for PickerDelegate {
        #[unsafe(method(documentPicker:didPickDocumentsAtURLs:))]
        fn did_pick(&self, _controller: &UIDocumentPickerViewController, urls: &NSArray<NSURL>) {
            self.picked(urls.to_vec());
        }

        #[unsafe(method(documentPickerWasCancelled:))]
        fn cancelled(&self, _controller: &UIDocumentPickerViewController) {
            self.picked(Vec::new());
        }
    }
);

impl PickerDelegate {
    fn new(mtm: MainThreadMarker, shared: Arc<Shared>, folders: Folders, asked: Asked) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(PickerIvars { shared, folders, asked: RefCell::new(Some(asked)) });
        // SAFETY: NSObject's `init` on a freshly allocated object.
        unsafe { msg_send![super(this), init] }
    }

    /// The picker answered: `urls` chosen, none when it was cancelled.
    fn picked(&self, urls: Vec<Retained<NSURL>>) {
        let ivars = self.ivars();
        let shared = ivars.shared.clone();
        match ivars.asked.borrow_mut().take() {
            // Answered already.
            None => {}
            Some(Asked::Save { name, temporary }) => {
                if urls.is_empty() {
                    shared.failed(i18n::fmt("{name} wasn't saved: no place was chosen for it.", &[("name", &name)]));
                } else {
                    // The picker has copied the file where the user chose.
                    shared.add(Event::Saved { name });
                }
                remove_with_folder(&temporary);
                count(&shared.showing, false);
                wake(self.mtm());
            }
            Some(Asked::Open { .. }) if urls.is_empty() => {
                shared.add(Event::Cancelled);
                count(&shared.showing, false);
                wake(self.mtm());
            }
            Some(Asked::Open { purpose }) => {
                // Answered: the copies are work from now on. The window is woken, so it looks
                // again while they run.
                count(&shared.showing, false);
                count(&shared.working, true);
                wake(self.mtm());
                // The documents are the originals, wherever they are (iCloud Drive, another
                // app's folder): each is read inside its security scope, held until its copy is
                // made.
                // One folder for what was picked together, and for nothing else, with a folder of
                // its own for each folder the documents came from: a file read with the one
                // beside it (a .shp and its .dbf) finds that one, not one of the same name picked
                // from elsewhere, and a later pick can't replace a file the session has open.
                let root = ivars.folders.support.join("picked");
                let picked = root.join(unique());
                // Copied off the main thread: a document can be large, or come from the network.
                let (copied, unstarted) = (shared.clone(), purpose.clone());
                let copying = std::thread::Builder::new().name("picked documents".into()).spawn(move || {
                    // Copies of documents the user has elsewhere: left out of the device's
                    // backups (iCloud, a computer's), as the app can be given them again.
                    if std::fs::create_dir_all(&root).is_ok() {
                        exclude_from_backup(&copied, &root);
                    }
                    let mut copies = 0usize;
                    let mut sources: Vec<Option<String>> = Vec::new();
                    for url in urls {
                        let source = url.URLByDeletingLastPathComponent().and_then(|u| u.path()).map(|p| p.to_string());
                        let k = sources.iter().position(|s| *s == source).unwrap_or_else(|| {
                            sources.push(source);
                            sources.len() - 1
                        });
                        copies += usize::from(copy_in(&copied, &purpose, &url, &picked.join(k.to_string())));
                    }
                    // Nothing copied (each failure said): this request ends, by name, as
                    // another may have been asked for meanwhile (Import DXF stops waiting).
                    if copies == 0 {
                        copied.add(Event::Ended { purpose });
                    }
                    count(&copied.working, false);
                });
                if let Err(e) = copying {
                    shared.failed(i18n::fmt("The files chosen weren't opened: {error}", &[("error", &e.to_string())]));
                    shared.add(Event::Ended { purpose: unstarted });
                    count(&shared.working, false);
                }
            }
        }
    }
}

/// Leaves `folder` (and what is in it) out of the device's backups, or says it couldn't.
fn exclude_from_backup(shared: &Shared, folder: &Path) {
    let url = NSURL::fileURLWithPath(&NSString::from_str(&folder.to_string_lossy()));
    let yes = objc2_foundation::NSNumber::numberWithBool(true);
    // SAFETY: the key's documented value is an NSNumber holding a boolean.
    let set = unsafe { url.setResourceValue_forKey_error(Some(&yes), objc2_foundation::NSURLIsExcludedFromBackupKey) };
    if let Err(e) = set {
        // Not data lost: the copies are then backed up with the app, which is said.
        let error = e.localizedDescription().to_string();
        log::warn!("{} is kept in the device's backups: {error}", folder.display());
        shared.failed(i18n::fmt("The copies of picked files will be in this device's backups: {error}", &[("error", &error)]));
    }
}

/// Copies the picked document at `url` into `folder` in the app's storage, where the front end
/// can open it by path, and says so; or says why it wasn't. The document is read inside its
/// security scope, through a file coordinator: one in iCloud Drive is downloaded first, and
/// another app writing it is waited for. Returns whether it was copied.
fn copy_in(shared: &Shared, purpose: &str, url: &NSURL, folder: &Path) -> bool {
    let name = safe_name(&url.lastPathComponent().map(|n| n.to_string()).unwrap_or_default());
    // SAFETY: a URL the document picker returned; the scope ends below, once read.
    let scoped = unsafe { url.startAccessingSecurityScopedResource() };
    let mut file = folder.join(&name);
    // Two documents of one name from one folder can't be; a name taken is kept apart all the same.
    let mut n = 2;
    while file.exists() {
        file = folder.join(format!("{n}-{name}"));
        n += 1;
    }
    let copied = coordinated_copy(url, folder, &file);
    if scoped {
        // SAFETY: the scope `startAccessingSecurityScopedResource` started above.
        unsafe { url.stopAccessingSecurityScopedResource() };
    }
    match copied {
        Ok(_) => {
            // The copy's own name: a second document of one name is told from the first.
            let shown = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(name);
            shared.add(Event::Picked { purpose: purpose.to_string(), name: shown, path: file });
            true
        }
        Err(error) => {
            // Half a copy would only fill the storage: nothing reports its path to remove it.
            remove_with_folder(&file);
            shared.failed(i18n::fmt("{name} wasn't opened: {error}", &[("name", &name), ("error", &error)]));
            false
        }
    }
}

/// Copies the document at `url` to `file` (in `folder`, made first) inside a coordinated read.
fn coordinated_copy(url: &NSURL, folder: &Path, file: &Path) -> Result<(), String> {
    use objc2::AnyThread as _;
    std::fs::create_dir_all(folder).map_err(|e| e.to_string())?;
    let result: RefCell<Option<Result<(), String>>> = RefCell::new(None);
    let reader = block2::RcBlock::new(|read: std::ptr::NonNull<NSURL>| {
        // SAFETY: the URL the coordinator hands its reader, alive while the reader runs.
        let read = unsafe { read.as_ref() };
        let copied = read
            .path()
            .map(|p| PathBuf::from(p.to_string()))
            .ok_or_else(|| i18n::fmt("the system gave no path for it", &[]))
            .and_then(|path| std::fs::copy(path, file).map(|_| ()).map_err(|e| e.to_string()));
        result.replace(Some(copied));
    });
    let coordinator = objc2_foundation::NSFileCoordinator::initWithFilePresenter(objc2_foundation::NSFileCoordinator::alloc(), None);
    let mut error = None;
    coordinator.coordinateReadingItemAtURL_options_error_byAccessor(
        url,
        objc2_foundation::NSFileCoordinatorReadingOptions::empty(),
        Some(&mut error),
        &reader,
    );
    // The reader is done with: what it found is taken.
    drop(reader);
    match (result.into_inner(), error) {
        (Some(copied), _) => copied,
        (None, Some(error)) => Err(error.localizedDescription().to_string()),
        (None, None) => Err(i18n::fmt("the system didn't let it be read", &[])),
    }
}

/// A document's file name, without anything that would put it outside the folder it is copied
/// to.
fn safe_name(name: &str) -> String {
    let cleaned: String = name.chars().map(|c| if matches!(c, '/' | '\\' | '\t' | '\n' | '\r' | '\0') { '_' } else { c }).collect();
    let trimmed = cleaned.trim_start_matches('.');
    match (cleaned.len() - trimmed.len(), trimmed) {
        (_, "") => "picked".to_string(),
        (0, _) => cleaned,
        (_, rest) => format!("_{rest}"),
    }
}

/// Removes `path`, and its folders once they are empty.
fn remove_with_folder(path: &Path) {
    let _ = std::fs::remove_file(path);
    // Its folder and the one above (a pick's folder for its source folders), each only while
    // empty.
    for folder in path.ancestors().skip(1).take(2) {
        let _ = std::fs::remove_dir(folder);
    }
}

/// A name no other call in this run gives: the time, and a count.
fn unique() -> String {
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    format!("{ms}-{}", COUNT.fetch_add(1, Ordering::Relaxed))
}

struct CameraIvars {
    shared: Arc<Shared>,
    photos: PathBuf,
    answered: Cell<bool>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and this class doesn't implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "RustGISCameraDelegate"]
    #[ivars = CameraIvars]
    struct CameraDelegate;

    unsafe impl NSObjectProtocol for CameraDelegate {}

    unsafe impl UINavigationControllerDelegate for CameraDelegate {}

    unsafe impl UIImagePickerControllerDelegate for CameraDelegate {
        #[unsafe(method(imagePickerController:didFinishPickingMediaWithInfo:))]
        fn did_finish(
            &self,
            picker: &UIImagePickerController,
            info: &objc2_foundation::NSDictionary<UIImagePickerControllerInfoKey, objc2::runtime::AnyObject>,
        ) {
            // SAFETY: a key UIKit defines, read from the dictionary UIKit gave.
            let image = info.objectForKey(unsafe { UIImagePickerControllerOriginalImage }).and_then(|i| i.downcast::<UIImage>().ok());
            self.photo(image.as_deref());
            picker.dismissViewControllerAnimated_completion(true, None);
        }

        #[unsafe(method(imagePickerControllerDidCancel:))]
        fn did_cancel(&self, picker: &UIImagePickerController) {
            if !self.ivars().answered.replace(true) {
                self.ivars().shared.add(Event::Cancelled);
                count(&self.ivars().shared.showing, false);
                wake(self.mtm());
            }
            picker.dismissViewControllerAnimated_completion(true, None);
        }
    }
);

impl CameraDelegate {
    fn new(mtm: MainThreadMarker, shared: Arc<Shared>, photos: PathBuf) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(CameraIvars { shared, photos, answered: Cell::new(false) });
        // SAFETY: NSObject's `init` on a freshly allocated object.
        unsafe { msg_send![super(this), init] }
    }

    /// The camera took `image` (none: UIKit gave no picture): written as a JPEG to the photos
    /// folder, and said.
    fn photo(&self, image: Option<&UIImage>) {
        let ivars = self.ivars();
        if ivars.answered.replace(true) {
            return;
        }
        let shared = ivars.shared.clone();
        count(&shared.showing, false);
        // Encoded here, on the main thread, as UIKit's images are read there (one step, no
        // Cancel); written off it.
        let Some(jpeg) = image.and_then(|image| image.jpeg_representation(0.9)).map(|data| data.to_vec()) else {
            shared.failed(i18n::fmt("The photo wasn't kept: {error}", &[("error", &i18n::fmt("the camera gave no picture", &[]))]));
            // No photo: the camera's answer is none, as when it is cancelled.
            shared.add(Event::Cancelled);
            wake(self.mtm());
            return;
        };
        count(&shared.working, true);
        wake(self.mtm());
        let (photos, writer) = (ivars.photos.clone(), shared.clone());
        let writing = std::thread::Builder::new().name("photo".into()).spawn(move || {
            let name = format!("photo-{}.jpg", unique());
            let path = photos.join(&name);
            match std::fs::create_dir_all(&photos).and_then(|()| std::fs::write(&path, jpeg)) {
                Ok(()) => writer.add(Event::Photo { name, path }),
                Err(e) => {
                    writer.failed(i18n::fmt("The photo wasn't kept: {error}", &[("error", &e.to_string())]));
                    // No photo: the camera's answer is none, as when it is cancelled.
                    writer.add(Event::Cancelled);
                }
            }
            count(&writer.working, false);
        });
        if let Err(e) = writing {
            shared.failed(i18n::fmt("The photo wasn't kept: {error}", &[("error", &e.to_string())]));
            shared.add(Event::Cancelled);
            count(&shared.working, false);
        }
    }
}

struct LocationIvars {
    shared: Arc<Shared>,
    /// Whether positions are wanted: a refusal said after they no longer are isn't said.
    locating: Cell<bool>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and this class doesn't implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "RustGISLocationDelegate"]
    #[ivars = LocationIvars]
    struct LocationDelegate;

    unsafe impl NSObjectProtocol for LocationDelegate {}

    unsafe impl CLLocationManagerDelegate for LocationDelegate {
        #[unsafe(method(locationManager:didUpdateLocations:))]
        fn did_update(&self, _manager: &CLLocationManager, locations: &NSArray<CLLocation>) {
            if !self.ivars().locating.get() {
                return;
            }
            for location in locations.iter() {
                match fix(&location) {
                    Some(fix) => self.ivars().shared.add(Event::Position(fix)),
                    // Counted, and said with the positions taken next.
                    None => self.ivars().shared.events.lock().unwrap_or_else(|e| e.into_inner()).reject(),
                }
            }
        }

        #[unsafe(method(locationManager:didFailWithError:))]
        fn did_fail(&self, manager: &CLLocationManager, error: &NSError) {
            let code = error.code();
            // No position yet: Core Location goes on trying.
            if code == CLError::LocationUnknown.0 {
                return;
            }
            let why = if code == CLError::Denied.0 {
                i18n::fmt("RustGIS wasn't given leave to use this device's location: its position can't be shown.", &[])
            } else {
                i18n::fmt("This device's location can't be read: {error}", &[("error", &error.localizedDescription().to_string())])
            };
            self.stopped(manager, why);
        }

        #[unsafe(method(locationManagerDidChangeAuthorization:))]
        fn did_change_authorization(&self, manager: &CLLocationManager) {
            // SAFETY: a property read on the manager that called.
            let status = unsafe { manager.authorizationStatus() };
            if status == CLAuthorizationStatus::Denied || status == CLAuthorizationStatus::Restricted {
                self.stopped(
                    manager,
                    i18n::fmt("RustGIS wasn't given leave to use this device's location: its position can't be shown.", &[]),
                );
            } else if status != CLAuthorizationStatus::NotDetermined && self.ivars().locating.get() {
                // The user allowed it: positions start now, not before (asked before the answer,
                // Core Location reports the location as refused).
                // SAFETY: a plain call on the manager that called, on the main thread.
                unsafe { manager.startUpdatingLocation() };
            }
        }
    }
);

impl LocationDelegate {
    fn new(mtm: MainThreadMarker, shared: Arc<Shared>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(LocationIvars { shared, locating: Cell::new(false) });
        // SAFETY: NSObject's `init` on a freshly allocated object.
        unsafe { msg_send![super(this), init] }
    }

    /// Positioning ended without the front end asking: why is said, and [`Event::Stopped`]
    /// tells the interface no position is coming.
    fn stopped(&self, manager: &CLLocationManager, why: String) {
        if !self.ivars().locating.replace(false) {
            return;
        }
        // SAFETY: plain calls on the manager this delegate serves, on the main thread.
        unsafe {
            manager.stopUpdatingLocation();
            manager.setAllowsBackgroundLocationUpdates(false);
        }
        self.ivars().shared.failed(why);
        self.ivars().shared.add(Event::Stopped);
    }
}

/// Where `location` is, or `None` when Core Location says it isn't valid (a negative accuracy).
fn fix(location: &CLLocation) -> Option<Fix> {
    // SAFETY: plain property reads on a location Core Location gave.
    let (place, accuracy, time) = unsafe { (location.coordinate(), location.horizontalAccuracy(), location.timestamp()) };
    if accuracy < 0.0 || !(-180.0..=180.0).contains(&place.longitude) || !(-90.0..=90.0).contains(&place.latitude) {
        return None;
    }
    let ms = (time.timeIntervalSince1970() * 1000.0).round();
    Some(Fix {
        lon: place.longitude,
        lat: place.latitude,
        accuracy: Some(accuracy).filter(|a| a.is_finite()),
        time: (ms.is_finite() && ms >= 0.0).then_some(ms as i64),
    })
}

struct KeyboardIvars {
    /// The height the soft keyboard takes from the bottom of the screen, in points.
    height: Cell<f64>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and this class doesn't implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "RustGISKeyboardObserver"]
    #[ivars = KeyboardIvars]
    struct KeyboardObserver;

    impl KeyboardObserver {
        #[unsafe(method(keyboardChanged:))]
        fn changed(&self, notification: &NSNotification) {
            let mtm = self.mtm();
            // SAFETY: the key and the value type UIKit documents for this notification.
            let frame = notification.userInfo().and_then(|info| info.objectForKey(unsafe { UIKeyboardFrameEndUserInfoKey }))
                .and_then(|value| value.downcast::<NSValue>().ok())
                .map(|value| unsafe { value.CGRectValue() });
            // The keyboard's frame is the screen's; the window's frame there (which isn't the whole
            // screen in Stage Manager or Split View) says how much of the window it covers.
            let Some(window) = key_window(mtm).map(|window| window.frame()) else { return };
            let bottom = window.origin.y + window.size.height;
            // Only a keyboard across the window takes room from it: a floating one beside a window
            // in Split View or Stage Manager takes none.
            let height = frame
                .map(|f| {
                    let across = f.origin.x < window.origin.x + window.size.width && f.origin.x + f.size.width > window.origin.x;
                    if across { (bottom - f.origin.y).clamp(0.0, window.size.height) } else { 0.0 }
                })
                .unwrap_or(0.0);
            self.ivars().height.set(height);
        }

        #[unsafe(method(keyboardHidden:))]
        fn hidden(&self, _notification: &NSNotification) {
            self.ivars().height.set(0.0);
        }
    }
);

impl KeyboardObserver {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(KeyboardIvars { height: Cell::new(0.0) });
        // SAFETY: NSObject's `init` on a freshly allocated object.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        let center = NSNotificationCenter::defaultCenter();
        // SAFETY: the selectors are this class's own methods, taking the notification; the
        // observer is removed in `Ios`'s Drop, and lives as long as it.
        unsafe {
            center.addObserver_selector_name_object(&this, sel!(keyboardChanged:), Some(UIKeyboardWillChangeFrameNotification), None);
            center.addObserver_selector_name_object(&this, sel!(keyboardHidden:), Some(UIKeyboardWillHideNotification), None);
        }
        this
    }
}

thread_local! {
    /// The app's window scene, once UIKit has connected it ([`SceneDelegate`]).
    static SCENE: RefCell<Option<Retained<UIWindowScene>>> = const { RefCell::new(None) };
    /// winit's window, once it is shown ([`WindowWatcher`]).
    static WINDOW: RefCell<Option<Retained<UIWindow>>> = const { RefCell::new(None) };
}

/// Puts winit's window in the app's scene once both are there, whichever comes second: iOS
/// shows a window that is in no scene as nothing at all, and never asks it to draw. winit 0.30
/// makes its window the way apps did before scenes, after the scene is connected.
fn attach_window() {
    let Some(scene) = SCENE.with(|kept| kept.borrow().clone()) else { return };
    let Some(window) = WINDOW.with(|kept| kept.borrow_mut().take()) else { return };
    if window.windowScene().is_none() {
        window.setWindowScene(Some(&scene));
        window.makeKeyAndVisible();
    }
    fill_screen(&window);
    match window.rootViewController().and_then(|controller| controller.view()) {
        Some(view) => take_return(&view),
        // winit makes its view controller with the window, so this shouldn't happen: said in the
        // log, as Return then does nothing for the session.
        None => log::warn!("winit's window has no view yet: the soft keyboard's Return won't reach the form"),
    }
}

/// The name of [`take_return`]'s class.
const RETURN_KEY: &std::ffi::CStr = c"RustGISKeyView";

thread_local! {
    /// Whether the soft keyboard's Return was pressed since the shell last asked
    /// ([`Ios::take_return`]).
    static RETURNED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Makes the soft keyboard's Return reach egui, by making winit's view an instance of a
/// subclass of winit's whose `insertText:` keeps a Return for the shell ([`RETURNED`]) and
/// passes everything else on. winit 0.30 hands Return on as the character `"\n"`, which egui
/// takes for no key at all: a field then couldn't be left with Return. As Enter, it does what
/// each field does with Enter.
fn take_return(view: &objc2_ui_kit::UIView) {
    use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, Sel};
    if view.class().name() == RETURN_KEY {
        return;
    }
    extern "C-unwind" fn insert_text(this: &AnyObject, _cmd: Sel, text: &NSString) {
        if text.to_string() == "\n" {
            RETURNED.with(|r| r.set(true));
            if let Some(mtm) = MainThreadMarker::new() {
                wake(mtm);
            }
            return;
        }
        let Some(winit) = this.class().superclass() else {
            // A subclass always has one: said in the log should it not, as the text is dropped.
            log::warn!("typed text was dropped: winit's view has no superclass");
            return;
        };
        // SAFETY: `this` is winit's view (the class is only given to it), whose class is the
        // superclass here; its `insertText:` takes the text.
        unsafe { msg_send![super(this, winit), insertText: text] }
    }
    let class = AnyClass::get(RETURN_KEY).or_else(|| {
        let mut builder = ClassBuilder::new(RETURN_KEY, view.class())?;
        // SAFETY: the method has the signature UIKeyInput's `insertText:` has.
        unsafe { builder.add_method(sel!(insertText:), insert_text as extern "C-unwind" fn(_, _, _)) };
        Some(builder.register())
    });
    if let Some(class) = class {
        // SAFETY: the new class is a subclass of the view's own, with no instance variables of
        // its own, so the view is a valid instance of it.
        unsafe { objc2::ffi::object_setClass((view as *const objc2_ui_kit::UIView).cast_mut().cast(), class) };
    }
}

/// The name of [`fill_screen`]'s class.
const FULL_SCREEN: &std::ffi::CStr = c"RustGISFullScreenWindow";

/// Gives winit's window a safe area of none, the real one kept as `rustgisSafeAreaInsets`
/// ([`safe_area`]), by making it an instance of a subclass of winit's that says so.
///
/// eframe 0.37 makes its GPU surface winit's `inner_size`, which winit 0.30 takes from the
/// window's safe area, while egui lays out over its `outer_size`, the whole screen: on a phone
/// with a notch or a home indicator the surface would be stretched over the screen, its bottom
/// cut off. With no safe area the two agree, and the shell keeps the interface out of the real
/// one itself ([`Ios::insets`]).
fn fill_screen(window: &UIWindow) {
    use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, Sel};
    use objc2_ui_kit::UIEdgeInsets;
    if window.class().name() == FULL_SCREEN {
        return;
    }
    extern "C-unwind" fn none(_this: &AnyObject, _cmd: Sel) -> UIEdgeInsets {
        UIEdgeInsets { top: 0.0, left: 0.0, bottom: 0.0, right: 0.0 }
    }
    extern "C-unwind" fn real(this: &AnyObject, _cmd: Sel) -> UIEdgeInsets {
        // SAFETY: `this` is a UIWindow (the class is only given to one); UIWindow's own
        // `safeAreaInsets` takes nothing and returns the insets.
        unsafe { msg_send![super(this, UIWindow::class()), safeAreaInsets] }
    }
    let class = AnyClass::get(FULL_SCREEN).or_else(|| {
        let mut builder = ClassBuilder::new(FULL_SCREEN, window.class())?;
        // SAFETY: both methods have the signature UIKit's `safeAreaInsets` has.
        unsafe {
            builder.add_method(sel!(safeAreaInsets), none as extern "C-unwind" fn(_, _) -> _);
            builder.add_method(sel!(rustgisSafeAreaInsets), real as extern "C-unwind" fn(_, _) -> _);
        }
        Some(builder.register())
    });
    if let Some(class) = class {
        // SAFETY: the new class is a subclass of the window's own, with no instance variables of
        // its own, so the window is a valid instance of it.
        unsafe { objc2::ffi::object_setClass((window as *const UIWindow).cast_mut().cast(), class) };
        window.setNeedsLayout();
    }
}

/// The window's real safe area, in points: from before [`fill_screen`] hid it from winit.
fn safe_area(window: &UIWindow) -> objc2_ui_kit::UIEdgeInsets {
    if window.class().name() == FULL_SCREEN {
        // SAFETY: the method `fill_screen` added to the window's class.
        unsafe { msg_send![window, rustgisSafeAreaInsets] }
    } else {
        window.safeAreaInsets()
    }
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and this class doesn't implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "RustGISSceneDelegate"]
    struct SceneDelegate;

    unsafe impl NSObjectProtocol for SceneDelegate {}

    unsafe impl UISceneDelegate for SceneDelegate {}

    unsafe impl UIWindowSceneDelegate for SceneDelegate {}

    impl SceneDelegate {
        /// The app's scene is there: kept, for winit's window ([`attach_window`]).
        #[unsafe(method(scene:willConnectToSession:options:))]
        fn will_connect(&self, scene: &UIScene, _session: &objc2::runtime::AnyObject, _options: &objc2::runtime::AnyObject) {
            if let Ok(scene) = scene.retain().downcast::<UIWindowScene>() {
                SCENE.with(|kept| kept.replace(Some(scene)));
                attach_window();
            }
        }
    }
);

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and this class doesn't implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "RustGISWindowWatcher"]
    struct WindowWatcher;

    impl WindowWatcher {
        /// A window was shown: one in no scene is winit's, kept for the scene
        /// ([`attach_window`]).
        #[unsafe(method(windowShown:))]
        fn shown(&self, notification: &NSNotification) {
            let Some(window) = notification.object().and_then(|o| o.downcast::<UIWindow>().ok()) else { return };
            if window.windowScene().is_none() {
                WINDOW.with(|kept| kept.replace(Some(window)));
                attach_window();
            }
        }
    }
);

impl WindowWatcher {
    /// Starts watching for winit's window; the watcher lives as long as the app.
    fn start(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSObject's `init` on a freshly allocated object.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        // SAFETY: the selector is this class's own method, taking the notification; `main`
        // leaks the watcher, so it outlives the observation.
        unsafe {
            NSNotificationCenter::defaultCenter().addObserver_selector_name_object(
                &this,
                sel!(windowShown:),
                Some(UIWindowDidBecomeVisibleNotification),
                None,
            );
        }
        this
    }
}

/// The iPhone or iPad, as a [`Platform`].
pub struct Ios {
    mtm: MainThreadMarker,
    shared: Arc<Shared>,
    folders: Folders,
    /// The document picker's delegate while one is shown (UIKit holds it weakly).
    picker: RefCell<Option<Retained<PickerDelegate>>>,
    /// The camera's delegate while it is shown.
    camera: RefCell<Option<Retained<CameraDelegate>>>,
    location: Retained<CLLocationManager>,
    located: Retained<LocationDelegate>,
    keyboard: Retained<KeyboardObserver>,
    /// A file to save being written, until its export picker shows.
    saving: RefCell<Option<Saving>>,
}

/// Whether a file to save is written: `None` while it is being written.
type Written = Arc<Mutex<Option<Result<(), String>>>>;

/// A file to save, being written to the app's tmp folder before its export picker shows.
struct Saving {
    name: String,
    temporary: PathBuf,
    done: Written,
}

impl Drop for Ios {
    fn drop(&mut self) {
        // SAFETY: removes the observer `KeyboardObserver::new` added.
        unsafe { NSNotificationCenter::defaultCenter().removeObserver(&self.keyboard) };
    }
}

impl Ios {
    fn new(mtm: MainThreadMarker, folders: Folders) -> Ios {
        let shared = Arc::new(Shared::default());
        // SAFETY: a manager made and used on the main thread only.
        let location = unsafe { CLLocationManager::new() };
        let located = LocationDelegate::new(mtm, shared.clone());
        // SAFETY: plain setters on a manager made here; the delegate is held beside it.
        unsafe {
            location.setDelegate(Some(ProtocolObject::from_ref(&*located)));
            location.setDesiredAccuracy(kCLLocationAccuracyBest);
            // A track recorded out of sight goes on until it is stopped.
            location.setPausesLocationUpdatesAutomatically(false);
        }
        Ios {
            mtm,
            shared,
            folders,
            picker: RefCell::new(None),
            camera: RefCell::new(None),
            location,
            located,
            keyboard: KeyboardObserver::new(mtm),
            saving: RefCell::new(None),
        }
    }

    /// Shows the export picker for the file being saved, once it is written; or says why it
    /// wasn't. Asked each frame (`take_events`), on the main thread.
    fn show_export(&self) {
        let written = {
            let saving = self.saving.borrow();
            let Some(saving) = saving.as_ref() else { return };
            match saving.done.lock().unwrap_or_else(|e| e.into_inner()).take() {
                Some(written) => written,
                None => return,
            }
        };
        let Some(Saving { name, temporary, .. }) = self.saving.borrow_mut().take() else { return };
        let presenter = written.and_then(|()| self.presenter());
        let presenter = match presenter {
            Ok(presenter) => presenter,
            Err(error) => {
                remove_with_folder(&temporary);
                self.shared.failed(i18n::fmt("{name} wasn't saved: {error}", &[("name", &name), ("error", &error)]));
                return;
            }
        };
        let url = NSURL::fileURLWithPath(&NSString::from_str(&temporary.to_string_lossy()));
        let urls = NSArray::from_retained_slice(&[url]);
        let picker =
            UIDocumentPickerViewController::initForExportingURLs_asCopy(UIDocumentPickerViewController::alloc(self.mtm), &urls, true);
        let delegate = PickerDelegate::new(self.mtm, self.shared.clone(), self.folders.clone(), Asked::Save { name, temporary });
        picker.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        self.picker.replace(Some(delegate));
        if let Err(error) = self.present(&presenter, &picker) {
            // Not shown: its delegate will never answer, so the file goes now.
            if let Some(Asked::Save { name, temporary }) =
                self.picker.borrow().as_ref().and_then(|delegate| delegate.ivars().asked.borrow_mut().take())
            {
                remove_with_folder(&temporary);
                self.shared.failed(i18n::fmt("{name} wasn't saved: {error}", &[("name", &name), ("error", &error)]));
            }
        }
    }

    /// Shows `picker` over `presenter`, counted as showing until it answers; or says why it
    /// couldn't be. UIKit declines a presentation (another under way, a presenter leaving the
    /// window) with only a log line, and then no delegate ever answers: counted, it would hold
    /// back every later picker until the app is started again.
    fn present(&self, presenter: &UIViewController, picker: &UIViewController) -> Result<(), String> {
        count(&self.shared.showing, true);
        presenter.presentViewController_animated_completion(picker, true, None);
        if picker.presentingViewController().is_none() {
            count(&self.shared.showing, false);
            return Err(i18n::fmt("The picker couldn't be shown just now: try again.", &[]));
        }
        Ok(())
    }

    /// The window's view controller on top, to show a picker over.
    fn presenter(&self) -> Result<Retained<UIViewController>, String> {
        let window = key_window(self.mtm).ok_or_else(|| i18n::fmt("RustGIS has no window to show the picker over yet.", &[]))?;
        let mut top = window.rootViewController().ok_or_else(|| i18n::fmt("RustGIS has no window to show the picker over yet.", &[]))?;
        while let Some(next) = top.presentedViewController() {
            top = next;
        }
        Ok(top)
    }

    /// `Err(what)` while a picker of ours is up or a file to save waits for one: the caller is
    /// told this one wasn't shown.
    fn not_already_asking(&self, what: String) -> Result<(), String> {
        if self.shared.showing() || self.saving.borrow().is_some() { Err(what) } else { Ok(()) }
    }
}

/// The app's window in front, where it has one.
fn key_window(mtm: MainThreadMarker) -> Option<Retained<UIWindow>> {
    let app = UIApplication::sharedApplication(mtm);
    // Each scene is checked to be a window scene before it is used as one.
    app.connectedScenes()
        .iter()
        .filter_map(|scene| scene.downcast::<UIWindowScene>().ok())
        .find_map(|scene| scene.keyWindow().or_else(|| scene.windows().firstObject()))
}

impl Platform for Ios {
    fn pick_files(&self, purpose: &str) -> Result<(), String> {
        self.not_already_asking(i18n::fmt("Another file is already being chosen. Choose files again when it is done.", &[]))?;
        let presenter = self.presenter()?;
        // Any document: what the front end can't open, it says.
        // SAFETY: a type UniformTypeIdentifiers defines.
        let types = NSArray::from_retained_slice(&[unsafe { UTTypeItem }.retain()]);
        let picker = UIDocumentPickerViewController::initForOpeningContentTypes_asCopy(
            UIDocumentPickerViewController::alloc(self.mtm),
            &types,
            false,
        );
        // Several files only where the front end takes several, as on Android: a Shapefile's
        // parts are chosen together, while two projects would replace one another and a
        // dialog's request (a DXF's options) goes with one file.
        picker.setAllowsMultipleSelection(purpose == "data" || purpose == "shapefile");
        let delegate =
            PickerDelegate::new(self.mtm, self.shared.clone(), self.folders.clone(), Asked::Open { purpose: purpose.to_string() });
        picker.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        self.picker.replace(Some(delegate));
        self.present(&presenter, &picker)
    }

    fn save_file(&self, name: &str, bytes: Vec<u8>) -> Result<(), String> {
        // Refused, not dropped: the caller is told the file wasn't saved.
        self.not_already_asking(i18n::fmt("{name} wasn't saved: another file is waiting for its place to be chosen.", &[("name", name)]))?;
        // The export picker copies a file: it is written to the app's tmp folder first, under
        // the name offered (off the main thread: a project or an export can be large), and the
        // picker is shown by the frame after it is written ([`Ios::show_export`]). The file is
        // removed once the picker has answered.
        let temporary = self.folders.temporary.join("save").join(unique()).join(safe_name(name));
        let done: Written = Arc::default();
        let (file, finished) = (temporary.clone(), done.clone());
        let writing = std::thread::Builder::new().name("file to save".into()).spawn(move || {
            let written = file.parent().map(std::fs::create_dir_all).unwrap_or(Ok(())).and_then(|()| std::fs::write(&file, &bytes));
            *finished.lock().unwrap_or_else(|e| e.into_inner()) = Some(written.map_err(|e| e.to_string()));
        });
        if let Err(e) = writing {
            return Err(i18n::fmt("{name} wasn't saved: {error}", &[("name", name), ("error", &e.to_string())]));
        }
        // Waiting from now: another save is refused until this one's picker has answered.
        self.saving.replace(Some(Saving { name: name.to_string(), temporary, done }));
        Ok(())
    }

    fn locate(&self, on: bool) -> Result<(), String> {
        let manager = &self.location;
        self.located.ivars().locating.set(on);
        // SAFETY: plain calls on the manager made in `new`, on the main thread.
        unsafe {
            if !on {
                manager.stopUpdatingLocation();
                return Ok(());
            }
            match manager.authorizationStatus() {
                CLAuthorizationStatus::Denied | CLAuthorizationStatus::Restricted => {
                    self.located.stopped(
                        manager,
                        i18n::fmt("RustGIS wasn't given leave to use this device's location: allow it in Settings › Privacy & Security › Location Services.", &[]),
                    );
                    return Ok(());
                }
                // Asked: positions start once the user allows them
                // (`locationManagerDidChangeAuthorization`).
                CLAuthorizationStatus::NotDetermined => manager.requestWhenInUseAuthorization(),
                _ => manager.startUpdatingLocation(),
            }
        }
        Ok(())
    }

    fn keep_locating(&self, on: bool) -> Result<(), String> {
        // With the app's `location` background mode (Info.plist), positions keep coming out of
        // sight while this is on; iOS shows that the app is using the location in the status
        // bar, as Android shows its notice.
        // SAFETY: plain setters on the manager made in `new`, on the main thread.
        unsafe {
            self.location.setAllowsBackgroundLocationUpdates(on);
            self.location.setShowsBackgroundLocationIndicator(on);
        }
        Ok(())
    }

    fn take_photo(&self) -> Result<(), String> {
        if !UIImagePickerController::isSourceTypeAvailable(UIImagePickerControllerSourceType::Camera, self.mtm) {
            return Err(i18n::fmt("This device has no camera.", &[]));
        }
        self.not_already_asking(i18n::fmt("A photo is already being taken. Take another when it is done.", &[]))?;
        let presenter = self.presenter()?;
        let picker = UIImagePickerController::new(self.mtm);
        picker.setSourceType(UIImagePickerControllerSourceType::Camera);
        let delegate = CameraDelegate::new(self.mtm, self.shared.clone(), self.folders.support.join("photos"));
        // SAFETY: the delegate conforms to both protocols the picker asks of it.
        unsafe { picker.setDelegate(Some(&delegate)) };
        self.camera.replace(Some(delegate));
        self.present(&presenter, &picker)
    }

    fn take_events(&self) -> Vec<Event> {
        self.show_export();
        self.shared.events.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

impl shell::Phone for Ios {
    fn insets(&self, pixels_per_point: f32) -> Option<(egui::SafeAreaInsets, Option<bool>)> {
        let window = key_window(self.mtm)?;
        let safe = safe_area(&window);
        let keyboard = self.keyboard.ivars().height.get();
        // UIKit's points are the screen's; egui's are those over its own zoom.
        let scale = window.screen().scale() as f32;
        let points = |edge: f64| (edge.max(0.0) as f32) * scale / pixels_per_point.max(0.1);
        let edges = egui::SafeAreaInsets(egui::epaint::MarginF32 {
            left: points(safe.left),
            top: points(safe.top),
            right: points(safe.right),
            // The soft keyboard, while it is up, takes the bottom too: what is typed in is kept
            // above it.
            bottom: points(safe.bottom.max(keyboard)),
        });
        Some((edges, Some(keyboard > 0.0)))
    }

    fn bars_light(&self, light: bool, frame: &eframe::Frame) {
        use winit::platform::ios::{StatusBarStyle, WindowExtIOS};
        if let Some(window) = frame.winit_window() {
            window.set_preferred_status_bar_style(if light { StatusBarStyle::DarkContent } else { StatusBarStyle::LightContent });
        }
    }

    fn take_return(&self) -> bool {
        RETURNED.with(|r| r.replace(false))
    }

    fn busy(&self) -> bool {
        // Not while a picker is merely up (the user may browse for minutes): its answer wakes
        // the window ([`wake`]).
        self.shared.working() || self.saving.borrow().is_some() || !self.shared.events.lock().unwrap_or_else(|e| e.into_inner()).is_empty()
    }

    fn picking(&self) -> bool {
        self.shared.showing()
    }
}

/// `log`'s records, on the app's standard error: Xcode's console and `xcrun simctl launch
/// --console` show them, as `adb logcat` shows Android's.
struct Logger;

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            eprintln!("rustgis {} {}: {}", record.level(), record.target(), record.args());
        }
    }

    fn flush(&self) {}
}

/// The app's folders: `Library/Application Support` and `tmp` in its sandbox.
fn folders() -> Folders {
    let home = PathBuf::from(NSHomeDirectory().to_string());
    Folders { support: home.join("Library").join("Application Support"), temporary: home.join("tmp") }
}

/// The iOS app's entry point, from the binary's `main` on the main thread. It never returns:
/// UIKit runs the app until the system ends it.
pub fn main() {
    static LOGGER: Logger = Logger;
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
    let Some(mtm) = MainThreadMarker::new() else {
        eprintln!("RustGIS on iOS starts on the main thread only.");
        return;
    };
    // iOS 27 refuses an app without scenes: `Info.plist` names this class as the scene's
    // delegate, which UIKit looks up by name, so it is made before UIKit starts.
    let _ = SceneDelegate::class();
    // Kept for the app's life: the notification center doesn't hold its observers.
    std::mem::forget(WindowWatcher::start(mtm));
    let folders = folders();
    let made = std::fs::create_dir_all(&folders.support).is_ok() && rustgis_engine::data::dirs::set_root(folders.support.clone());
    let mut app = shell::front_end(made);
    // The app's Documents folder: files put there from a computer (Finder, `xcrun simctl`) are
    // opened once the window is up, as on Android.
    let documents = PathBuf::from(NSHomeDirectory().to_string()).join("Documents");
    let pushed = shell::pushed(Some(documents).filter(|d| d.is_dir()), &mut app);
    let phone = Ios::new(mtm, folders);
    let mut wgpu_options = eframe::egui_wgpu::WgpuConfiguration::default();
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut wgpu_options.wgpu_setup {
        // egui's limits, no better than the GPU's own: the iOS Simulator's Metal falls short of
        // wgpu's defaults (15 inter-stage shader variables, not 16), and the window would close.
        setup.device_descriptor = std::sync::Arc::new(|adapter| eframe::wgpu::DeviceDescriptor {
            label: Some("egui wgpu device"),
            required_limits: eframe::wgpu::Limits { max_texture_dimension_2d: 8192, ..Default::default() }
                .or_worse_values_from(&adapter.limits()),
            ..Default::default()
        });
    }
    let options = eframe::NativeOptions { wgpu_options, ..Default::default() };
    if let Err(e) = shell::run(app, Ok(phone), pushed, options) {
        eprintln!("the app stopped: {e}");
    }
}
