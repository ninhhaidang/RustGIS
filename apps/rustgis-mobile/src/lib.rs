//! RustGIS on Android and iOS (#1902).
//!
//! The desktop front end (`rustgis-ui-egui`) with the touch interface, in a `NativeActivity` on
//! Android and a UIKit window on iOS, with the phone (a `rustgis_engine::platform::Platform`)
//! standing where the browser stands for the web app: the system's file picker, the device's
//! position, the camera. `shell` is what both share. See `README.md` for how to build and install
//! them.
//!
//! Not here yet: preferences and the keychain.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(any(target_os = "android", target_os = "ios", test))]
#[cfg_attr(test, expect(dead_code, reason = "host tests exercise picker contracts without starting a phone window"))]
mod shell;

#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "android")]
mod phone;

#[cfg(target_os = "ios")]
mod ios;
#[cfg(target_os = "ios")]
pub use ios::main as ios_main;

/// The files of `picked` to open, each by its path: all but the parts another of them reads from
/// beside it (a Shapefile's `.dbf`, `.shx`, `.prj` and `.cpg`, a MapInfo file's `.mid`), matched
/// by name whatever the case. A part picked without its main file stays, and is refused by name.
#[cfg_attr(not(any(target_os = "android", target_os = "ios", test)), expect(dead_code, reason = "only a phone picks files"))]
fn without_sidecars(picked: Vec<std::path::PathBuf>) -> Vec<std::path::PathBuf> {
    let lower = |part: Option<&std::ffi::OsStr>| part.map(|p| p.to_string_lossy().to_lowercase()).unwrap_or_default();
    let named: Vec<(String, String)> = picked.iter().map(|p| (lower(p.file_stem()), lower(p.extension()))).collect();
    let has = |stem: &str, extension: &str| named.iter().any(|(s, e)| s == stem && e == extension);
    let mut kept = Vec::new();
    for (path, (stem, extension)) in picked.iter().zip(&named) {
        let read_by = match extension.as_str() {
            "dbf" | "shx" | "prj" | "cpg" => "shp",
            "mid" => "mif",
            _ => "",
        };
        if read_by.is_empty() || !has(stem, read_by) {
            kept.push(path.clone());
        }
    }
    kept
}

/// What starts the name of a file from the picker for `purpose` in the inbox, as the browser's
/// shell marks them: the front end sends a marked file to the dialog that asked for it. A DXF
/// request (`dxf:<id>`) carries its id, as Import DXF matches its answer by it.
#[cfg(any(target_os = "android", target_os = "ios", test))]
fn mark(purpose: &str) -> String {
    if let Some(id) = purpose.strip_prefix("dxf:") {
        return format!("{}{id}:", rustgis_ui_egui::DXF_PICK);
    }
    if let Some(id) = purpose.strip_prefix("marker:") {
        return format!("{}{id}:", rustgis_ui_egui::MARKER_PICK);
    }
    match purpose {
        "shortcuts" => rustgis_ui_egui::SHORTCUT_PICK,
        "script" => rustgis_ui_egui::SCRIPT_PICK,
        "text" => rustgis_ui_egui::DELIMITED_PICK,
        "shapefile" => rustgis_ui_egui::SHAPEFILE_PICK,
        "assistantPicture" => rustgis_ui_egui::ASSISTANT_PICK,
        p if p.starts_with(rustgis_ui_egui::TOOL_FILE_PURPOSE) => rustgis_ui_egui::TOOL_FILE_PICK,
        _ => "",
    }
    .to_string()
}

/// The request for files the front end is waiting on, as the shell follows it: which request
/// each answer ends. A request ended with no file is answered empty by the shell, so a dialog
/// waiting for it (Import DXF) stops waiting.
#[cfg(any(target_os = "android", target_os = "ios", test))]
#[derive(Default)]
struct Requests {
    /// What the picker asked for last, and accepted, was asked for (`Platform::pick_files`'s
    /// purpose); empty when nothing is waited on.
    current: String,
    /// Whether the phone was seen busy with it (a picker up, its files being copied).
    seen_busy: bool,
    /// Whether the camera was asked for a photo and hasn't answered: a cancel then is the
    /// camera's, and ends no request for files.
    photo: bool,
}

#[cfg(any(target_os = "android", target_os = "ios", test))]
impl Requests {
    /// A request for `purpose`, `refused` by the phone or not. Returns the request to end now:
    /// a refused one. An accepted one is waited on from now, even while the files of the one
    /// before it are still being copied.
    fn asked(&mut self, purpose: &str, refused: bool) -> Option<String> {
        if refused {
            return Some(purpose.to_string());
        }
        self.current = purpose.to_string();
        self.seen_busy = false;
        None
    }

    /// A file picked for `purpose` arrived: it answers that request, and no other.
    fn picked(&mut self, purpose: &str) {
        if self.current == purpose {
            self.current.clear();
            self.seen_busy = false;
        }
    }

    /// The camera was asked for a photo ([`Requests::cancelled`] is then the camera's).
    fn photo_asked(&mut self) {
        self.photo = true;
    }

    /// The camera answered with a photo.
    fn photo_taken(&mut self) {
        self.photo = false;
    }

    /// A picker or the camera closed with no choice. The camera's (asked for since, and not
    /// answered) ends nothing; a picker's ends the request waited on.
    fn cancelled(&mut self) -> Option<String> {
        if std::mem::take(&mut self.photo) {
            return None;
        }
        self.seen_busy = false;
        Some(std::mem::take(&mut self.current)).filter(|purpose| !purpose.is_empty())
    }

    /// The request for `purpose` ended with no file (every copy failed): that one, whichever
    /// is waited on now.
    fn ended(&mut self, purpose: &str) -> Option<String> {
        self.picked(purpose);
        Some(purpose.to_string())
    }

    /// Each frame, whether the phone is busy with a picker or its files (`pending`). Returns
    /// the request to end: the one waited on, once the phone, seen busy with it, no longer is
    /// and nothing answered it (Android says no more when a copy fails).
    fn frame(&mut self, pending: bool) -> Option<String> {
        if self.current.is_empty() {
            return None;
        }
        if pending {
            self.seen_busy = true;
            None
        } else if self.seen_busy {
            self.seen_busy = false;
            Some(std::mem::take(&mut self.current))
        } else {
            None
        }
    }
}

/// What the phone reported, oldest first, until the front end takes it: at most `most`
/// positions. Past that the oldest position goes, and how many went is said when they are
/// taken; nothing else is dropped (a picked file, a failure), as positions are what pile up, out
/// of sight for days.
#[cfg(any(target_os = "ios", test))]
struct Queue {
    events: std::collections::VecDeque<rustgis_engine::platform::Event>,
    most: usize,
    positions: usize,
    dropped: usize,
    /// Positions the system reported that weren't usable (no accuracy it vouches for, or not
    /// on the Earth), left out since the last take.
    rejected: usize,
}

#[cfg(any(target_os = "ios", test))]
impl Queue {
    fn new(most: usize) -> Queue {
        Queue { events: Default::default(), most: most.max(1), positions: 0, dropped: 0, rejected: 0 }
    }

    fn add(&mut self, event: rustgis_engine::platform::Event) {
        use rustgis_engine::platform::Event;
        if matches!(event, Event::Position(_)) {
            if self.positions >= self.most
                && let Some(oldest) = self.events.iter().position(|e| matches!(e, Event::Position(_)))
            {
                self.events.remove(oldest);
                self.positions -= 1;
                self.dropped += 1;
            }
            self.positions += 1;
        }
        self.events.push_back(event);
    }

    /// Counts a position the system reported that wasn't usable: said when the events are taken.
    fn reject(&mut self) {
        self.rejected += 1;
    }

    fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Everything waiting, oldest first, after a failure that says how many positions were
    /// dropped, when any were.
    fn take(&mut self) -> Vec<rustgis_engine::platform::Event> {
        let mut taken = Vec::with_capacity(self.events.len() + 1);
        if self.dropped > 0 {
            let text = rustgis_ui_egui::i18n::fmt(
                "{count} positions found while RustGIS was out of sight were dropped: more were waiting than it keeps.",
                &[("count", &self.dropped.to_string())],
            );
            taken.push(rustgis_engine::platform::Event::Failed(text));
        }
        if self.rejected > 0 {
            let text = rustgis_ui_egui::i18n::fmt(
                "{count} positions the system reported weren't used: it gave them no accuracy, or they weren't on the Earth.",
                &[("count", &self.rejected.to_string())],
            );
            taken.push(rustgis_engine::platform::Event::Failed(text));
        }
        taken.extend(self.events.drain(..));
        self.positions = 0;
        self.dropped = 0;
        self.rejected = 0;
        taken
    }
}

#[cfg(test)]
mod tests {
    use super::without_sidecars;
    use std::path::PathBuf;

    fn kept(names: &[&str]) -> Vec<String> {
        without_sidecars(names.iter().map(|n| PathBuf::from("/picked/1").join(n)).collect())
            .iter()
            .map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
            .collect()
    }

    #[test]
    fn parts_picked_with_their_file_are_left_to_it() {
        assert_eq!(kept(&["roads.shp", "roads.dbf", "roads.shx", "roads.prj", "roads.cpg"]), ["roads.shp"]);
        assert_eq!(kept(&["ROADS.SHP", "Roads.DBF", "parcels.mif", "parcels.MID"]), ["ROADS.SHP", "parcels.mif"]);
    }

    #[test]
    fn a_dxf_request_keeps_its_id_in_its_mark() {
        assert_eq!(super::mark("dxf:7"), format!("{}7:", rustgis_ui_egui::DXF_PICK));
        assert_eq!(super::mark("shapefile"), rustgis_ui_egui::SHAPEFILE_PICK);
        assert_eq!(super::mark("script"), rustgis_ui_egui::SCRIPT_PICK);
        assert_eq!(super::mark("marker:4"), format!("{}4:", rustgis_ui_egui::MARKER_PICK));
        assert_eq!(super::mark("assistantPicture"), rustgis_ui_egui::ASSISTANT_PICK);
        assert_eq!(super::mark("data"), "");
    }

    #[test]
    fn the_oldest_positions_go_past_the_limit_and_are_counted() {
        use rustgis_engine::platform::{Event, Fix};
        let at = |lon: f64| Event::Position(Fix { lon, lat: 0.0, accuracy: None, time: None, elapsed_realtime_nanos: None });
        let mut queue = super::Queue::new(2);
        queue.add(Event::Cancelled);
        for lon in [1.0, 2.0, 3.0] {
            queue.add(at(lon));
        }
        let taken = queue.take();
        // The dropped position is said first; the picker's answer is kept.
        assert!(matches!(&taken[0], Event::Failed(text) if text.starts_with("1 positions")), "{taken:?}");
        assert_eq!(&taken[1..], &[Event::Cancelled, at(2.0), at(3.0)]);
        // Taken, the queue starts again: under the limit, nothing is said.
        queue.add(at(4.0));
        assert_eq!(queue.take(), vec![at(4.0)]);
        assert!(queue.is_empty());
    }

    #[test]
    fn unusable_positions_are_counted_and_said() {
        use rustgis_engine::platform::Event;
        let mut queue = super::Queue::new(10);
        queue.reject();
        queue.reject();
        queue.add(Event::Cancelled);
        let taken = queue.take();
        assert!(matches!(&taken[0], Event::Failed(text) if text.starts_with("2 positions the system reported")), "{taken:?}");
        assert_eq!(&taken[1..], &[Event::Cancelled]);
        // None left out: nothing said.
        queue.add(Event::Cancelled);
        assert_eq!(queue.take(), vec![Event::Cancelled]);
    }

    #[test]
    fn each_answer_ends_its_own_request() {
        let mut requests = super::Requests::default();
        // A DXF request, accepted; its picker answers and its files are copied.
        assert_eq!(requests.asked("dxf:1", false), None);
        assert_eq!(requests.frame(true), None);
        // While they are copied, another DXF request: accepted, its picker up.
        assert_eq!(requests.asked("dxf:2", false), None);
        assert_eq!(requests.frame(true), None);
        // Every copy of the first fails: the first ends, the second stays waited on.
        assert_eq!(requests.ended("dxf:1"), Some("dxf:1".into()));
        assert_eq!(requests.frame(true), None);
        // A file of the first, had one come, wouldn't answer the second either.
        requests.picked("dxf:1");
        // The second's picker is cancelled: it ends.
        assert_eq!(requests.cancelled(), Some("dxf:2".into()));
        assert_eq!(requests.frame(false), None);
    }

    #[test]
    fn a_camera_cancel_ends_no_request_for_files() {
        let mut requests = super::Requests::default();
        // A DXF pick whose file is still being copied, then the camera, cancelled.
        assert_eq!(requests.asked("dxf:1", false), None);
        assert_eq!(requests.frame(true), None);
        requests.photo_asked();
        assert_eq!(requests.cancelled(), None);
        // The copy arrives: it answers its request, still open.
        requests.picked("dxf:1");
        assert_eq!(requests.frame(false), None);
        // A photo that wasn't kept: the camera's answer is a cancel too, and the next cancel is
        // a picker's.
        requests.photo_asked();
        assert_eq!(requests.cancelled(), None);
        assert_eq!(requests.asked("dxf:3", false), None);
        assert_eq!(requests.cancelled(), Some("dxf:3".into()));
        // A photo taken leaves the next cancel to a picker.
        requests.photo_asked();
        requests.photo_taken();
        assert_eq!(requests.asked("dxf:2", false), None);
        assert_eq!(requests.cancelled(), Some("dxf:2".into()));
    }

    #[test]
    fn a_refused_request_ends_at_once_and_a_quiet_one_once_the_phone_is_done() {
        let mut requests = super::Requests::default();
        assert_eq!(requests.asked("dxf:1", true), Some("dxf:1".into()));
        assert_eq!(requests.cancelled(), None);
        // Accepted, then the phone busy, then idle with no answer (a copy failed on Android).
        assert_eq!(requests.asked("dxf:2", false), None);
        assert_eq!(requests.frame(false), None, "not seen busy yet: Android shows its picker a moment later");
        assert_eq!(requests.frame(true), None);
        assert_eq!(requests.frame(false), Some("dxf:2".into()));
        // A file answers its request: nothing is left to end.
        assert_eq!(requests.asked("data", false), None);
        assert_eq!(requests.frame(true), None);
        requests.picked("data");
        assert_eq!(requests.frame(false), None);
    }

    #[test]
    fn a_part_picked_alone_is_kept_to_be_refused_by_name() {
        assert_eq!(kept(&["roads.dbf", "lakes.shp", "notes.prj"]), ["roads.dbf", "lakes.shp", "notes.prj"]);
        // Other files are never parts.
        assert_eq!(kept(&["a.gpkg", "a.geojson"]), ["a.gpkg", "a.geojson"]);
    }
}
