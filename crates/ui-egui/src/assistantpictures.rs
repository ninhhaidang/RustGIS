//! Pictures attached to the AI Assistant's next message: picked with the composer's picture
//! button (the system's dialog on a computer, the browser's or the phone's picker elsewhere) or
//! pasted into the message box. Each is read by [`rustgis_assistant::attach::prepare`], off the
//! window's thread on the desktop, and shown above the box until it is sent or taken off.

use egui::{RichText, Ui};
use rustgis_assistant::attach::{self, Picture};
use rustgis_assistant::provider::Note;

use crate::RustGisApp;
use crate::theme::Tokens;

/// The pictures of the message being written.
#[derive(Default)]
pub struct Pictures {
    /// Ready to go with the message.
    ready: Vec<Picture>,
    /// Being read, with their names.
    #[cfg(not(target_arch = "wasm32"))]
    reading: Vec<(String, crate::Slot<Result<Picture, Note>>)>,
    /// Pictures not attached since the last notice, said together ([`say_refused`]): those past
    /// [`attach::MAX_PER_MESSAGE`], by name, and those that couldn't be read, with why.
    left_out: Vec<String>,
    refused: Vec<Note>,
}

impl Pictures {
    /// Whether a picture is still being read: the message waits for it.
    pub fn reading(&self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        {
            !self.reading.is_empty()
        }
        #[cfg(target_arch = "wasm32")]
        {
            false
        }
    }

    /// Whether the message has a picture ready.
    pub fn any(&self) -> bool {
        !self.ready.is_empty()
    }

    /// The pictures ready, taken for the message sent.
    pub fn take(&mut self) -> Vec<Picture> {
        std::mem::take(&mut self.ready)
    }

    /// How many the message has or is reading.
    fn count(&self) -> usize {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.ready.len() + self.reading.len()
        }
        #[cfg(target_arch = "wasm32")]
        {
            self.ready.len()
        }
    }
}

/// The picker purpose of the composer's picture button ([`crate::ASSISTANT_PICK`]).
pub const PURPOSE: &str = "assistantPicture";

/// The composer's picture button: picks pictures for the message.
pub fn button(app: &mut RustGisApp, ui: &mut Ui) {
    let full = app.assistant.pictures.count() >= attach::MAX_PER_MESSAGE;
    let hover = if full {
        crate::i18n::fmt("A message takes at most {n} pictures", &[("n", &attach::MAX_PER_MESSAGE.to_string())])
    } else {
        tl!("Attach a picture (PNG, JPEG or WebP), or paste one into the message box").to_string()
    };
    let clicked = ui.add_enabled_ui(!full, |ui| crate::widgets::icon_button(ui, "picture", &hover).clicked()).inner;
    if !clicked {
        return;
    }
    if let Some(open) = &app.services.open_async {
        open(PURPOSE);
        return;
    }
    // Only as many files are read as the message has room for; the rest are named.
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut paths = pick_files().unwrap_or_default();
        let room = attach::MAX_PER_MESSAGE.saturating_sub(app.assistant.pictures.count()).min(paths.len());
        app.assistant.pictures.left_out.extend(paths.drain(room..).map(|p| file_name(&p)));
        // Each file is read on the worker too: a slow disk doesn't hold the window.
        for path in paths {
            start(app, file_name(&path), move || attach::prepare(&path, &attach::read(&path).map_err(Note::text)?));
        }
        say_refused(app);
    }
}

/// The name of the file at `path`, as a picture is named.
#[cfg(not(target_arch = "wasm32"))]
fn file_name(path: &str) -> String {
    std::path::Path::new(path).file_name().map_or_else(|| path.to_string(), |n| n.to_string_lossy().into_owned())
}

/// The pictures the system's dialog gives (none in a unit test, or cancelled).
#[cfg(all(not(target_arch = "wasm32"), not(test)))]
fn pick_files() -> Option<Vec<String>> {
    crate::filedialog::FileDialog::new()
        .add_filter(tl!("Pictures"), attach::EXTENSIONS)
        .pick_files()
        .map(|ps| ps.iter().map(|p| p.display().to_string()).collect())
}

#[cfg(all(not(target_arch = "wasm32"), test))]
fn pick_files() -> Option<Vec<String>> {
    None
}

/// Says the pictures not attached since it last did: one notice for those past
/// [`attach::MAX_PER_MESSAGE`], with their names, and one for each kind of refusal, with a count.
fn say_refused(app: &mut RustGisApp) {
    let pictures = &mut app.assistant.pictures;
    let (left_out, refused) = (std::mem::take(&mut pictures.left_out), std::mem::take(&mut pictures.refused));
    if !left_out.is_empty() {
        let text = crate::i18n::fmt(
            "{names} weren't attached: a message takes at most {n} pictures",
            &[("names", &left_out.join(", ")), ("n", &attach::MAX_PER_MESSAGE.to_string())],
        );
        app.session.notify(text, true);
    }
    let mut kinds: Vec<(&'static str, Vec<Note>)> = Vec::new();
    for why in refused {
        match kinds.iter_mut().find(|(t, _)| *t == why.template) {
            Some((_, notes)) => notes.push(why),
            None => kinds.push((why.template, vec![why])),
        }
    }
    for (_, notes) in kinds {
        let Some(first) = notes.first() else { continue };
        let text = if notes.len() == 1 {
            crate::i18n::note(first)
        } else {
            // The first refusal goes into the count's message in logical order.
            let first = crate::i18n::note_raw(first);
            crate::i18n::fmt("{n} pictures weren't attached, such as {first}", &[("n", &notes.len().to_string()), ("first", &first)])
        };
        app.session.notify(text, true);
    }
}

/// Whether the message has room for another picture; one it hasn't is named in the next notice
/// ([`say_refused`]).
fn room(app: &mut RustGisApp, name: &str) -> bool {
    if app.assistant.pictures.count() < attach::MAX_PER_MESSAGE {
        return true;
    }
    app.assistant.pictures.left_out.push(name.to_string());
    false
}

/// A picture file for the message (from the dialog, or the browser's or phone's picker), read
/// and made ready to send. One past [`attach::MAX_PER_MESSAGE`], or one that can't be sent, is
/// said in a notice.
pub fn received(app: &mut RustGisApp, name: &str, bytes: Vec<u8>) {
    if !room(app, name) {
        return;
    }
    let name = name.to_string();
    start(app, name.clone(), move || attach::prepare(&name, &bytes));
}

/// A pasted picture's pixels for the message. Its size is checked before its pixels are copied.
fn pasted(app: &mut RustGisApp, image: std::sync::Arc<egui::ColorImage>) {
    let name = tl!("pasted picture").to_string();
    if !room(app, &name) {
        return;
    }
    let [w, h] = image.size;
    let (w, h) = (u32::try_from(w).unwrap_or(u32::MAX), u32::try_from(h).unwrap_or(u32::MAX));
    if !attach::fits(w, h) {
        app.assistant.pictures.refused.push(attach::too_many_pixels(name));
        return;
    }
    // Its pixels are copied on the worker, with the rest of the reading.
    start(app, name.clone(), move || {
        let mut rgba = Vec::with_capacity(image.pixels.len().saturating_mul(4));
        for c in &image.pixels {
            rgba.extend_from_slice(&c.to_srgba_unmultiplied());
        }
        drop(image);
        attach::from_rgba(&name, w, h, rgba)
    });
}

/// Reads a picture: on a worker on the desktop (decoding a large photo takes a moment), at once
/// in the browser, which has one thread. When the system refuses the worker, the picture isn't
/// read (that would hold the window) and that is said: it can be attached again. A reader that
/// panics is said as a picture that can't be read, not waited on. Decoding is
/// one step with no check for Cancel; taking the picture off while it is read drops it when the
/// worker is done.
fn start(app: &mut RustGisApp, name: String, read: impl FnOnce() -> Result<Picture, Note> + Send + 'static) {
    let label = name.clone();
    let read = move || {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(read))
            .unwrap_or_else(|_| Err(Note::with("{name}: the picture can't be read: its decoder failed", [("name", label)])))
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        let slot: crate::Slot<Result<Picture, Note>> = Default::default();
        let out = slot.clone();
        let spawned = std::thread::Builder::new().name("assistant picture".into()).spawn(move || {
            // One picture is decoded at a time, so pictures picked together hold one decode's
            // memory, not one each.
            let _turn = DECODING.lock().unwrap_or_else(|e| e.into_inner());
            let picture = read();
            if let Ok(mut out) = out.lock() {
                *out = Some(picture);
            }
        });
        if spawned.is_ok() {
            app.assistant.pictures.reading.push((name, slot));
        } else {
            finished(
                app,
                Err(Note::with(
                    "{name}: the picture wasn't read: the system refused a thread to read it on; attach it again",
                    [("name", name)],
                )),
            );
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = name;
        finished(app, read());
    }
}

/// A picture read: ready for the message, or said in a notice when it can't be sent.
fn finished(app: &mut RustGisApp, picture: Result<Picture, Note>) {
    match picture {
        Ok(p) => app.assistant.pictures.ready.push(p),
        Err(why) => app.assistant.pictures.refused.push(why),
    }
}

/// Held while a picture is decoded ([`start`]).
#[cfg(not(target_arch = "wasm32"))]
static DECODING: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Takes in the pictures read since the last frame, and a picture pasted into the focused message
/// box (`focused`).
pub fn poll(app: &mut RustGisApp, ui: &Ui, focused: bool) {
    // Another frame while a picture is read, and one after the last is: a message waiting for it
    // (a suggestion) is sent at the start of a frame, before this takes the pictures in.
    let waited = app.assistant.pictures.reading();
    if take_read(app) {
        ui.ctx().request_repaint_after(web_time::Duration::from_millis(100));
    } else if waited {
        ui.ctx().request_repaint();
    }
    if focused {
        let images: Vec<std::sync::Arc<egui::ColorImage>> = ui.input(|i| {
            i.events.iter().filter_map(|e| if let egui::Event::PasteImage(img) = e { Some(img.clone()) } else { None }).collect()
        });
        for image in images {
            pasted(app, image);
        }
    }
}

/// Takes in the pictures read since it was last asked, and says those that couldn't be
/// attached; says whether others are still being read.
fn take_read(app: &mut RustGisApp) -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut done = Vec::new();
        app.assistant.pictures.reading.retain(|(_, slot)| match slot.lock().ok().and_then(|mut s| s.take()) {
            Some(r) => {
                done.push(r);
                false
            }
            None => true,
        });
        for r in done {
            finished(app, r);
        }
    }
    say_refused(app);
    app.assistant.pictures.reading()
}

/// The message's pictures above the box: each with its size, those still being read with a
/// spinner, and a button on each that takes it off.
pub fn chips(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    if app.assistant.pictures.count() == 0 {
        return;
    }
    let off = |ui: &mut Ui| ui.small_button("×").on_hover_text(tl!("Take this picture off the message")).clicked();
    let mut removed = None;
    #[cfg(not(target_arch = "wasm32"))]
    let mut dropped = None;
    ui.horizontal_wrapped(|ui| {
        for (i, p) in app.assistant.pictures.ready.iter().enumerate() {
            let label = format!("{} · {} × {}", p.name, p.width, p.height);
            ui.label(RichText::new(label).small().color(t.text));
            if off(ui) {
                removed = Some(i);
            }
            ui.add_space(6.0);
        }
        #[cfg(not(target_arch = "wasm32"))]
        for (i, (name, _)) in app.assistant.pictures.reading.iter().enumerate() {
            ui.spinner();
            ui.label(RichText::new(name).small().color(t.text_dim));
            if off(ui) {
                dropped = Some(i);
            }
            ui.add_space(6.0);
        }
    });
    if let Some(i) = removed
        && i < app.assistant.pictures.ready.len()
    {
        app.assistant.pictures.ready.remove(i);
    }
    // Its worker finishes, and what it read goes nowhere.
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(i) = dropped
        && i < app.assistant.pictures.reading.len()
    {
        app.assistant.pictures.reading.remove(i);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png() -> Vec<u8> {
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(3, 2, image::Rgba([10, 20, 30, 255])))
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    /// Takes in the pictures being read, once each is (a worker reads it).
    fn read_all(app: &mut RustGisApp) {
        for _ in 0..1_000_000 {
            if !take_read(app) {
                return;
            }
            std::thread::yield_now();
        }
        panic!("the picture was never read");
    }

    #[test]
    fn the_frame_that_takes_in_the_last_picture_asks_for_another() {
        let mut app = RustGisApp::with_sample();
        received(&mut app, "map.png", png());
        // The worker is done; nothing else happens in the window.
        for _ in 0..1_000_000 {
            if app.assistant.pictures.reading.iter().all(|(_, slot)| slot.lock().is_ok_and(|s| s.is_some())) {
                break;
            }
            std::thread::yield_now();
        }
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| poll(&mut app, ui, false));
        // No renderer here takes the frame's textures.
        out.textures_delta.clear();
        assert!(app.assistant.pictures.any() && !app.assistant.pictures.reading());
        assert!(ctx.has_requested_repaint(), "a queued message is sent on the next frame");
    }

    #[test]
    fn a_picked_picture_goes_with_the_next_message() {
        let mut app = RustGisApp::with_sample();
        let cfg = rustgis_assistant::Config {
            provider: "custom".into(),
            base_url: "http://localhost:9/v1".into(),
            model: "test".into(),
            key: String::new(),
            account: String::new(),
            client_id: String::new(),
            session: String::new(),
            reasoning: None,
            tool_output_token_limit: rustgis_assistant::tools::DEFAULT_OUTPUT_TOKENS,
        };
        app.assistant.agent = Some(rustgis_assistant::Agent::new(cfg, &app.session));
        let layers = app.session.map().layers.len();
        // As the browser's or the phone's picker answers: a picture, and a cancelled pick.
        let _ = app.open_picked(&format!("{}map.png", crate::ASSISTANT_PICK), &png());
        let _ = app.open_picked(crate::ASSISTANT_PICK, &[]);
        read_all(&mut app);
        assert!(app.assistant.pictures.any());
        assert_eq!(app.assistant.pictures.ready.len(), 1, "a cancelled pick attaches nothing");
        assert_eq!(app.session.map().layers.len(), layers, "a picture for the assistant isn't opened on the map");
        // A file that isn't a picture is said, and attaches nothing.
        received(&mut app, "notes.txt", b"text".to_vec());
        read_all(&mut app);
        assert_eq!(app.assistant.pictures.ready.len(), 1);
        assert!(app.session.notices_since(0).iter().any(|n| n.text.contains("PNG, JPEG or WebP")));
        // Past the four a message takes: the rest are named, together, in one notice.
        let before = app.session.notice_mark();
        for name in ["b.png", "c.png", "d.png", "e.png", "f.png"] {
            received(&mut app, name, png());
        }
        read_all(&mut app);
        assert_eq!(app.assistant.pictures.ready.len(), 4);
        let said: Vec<String> = app.session.notices_since(before).iter().map(|n| n.text.clone()).collect();
        assert_eq!(said, ["e.png, f.png weren't attached: a message takes at most 4 pictures"]);
        // Sent with the message, and gone from the composer.
        let agent = app.assistant.agent.as_mut().unwrap();
        let step = agent.ask_with("like this", app.assistant.pictures.take(), &app.session);
        assert!(matches!(step, rustgis_assistant::Step::Send(_)));
        assert!(!app.assistant.pictures.any());
        let Some(rustgis_assistant::provider::Turn::User(_, sent)) = app.assistant.agent.as_ref().unwrap().turns().last() else {
            panic!("message")
        };
        assert_eq!(sent.len(), 4);
        assert_eq!((sent[0].name.as_str(), sent[0].width, sent[0].height), ("map.png", 3, 2));
    }
}
