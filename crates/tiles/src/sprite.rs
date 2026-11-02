//! Sprites: the icons and fill patterns a style draws, packed into one picture with an index.
//!
//! A style's `sprite` address names two files, `<address>.json` (where each image is in the
//! picture, its pixel ratio and whether it is an SDF icon) and `<address>.png`, and their `@2x`
//! versions for high-density screens. Both come through the tile cache; images are cut out of
//! the picture the first time they are asked for, and SDF icons are coloured as they are drawn.
//! A style may name several sprites, each with an id: one is its `default`, and the images of
//! the others are named `<id>:<name>`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use tiny_skia::{IntRect, Pixmap};

use crate::cache::{self, Payload, State, Want};
use crate::style::{MAX_SPRITES, Rgba};

/// Where one image is in the picture.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Entry {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    ratio: f32,
    sdf: bool,
}

/// A style's loaded sprites, by id.
pub struct Sprite {
    sheets: Vec<(String, Arc<Sheet>)>,
}

impl std::fmt::Debug for Sprite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.sheets.iter().map(|(id, s)| (id, s.index.len()))).finish()
    }
}

/// One sprite: its picture and index.
struct Sheet {
    sheet: Pixmap,
    index: HashMap<String, Entry>,
    /// Images cut out so far, by name and (for SDF icons) colour: looked up by `&str`, so a
    /// hit allocates nothing.
    cut: Mutex<HashMap<String, HashMap<Option<Rgba>, Arc<Pixmap>>>>,
}

/// An image from a sprite.
#[derive(Clone)]
pub struct Image {
    pub pixmap: Arc<Pixmap>,
    /// Pixels per logical point it was drawn for (2 in an `@2x` sprite).
    pub ratio: f32,
}

/// The most images a sprite index lists.
const MAX_IMAGES: usize = 10_000;
/// The largest side of one image (icons and patterns are tens of pixels).
const MAX_SIDE: f64 = 1024.0;
/// The most bytes of cut images a sheet keeps.
const MAX_CUT_BYTES: usize = 32 << 20;
/// The most bytes of decoded pictures the kept sheets hold together.
const MAX_SHEET_BYTES: usize = 256 << 20;
/// The most bytes of cut images the kept sheets hold together: a style may have many sprites.
const MAX_ALL_CUT_BYTES: usize = 64 << 20;

impl Sprite {
    /// A style's one sprite, from its index and picture.
    pub fn new(index: &serde_json::Value, sheet: Pixmap) -> Sprite {
        Sprite { sheets: vec![("default".to_owned(), Arc::new(Sheet::new(index, sheet)))] }
    }

    /// A style's sprites, each from its id, index and picture.
    #[cfg(test)]
    pub(crate) fn of(sheets: Vec<(&str, serde_json::Value, Pixmap)>) -> Sprite {
        Sprite { sheets: sheets.into_iter().map(|(id, index, sheet)| (id.to_owned(), Arc::new(Sheet::new(&index, sheet)))).collect() }
    }

    /// The sheet with the image `name`, and its name there, as MapLibre names them: a sprite's
    /// images are `<id>:<name>`, and the `default` sprite's just `<name>`. Looking anywhere else
    /// would draw another sprite's image of that name, and keep `coalesce` from going on to its
    /// next choice. An id may hold a colon itself (`harbour:night:light`), so the name is matched
    /// against each id, not split, and the longest id that has the image wins.
    fn find<'a>(&self, name: &'a str) -> Option<(&Sheet, &'a str)> {
        let named = self
            .sheets
            .iter()
            .filter(|(id, _)| id != "default")
            .filter_map(|(id, s)| {
                let bare = name.strip_prefix(id.as_str())?.strip_prefix(':')?;
                s.usable(bare).then_some((id.len(), (&**s, bare)))
            })
            .max_by_key(|(len, _)| *len)
            .map(|(_, found)| found);
        named.or_else(|| {
            let (_, s) = self.sheets.iter().find(|(id, _)| id == "default")?;
            s.usable(name).then_some((&**s, name))
        })
    }

    /// The ids of the style's sprites that were read. They change when one that failed is read
    /// after all, or another fails instead, and what was made from them can then be made again.
    pub fn sheet_ids(&self) -> Vec<String> {
        self.sheets.iter().map(|(id, _)| id.clone()).collect()
    }

    /// Whether a sprite has the image `name`, one that can be drawn.
    pub fn has(&self, name: &str) -> bool {
        self.find(name).is_some()
    }

    /// The image `name`, if a sprite has it. An SDF icon is drawn in `color` (black when none
    /// is given).
    pub fn image(&self, name: &str, color: Option<Rgba>) -> Option<Image> {
        let (sheet, name) = self.find(name)?;
        sheet.image(name, color)
    }
}

impl Sheet {
    fn new(index: &serde_json::Value, sheet: Pixmap) -> Sheet {
        let num = |v: &serde_json::Value, k: &str| v.get(k).and_then(serde_json::Value::as_f64);
        let index = index
            .as_object()
            .into_iter()
            .flatten()
            .take(MAX_IMAGES)
            .filter_map(|(name, v)| {
                let (x, y, w, h) = (num(v, "x")?, num(v, "y")?, num(v, "width")?, num(v, "height")?);
                if [x, y].iter().any(|n| !(n.is_finite() && *n >= 0.0 && *n <= 16_384.0))
                    || ![w, h].iter().all(|n| n.is_finite() && *n >= 1.0 && *n <= MAX_SIDE)
                {
                    return None;
                }
                let ratio = num(v, "pixelRatio").filter(|r| r.is_finite() && *r > 0.0 && *r <= 8.0).unwrap_or(1.0) as f32;
                let sdf = v.get("sdf").and_then(serde_json::Value::as_bool).unwrap_or(false);
                Some((name.clone(), Entry { x: x as u32, y: y as u32, w: w as u32, h: h as u32, ratio, sdf }))
            })
            .collect();
        Sheet { sheet, index, cut: Mutex::default() }
    }

    /// Whether the index lists `name` wholly inside the picture: an entry reaching past it can't
    /// be cut (cutting would otherwise clip it to a part).
    fn usable(&self, name: &str) -> bool {
        self.index.get(name).is_some_and(|e| self.fits(e))
    }

    fn fits(&self, e: &Entry) -> bool {
        e.x.checked_add(e.w).is_some_and(|r| r <= self.sheet.width()) && e.y.checked_add(e.h).is_some_and(|b| b <= self.sheet.height())
    }

    /// The bytes of the images cut so far.
    fn cut_bytes(&self) -> usize {
        self.cut.lock().unwrap_or_else(|p| p.into_inner()).values().flat_map(HashMap::values).map(|p| p.data().len()).sum()
    }

    /// Starts this sheet's and `others`' cut images again if `more` bytes would take them past
    /// `budget`: images are cut again when next asked for.
    fn make_room(&self, others: &[Arc<Sheet>], more: usize, budget: usize) {
        let others = || others.iter().map(|s| &**s).filter(|s| !std::ptr::eq(*s, self));
        if self.cut_bytes() + others().map(Sheet::cut_bytes).sum::<usize>() + more > budget {
            for s in others().chain([self]) {
                s.cut.lock().unwrap_or_else(|p| p.into_inner()).clear();
            }
        }
    }

    fn image(&self, name: &str, color: Option<Rgba>) -> Option<Image> {
        let (key, e) = self.index.get_key_value(name).map(|(k, e)| (k.as_str(), *e))?;
        let color = e.sdf.then(|| color.unwrap_or([0, 0, 0, 255]));
        if let Some(p) = self.cut.lock().unwrap_or_else(|p| p.into_inner()).get(key).and_then(|by_color| by_color.get(&color)) {
            return Some(Image { pixmap: p.clone(), ratio: e.ratio });
        }
        // Cut outside the lock.
        if !self.fits(&e) {
            return None;
        }
        let mut p = self.sheet.clone_rect(IntRect::from_xywh(e.x as i32, e.y as i32, e.w, e.h)?)?;
        if let Some(c) = color {
            sdf_fill(&mut p, c);
        }
        let p = Arc::new(p);
        // One cut at a time from here, so workers can't each see room for theirs and together go
        // past the budget. (A cut image is found without this lock. The kept sheets are listed
        // before any sheet's images are locked, and each is locked on its own.)
        static ROOM: Mutex<()> = Mutex::new(());
        let _room = ROOM.lock().unwrap_or_else(|p| p.into_inner());
        let others: Vec<Arc<Sheet>> = sprites().lock().unwrap_or_else(|p| p.into_inner()).values().cloned().collect();
        self.make_room(&others, p.data().len(), MAX_ALL_CUT_BYTES);
        let mut cut = self.cut.lock().unwrap_or_else(|p| p.into_inner());
        // Kept within a byte budget: an SDF icon cut in many colours, or a style naming
        // thousands of images, starts the cache again rather than growing it.
        let bytes: usize = cut.values().flat_map(HashMap::values).map(|p| p.data().len()).sum();
        if bytes + p.data().len() > MAX_CUT_BYTES {
            cut.clear();
        }
        cut.entry(key.to_string()).or_default().insert(color, p.clone());
        Some(Image { pixmap: p, ratio: e.ratio })
    }
}

/// Colours an SDF icon: its alpha is a distance from the edge (0.75 at the edge, more inside),
/// which becomes `c`, smoothed over a pixel.
fn sdf_fill(p: &mut Pixmap, c: Rgba) {
    const EDGE: f32 = 0.75;
    const SMOOTH: f32 = 0.06;
    for px in p.data_mut().as_chunks_mut::<4>().0 {
        let d = f32::from(px[3]) / 255.0;
        let t = ((d - (EDGE - SMOOTH)) / (2.0 * SMOOTH)).clamp(0.0, 1.0);
        let a = t * t * (3.0 - 2.0 * t) * f32::from(c[3]) / 255.0;
        for (i, ch) in c.iter().take(3).enumerate() {
            px[i] = (f32::from(*ch) * a).round() as u8;
        }
        px[3] = (a * 255.0).round() as u8;
    }
}

pub enum Load {
    Ready(Arc<Sprite>),
    Pending,
    Failed,
}

fn sprites() -> &'static Mutex<HashMap<String, Arc<Sheet>>> {
    static S: OnceLock<Mutex<HashMap<String, Arc<Sheet>>>> = OnceLock::new();
    S.get_or_init(Mutex::default)
}

/// Forgets the decoded sprite sheets, so they are read again (with [`crate::cache::clear`]).
pub(crate) fn forget() {
    sprites().lock().unwrap_or_else(|p| p.into_inner()).clear();
}

/// `<address><suffix>`, the suffix going before any query string.
fn with_suffix(base: &str, suffix: &str) -> String {
    match base.split_once('?') {
        Some((path, query)) => format!("{path}{suffix}?{query}"),
        None => format!("{base}{suffix}"),
    }
}

/// What to say of `sprites` (of the `listed` a style has, as (id, address)) that `load` couldn't
/// read, if any: they lose only their own images (`what`), but that is said. Nothing while they
/// are on their way.
pub(crate) fn unread_note(sprites: &[(String, String)], listed: usize, load: &Load, what: &str) -> Option<String> {
    let read: Vec<&str> = match load {
        Load::Pending => return None,
        Load::Failed => Vec::new(),
        Load::Ready(s) => s.sheets.iter().map(|(id, _)| id.as_str()).collect(),
    };
    let unread: Vec<&str> = sprites.iter().map(|(id, _)| id.as_str()).filter(|id| !read.contains(id)).collect();
    (!unread.is_empty())
        .then(|| format!("{} of {listed} sprites couldn't be read, so their {what} aren't drawn: {}", unread.len(), unread.join(", ")))
}

/// A sheet on its way.
enum SheetLoad {
    Ready(Arc<Sheet>),
    Pending,
    Failed,
}

/// A style's sprites, as (id, address without `.json`/`.png`), at high density when `hi_dpi`
/// and a sprite has it, else at 1×. Ready once none is on its way, with those that could be
/// read; one that can't only loses its images.
pub fn load(sprites: &[(String, String)], hi_dpi: bool) -> Load {
    load_with(sprites, |base| load_sheet(base, hi_dpi))
}

fn load_with(sprites: &[(String, String)], sheet: impl Fn(&str) -> SheetLoad) -> Load {
    let mut sheets = Vec::new();
    let mut pending = false;
    for (id, base) in sprites {
        match sheet(base) {
            SheetLoad::Ready(s) => sheets.push((id.clone(), s)),
            SheetLoad::Pending => pending = true,
            SheetLoad::Failed => {}
        }
    }
    if pending {
        Load::Pending
    } else if sheets.is_empty() {
        Load::Failed
    } else {
        Load::Ready(Arc::new(Sprite { sheets }))
    }
}

/// Keeps a decoded sheet. Room for every sprite of a style at both densities, since the mesh path
/// asks for 1× and a high-density screen's drawing for `@2x` on every frame: fewer would start
/// again each time, and cut the icons again. Past that, or past `budget` bytes of pictures, it
/// starts again rather than grow.
fn keep(all: &mut HashMap<String, Arc<Sheet>>, key: String, sheet: Arc<Sheet>, budget: usize) {
    let others = all.iter().filter(|(k, _)| **k != key);
    let bytes: usize = others.clone().map(|(_, s)| s.sheet.data().len()).sum();
    if others.count() >= 2 * MAX_SPRITES || bytes + sheet.sheet.data().len() > budget {
        all.clear();
    }
    all.insert(key, sheet);
}

fn load_sheet(base: &str, hi_dpi: bool) -> SheetLoad {
    if hi_dpi {
        match load_one(base, "@2x") {
            SheetLoad::Failed => {}
            other => return other,
        }
    }
    load_one(base, "")
}

fn load_one(base: &str, density: &str) -> SheetLoad {
    let key = format!("{base}|{density}");
    if let Some(s) = sprites().lock().unwrap_or_else(|p| p.into_inner()).get(&key) {
        return SheetLoad::Ready(s.clone());
    }
    let index = cache::get(&with_suffix(base, &format!("{density}.json")), Want::Json);
    let sheet = cache::get(&with_suffix(base, &format!("{density}.png")), Want::Image);
    match (index, sheet) {
        (State::Ready(i), State::Ready(p)) => match (&*i, &*p) {
            (Payload::Json(index), Payload::Image(sheet)) => {
                let s = Arc::new(Sheet::new(index, sheet.clone()));
                keep(&mut sprites().lock().unwrap_or_else(|p| p.into_inner()), key, s.clone(), MAX_SHEET_BYTES);
                SheetLoad::Ready(s)
            }
            _ => SheetLoad::Failed,
        },
        (State::Failed(_), _) | (_, State::Failed(_)) => SheetLoad::Failed,
        _ => SheetLoad::Pending,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet() -> Pixmap {
        let mut p = Pixmap::new(8, 4).unwrap();
        p.fill(tiny_skia::Color::from_rgba8(255, 0, 0, 255));
        p
    }

    #[test]
    fn forgetting_drops_the_decoded_sheets() {
        let key = "forget-test|".to_string();
        let sprite = Sheet { sheet: sheet(), index: Default::default(), cut: Default::default() };
        sprites().lock().unwrap().insert(key.clone(), Arc::new(sprite));
        forget();
        assert!(!sprites().lock().unwrap().contains_key(&key));
    }

    #[test]
    fn cuts_images_out() {
        let index = serde_json::json!({
            "dot": {"x": 0, "y": 0, "width": 4, "height": 4, "pixelRatio": 2},
            "sdf": {"x": 4, "y": 0, "width": 4, "height": 4, "pixelRatio": 1, "sdf": true},
            "bad": {"x": 6, "y": 0, "width": 40, "height": 4},
            "huge": {"x": 0, "y": 0, "width": 4096, "height": 4},
            "junk": {"x": -1}
        });
        let s = Sprite::new(&index, sheet());
        let dot = s.image("dot", None).unwrap();
        assert_eq!((dot.pixmap.width(), dot.ratio), (4, 2.0));
        // Several sprites: `<id>:<name>`.
        // A style's one sprite is `default`: its images are named without an id.
        assert!(s.image("base:dot", None).is_none());
        assert!(s.image("bad", None).is_none() && s.image("junk", None).is_none() && s.image("nope", None).is_none());
        // Listed, but reaching past the picture: not there for `image` either, so `coalesce`
        // goes on to the next.
        assert!(!s.has("bad") && s.has("dot"));
        assert!(s.image("huge", None).is_none(), "over the largest side");
        // An SDF icon takes the colour asked for (full alpha is well inside the edge).
        let blue = s.image("sdf", Some([0, 0, 255, 255])).unwrap();
        let px = blue.pixmap.pixel(1, 1).unwrap();
        assert_eq!((px.red(), px.blue(), px.alpha()), (0, 255, 255));
    }

    #[test]
    fn several_sprites_name_their_images_by_id() {
        let sheet_of = |names: &[&str]| {
            let index: serde_json::Map<_, _> =
                names.iter().map(|n| (n.to_string(), serde_json::json!({"x": 0, "y": 0, "width": 4, "height": 4}))).collect();
            Arc::new(Sheet::new(&serde_json::Value::Object(index), sheet()))
        };
        let s = Sprite {
            sheets: vec![
                ("base".to_owned(), sheet_of(&["icon-bus", "anchor"])),
                ("nautical".to_owned(), sheet_of(&["anchor", "light/red"])),
            ],
        };
        assert!(s.has("nautical:light/red") && s.has("base:icon-bus"));
        // The id picks the sprite when both have the name.
        let (picked, name) = s.find("nautical:anchor").unwrap();
        assert!(std::ptr::eq(picked, &*s.sheets[1].1) && name == "anchor");
        // Only in the sprite named: not in another that has an image of that name.
        assert!(!s.has("base:light/red") && !s.has("nautical:icon-bus") && !s.has("nautical:") && !s.has(""));
        // Without an id, only in the `default` sprite, which this style hasn't got.
        assert!(!s.has("icon-bus") && !s.has("other:icon-bus"));
        let with_default = Sprite { sheets: vec![("default".to_owned(), sheet_of(&["pin"])), ("nautical".to_owned(), sheet_of(&["pin"]))] };
        let (d, name) = with_default.find("pin").unwrap();
        assert!(std::ptr::eq(d, &*with_default.sheets[0].1) && name == "pin");
        assert!(with_default.has("nautical:pin") && !with_default.has("default:missing"));
        // Nothing to read is no sprite.
        assert!(matches!(load(&[], false), Load::Failed));
    }

    #[test]
    fn an_id_may_hold_a_colon() {
        let at = serde_json::json!({"x": 0, "y": 0, "width": 4, "height": 4});
        let s = Sprite::of(vec![
            ("harbour:night", serde_json::json!({"light": at}), sheet()),
            ("harbour", serde_json::json!({"crane": at}), sheet()),
            ("default", serde_json::json!({"pin": at, "other:pin": at}), sheet()),
        ]);
        let (found, name) = s.find("harbour:night:light").unwrap();
        assert!(std::ptr::eq(found, &*s.sheets[0].1) && name == "light");
        assert!(s.has("harbour:crane"));
        // Both ids fit and both have an image of that name: the longest.
        let both = Sprite::of(vec![("a", serde_json::json!({"b:icon": at}), sheet()), ("a:b", serde_json::json!({"icon": at}), sheet())]);
        let (found, name) = both.find("a:b:icon").unwrap();
        assert!(std::ptr::eq(found, &*both.sheets[1].1) && name == "icon");
        // Named by neither id: a `default` image, whose name may hold one.
        assert!(s.has("other:pin") && !s.has("other:crane") && !s.has("harbour:night:crane"));
        // The `default` sprite's images have no prefix.
        assert!(s.has("pin") && !s.has("default:pin"));
    }

    #[test]
    fn cut_images_are_kept_within_a_budget_across_sheets() {
        let at = serde_json::json!({"x": 0, "y": 0, "width": 4, "height": 4});
        let sheet_of = || Arc::new(Sheet::new(&serde_json::json!({"a": at}), sheet()));
        let (one, two) = (sheet_of(), sheet_of());
        one.image("a", None).unwrap();
        two.image("a", None).unwrap();
        let (a, b) = (one.cut_bytes(), two.cut_bytes());
        assert!(a > 0 && b > 0);
        // Room left: both stay. (`one` listed too counts once.)
        one.make_room(&[one.clone(), two.clone()], 1, a + b + 1);
        assert_eq!((one.cut_bytes(), two.cut_bytes()), (a, b));
        // No room: both start again, and cut their images when next asked.
        one.make_room(&[one.clone(), two.clone()], 2, a + b + 1);
        assert_eq!((one.cut_bytes(), two.cut_bytes()), (0, 0));
        assert!(two.image("a", None).is_some());
    }

    #[test]
    fn every_sprite_of_a_style_is_read() {
        let one = |name: &str| {
            let index = serde_json::json!({ name: {"x": 0, "y": 0, "width": 4, "height": 4} });
            SheetLoad::Ready(Arc::new(Sheet::new(&index, sheet())))
        };
        let style: Vec<_> = ["a", "b"].iter().map(|id| (id.to_string(), id.to_string())).collect();
        // Both read: each with its own images, in the style's order.
        let Load::Ready(s) = load_with(&style, |base| one(base)) else { panic!("ready") };
        assert_eq!(s.sheet_ids(), ["a", "b"]);
        assert!(s.has("a:a") && s.has("b:b") && !s.has("a:b") && !s.has("b:a"));
        // One that fails only loses its own.
        let Load::Ready(s) = load_with(&style, |base| if base == "a" { SheetLoad::Failed } else { one(base) }) else { panic!("ready") };
        assert_eq!(s.sheet_ids(), ["b"]);
        // One on its way holds back the others, whichever it is.
        assert!(matches!(load_with(&style, |base| if base == "a" { SheetLoad::Pending } else { one(base) }), Load::Pending));
        assert!(matches!(load_with(&style, |base| if base == "b" { SheetLoad::Pending } else { one(base) }), Load::Pending));
        // None read.
        assert!(matches!(load_with(&style, |_| SheetLoad::Failed), Load::Failed));
    }

    #[test]
    fn says_which_sprites_couldnt_be_read() {
        let ids = |list: &[&str]| list.iter().map(|id| (id.to_string(), format!("https://x/{id}"))).collect::<Vec<_>>();
        let index = serde_json::json!({});
        let read = |list: &[&str]| Load::Ready(Arc::new(Sprite::of(list.iter().map(|id| (*id, index.clone(), sheet())).collect())));
        let style = ids(&["base", "nautical", "roads"]);
        // Some read: those that weren't, and how many.
        let said = unread_note(&style, 3, &read(&["nautical"]), "icons and patterns").unwrap();
        assert_eq!(said, "2 of 3 sprites couldn't be read, so their icons and patterns aren't drawn: base, roads");
        // None read: all of them.
        let said = unread_note(&style, 3, &Load::Failed, "icons").unwrap();
        assert_eq!(said, "3 of 3 sprites couldn't be read, so their icons aren't drawn: base, nautical, roads");
        // All read, on their way, or none to read: nothing to say.
        assert_eq!(unread_note(&style, 3, &read(&["base", "nautical", "roads"]), "icons"), None);
        assert_eq!(unread_note(&style, 3, &Load::Pending, "icons"), None);
        assert_eq!(unread_note(&[], 0, &Load::Failed, "icons"), None);
        // Counted among all the style lists, as its other notices are.
        let said = unread_note(&style, 5, &Load::Failed, "icons").unwrap();
        assert!(said.starts_with("3 of 5 sprites couldn't be read"), "{said}");
    }

    #[test]
    fn keeps_every_sprite_of_a_style_at_both_densities() {
        let mut all = HashMap::new();
        let one = || Arc::new(Sheet::new(&serde_json::json!({}), sheet()));
        let keys: Vec<String> = (0..MAX_SPRITES).flat_map(|i| [format!("s{i}|"), format!("s{i}|@2x")]).collect();
        for key in &keys {
            keep(&mut all, key.clone(), one(), usize::MAX);
        }
        assert_eq!(all.len(), keys.len(), "none of them evicted");
        // Asked for again, nothing is lost.
        keep(&mut all, keys[0].clone(), one(), usize::MAX);
        assert_eq!(all.len(), keys.len());
        // Past that, it starts again.
        keep(&mut all, "another|".to_owned(), one(), usize::MAX);
        assert_eq!(all.len(), 1);
        // And past the bytes, however few sheets: each picture here is 8 × 4 × 4 bytes.
        let mut all = HashMap::new();
        let side = one().sheet.data().len();
        keep(&mut all, "a|".to_owned(), one(), 2 * side);
        keep(&mut all, "b|".to_owned(), one(), 2 * side);
        assert_eq!(all.len(), 2);
        keep(&mut all, "c|".to_owned(), one(), 2 * side);
        assert_eq!(all.len(), 1);
    }

    #[test]
    fn suffixes_go_before_the_query() {
        assert_eq!(with_suffix("https://a/sprite?key=1", "@2x.png"), "https://a/sprite@2x.png?key=1");
        assert_eq!(with_suffix("https://a/sprite", ".json"), "https://a/sprite.json");
    }
}
