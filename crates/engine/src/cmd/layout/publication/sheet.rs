//! Publication sheets, figures and styles, and the boxes they are drawn with.

use super::*;

/// One of `layout.publication`'s profiles.
#[derive(Clone, Copy, Debug)]
pub struct PublicationProfile {
    pub id: &'static str,
    pub label: &'static str,
    /// A sheet (a frame, title and boxed footer), else a figure (the map edge to edge, its title
    /// and sources left to the caption).
    pub sheet: bool,
    /// The page, in mm. A height of 0 follows the map's shape.
    pub width: f64,
    pub height: f64,
}

/// `layout.publication`'s figures, for a journal or a thesis: the map edge to edge, its title and
/// sources left to the caption.
pub(in crate::cmd::layout) const FIGURES: &[PublicationProfile] = &[
    PublicationProfile { id: "figure-1col", label: "Journal figure, 1 column (90 mm)", sheet: false, width: 90.0, height: 0.0 },
    PublicationProfile { id: "figure-1.5col", label: "Journal figure, 1.5 columns (140 mm)", sheet: false, width: 140.0, height: 0.0 },
    PublicationProfile { id: "figure-2col", label: "Journal figure, 2 columns (190 mm)", sheet: false, width: 190.0, height: 0.0 },
    PublicationProfile { id: "thesis-full", label: "Thesis figure, full page (160 × 225 mm)", sheet: false, width: 160.0, height: 225.0 },
    PublicationProfile { id: "thesis-half", label: "Thesis figure, half page (160 × 110 mm)", sheet: false, width: 160.0, height: 110.0 },
];

/// `layout.publication`'s profiles: a sheet for printing on every page size `layout.new` offers
/// ([`PAGES`], in its order), portrait then landscape (`a4-portrait`, "A4 portrait sheet"), then
/// the journal and thesis figures. Built once from the two lists, so a page size added to `PAGES`
/// is a sheet too; its names live for the program, as a constant's would.
pub static PUBLICATION_PROFILES: LazyLock<Vec<PublicationProfile>> = LazyLock::new(|| {
    let name = |text: String| -> &'static str { Box::leak(text.into_boxed_str()) };
    let sheets = PAGES.iter().flat_map(|&(size, label, w, h)| {
        [("portrait", w, h), ("landscape", h, w)].map(|(orientation, width, height)| PublicationProfile {
            id: name(format!("{size}-{orientation}")),
            label: name(format!("{label} {orientation} sheet")),
            sheet: true,
            width,
            height,
        })
    });
    sheets.chain(FIGURES.iter().copied()).collect()
});

/// `layout.publication`'s styles for a sheet, by the name `"style"` takes, with their names in
/// English: the default first.
pub const PUBLICATION_STYLES: &[(&str, &str)] = &[
    ("classic", "Classic"),
    ("banner", "Banner"),
    ("panel", "Panel"),
    ("editorial", "Editorial"),
    ("modular", "Modular"),
    ("poster", "Poster"),
];

/// The credit line `layout.publication` writes when `"author"` isn't given (`{date}` is filled
/// with today's date when the layout is drawn).
pub const PUBLICATION_DEFAULT_CREDIT: &str = "Made with RustGIS · {date}";

/// The longest text a publication layout takes in one field, in characters.
pub const MAX_PUBLICATION_TEXT: usize = 2000;

/// The default accent colour of a [`PUBLICATION_STYLES`] style id, or `None` if unknown.
pub fn publication_style_accent(style: &str) -> Option<Rgba> {
    Style::parse(style).map(Style::accent)
}

/// How a publication sheet is arranged around its map.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(in crate::cmd::layout) enum Style {
    /// The title over the map, the footer's boxes under it.
    Classic,
    /// The title and subtitle in a band of the accent colour across the top.
    Banner,
    /// The map at the left; a panel at the right with the title, the locator, the legend, the
    /// sources and the reference.
    Panel,
    /// A magazine's page: the title large in serif, a column at the right with the note, the
    /// locator and the legend.
    Editorial,
    /// Cards on a light page: the map in one, the locator and the legend in one each beside it,
    /// the sources and the reference in tinted ones under them.
    Modular,
    /// The page in the accent colour: the title and subtitle in light text across the top, the
    /// map in a light card, the footer in darker blocks.
    Poster,
}

impl Style {
    pub(in crate::cmd::layout) fn parse(code: &str) -> Option<Self> {
        match code.trim() {
            "classic" => Some(Style::Classic),
            "banner" => Some(Style::Banner),
            "panel" => Some(Style::Panel),
            "editorial" => Some(Style::Editorial),
            "modular" => Some(Style::Modular),
            "poster" => Some(Style::Poster),
            _ => None,
        }
    }

    pub(in crate::cmd::layout) fn code(self) -> &'static str {
        match self {
            Style::Classic => "classic",
            Style::Banner => "banner",
            Style::Panel => "panel",
            Style::Editorial => "editorial",
            Style::Modular => "modular",
            Style::Poster => "poster",
        }
    }

    /// The accent colour when none is given: a deep blue, green or brown (a poster's page).
    pub(in crate::cmd::layout) fn accent(self) -> Rgba {
        match self {
            Style::Classic => Rgba::rgb(0x2B, 0x26, 0x20),
            Style::Banner => Rgba::rgb(0x4A, 0x34, 0x26),
            Style::Panel => Rgba::rgb(0x2E, 0x5E, 0x3A),
            Style::Editorial => Rgba::rgb(0x1F, 0x2D, 0x4D),
            Style::Modular => Rgba::rgb(0x2F, 0x6B, 0x4F),
            Style::Poster => Rgba::rgb(0x3A, 0x26, 0x18),
        }
    }

    /// Whether its surrounds sit on cards with rounded corners.
    pub(in crate::cmd::layout) fn cards(self) -> bool {
        matches!(self, Style::Modular | Style::Poster)
    }
}

/// How a style draws its footer: its boxes' fill, outline and corners, and its texts' colours.
#[derive(Clone, Copy)]
pub(in crate::cmd::layout) struct Look {
    pub(in crate::cmd::layout) head: Rgba,
    pub(in crate::cmd::layout) body: Rgba,
    pub(in crate::cmd::layout) small: Rgba,
    pub(in crate::cmd::layout) fill: Rgba,
    pub(in crate::cmd::layout) outline: Rgba,
    pub(in crate::cmd::layout) width: f64,
    pub(in crate::cmd::layout) radius: f64,
}

/// Where a style sets the locator and the legend (under it) beside the map rather than over it:
/// the room left for them there, in page millimetres.
#[derive(Clone, Copy, Debug)]
pub(in crate::cmd::layout) struct Column {
    pub(in crate::cmd::layout) x: f64,
    pub(in crate::cmd::layout) y: f64,
    pub(in crate::cmd::layout) w: f64,
    pub(in crate::cmd::layout) h: f64,
}

/// A rectangle with no outline (`width` 0) or no fill (`fill` transparent).
pub(in crate::cmd::layout) fn pub_box(fill: Rgba, outline: Rgba, width: f64) -> ElementKind {
    pub_card(fill, outline, width, 0.0)
}

/// [`pub_box`] with its corners rounded by `radius` millimetres.
pub(in crate::cmd::layout) fn pub_card(fill: Rgba, outline: Rgba, width: f64, radius: f64) -> ElementKind {
    ElementKind::Rectangle { fill, outline, width, radius, dash: None }
}

/// Where a page's visual centre sits down it, as a share of its height: a little above its middle,
/// where the eye takes a page's centre to be.
pub(in crate::cmd::layout) const VISUAL_CENTRE_DOWN: f64 = 0.45;

/// How far `at` (page mm) is from the visual centre of a page `page` (mm) wide and tall.
pub(in crate::cmd::layout) fn visual_centre_offset(at: (f64, f64), (w, h): (f64, f64)) -> f64 {
    (at.0 - w / 2.0).hypot(at.1 - h * VISUAL_CENTRE_DOWN)
}
