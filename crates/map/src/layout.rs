//! Page layouts: a page (in millimetres) holding map frames and the surrounds that explain them
//! (north arrows, scale bars, legends, text, pictures, tables, charts), exported as images or PDF.
//!
//! Element positions are in millimetres from the page's top-left corner. A map frame shows a map
//! of the project at its own centre and scale, independent of the map view.

use std::collections::BTreeMap;

use rustgis_geom::Coord;
use serde::{Deserialize, Serialize};

use crate::raster::{Colorizer, Normalizer, RasterLayer, RasterMask, RasterRenderer, RasterStyle};
use crate::report::StatKind;
use crate::symbology::Dash;
use crate::{Camera, Layer, Map, Rgba};

mod chart;
mod colorbar;
mod elements;
mod frame;
mod grid;
mod legend;
mod lines;
mod north;
mod scale_bar;
mod stats;
mod text;

pub use chart::*;
pub use colorbar::*;
pub use elements::*;
pub use frame::*;
pub use grid::*;
pub use legend::*;
pub use lines::*;
pub use north::*;
pub use scale_bar::*;
pub use stats::*;
pub use text::*;

/// Logical points per millimetre (96 points per inch).
pub const PT_PER_MM: f64 = 96.0 / 25.4;
/// Metres of paper per logical point.
pub const PAPER_M_PER_PT: f64 = 0.0254 / 96.0;

/// The tokens a layout's text expands besides the map's ([`crate::graphics::TOKENS`], from its
/// first map frame), with what each shows.
pub const PAGE_TOKENS: &[(&str, &str)] =
    &[("{page}", "Page number"), ("{pages}", "Number of pages"), ("{pageName}", "Page name"), ("{layout}", "Layout name")];

/// The smallest element side, in mm: thin enough for a hairline rule (a 0.05 mm rectangle).
pub const MIN_ELEMENT_MM: f64 = 0.05;

/// The largest element id a layout keeps (imported ones above it are renumbered).
const MAX_ID: u32 = 1_000_000;

/// Page sizes: (id, label, width mm, height mm), portrait.
pub const PAGES: &[(&str, &str, f64, f64)] = &[
    ("letter", "Letter", 215.9, 279.4),
    ("legal", "Legal", 215.9, 355.6),
    ("tabloid", "Tabloid", 279.4, 431.8),
    ("a5", "A5", 148.0, 210.0),
    ("a4", "A4", 210.0, 297.0),
    ("a3", "A3", 297.0, 420.0),
    ("a2", "A2", 420.0, 594.0),
    ("a1", "A1", 594.0, 841.0),
    ("a0", "A0", 841.0, 1189.0),
];

fn is_false(v: &bool) -> bool {
    !*v
}

/// Where [`Layout::arrange`] moves an element in the drawing order, in which later elements are
/// drawn over earlier ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arrange {
    /// Over every other element.
    Front,
    /// Over the element above it.
    Forward,
    /// Under the element below it.
    Backward,
    /// Under every other element.
    Back,
}

impl Arrange {
    pub const ALL: [Arrange; 4] = [Arrange::Front, Arrange::Forward, Arrange::Backward, Arrange::Back];

    pub fn code(self) -> &'static str {
        match self {
            Arrange::Front => "front",
            Arrange::Forward => "forward",
            Arrange::Backward => "backward",
            Arrange::Back => "back",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Arrange::Front => "Bring to Front",
            Arrange::Forward => "Bring Forward",
            Arrange::Backward => "Send Backward",
            Arrange::Back => "Send to Back",
        }
    }

    pub fn from_name(s: &str) -> Option<Arrange> {
        Arrange::ALL.into_iter().find(|v| v.code().eq_ignore_ascii_case(s.trim()))
    }
}

/// The dark grey a scale bar draws in by default, and a legend, colorbar, table or chart without
/// an ink of its own ([`ElementKind::ink`]).
pub const INK: Rgba = Rgba::rgb(0x22, 0x22, 0x22);

fn ink() -> Rgba {
    INK
}

fn white() -> Rgba {
    Rgba::rgb(0xFF, 0xFF, 0xFF)
}

fn yes() -> bool {
    true
}

/// Which way a colorbar runs, or a chart's bars.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Orientation {
    #[default]
    Horizontal,
    Vertical,
}

fn is_default<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}

/// An element on the page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Element {
    pub id: u32,
    pub name: String,
    /// Top-left corner and size, in millimetres.
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    #[serde(default = "yes")]
    pub visible: bool,
    /// Locked, it can be selected but not moved, resized, deleted or edited until unlocked.
    #[serde(default, skip_serializing_if = "is_false")]
    pub locked: bool,
    /// The box drawn behind its content (#2257); none on a map frame, and none by default, so a
    /// layout saved before it draws as it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<Background>,
    pub kind: ElementKind,
}

/// The box drawn behind an element's content (#2257): its fill, an outline when it has a width,
/// rounded corners, and how far it reaches past the element's box on every side. The content is
/// drawn as without it, in the element's box.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Background {
    /// White unless given, as `layout.update`'s `{"background": {}}` makes it.
    #[serde(default = "white")]
    pub fill: Rgba,
    #[serde(default = "ink")]
    pub outline: Rgba,
    /// The outline's width in points; 0 (the default) draws none.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub width: f64,
    /// The corners' radius, in millimetres.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub radius: f64,
    /// How far it reaches past the element's box on every side, in millimetres.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub padding: f64,
}

/// The widest a background's outline may be (points), as a rectangle's.
pub const MAX_BACKGROUND_WIDTH: f64 = 50.0;
/// The largest corner radius a background takes (mm), as a rectangle's.
pub const MAX_BACKGROUND_RADIUS: f64 = 50.0;
/// The farthest a background reaches past its element's box on each side (mm).
pub const MAX_BACKGROUND_PADDING: f64 = 50.0;

impl Background {
    /// Filled with `fill`: no outline, square corners, no padding.
    pub fn filled(fill: Rgba) -> Background {
        Background { fill, outline: ink(), width: 0.0, radius: 0.0, padding: 0.0 }
    }

    /// Its numbers brought into range: finite, 0 or more, at most their limits.
    pub fn clamped(self) -> Background {
        let keep = |v: f64, max: f64| finite_or(v, 0.0).clamp(0.0, max);
        Background {
            width: keep(self.width, MAX_BACKGROUND_WIDTH),
            radius: keep(self.radius, MAX_BACKGROUND_RADIUS),
            padding: keep(self.padding, MAX_BACKGROUND_PADDING),
            ..self
        }
    }
}

impl Element {
    /// Whether the point (mm) is on the element, its box turned as it is drawn; with a background,
    /// on the background, padding included.
    pub fn contains(&self, x: f64, y: f64) -> bool {
        let (x, y) = self.unturned(x, y);
        let (bx, by, bw, bh) = self.background_box();
        x >= bx && x <= bx + bw && y >= by && y <= by + bh
    }

    /// The box its background fills (x, y, w, h in mm, unturned): its own box grown by the
    /// background's padding, or its own box when it has none.
    pub fn background_box(&self) -> (f64, f64, f64, f64) {
        let pad = self.background.map_or(0.0, |b| b.padding);
        (self.x - pad, self.y - pad, self.w + 2.0 * pad, self.h + 2.0 * pad)
    }

    /// How far it is turned about its centre, in degrees clockwise: a text's `rotation`, else 0.
    pub fn rotation(&self) -> f64 {
        match self.kind {
            ElementKind::Text { rotation, .. } => rotation,
            _ => 0.0,
        }
    }

    /// The point (mm) seen in the element's unturned box: turned back about its centre.
    pub fn unturned(&self, x: f64, y: f64) -> (f64, f64) {
        if self.rotation() == 0.0 {
            return (x, y);
        }
        let (cx, cy) = (self.x + self.w / 2.0, self.y + self.h / 2.0);
        let (sin, cos) = (-self.rotation()).to_radians().sin_cos();
        let (dx, dy) = (x - cx, y - cy);
        (cx + dx * cos - dy * sin, cy + dx * sin + dy * cos)
    }

    /// The rectangle (x, y, w, h in mm) its turned box takes up on the page: its box when it isn't
    /// turned.
    pub fn bounds(&self) -> (f64, f64, f64, f64) {
        let (sin, cos) = self.rotation().to_radians().sin_cos();
        let (w, h) = (self.w * cos.abs() + self.h * sin.abs(), self.w * sin.abs() + self.h * cos.abs());
        (self.x + (self.w - w) / 2.0, self.y + (self.h - h) / 2.0, w, h)
    }
}

/// An angle in degrees as a text's `rotation` keeps it: within -180..=180 (450 is 90, 270 is -90),
/// 0 when it isn't a number.
pub fn rotation(degrees: f64) -> f64 {
    if !degrees.is_finite() {
        return 0.0;
    }
    let a = degrees.rem_euclid(360.0);
    // `rem_euclid` of a tiny negative angle can round to 360 itself.
    if a > 180.0 { a - 360.0 } else { a }
}

/// A map series: one page per feature of an index layer, the main frame zoomed to each.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MapSeries {
    /// The frame that follows the pages.
    pub frame: u32,
    /// The index layer, in the frame's map.
    pub layer: String,
    /// The field naming each page (`{pageName}`); the row number when empty.
    #[serde(default)]
    pub name_field: String,
    /// Space around each page's feature, as a fraction of its extent.
    #[serde(default = "margin")]
    pub margin: f64,
    /// The page shown (0-based row of the index layer).
    #[serde(default)]
    pub page: usize,
}

/// A series row's name, or its one-based row number when the field is missing, null or blank.
pub fn series_page_name(layer: &Layer, fi: Option<usize>, row: usize) -> String {
    fi.map(|i| layer.dataset.value(row, i))
        .filter(|v| !v.is_null())
        .map(ToString::to_string)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| (row + 1).to_string())
}

fn margin() -> f64 {
    0.1
}

/// Most guides a layout keeps in each direction.
pub const MAX_GUIDES: usize = 64;

/// Ruler guides for lining elements up: never printed or exported.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Guides {
    /// Vertical guides, at these distances from the page's left edge (mm).
    #[serde(default)]
    pub x: Vec<f64>,
    /// Horizontal guides, at these distances from the page's top edge (mm).
    #[serde(default)]
    pub y: Vec<f64>,
}

impl Guides {
    pub fn is_empty(&self) -> bool {
        self.x.is_empty() && self.y.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    pub name: String,
    pub width: f64,
    pub height: f64,
    pub elements: Vec<Element>,
    #[serde(default)]
    pub series: Option<MapSeries>,
    #[serde(default, skip_serializing_if = "Guides::is_empty")]
    pub guides: Guides,
    /// Numbers its surrounds print (graticule, scale bar and colorbar labels, `{scale}`) take a
    /// decimal comma and a point between thousands (`0,05`, `1:6.300.000`), as maps in Portuguese,
    /// Spanish, French or German do; off, the other way round.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub decimal_comma: bool,
    /// The page's colour, painted under every element (#2855); none, white. Always opaque: a
    /// page has nothing under it to show through, so a translucent one in a hand-edited file is
    /// read as it looks on white paper.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paper: Option<Rgba>,
    #[serde(default)]
    next_id: u32,
    /// Process-local identity, kept by undo snapshots but never stored in a project.
    #[serde(skip)]
    instance: Instance,
}

/// Identifies a live layout, whatever its name or place among the project's layouts (#2802). Not
/// part of the layout's value: layouts that differ only by it are equal.
#[derive(Clone, Copy, Debug)]
struct Instance(u64);

impl Default for Instance {
    fn default() -> Self {
        Instance(crate::next_layer_instance())
    }
}

impl PartialEq for Instance {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Layout {
    /// Identifies this live layout, whatever its name or place among the project's layouts.
    pub fn instance_id(&self) -> u64 {
        self.instance.0
    }

    /// Starts a distinct live layout after copying one. Undo snapshots keep the original identity.
    pub fn renew_instance_id(&mut self) {
        self.instance = Instance::default();
    }

    /// Resolves a series' index layer from its frame and the optional page-name column.
    pub fn series_index<'a>(&self, maps: &'a [Map]) -> Option<(&'a Layer, Option<usize>)> {
        let series = self.series.as_ref()?;
        let ElementKind::MapFrame { map, .. } = &self.element(series.frame)?.kind else { return None };
        let layer = maps.iter().find(|m| m.name.eq_ignore_ascii_case(map))?.layer_by_name(&series.layer)?;
        Some((layer, layer.dataset.field_index(&series.name_field)))
    }

    pub fn new(name: impl Into<String>, width: f64, height: f64) -> Self {
        Layout {
            name: name.into(),
            width,
            height,
            elements: Vec::new(),
            series: None,
            guides: Guides::default(),
            decimal_comma: false,
            paper: None,
            next_id: 1,
            instance: Instance::default(),
        }
    }

    /// Adds an element and returns its id.
    pub fn add(&mut self, name: impl Into<String>, (x, y, w, h): (f64, f64, f64, f64), kind: ElementKind) -> u32 {
        let id = self.next_id.max(self.elements.iter().map(|e| e.id).max().unwrap_or(0).saturating_add(1));
        self.next_id = id.saturating_add(1);
        // Templates on tiny pages can compute sizes near zero.
        let (w, h) = (finite_or(w, 1.0).clamp(MIN_ELEMENT_MM, 5000.0), finite_or(h, 1.0).clamp(MIN_ELEMENT_MM, 5000.0));
        self.elements.push(Element { id, name: name.into(), x, y, w, h, visible: true, locked: false, background: None, kind });
        id
    }

    pub fn element(&self, id: u32) -> Option<&Element> {
        self.elements.iter().find(|e| e.id == id)
    }
    pub fn element_mut(&mut self, id: u32) -> Option<&mut Element> {
        self.elements.iter_mut().find(|e| e.id == id)
    }

    /// The first map frame.
    pub fn first_frame(&self) -> Option<u32> {
        self.elements.iter().find(|e| matches!(e.kind, ElementKind::MapFrame { .. })).map(|e| e.id)
    }

    /// The topmost element at a page point (mm).
    pub fn hit(&self, x: f64, y: f64) -> Option<u32> {
        self.elements.iter().rev().find(|e| e.visible && e.contains(x, y)).map(|e| e.id)
    }

    /// Where element `id` is drawn among the others, and how many there are: 0 is drawn first,
    /// under the rest, and the last over them.
    pub fn draw_order(&self, id: u32) -> Option<(usize, usize)> {
        Some((self.elements.iter().position(|e| e.id == id)?, self.elements.len()))
    }

    /// Moves element `id` in the drawing order: over every other, one place up, one down, or
    /// under every other. Its new place, or None when there's no such element.
    pub fn arrange(&mut self, id: u32, to: Arrange) -> Option<usize> {
        let (i, n) = self.draw_order(id)?;
        let last = n - 1;
        let j = match to {
            Arrange::Front => last,
            Arrange::Forward => (i + 1).min(last),
            Arrange::Backward => i.saturating_sub(1),
            Arrange::Back => 0,
        };
        let e = self.elements.remove(i);
        self.elements.insert(j, e);
        Some(j)
    }

    /// The locked element that removing `id` would take with it: the element itself, or one of
    /// the surrounds that belong to it.
    pub fn locked_in_removal(&self, id: u32) -> Option<&Element> {
        self.elements.iter().find(|e| e.locked && (e.id == id || e.kind.frame() == Some(id)))
    }

    /// Removes an element and the surrounds that belong to it.
    pub fn remove(&mut self, id: u32) -> bool {
        let before = self.elements.len();
        self.elements.retain(|e| e.id != id && e.kind.frame() != Some(id));
        for e in &mut self.elements {
            if let ElementKind::MapFrame { extent_of, .. } = &mut e.kind
                && *extent_of == Some(id)
            {
                *extent_of = None;
            }
        }
        if self.series.as_ref().is_some_and(|s| s.frame == id) {
            self.series = None;
        }
        self.elements.len() != before
    }

    /// Keeps the page and elements sane after loading or editing (layout files are untrusted):
    /// sizes in range, unique element ids, and no references to missing frames. Returns what it
    /// changed, so a project reader can say so.
    pub fn clamp(&mut self) -> LayoutRepairs {
        let page = (self.width, self.height, self.guides.clone(), self.series.clone());
        let count = self.elements.len();
        self.width = finite_or(self.width, 210.0).clamp(20.0, 5000.0);
        self.height = finite_or(self.height, 297.0).clamp(20.0, 5000.0);
        // A page colour is opaque (a page has nothing under it to show through) and white is the
        // page's own (#2855): a hand-edited translucent one is read as it looks on white paper,
        // and said.
        let white = Rgba::rgb(255, 255, 255);
        let translucent = self.paper.is_some_and(|c| c.a() != 255);
        self.paper = self.paper.map(|c| white.lerp(c.with_alpha(255), f64::from(c.a()) / 255.0)).filter(|c| *c != white);
        // The same limits `layout.guides` sets: the view walks these lists every frame.
        for list in [&mut self.guides.x, &mut self.guides.y] {
            list.retain(|v| v.is_finite() && v.abs() <= 5000.0);
            list.truncate(MAX_GUIDES);
        }
        self.elements.truncate(500);
        let mut seen = std::collections::HashSet::new();
        let mut next = self.elements.iter().map(|e| e.id).filter(|id| *id <= MAX_ID).max().unwrap_or(0).saturating_add(1);
        let mut changed = Vec::with_capacity(self.elements.len());
        let mut mgrs_off = 0usize;
        for e in &mut self.elements {
            // A picture's image (often large) is never changed here, so it isn't copied.
            let mut before = Element {
                name: e.name.clone(),
                kind: match &e.kind {
                    ElementKind::Picture { .. } => ElementKind::Picture { image: String::new() },
                    kind => kind.clone(),
                },
                ..*e
            };
            // A bearing past half a turn names the same direction once normalized, and a pitch
            // too small to tilt is drawn flat: neither is a change.
            if let ElementKind::MapFrame { bearing, pitch, .. } = &mut before.kind {
                if bearing.is_finite() {
                    *bearing = crate::camera::normalize_bearing(*bearing);
                }
                if (0.0..1e-6).contains(pitch) {
                    *pitch = 0.0;
                }
            }
            // One subdivision is the division whole, as none: it draws the same.
            if let ElementKind::ScaleBar { subdivisions, .. } = &mut before.kind
                && *subdivisions == 1
            {
                *subdivisions = 0;
            }
            // A bar outline of width 0 draws none, as the commands take it: letting it go is no
            // change.
            if let ElementKind::Chart { bar_outline, .. } = &mut before.kind
                && bar_outline.is_some_and(|o| o.width == 0.0)
            {
                *bar_outline = None;
            }
            // Ids stay well below u32::MAX so new elements never run out of numbers. A repeated id
            // (only a hand-edited file has one) is renumbered; references to it keep meaning the
            // first element with that id, as they did when the file was written.
            if e.id == 0 || e.id > MAX_ID || !seen.insert(e.id) {
                e.id = next;
                seen.insert(next);
                next = next.saturating_add(1);
            }
            e.w = finite_or(e.w, 20.0).clamp(MIN_ELEMENT_MM, 5000.0);
            e.h = finite_or(e.h, 20.0).clamp(MIN_ELEMENT_MM, 5000.0);
            e.x = finite_or(e.x, 0.0).clamp(-5000.0, 5000.0);
            e.y = finite_or(e.y, 0.0).clamp(-5000.0, 5000.0);
            let mut cut = e.name.chars().nth(200).is_some();
            e.name = e.name.chars().take(200).collect();
            match &mut e.kind {
                ElementKind::MapFrame { center, scale, shape, grid, bearing, pitch, extent_width, border_width, .. } => {
                    *scale = finite_or(*scale, 50_000_000.0).clamp(MIN_FRAME_SCALE, MAX_FRAME_SCALE);
                    *extent_width = finite_or(*extent_width, INDICATOR_WIDTH).clamp(MIN_INDICATOR_WIDTH, MAX_INDICATOR_WIDTH);
                    *border_width = finite_or(*border_width, FRAME_BORDER_WIDTH).clamp(0.0, MAX_FRAME_BORDER_WIDTH);
                    *bearing = crate::camera::normalize_bearing(*bearing);
                    *pitch = crate::camera::normalize_pitch(*pitch);
                    if !center.is_finite() {
                        *center = Coord::new(0.0, 0.0);
                    }
                    center.x = center.x.clamp(-540.0, 540.0);
                    // A pole is a centre like any other on a polar map: the map's projection
                    // decides where the frame is drawn ([`frame_camera`], #2684).
                    center.y = center.y.clamp(-90.0, 90.0);
                    if let FrameShape::Polygon(p) = shape {
                        let points = p.len();
                        p.retain(|(u, v)| u.is_finite() && v.is_finite());
                        p.truncate(10_000);
                        cut |= p.len() < points;
                        p.iter_mut().for_each(|(u, v)| (*u, *v) = (u.clamp(0.0, 1.0), v.clamp(0.0, 1.0)));
                        if p.len() < 3 {
                            *shape = FrameShape::Rectangle;
                        }
                    }
                    if let Some(g) = grid {
                        // A graticule's step in degrees, a measured grid's in its system's units.
                        let step = finite_or(g.step, 0.0);
                        g.step = match g.kind {
                            GridKind::Graticule => step.clamp(0.0, MAX_GRATICULE_STEP),
                            GridKind::Measured if step <= 0.0 => 0.0,
                            GridKind::Measured => step.clamp(MIN_MEASURED_STEP, MAX_MEASURED_STEP),
                        };
                        g.tick_length = finite_or(g.tick_length, tick_length()).clamp(0.0, MAX_TICK_MM);
                        // MGRS labels name a WGS 84 UTM zone's squares, on a measured grid: on a
                        // graticule, or with another system named (only a hand-edited file has
                        // either), they are left off, and counted on their own.
                        let named_other = g.crs.as_deref().is_some_and(|c| !c.trim().is_empty() && utm_zone_of(c).is_none());
                        if g.mgrs && (g.kind != GridKind::Measured || named_other) {
                            g.mgrs = false;
                            mgrs_off = mgrs_off.saturating_add(1);
                            // (Counted by `mgrs_off` and said in its own notice, so `before` matches:
                            // not an adjusted element too.)
                            if let ElementKind::MapFrame { grid: Some(b), .. } = &mut before.kind {
                                b.mgrs = false;
                            }
                        }
                        g.width = finite_or(g.width, grid_width()).clamp(0.0, MAX_GRID_WIDTH);
                        g.label_size = layout_text_in_range(g.label_size, GRID_LABEL_PT);
                    }
                }
                ElementKind::Legend { exclude, columns, labels, text_size, .. } => {
                    *text_size = layout_text_in_range(*text_size, LEGEND_TEXT_PT);
                    // Names stay whole: a layer's own name isn't cut, and a cut one would no
                    // longer match it.
                    cut |= exclude.len() > MAX_LEGEND_EXCLUDED;
                    exclude.truncate(MAX_LEGEND_EXCLUDED);
                    *columns = (*columns).clamp(1, MAX_LEGEND_COLUMNS);
                    // The same for the names labelled; a label is cut to its length, and a blank
                    // one shows the name, as it would without.
                    labels.retain(|_, label| !label.trim().is_empty());
                    let labelled = labels.len();
                    // One label per layer, as a command keeps: of two spellings of a name (any
                    // case), the first in order stays and the other is cut.
                    let mut seen = std::collections::HashSet::new();
                    labels.retain(|name, _| seen.insert(name.to_ascii_lowercase()));
                    while labels.len() > MAX_LEGEND_LABELS {
                        labels.pop_last();
                    }
                    cut |= labels.len() < labelled;
                    for label in labels.values_mut() {
                        let end = label.char_indices().nth(MAX_LEGEND_LABEL_CHARS).map_or(label.len(), |(i, _)| i);
                        cut |= end < label.len();
                        label.truncate(end);
                    }
                }
                ElementKind::Text { text, size, letter_spacing, rotation: turn, valign, line_spacing, case, .. } => {
                    *turn = rotation(*turn);
                    // A file's unknown name or wrong kind of value is read as a marker: back to the
                    // default.
                    *valign = valign.kept();
                    *case = case.kept();
                    *line_spacing = line_spacing_in_range(*line_spacing);
                    *size = finite_or(*size, 12.0).clamp(*crate::graphics::FONT_SIZE.start(), *crate::graphics::FONT_SIZE.end());
                    *letter_spacing = finite_or(*letter_spacing, 0.0).clamp(MIN_LETTER_SPACING, MAX_LETTER_SPACING);
                    let end = text.char_indices().nth(MAX_TEXT_CHARS).map_or(text.len(), |(i, _)| i);
                    cut |= end < text.len();
                    text.truncate(end);
                }
                ElementKind::Table { rows, .. } => *rows = (*rows).clamp(1, 500),
                // A maximum the chart can't draw to (only a hand-edited file has one) gives it its
                // bars' own scale back, counted with the values out of range (#2487 part 4); a bar
                // outline's width past 5 points is held to them, and one below 0 let go (part 4b).
                ElementKind::Chart { max, bar_outline, .. } => {
                    *max = chart_max(*max);
                    *bar_outline = chart_outline(*bar_outline);
                }
                ElementKind::ScaleBar { divisions, text_size, subdivisions, divisions_before_zero, decimals, unit_label, .. } => {
                    *divisions = (*divisions).clamp(1, MAX_SCALE_DIVISIONS);
                    *text_size = layout_text_in_range(*text_size, SCALE_BAR_TEXT_PT);
                    // One part is the whole division: saved as none, it asks for no details.
                    *subdivisions = if *subdivisions == 1 { 0 } else { (*subdivisions).min(MAX_SCALE_SUBDIVISIONS) };
                    *divisions_before_zero = (*divisions_before_zero).min(MAX_SCALE_BEFORE_ZERO);
                    // The two together are drawn as at most ten: the ones before zero give way.
                    *divisions_before_zero = (*divisions_before_zero).min(MAX_SCALE_DIVISIONS.saturating_sub(*divisions));
                    // A file's invalid count (negative, fractional) is read as a marker: back to automatic.
                    *decimals = match *decimals {
                        Some(SCALE_DECIMALS_INVALID) => None,
                        d => d.map(|d| d.min(MAX_SCALE_DECIMALS as u8)),
                    };
                    // The label is one line: a break in a hand-edited file is a space.
                    if let Some(label) = unit_label {
                        *label = label.replace("\r\n", " ").replace(['\n', '\r'], " ");
                        let end = label.char_indices().nth(MAX_UNIT_LABEL_CHARS).map_or(label.len(), |(i, _)| i);
                        cut |= end < label.len();
                        label.truncate(end);
                    }
                }
                ElementKind::Rectangle { width, .. } | ElementKind::Ellipse { width, .. } => {
                    *width = finite_or(*width, 1.0).clamp(0.0, 50.0)
                }
                ElementKind::Line { width, .. } => *width = finite_or(*width, 1.0).clamp(0.1, 50.0),
                _ => {}
            }
            // A map frame draws its map and its paper itself: a background on one (a hand-edited
            // file) is dropped.
            e.background = e.background.filter(|_| e.kind.takes_background()).map(Background::clamped);
            let kind_changed = !matches!(e.kind, ElementKind::Picture { .. }) && e.kind != before.kind;
            changed.push((
                kind_changed
                    || e.background != before.background
                    || (e.id, &e.name, e.x, e.y, e.w, e.h, e.visible, e.locked)
                        != (before.id, &before.name, before.x, before.y, before.w, before.h, before.visible, before.locked),
                cut,
            ));
        }
        self.next_id = next;
        // References must point at map frames that exist.
        let frames: std::collections::HashSet<u32> =
            self.elements.iter().filter(|e| matches!(e.kind, ElementKind::MapFrame { .. })).map(|e| e.id).collect();
        let (mut adjusted, mut cut_count) = (0usize, 0usize);
        let elements = std::mem::take(&mut self.elements);
        for (mut e, (mut changed, cut)) in elements.into_iter().zip(changed) {
            if e.kind.frame().is_some_and(|f| !frames.contains(&f)) {
                continue;
            }
            if let ElementKind::MapFrame { extent_of, .. } = &mut e.kind
                && extent_of.is_some_and(|o| !frames.contains(&o) || o == e.id)
            {
                *extent_of = None;
                changed = true;
            }
            adjusted = adjusted.saturating_add(usize::from(changed));
            cut_count = cut_count.saturating_add(usize::from(cut));
            self.elements.push(e);
        }
        let mut series_removed = false;
        if let Some(s) = &mut self.series {
            if !frames.contains(&s.frame) {
                self.series = None;
                series_removed = true;
            } else {
                s.margin = finite_or(s.margin, 0.1).clamp(0.0, 5.0);
            }
        }
        let series_adjusted = !series_removed && self.series != page.3;
        let page_adjusted = series_adjusted || translucent || (self.width, self.height, &self.guides) != (page.0, page.1, &page.2);
        LayoutRepairs {
            elements_adjusted: adjusted,
            elements_cut: cut_count,
            elements_removed: count.saturating_sub(self.elements.len()),
            page_adjusted: usize::from(page_adjusted),
            series_removed: usize::from(series_removed),
            mgrs_off,
        }
    }
}

/// What [`Layout::clamp`] changed in a layout.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LayoutRepairs {
    /// Elements kept with a value brought into range, a new id, a reference cleared, or a unit
    /// label's line breaks replaced.
    pub elements_adjusted: usize,
    /// Elements with a name, text, polygon points, legend exclusions, scale bar unit labels or
    /// legend labels cut to their limits (the same element may count as adjusted too).
    pub elements_cut: usize,
    /// Elements removed: beyond the 500 a layout holds, or tied to a map frame it doesn't have.
    pub elements_removed: usize,
    /// Whether the page size, guides, page colour or map series margin changed (0 or 1).
    pub page_adjusted: usize,
    /// Whether the map series was removed, its map frame missing (0 or 1).
    pub series_removed: usize,
    /// Map frames whose grid had MGRS labels on a graticule or over a system other than a WGS 84
    /// UTM zone, turned off.
    pub mgrs_off: usize,
}

impl LayoutRepairs {
    /// Whether nothing was changed.
    pub fn is_empty(&self) -> bool {
        *self == LayoutRepairs::default()
    }
}

fn finite_or(v: f64, d: f64) -> f64 {
    if v.is_finite() { v } else { d }
}

fn is_zero(v: &f64) -> bool {
    *v == 0.0
}

#[cfg(test)]
mod tests;
