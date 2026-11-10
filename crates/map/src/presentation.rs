//! Presentations: pages shown one after another, each a map view, text, an image or a video,
//! with a title, speaker notes and a duration.

use rustgis_geom::Coord;
use serde::{Deserialize, Serialize};

use crate::Rgba;

/// Most pages a presentation keeps (files are hostile).
pub const MAX_PAGES: usize = 500;
/// Longest page title, page text and speaker notes, caption and video source, in characters.
pub const MAX_TITLE: usize = 500;
pub const MAX_TEXT: usize = 20_000;
pub const MAX_CAPTION: usize = 2000;
pub const MAX_SOURCE: usize = 4000;
/// Largest base64 picture: a 16 MB file.
pub const MAX_IMAGE: usize = 22 << 20;
/// The default page: 16:9, 254 × 142.875 mm (10 × 5.625 in).
pub const WIDTH: f64 = 254.0;
pub const HEIGHT: f64 = 142.875;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Presentation {
    pub name: String,
    /// Page size in millimetres.
    pub width: f64,
    pub height: f64,
    pub pages: Vec<Page>,
}

impl Default for Presentation {
    fn default() -> Self {
        Presentation { name: "Presentation".into(), width: WIDTH, height: HEIGHT, pages: Vec::new() }
    }
}

impl Presentation {
    /// Bounds a presentation read from a file, returning counts of discarded content.
    pub fn clamp(&mut self) -> crate::content::ContentRepairs {
        let mut repairs = crate::content::ContentRepairs::default();
        let side = |v: f64, d: f64| if v.is_finite() { v.clamp(50.0, 2000.0) } else { d };
        let dimensions = (self.width, self.height);
        self.width = side(self.width, WIDTH);
        self.height = side(self.height, HEIGHT);
        repairs.settings += usize::from(dimensions.0 != self.width) + usize::from(dimensions.1 != self.height);
        repairs.cut(&mut self.name, 200);
        repairs.truncate(&mut self.pages, MAX_PAGES);
        for p in &mut self.pages {
            repairs.cut(&mut p.title, MAX_TITLE);
            repairs.cut(&mut p.notes, MAX_TEXT);
            let seconds = p.seconds;
            p.seconds = if p.seconds.is_finite() { p.seconds.clamp(0.5, 3600.0) } else { 5.0 };
            repairs.settings += usize::from(seconds != p.seconds);
            match &mut p.kind {
                PageKind::Map { map, center, width_m } => {
                    repairs.cut(map, MAX_TITLE);
                    let view = (center.x, center.y, *width_m);
                    if !(center.x.is_finite() && center.y.is_finite()) {
                        *center = Coord::new(0.0, 0.0);
                    }
                    center.x = center.x.clamp(-180.0, 180.0);
                    center.y = center.y.clamp(-85.0, 85.0);
                    *width_m = if width_m.is_finite() { width_m.clamp(10.0, 4e7) } else { 4e7 };
                    repairs.settings += usize::from(view.0 != center.x) + usize::from(view.1 != center.y) + usize::from(view.2 != *width_m);
                }
                PageKind::Blank { text, .. } => repairs.cut(text, MAX_TEXT),
                // A picture too large to be one the commands accept is dropped.
                PageKind::Image { image, caption } => {
                    if image.len() > MAX_IMAGE {
                        image.clear();
                        repairs.images += 1;
                    }
                    repairs.cut(caption, MAX_CAPTION);
                }
                PageKind::Video { source, poster, caption } => {
                    repairs.cut(source, MAX_SOURCE);
                    if poster.as_ref().is_some_and(|x| x.len() > MAX_IMAGE) {
                        *poster = None;
                        repairs.images += 1;
                    }
                    repairs.cut(caption, MAX_CAPTION);
                }
            }
        }
        repairs
    }
}

/// One page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    #[serde(default)]
    pub title: String,
    pub kind: PageKind,
    /// Speaker notes (not drawn).
    #[serde(default)]
    pub notes: String,
    /// How long the page shows when the presentation plays.
    #[serde(default = "five")]
    pub seconds: f64,
}

fn five() -> f64 {
    5.0
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum PageKind {
    /// A map at a place: its centre (longitude/latitude) and the ground width shown, in metres.
    #[serde(rename_all = "camelCase")]
    Map { map: String, center: Coord, width_m: f64 },
    /// Text on a coloured page.
    Blank {
        #[serde(default)]
        text: String,
        background: Rgba,
    },
    /// A picture (base64 PNG/JPEG) with a caption.
    Image {
        image: String,
        #[serde(default)]
        caption: String,
    },
    /// A video (a file path or URL), shown by its poster picture until played.
    Video {
        source: String,
        #[serde(default)]
        poster: Option<String>,
        #[serde(default)]
        caption: String,
    },
}

impl PageKind {
    pub fn code(&self) -> &'static str {
        match self {
            PageKind::Map { .. } => "map",
            PageKind::Blank { .. } => "blank",
            PageKind::Image { .. } => "image",
            PageKind::Video { .. } => "video",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_and_round_trips() {
        let mut p = Presentation {
            width: f64::NAN,
            pages: vec![Page {
                title: "Rome".into(),
                kind: PageKind::Map { map: "World".into(), center: Coord::new(f64::NAN, 95.0), width_m: -1.0 },
                notes: String::new(),
                seconds: f64::INFINITY,
            }],
            ..Presentation::default()
        };
        p.pages.push(Page {
            title: String::new(),
            kind: PageKind::Video { source: "v".repeat(MAX_SOURCE + 9), poster: Some("x".repeat(MAX_IMAGE + 1)), caption: String::new() },
            notes: String::new(),
            seconds: 5.0,
        });
        let repairs = p.clamp();
        assert_eq!((repairs.text, repairs.images), (1, 1));
        assert!(
            matches!(&p.pages[1].kind, PageKind::Video { source, poster: None, .. } if source.len() == MAX_SOURCE),
            "long sources are cut and oversized posters dropped"
        );
        assert_eq!(p.width, WIDTH);
        assert_eq!(p.pages[0].seconds, 5.0);
        assert!(matches!(p.pages[0].kind, PageKind::Map { center, width_m, .. } if center.y == 0.0 && width_m == 10.0));
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<Presentation>(&json).unwrap(), p);
    }
}
