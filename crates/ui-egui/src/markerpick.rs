//! «Image…» in the marker menus (#2266 part B): the symbol editor's Shape, the stroke editor's
//! Shape and the fill editor's Marker each end with it, and it makes a PNG, JPEG or SVG file the
//! marker. The file is read into the command that sets the marker (`layer.marker`,
//! `layer.lineStrokes`, `layer.fillLayers`), which keeps the image as it does one given by
//! script. A file that isn't a PNG, JPEG or SVG, one too large, or one that doesn't decode or that
//! RustGIS doesn't draw is said in a notice and leaves the marker as it was.

use std::io::Read;

use egui::Ui;
use serde_json::{Value, json};

use crate::RustGisApp;

/// A marker waiting for its image: the command that sets it, with the image's place in its
/// parameters (a JSON pointer) left empty until the file is read. Built when «Image…» is clicked,
/// from what the editor shows then.
#[derive(Clone, Debug, PartialEq)]
pub struct MarkerImage {
    cmd: &'static str,
    params: Value,
    at: String,
    /// Images that can't be drawn the other marks of the call send back as circles.
    replaced: usize,
    /// The browser's or phone's picker request this marker waits on (`marker:<id>`): only its
    /// answer is the marker's.
    pub(crate) request: u64,
    /// The live layer it was asked on ([`rustgis_engine::map::Layer::instance_id`]): a file
    /// arriving after its layer left the map (another map, a removed layer) changes nothing.
    layer: Option<u64>,
}

/// The next picker request's number.
static NEXT_REQUEST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// The notice for an image file over [`crate::MARKER_FILE_MAX_BYTES`], as every shell says it.
pub fn marker_file_too_large() -> &'static str {
    tl!("This image is too large for a marker: at most about 1.5 MiB")
}

impl MarkerImage {
    /// A point symbol's marker on layer `layer`, through `layer.marker`.
    pub(crate) fn symbol(layer: u32) -> Self {
        Self {
            cmd: "layer.marker",
            params: json!({"layer": layer, "image": ""}),
            at: "/image".into(),
            replaced: 0,
            request: 0,
            layer: None,
        }
    }

    /// Mark `i` of `params`' `list` (`"strokes"` with `"markers"`, or `"fills"`), as the editor
    /// sends it (`params` for every mark in the list, on layer `layer`): it becomes an image.
    pub(crate) fn in_list(cmd: &'static str, mut params: Value, layer: u32, list: &str, i: usize, replaced: usize) -> Self {
        params["layer"] = json!(layer);
        let markers = if list == "strokes" { "/markers" } else { "" };
        let mark = if list == "strokes" { "mark" } else { "marker" };
        let place = format!("/{list}/{i}{markers}");
        if let Some(m) = params.pointer_mut(&place).filter(|m| m.is_object()) {
            m[mark] = json!("image");
            m["char"] = Value::Null;
            m["font"] = Value::Null;
            m["image"] = json!("");
        }
        Self { cmd, params, at: format!("{place}/image"), replaced, request: 0, layer: None }
    }

    /// Runs the command with `bytes` as the image, as base64. A file that is too large, or that
    /// doesn't decode as a PNG, JPEG or SVG (another format, a broken one, or an SVG RustGIS doesn't
    /// draw), is said in a notice in a file's terms (the command's speak of base64) and runs nothing.
    pub(crate) fn apply(mut self, app: &mut RustGisApp, bytes: &[u8]) -> Result<Value, String> {
        use base64::Engine;
        // The size first, so a file too large isn't decoded.
        let b64 = (bytes.len() <= crate::MARKER_FILE_MAX_BYTES).then(|| base64::engine::general_purpose::STANDARD.encode(bytes));
        let refused = match &b64 {
            None => Some(marker_file_too_large().to_string()),
            Some(b64) => {
                use rustgis_engine::render::MarkerRefusal;
                // One phrase per refusal, whole, for translation.
                rustgis_engine::render::picture_marker_check(b64).err().map(|why| match why {
                    MarkerRefusal::NotAnImage => tl!("This image can't be a marker: it isn't a readable PNG, JPEG or SVG").to_string(),
                    MarkerRefusal::TooManyPixels => crate::i18n::fmt(
                        "This image can't be a marker: it has more pixels than {side} × {side}; make it smaller",
                        &[("side", &rustgis_engine::render::MARKER_PIXELS_SIDE.to_string())],
                    ),
                    MarkerRefusal::TooMuchToDraw => crate::i18n::fmt(
                        "This image can't be a marker: it is an SVG with more to draw than a {side} × {side} image",
                        &[("side", &rustgis_engine::render::MARKER_PIXELS_SIDE.to_string())],
                    ),
                    // Past the file's own cap above, for completeness.
                    MarkerRefusal::TooManyBytes => crate::i18n::fmt(
                        "This image can't be a marker: it is longer than {mib} MiB as base64; make it smaller",
                        &[("mib", &(rustgis_engine::map::symbology::MAX_PICTURE_BYTES >> 20).to_string())],
                    ),
                    MarkerRefusal::SvgTooNested => tl!(
                        "This image can't be a marker: it is an SVG with more than 40,000 XML nodes or nested deeper than 64 levels"
                    )
                    .to_string(),
                    MarkerRefusal::SvgUnknownRadius => {
                        tl!("This image can't be a marker: it is an SVG with a radius whose size RustGIS can't know (in em or ex, or in % of a size in em or ex, or of a nested size in % over 100%)").to_string()
                    }
                    MarkerRefusal::SvgTooSmall => tl!("This image can't be a marker: it is an SVG too small to draw").to_string(),
                    MarkerRefusal::SvgNotUtf8 => tl!("This image can't be a marker: it is an SVG that isn't UTF-8 text").to_string(),
                    MarkerRefusal::SvgUndrawn => {
                        tl!("This image can't be a marker: it is an SVG with filters, text, images, animation or links to other files, which RustGIS doesn't draw in marker images").to_string()
                    }
                    MarkerRefusal::PastRunPixels => tl!(
                        "This image can't be a marker: RustGIS has decoded as many marker images as it does until it restarts; restart it to use another"
                    )
                    .to_string(),
                })
            }
        };
        if let Some(e) = refused {
            app.session.notify(e.clone(), true);
            return Err(e);
        }
        // A layer that left the map while the file came, or a mark no longer in the list (it
        // can't be, from the editor), runs nothing.
        let gone = self.layer.is_some_and(|i| self.live_layer(app) != Some(i));
        let (false, Some(slot), Some(b64)) = (gone, self.params.pointer_mut(&self.at), b64) else {
            let e = tl!("The marker to change is no longer there");
            app.session.notify(e, true);
            return Err(e.to_string());
        };
        *slot = json!(b64);
        let r = app.run(self.cmd, self.params);
        if r.is_ok() {
            crate::panes::notify_image_replaced(app, self.replaced);
        }
        r
    }

    /// The live layer now on the map with the marker's layer id, if any.
    fn live_layer(&self, app: &RustGisApp) -> Option<u64> {
        let id = self.params["layer"].as_u64().and_then(|id| u32::try_from(id).ok())?;
        app.session.map().layer(id).map(|l| l.instance_id())
    }
}

/// The marker menus' last entry, after the shapes (and Character): picks an image file as the
/// marker, or another for the one it has (`on`).
pub(crate) fn entry(ui: &mut Ui, on: bool) -> bool {
    ui.selectable_label(on, tl!("Image…")).on_hover_text(tl!("A PNG, JPEG or SVG file as the marker")).clicked()
}

/// Asks for the image of `marker`: the system's dialog on a computer, read at once; the browser's
/// or the phone's `"marker:<id>"` picker elsewhere, whose file comes back marked with its request
/// ([`crate::MARKER_PICK`]) for [`RustGisApp::open_picked`] to give to the marker. A drop, another
/// picker's file or an earlier request's never reaches it.
pub(crate) fn pick(app: &mut RustGisApp, mut marker: MarkerImage) {
    if let Some(open) = &app.services.open_async {
        marker.request = NEXT_REQUEST.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        marker.layer = marker.live_layer(app);
        let purpose = format!("marker:{}", marker.request);
        app.ui.pending_marker_image = Some(marker);
        open(&purpose);
    } else if let Some(path) = crate::dialogs::pick_open(crate::MARKER_EXTENSIONS, tl!("Pictures")) {
        match read_capped(&path) {
            Ok(bytes) => {
                let _ = marker.apply(app, &bytes);
            }
            Err(e) => app.session.notify(e, true),
        }
    }
}

/// The file at `path`, read no further than one byte past [`crate::MARKER_FILE_MAX_BYTES`]:
/// [`MarkerImage::apply`] refuses a file that long as too large, and a huge file isn't read whole
/// to be refused.
fn read_capped(path: &str) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(crate::MARKER_FILE_MAX_BYTES as u64 + 1).read_to_end(&mut bytes))
        .map_err(|e| crate::i18n::fmt("{path}: {error}", &[("path", path), ("error", &e.to_string())]))?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_engine::map::fills::FillPattern;
    use rustgis_engine::map::symbology::{LineMark, Marker};

    fn png() -> Vec<u8> {
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(4, 2, image::Rgba([10, 120, 230, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        png
    }

    fn drawn(m: Marker) -> bool {
        matches!(m, Marker::Picture(p) if rustgis_engine::render::picture_marker_draws(p))
    }

    fn layer<'a>(app: &'a RustGisApp, name: &str) -> &'a rustgis_engine::map::Layer {
        app.session.map().layer_by_name(name).unwrap()
    }

    fn id(app: &RustGisApp, name: &str) -> u32 {
        layer(app, name).id
    }

    /// Picks `path` with the system's dialog, as a click on «Image…» does on a computer.
    fn pick_file(app: &mut RustGisApp, path: &std::path::Path, marker: MarkerImage) {
        crate::dialogs::PICKED.with(|p| *p.borrow_mut() = Some(path.display().to_string()));
        pick(app, marker);
    }

    /// The notice for a file that isn't a readable PNG, JPEG or SVG the marker can take.
    const UNREADABLE: &str = "This image can't be a marker: it isn't a readable PNG, JPEG or SVG";

    /// «Image…» makes the file the marker of a point symbol, of one mark in a stack of strokes
    /// (the others as they were) and of a dot fill, on the layer it was clicked for even when
    /// another is active.
    #[test]
    fn a_picked_file_becomes_the_marker() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("icon.png");
        std::fs::write(&file, png()).unwrap();
        let mut app = RustGisApp::with_sample();
        let (cities, rivers, lakes) = (id(&app, "Cities"), id(&app, "Rivers"), id(&app, "Lakes"));
        app.session.active_layer = Some(rivers);

        pick_file(&mut app, &file, MarkerImage::symbol(cities));
        assert!(drawn(layer(&app, "Cities").renderer.base_symbol().marker));
        assert!(!drawn(layer(&app, "Rivers").renderer.base_symbol().marker));

        let strokes = json!([{"width": 3}, {"markers": {"mark": "triangle", "size": 7, "at": "middle"}}]);
        app.run("layer.lineStrokes", json!({"layer": "Rivers", "strokes": strokes})).unwrap();
        let list = layer(&app, "Rivers").line_strokes.clone();
        pick_file(&mut app, &file, crate::panes::stroke_image(rivers, None, &list, 1));
        let now = &layer(&app, "Rivers").line_strokes;
        assert_eq!(now[0], list[0], "the other stroke as it was");
        assert!(matches!(now[1].markers.map(|m| m.mark), Some(LineMark::Marker(m)) if drawn(m)));
        assert_eq!(now[1].markers.map(|m| (m.size, m.at)), list[1].markers.map(|m| (m.size, m.at)), "its size and place kept");

        let fills = json!([{"color": "#00C800"}, {"pattern": "dots", "marker": "square", "size": 5}]);
        app.run("layer.fillLayers", json!({"layer": "Lakes", "fills": fills})).unwrap();
        let list = layer(&app, "Lakes").fill_layers.clone();
        pick_file(&mut app, &file, crate::fill_editor::fill_image(lakes, None, &list, 1));
        let now = &layer(&app, "Lakes").fill_layers;
        assert_eq!(now[0], list[0]);
        assert!(matches!(now[1].pattern, FillPattern::Dots { marker, size: 5.0, .. } if drawn(marker)));
    }

    /// A file that isn't a PNG, JPEG or SVG, a PNG that doesn't decode, or one too large, is said in a
    /// file's terms and changes nothing (no undo step either); a dialog cancelled changes nothing
    /// and says nothing.
    #[test]
    fn a_file_that_cant_be_a_marker_is_said_and_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = RustGisApp::with_sample();
        let cities = id(&app, "Cities");
        let before = layer(&app, "Cities").renderer.base_symbol();
        let gif = dir.path().join("icon.gif");
        std::fs::write(&gif, b"GIF89a\x01\x00\x01\x00").unwrap();
        let broken = dir.path().join("broken.png");
        std::fs::write(&broken, b"\x89PNG\r\n\x1a\nnot an image").unwrap();
        let mut big = png();
        big.resize(crate::MARKER_FILE_MAX_BYTES + 1, 0);
        let large = dir.path().join("large.png");
        std::fs::write(&large, &big).unwrap();
        // Small in bytes, past the pixels a marker takes.
        let mut wide = Vec::new();
        image::GrayImage::from_pixel(2049, 2048, image::Luma([90]))
            .write_to(&mut std::io::Cursor::new(&mut wide), image::ImageFormat::Png)
            .unwrap();
        let many = dir.path().join("many.png");
        std::fs::write(&many, &wide).unwrap();
        let steps = app.session.undo_depth();
        let refusals = [
            (&gif, UNREADABLE),
            (&broken, UNREADABLE),
            (&large, "This image is too large for a marker: at most about 1.5 MiB"),
            (&many, "This image can't be a marker: it has more pixels than 2048 × 2048; make it smaller"),
        ];
        for (file, said) in refusals {
            pick_file(&mut app, file, MarkerImage::symbol(cities));
            assert_eq!(layer(&app, "Cities").renderer.base_symbol(), before);
            assert!(app.session.notices.iter().any(|n| n.error && n.text == said), "{said}: {:?}", app.session.notices);
        }
        let notices = app.session.notices.len();
        pick(&mut app, MarkerImage::symbol(cities));
        assert_eq!(layer(&app, "Cities").renderer.base_symbol(), before);
        assert_eq!(app.session.notices.len(), notices);
        assert_eq!(app.session.undo_depth(), steps);
    }

    /// «Image…» lists SVG files beside PNG and JPEG ones, on the computer and in the browser, and
    /// a picked SVG becomes the marker; one RustGIS doesn't draw (a filter) is said with why, and
    /// text that isn't an image is said as not a PNG, JPEG or SVG; neither changes anything
    /// (#2266 part E1b).
    #[test]
    fn a_picked_svg_becomes_the_marker_and_one_it_cant_draw_is_said() {
        assert_eq!(crate::MARKER_EXTENSIONS, ["png", "jpg", "jpeg", "svg"]);
        let dir = tempfile::tempdir().unwrap();
        let svg = |name: &str, body: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, format!(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 8 4">{body}</svg>"#)).unwrap();
            path
        };
        let pin = svg("pin.svg", r##"<rect width="8" height="4" fill="#c81e1e"/>"##);
        let blurred = svg(
            "blurred.svg",
            r#"<filter id="f"><feGaussianBlur stdDeviation="1"/></filter><rect width="8" height="4" filter="url(#f)"/>"#,
        );
        let text = dir.path().join("notes.svg");
        std::fs::write(&text, "not an image").unwrap();
        let mut app = RustGisApp::with_sample();
        let cities = id(&app, "Cities");
        let before = layer(&app, "Cities").renderer.base_symbol();
        let steps = app.session.undo_depth();
        let undrawn = "This image can't be a marker: it is an SVG with filters, text, images, animation or links to other files, which RustGIS doesn't draw in marker images";
        for (file, said) in [(&blurred, undrawn), (&text, UNREADABLE)] {
            pick_file(&mut app, file, MarkerImage::symbol(cities));
            assert_eq!(layer(&app, "Cities").renderer.base_symbol(), before);
            assert!(app.session.notices.iter().any(|n| n.error && n.text == said), "{said}: {:?}", app.session.notices);
        }
        assert_eq!(app.session.undo_depth(), steps);
        pick_file(&mut app, &pin, MarkerImage::symbol(cities));
        assert!(drawn(layer(&app, "Cities").renderer.base_symbol().marker));
        assert_eq!(app.session.undo_depth(), steps + 1);
    }

    /// In the browser «Image…» asks the `"marker:<id>"` picker for a file and the file it gives
    /// back, marked with its request, becomes the marker. A dropped PNG, or another picker's,
    /// leaves the marker waiting; a marked file that isn't a PNG, JPEG or SVG is said and changes
    /// nothing; an earlier request's file, arriving after another «Image…», is dropped; a file
    /// arriving after the map changed is said and changes nothing; a cancelled picker ends the
    /// wait.
    #[test]
    fn the_browsers_picture_goes_to_the_marker_that_asked() {
        let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut app = RustGisApp::with_sample();
        let ask = asked.clone();
        app.services.open_async = Some(Box::new(move |purpose| ask.lock().unwrap().push(purpose.to_string())));
        let cities = id(&app, "Cities");
        let request = |app: &RustGisApp| app.ui.pending_marker_image.as_ref().map(|m| m.request);
        let marked = |request: Option<u64>, name: &str| format!("{}{}:{name}", crate::MARKER_PICK, request.unwrap_or(0));
        app.run("layer.marker", json!({"layer": "Cities", "shape": "square"})).unwrap();

        pick(&mut app, MarkerImage::symbol(cities));
        let first = request(&app);
        assert_eq!(*asked.lock().unwrap(), [format!("marker:{}", first.unwrap())]);
        let _ = app.open_picked("dropped.png", &png());
        assert_eq!(layer(&app, "Cities").renderer.base_symbol().marker, Marker::Square, "a drop isn't the marker's");
        assert!(app.ui.pending_marker_image.is_some());

        assert!(app.open_picked(&marked(first, "icon.gif"), b"GIF89a\x01\x00\x01\x00").is_err());
        assert!(app.session.notices.iter().any(|n| n.error && n.text == UNREADABLE));
        assert_eq!(layer(&app, "Cities").renderer.base_symbol().marker, Marker::Square);

        // Two requests overlapping: the first one's file, arriving late, isn't the second's.
        pick(&mut app, MarkerImage::symbol(cities));
        let a = request(&app);
        pick(&mut app, MarkerImage::symbol(cities));
        let b = request(&app);
        assert_ne!(a, b);
        app.open_picked(&marked(a, "late.png"), &png()).unwrap();
        assert_eq!(layer(&app, "Cities").renderer.base_symbol().marker, Marker::Square, "an earlier request's file");
        assert!(
            app.session
                .notices
                .iter()
                .any(|n| n.text == "An image picked for an earlier Image… was dropped: pick it again for that marker")
        );
        assert_eq!(request(&app), b);
        app.open_picked(&marked(b, "icon.png"), &png()).unwrap();
        assert!(drawn(layer(&app, "Cities").renderer.base_symbol().marker));
        assert!(app.ui.pending_marker_image.is_none());

        pick(&mut app, MarkerImage::symbol(cities));
        let cancelled = request(&app);
        app.open_picked(&marked(cancelled, ""), &[]).unwrap();
        assert!(app.ui.pending_marker_image.is_none(), "a cancelled picker ends the wait");

        // The map changed while the file came: the other map's layer of that id isn't touched.
        app.run("layer.marker", json!({"layer": "Cities", "shape": "square"})).unwrap();
        pick(&mut app, MarkerImage::symbol(cities));
        let waiting = request(&app);
        app.run("map.new", Value::Null).unwrap();
        let steps = app.session.undo_depth();
        assert!(app.open_picked(&marked(waiting, "icon.png"), &png()).is_err());
        assert!(app.session.notices.iter().any(|n| n.error && n.text == "The marker to change is no longer there"));
        assert_eq!(app.session.undo_depth(), steps);
    }

    /// A mark the list no longer has runs nothing and says so.
    #[test]
    fn a_mark_no_longer_in_the_list_changes_nothing() {
        let mut app = RustGisApp::with_sample();
        let rivers = id(&app, "Rivers");
        let before = layer(&app, "Rivers").line_strokes.clone();
        let steps = app.session.undo_depth();
        let gone = MarkerImage::in_list("layer.lineStrokes", json!({"strokes": []}), rivers, "strokes", 3, 0);
        assert!(gone.apply(&mut app, &png()).is_err());
        assert_eq!(layer(&app, "Rivers").line_strokes, before);
        assert_eq!(app.session.undo_depth(), steps);
        assert!(app.session.notices.iter().any(|n| n.error && n.text == "The marker to change is no longer there"));
    }
}
