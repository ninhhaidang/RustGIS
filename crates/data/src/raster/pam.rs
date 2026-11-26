//! GDAL's auxiliary metadata (PAM): the `.aux.xml` file GDAL writes next to a raster to hold what
//! the file itself doesn't, such as band descriptions, no-data values, class names and computed
//! statistics.
//!
//! ```xml
//! <PAMDataset>
//!   <PAMRasterBand band="1">
//!     <Description>Blue</Description>
//!     <NoDataValue>0</NoDataValue>
//!     <CategoryNames>
//!       <Category>no data</Category>
//!       <Category>water</Category>
//!     </CategoryNames>
//!     <Metadata>
//!       <MDI key="STATISTICS_MINIMUM">12</MDI>
//!       <MDI key="STATISTICS_MAXIMUM">2400</MDI>
//!     </Metadata>
//!   </PAMRasterBand>
//! </PAMDataset>
//! ```

use std::path::{Path, PathBuf};

use roxmltree::ParsingOptions;

/// Largest sidecar read (16 MiB): real ones hold a few histograms at most.
const MAX_BYTES: u64 = 16 << 20;
/// Most bands described.
const MAX_BANDS: usize = 65_536;
/// Most class names read for a band: one for each value of a 16-bit band.
const MAX_CATEGORIES: usize = 65_536;
/// Most rows read from a band's raster attribute table.
const MAX_RAT_ROWS: usize = 262_144;
/// Most class values all of a sidecar's attribute tables hold together (each up to the highest
/// value a table names), so a few bytes of table can't make each of many bands hold 65,536 names.
const MAX_RAT_SLOTS: usize = 1 << 20;
/// Longest band description or class name read from GDAL's metadata tag, in characters.
const MAX_NAME: usize = 256;

/// Statistics GDAL stored for a band (`STATISTICS_*`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stats {
    pub min: f64,
    pub max: f64,
    pub mean: Option<f64>,
    pub std_dev: Option<f64>,
}

/// What a sidecar says about one band.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PamBand {
    /// The band's number, from 1.
    pub band: usize,
    pub description: Option<String>,
    pub nodata: Option<f64>,
    pub stats: Option<Stats>,
    /// The names of the band's classes, by value (GDAL's category names): `categories[v]` names
    /// value `v`, and is empty when that value has none. Empty when the band names no class.
    pub categories: Vec<String>,
    /// Whether `categories` come from a raster attribute table rather than from category names,
    /// which a source the raster already has them from (its GDAL metadata) takes precedence over.
    pub from_table: bool,
    /// The band's central wavelength in nanometres, when the file gives one ([`wavelength_nm`]).
    pub wavelength: Option<f64>,
}

/// What a sidecar says about a raster's bands.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pam {
    pub bands: Vec<PamBand>,
    /// What [`parse`] or [`parse_gdal_metadata`] left out or cut, one line a kind with a count,
    /// for the caller to say (AGENTS.md, Never silent). Empty when the document is read whole.
    pub lost: crate::loss::Notes,
}

/// Where [`Raster::apply_pam`](super::Raster::apply_pam) says a document's losses came from: a
/// GeoTIFF's GDAL metadata tag (42112) …
pub const GDAL_METADATA: &str = "its GDAL metadata";
/// … or a `.aux.xml` sidecar next to the file.
pub const SIDECAR: &str = "its .aux.xml sidecar";

impl Pam {
    /// A document that names nothing, with the reason it was left out.
    pub fn losing(why: String) -> Pam {
        Pam { bands: Vec::new(), lost: vec![why].into() }
    }

    /// The band numbered `band` (from 1).
    pub fn band(&self, band: usize) -> Option<&PamBand> {
        self.bands.iter().find(|b| b.band == band)
    }

    /// The no-data value the bands that give one agree on. A raster holds one no-data value for
    /// all its bands, so bands giving different ones give none.
    pub fn nodata(&self) -> Option<f64> {
        self.nodata_within(usize::MAX)
    }

    /// [`Pam::nodata`] of the bands from 1 to `bands`: those a raster of `bands` bands has.
    pub fn nodata_within(&self, bands: usize) -> Option<f64> {
        let mut values = self.bands.iter().filter(|b| (1..=bands).contains(&b.band)).filter_map(|b| b.nodata);
        let first = values.next()?;
        values.all(|v| v == first).then_some(first)
    }
}

/// GDAL's item for a band's central wavelength, in micrometres (its `IMAGERY` metadata domain,
/// GDAL 3.10 and later).
pub const CENTRAL_WAVELENGTH_UM: &str = "CENTRAL_WAVELENGTH_UM";

/// The band items that give a wavelength, in the order one is taken: GDAL's own, then the
/// `wavelength` and `central_wavelength` items rasterio, STAC exports and ENVI files write.
const WAVELENGTH_ITEMS: [&str; 3] = [CENTRAL_WAVELENGTH_UM, "wavelength", "central_wavelength"];

/// The item some files give the unit of their `wavelength` items in (ENVI's `wavelength units`).
const WAVELENGTH_UNITS: &str = "wavelength_units";

/// Whether a band metadata item named `name` is read as (or for) a wavelength.
fn is_wavelength_item(name: &str) -> bool {
    name.eq_ignore_ascii_case(WAVELENGTH_UNITS) || WAVELENGTH_ITEMS.iter().any(|w| name.eq_ignore_ascii_case(w))
}

/// The length unit a wavelength unit item names, however it is spelt.
#[derive(Debug, PartialEq)]
enum Unit {
    /// None: empty, or ENVI's `Unknown`.
    Unstated,
    Nanometres,
    Micrometres,
    /// Another, by its text (lowercase, spaces aside): not read.
    Unread(String),
}

/// The unit `text` names (nanometres or micrometres as ENVI and others spell them, any case).
fn unit_of(text: &str) -> Unit {
    let u = text.trim().to_lowercase();
    match u.as_str() {
        "" | "unknown" => Unit::Unstated,
        "nm" | "nanometer" | "nanometers" | "nanometre" | "nanometres" => Unit::Nanometres,
        "um" | "\u{b5}m" | "\u{3bc}m" | "micrometer" | "micrometers" | "micrometre" | "micrometres" | "micron" | "microns" => {
            Unit::Micrometres
        }
        _ => Unit::Unread(u),
    }
}

/// A wavelength item's value in nanometres: `None` when it isn't a length from 1 nm to 1 m (no
/// sensor's band lies outside), or its unit isn't a length this knows.
///
/// `CENTRAL_WAVELENGTH_UM` is in micrometres by its name; another item is in the unit `units`
/// gives (nanometres or micrometres, as ENVI and others spell them). With no unit (or ENVI's
/// `Unknown`), a value under 100 is in micrometres and any other in nanometres: imaging sensors
/// see from about 0.3 to 15 µm (300 to 15,000 nm), so the two never meet.
pub fn wavelength_nm(item: &str, text: &str, units: Option<&str>) -> Option<f64> {
    let v = text.trim().parse::<f64>().ok().filter(|v| v.is_finite() && *v > 0.0)?;
    // Micrometres in nanometres, to a millionth of one: 0.665 µm is 665 nm, not 665.0000000000001.
    let nm = |um: f64| (um * 1e9).round() / 1e6;
    // The result is checked: a huge micrometre value overflows, a tiny one rounds to 0, and a
    // length past those a sensor sees would be lost when written in micrometres or stretch the
    // chart's axis.
    let in_range = |w: f64| Some(w).filter(|w| (1.0..=1e9).contains(w));
    if item.eq_ignore_ascii_case(CENTRAL_WAVELENGTH_UM) {
        return in_range(nm(v));
    }
    let micro = match unit_of(units.unwrap_or_default()) {
        Unit::Unstated => v < 100.0,
        Unit::Nanometres => false,
        Unit::Micrometres => true,
        Unit::Unread(_) => return None,
    };
    in_range(if micro { nm(v) } else { v })
}

/// Whether two wavelengths (nanometres, `None` for none) are one: lengths a millionth of a
/// nanometre apart or less are, as micrometres are read to that, so only rounding is absorbed.
pub fn same_wavelength(a: Option<f64>, b: Option<f64>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => (a - b).abs() <= 1e-6,
        (a, b) => a.is_none() && b.is_none(),
    }
}

/// Keeps a band's first wavelength in `slot`: `true` when `nm` is another one, left out (the
/// same length under another item is the same one, [`same_wavelength`]).
fn keep_first_wavelength(slot: &mut Option<f64>, nm: f64) -> bool {
    match *slot {
        None => {
            *slot = Some(nm);
            false
        }
        Some(w) => !same_wavelength(Some(w), Some(nm)),
    }
}

/// How many of `units` after the first that names one name another unit than it ([`unit_of`]:
/// `nm` and `nanometers` are one): those are left out, the first being the one read. An empty
/// item names none.
fn other_units<'a>(units: impl Iterator<Item = &'a str>) -> usize {
    let mut units = units.map(str::trim).filter(|u| !u.is_empty());
    let Some(first) = units.next().map(unit_of) else { return 0 };
    units.filter(|u| unit_of(u) != first).count()
}

/// A sidecar node's metadata items `key` (any domain, any case): their texts, empty for an item
/// with none (still an item the file gave).
fn mdi_texts<'a>(node: roxmltree::Node<'a, '_>, key: &'a str) -> impl Iterator<Item = &'a str> {
    node.children()
        .filter(|n| n.has_tag_name("Metadata"))
        .flat_map(|m| m.children())
        .filter(move |n| n.has_tag_name("MDI") && n.attribute("key").is_some_and(|k| k.eq_ignore_ascii_case(key)))
        .map(|n| n.text().unwrap_or_default())
}

/// A sidecar node's metadata item `key` (any domain, any case): the text of the first that has
/// one (an empty item says nothing).
fn mdi_text<'a>(node: roxmltree::Node<'a, '_>, key: &str) -> Option<&'a str> {
    node.children()
        .filter(|n| n.has_tag_name("Metadata"))
        .flat_map(|m| m.children())
        .filter(|n| n.has_tag_name("MDI") && n.attribute("key").is_some_and(|k| k.eq_ignore_ascii_case(key)))
        .filter_map(|n| n.text())
        .find(|t| !t.trim().is_empty())
}

/// The sidecars GDAL would read for `path`: `x.tif.aux.xml`, then `x.aux.xml`.
pub fn sidecar_paths(path: &Path) -> Vec<PathBuf> {
    let mut long = path.as_os_str().to_owned();
    long.push(".aux.xml");
    vec![PathBuf::from(long), path.with_extension("aux.xml")]
}

/// The sidecar next to `path`: the first that parses. When one is there but none parses (can't be
/// looked at, too large, not text, not GDAL's), it names nothing and says why ([`Pam::losing`]):
/// each one's reason, named, when both are there; when the first doesn't and the other does, the
/// other is read and says the first was left out.
pub fn read(path: &Path) -> Option<Pam> {
    let mut whys: Vec<(String, String)> = Vec::new();
    for p in sidecar_paths(path) {
        let read = match std::fs::metadata(&p) {
            // No sidecar by that name (or none can be, the name being too long).
            Err(e) if matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidFilename) => continue,
            Err(e) => Err(format!("it can't be looked at ({e})")),
            Ok(meta) if !meta.is_file() => continue,
            Ok(meta) if meta.len() > MAX_BYTES => Err(format!("it is over {} MiB", MAX_BYTES >> 20)),
            Ok(_) => {
                crate::provenance::read_to_string(&p).map_err(|e| format!("it isn't UTF-8 text ({e})")).and_then(|t| parse_sidecar(&t))
            }
        };
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match read {
            Ok(mut pam) => {
                if let Some((skipped, e)) = whys.first() {
                    pam.lost.insert(0, format!("{skipped}: {e}: left out, {name} read instead"));
                }
                return Some(pam);
            }
            Err(e) => whys.push((name, e)),
        }
    }
    let reason = match whys.as_slice() {
        [] => return None,
        [(_, e)] => e.clone(),
        all => all.iter().map(|(name, e)| format!("{name}: {e}")).collect::<Vec<_>>().join("; "),
    };
    Some(Pam::losing(format!("{reason}: its statistics, no-data, band names and wavelengths are left out")))
}

/// Parses a sidecar's text. One that doesn't read as XML, or isn't a PAM document, names nothing:
/// it comes back with only [`Pam::lost`] saying so. Its size is checked by [`read`] only.
pub fn parse(text: &str) -> Option<Pam> {
    Some(
        parse_sidecar(text)
            .unwrap_or_else(|e| Pam::losing(format!("{e}: its statistics, no-data, band names and wavelengths are left out"))),
    )
}

/// [`parse`], with why a document isn't read as the error.
fn parse_sidecar(text: &str) -> Result<Pam, String> {
    let opts = ParsingOptions { allow_dtd: false, nodes_limit: crate::io::XML_NODES, ..ParsingOptions::default() };
    let doc = crate::xml::parse(text, opts).map_err(|e| format!("it doesn't read as XML ({e})"))?;
    let root = doc.root_element();
    if root.tag_name().name() != "PAMDataset" {
        let name = root.tag_name().name().chars().take(40).collect::<String>();
        return Err(format!("it is a <{name}> document, not GDAL's <PAMDataset>"));
    }
    let mut pam = Pam::default();
    // Names cut to `MAX_NAME` characters, class names past `MAX_CATEGORIES`, records for no band
    // from 1 to `MAX_BANDS`, a band's records after its first, and the scale, offset, units and
    // other band metadata items and colour tables that aren't read, counted.
    let (mut cut, mut beyond, mut odd_bands, mut unscaled, mut units) = (0usize, 0usize, 0usize, 0usize, 0usize);
    let (mut items, mut tables, mut odd_nodata, mut odd_wavelengths, mut other_wavelengths) = (0usize, 0usize, 0usize, 0usize, 0usize);
    // Wavelength units given again otherwise for the dataset or a band (the first is read).
    let mut units_again = other_units(mdi_texts(root, WAVELENGTH_UNITS));
    let (mut seen, mut twice) = (std::collections::HashSet::new(), 0usize);
    // Raster attribute tables of value ranges, class names of one for a value that isn't a whole
    // number from 0 to `MAX_CATEGORIES - 1` or that is named again, and rows past `MAX_RAT_ROWS`.
    let mut rat = RatLost::default();
    // The unit the dataset gives its bands' wavelengths in, for a band that gives none; a
    // wavelength of the dataset's own is for no band.
    let dataset_units = mdi_text(root, WAVELENGTH_UNITS);
    odd_wavelengths += WAVELENGTH_ITEMS.iter().map(|w| mdi_texts(root, w).count()).sum::<usize>();
    let mut kept = |t: &str| {
        cut += usize::from(t.chars().nth(MAX_NAME).is_some());
        t.chars().take(MAX_NAME).collect::<String>()
    };
    for b in root.children().filter(|n| n.has_tag_name("PAMRasterBand")) {
        let Some(band) = b.attribute("band").and_then(|v| v.trim().parse::<usize>().ok()).filter(|n| (1..=MAX_BANDS).contains(n)) else {
            odd_bands += 1;
            continue;
        };
        // A band's first record is the one read ([`Pam::band`] finds the first).
        if !seen.insert(band) {
            twice += 1;
            continue;
        }
        let text_of = |tag: &str| b.children().find(|n| n.has_tag_name(tag)).and_then(|n| n.text()).map(str::trim);
        // A scale of 1 and an offset of 0 change nothing, and an empty unit says nothing.
        let number = |tag: &str| text_of(tag).map(|v| v.parse::<f64>().ok());
        unscaled += usize::from(number("Scale").is_some_and(|v| v != Some(1.0)));
        unscaled += usize::from(number("Offset").is_some_and(|v| v != Some(0.0)));
        units += usize::from(text_of("UnitType").is_some_and(|u| !u.is_empty()));
        // A band's metadata items of any domain other than its statistics (`STATISTICS_*`, worked
        // out again from the values when not read) aren't read, as for the GDAL tag.
        items += b
            .children()
            .filter(|n| n.has_tag_name("Metadata"))
            .flat_map(|m| m.children())
            .filter(|n| n.has_tag_name("MDI") && n.attribute("key").is_none_or(|k| !k.starts_with("STATISTICS_") && !is_wavelength_item(k)))
            .count();
        // Its wavelength: the first that is a length of the items that give one, in that order and
        // in any domain (GDAL's in `IMAGERY`, ENVI's in the default one), as the metadata tag's.
        let wavelength_units = mdi_text(b, WAVELENGTH_UNITS).or(dataset_units);
        units_again += other_units(mdi_texts(b, WAVELENGTH_UNITS));
        let mut wavelength = None;
        for w in WAVELENGTH_ITEMS {
            for t in mdi_texts(b, w) {
                match wavelength_nm(w, t, wavelength_units) {
                    Some(nm) => other_wavelengths += usize::from(keep_first_wavelength(&mut wavelength, nm)),
                    None => odd_wavelengths += 1,
                }
            }
        }
        // A colour table here isn't read (only the file's own is): one with entries is counted.
        tables += usize::from(b.children().any(|n| n.has_tag_name("ColorTable") && n.children().any(|e| e.has_tag_name("Entry"))));
        let description = text_of("Description").filter(|d| !d.is_empty()).map(&mut kept);
        // A no-data value that isn't a number is counted (NaN, already no-data, and an infinity,
        // which no pixel equals once read, aren't).
        let given = text_of("NoDataValue").filter(|v| !v.is_empty());
        odd_nodata += usize::from(given.is_some_and(|v| v.parse::<f64>().is_err()));
        let nodata = given.and_then(|v| v.parse::<f64>().ok()).filter(|v| v.is_finite());
        // Only the default (unnamed) metadata domain holds the statistics.
        let mdi = |key: &str| {
            b.children()
                .filter(|n| n.has_tag_name("Metadata") && n.attribute("domain").is_none_or(str::is_empty))
                .flat_map(|m| m.children())
                .find(|n| n.has_tag_name("MDI") && n.attribute("key") == Some(key))
                .and_then(|n| n.text())
                .and_then(|v| v.trim().parse::<f64>().ok())
                .filter(|v| v.is_finite())
        };
        let stats = match (mdi("STATISTICS_MINIMUM"), mdi("STATISTICS_MAXIMUM")) {
            (Some(min), Some(max)) if min <= max => {
                Some(Stats { min, max, mean: mdi("STATISTICS_MEAN"), std_dev: mdi("STATISTICS_STDDEV").filter(|s| *s >= 0.0) })
            }
            _ => None,
        };
        let named: Vec<_> = b
            .children()
            .find(|n| n.has_tag_name("CategoryNames"))
            .into_iter()
            .flat_map(|c| c.children().filter(|n| n.has_tag_name("Category")))
            .collect();
        beyond += named.len().saturating_sub(MAX_CATEGORIES);
        let mut categories: Vec<String> =
            named.iter().take(MAX_CATEGORIES).map(|n| kept(n.text().map(str::trim).unwrap_or_default())).collect();
        // Values past the last named one have no name either.
        while categories.last().is_some_and(String::is_empty) {
            categories.pop();
        }
        // Without category names, a raster attribute table's name column names the classes.
        // A table beside category names isn't read, and is counted when it names classes too.
        let mut from_table = false;
        if let Some(table) = b.children().find(|n| n.has_tag_name("GDALRasterAttributeTable")) {
            if categories.is_empty() {
                categories = table_names(table, &mut kept, &mut rat);
                from_table = !categories.is_empty();
            } else {
                rat.named += usize::from(name_column(&table_fields(table)).is_some());
            }
        }
        pam.bands.push(PamBand { band, description, nodata, stats, categories, from_table, wavelength });
    }
    if odd_bands > 0 {
        pam.lost.push(format!("{odd_bands} band record(s) for no band from 1 to {MAX_BANDS} left out"));
    }
    if twice > 0 {
        pam.lost.push(format!("{twice} band record(s) given again left out (the first is kept)"));
    }
    if unscaled > 0 {
        pam.lost.push(format!("{unscaled} scale or offset(s) not applied: the values are as stored"));
    }
    if units > 0 {
        pam.lost.push(format!("{units} band unit(s) not read: left out"));
    }
    if items > 0 {
        pam.lost.push(format!("{items} band metadata item(s) not read: left out"));
    }
    if tables > 0 {
        pam.lost.push(format!("{tables} band colour table(s) not read: left out (only the file's own is drawn)"));
    }
    if odd_nodata > 0 {
        pam.lost.push(format!("{odd_nodata} no-data value(s) that aren't numbers left out: those pixels are read as values"));
    }
    if odd_wavelengths > 0 {
        pam.lost.push(format!(
            "{odd_wavelengths} wavelength(s) that aren't a length from 1 nm to 1 m in nanometres or micrometres, or for no band, left out"
        ));
    }
    if other_wavelengths > 0 {
        pam.lost.push(format!("{other_wavelengths} other wavelength(s) for a band that has one left out (the first is kept)"));
    }
    if units_again > 0 {
        pam.lost.push(format!("{units_again} other wavelength unit(s) for a band or the dataset left out (the first is kept)"));
    }
    if beyond > 0 {
        pam.lost.push(crate::loss::Note::loss(
            crate::loss::LossCode::DroppedOverLimit,
            format!("{beyond} class name(s) past the first {MAX_CATEGORIES} of a band left out"),
        ));
    }
    if rat.ranges > 0 {
        pam.lost.push(format!("{} attribute table(s) of value ranges not read as class names", rat.ranges));
    }
    if rat.unread_tables > 0 {
        pam.lost.push(format!(
            "{} attribute table(s) with a bin or a field type or usage that isn't a number not read as class names",
            rat.unread_tables
        ));
    }
    if rat.text_tables > 0 {
        pam.lost.push(format!("{} attribute table(s) with text fields but no name column not read as class names", rat.text_tables));
    }
    if rat.odd > 0 {
        pam.lost.push(format!(
            "{} class name(s) of an attribute table for a value that isn't a whole number from 0 to {} left out",
            rat.odd,
            MAX_CATEGORIES - 1
        ));
    }
    if rat.again > 0 {
        pam.lost.push(format!("{} class name(s) of an attribute table given again for a value left out (the first is kept)", rat.again));
    }
    if rat.rows > 0 {
        pam.lost.push(crate::loss::Note::loss(
            crate::loss::LossCode::DroppedOverLimit,
            format!("{} attribute table row(s) past the first {MAX_RAT_ROWS} left out", rat.rows),
        ));
    }
    if rat.over > 0 {
        pam.lost.push(crate::loss::Note::loss(
            crate::loss::LossCode::DroppedOverLimit,
            format!("{} class name(s) of an attribute table past the sidecar's first {MAX_RAT_SLOTS} class values left out", rat.over),
        ));
    }
    if rat.named > 0 {
        pam.lost.push(format!("{} attribute table(s) of class names not read: the band's category names are used", rat.named));
    }
    if cut > 0 {
        pam.lost.push(crate::loss::Note::loss(
            crate::loss::LossCode::ChangedCut,
            format!("{cut} band or class name(s) cut to {MAX_NAME} characters"),
        ));
    }
    Ok(pam)
}

/// What reading raster attribute tables left out, for [`parse_sidecar`] to say, and how many
/// class values they hold so far (against [`MAX_RAT_SLOTS`]).
#[derive(Default)]
struct RatLost {
    ranges: usize,
    unread_tables: usize,
    text_tables: usize,
    odd: usize,
    again: usize,
    rows: usize,
    over: usize,
    named: usize,
    slots: usize,
}

/// A table's fields, in order: each one's lower-case name, type and usage.
fn table_fields(table: roxmltree::Node<'_, '_>) -> Vec<(String, Option<u32>, Option<u32>)> {
    table
        .children()
        .filter(|n| n.has_tag_name("FieldDefn"))
        .map(|f| {
            let text = |tag: &str| f.children().find(|n| n.has_tag_name(tag)).and_then(|n| n.text()).map(str::trim);
            (
                text("Name").unwrap_or_default().to_lowercase(),
                text("Type").and_then(|t| t.parse().ok()),
                text("Usage").and_then(|u| u.parse().ok()),
            )
        })
        .collect()
}

/// The field of a table's class names: the one GDAL gives the name usage, else a text field
/// called `Class_Name` and the like.
fn name_column(fields: &[(String, Option<u32>, Option<u32>)]) -> Option<usize> {
    const NAMES: [&str; 6] = ["class_name", "classname", "class_names", "class", "name", "label"];
    fields.iter().position(|f| f.2 == Some(2)).or_else(|| fields.iter().position(|f| f.1 == Some(2) && NAMES.contains(&f.0.as_str())))
}

/// Whether a field's type or usage is given but isn't a number, so which column GDAL means by its
/// usage can't be told.
fn odd_field_numbers(table: roxmltree::Node<'_, '_>) -> bool {
    table.children().filter(|n| n.has_tag_name("FieldDefn")).any(|f| {
        ["Type", "Usage"].iter().any(|&tag| {
            f.children()
                .find(|n| n.has_tag_name(tag))
                .and_then(|n| n.text())
                .map(str::trim)
                .is_some_and(|t| !t.is_empty() && t.parse::<u32>().is_err())
        })
    })
}

/// The class names a GDAL raster attribute table holds, by value (as `PamBand::categories`).
/// The names are in its [`name_column`]. A table whose bins or field types or usages aren't
/// numbers isn't read, and neither is one with a text field but no name column: both are counted,
/// as those could hold names. A table with neither names nothing and loses nothing. A
/// row's value is its value field (the min-max usage, else one called `Value`); without one, the
/// whole number in its unit bin when the table gives linear binning (`Row0Min` and `BinSize`,
/// row `index` from `Row0Min`), else its `index` (GDAL's convention for a table with no value
/// field). A table of value ranges (bins wider than 1, or a min or a max with no value) can't
/// name single values and is counted.
fn table_names(table: roxmltree::Node<'_, '_>, kept: &mut dyn FnMut(&str) -> String, lost: &mut RatLost) -> Vec<String> {
    let fields = table_fields(table);
    // A bin that isn't a number (or a non-finite `Row0Min`) would number the rows without a notice.
    let bin_odd = table.attribute("Row0Min").is_some_and(|v| v.trim().parse::<f64>().ok().is_none_or(|v| !v.is_finite()))
        || table.attribute("BinSize").is_some_and(|v| v.trim().parse::<f64>().is_err());
    if bin_odd || odd_field_numbers(table) {
        lost.unread_tables += 1;
        return Vec::new();
    }
    let Some(name_at) = name_column(&fields) else {
        lost.text_tables += usize::from(fields.iter().any(|f| f.1 == Some(2)));
        return Vec::new();
    };
    let value_at = fields.iter().position(|f| f.2 == Some(5)).or_else(|| fields.iter().position(|f| f.0 == "value"));
    let (min, bin) = (
        table.attribute("Row0Min").and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite()),
        table.attribute("BinSize").and_then(|v| v.trim().parse::<f64>().ok()),
    );
    let binned = min.zip(bin);
    let has = |usage| fields.iter().any(|f| f.2 == Some(usage));
    let wide = bin.is_some_and(|b| b != 1.0);
    if wide || (value_at.is_none() && binned.is_none() && (has(3) || has(4))) {
        lost.ranges += 1;
        return Vec::new();
    }
    let rows: Vec<_> = table.children().filter(|n| n.has_tag_name("Row")).collect();
    lost.rows += rows.len().saturating_sub(MAX_RAT_ROWS);
    let mut categories: Vec<String> = Vec::new();
    for (i, row) in rows.iter().take(MAX_RAT_ROWS).enumerate() {
        let cell =
            |at: usize| row.children().filter(|n| n.has_tag_name("F")).nth(at).and_then(|n| n.text()).map(str::trim).unwrap_or_default();
        let name = cell(name_at);
        if name.is_empty() {
            continue;
        }
        // The row's number as the file gives it, else its place.
        let index = row.attribute("index").map_or(Some(i as f64), |v| v.trim().parse::<usize>().ok().map(|v| v as f64));
        let value = match (value_at, binned) {
            (Some(at), _) => cell(at).parse::<f64>().ok(),
            // The whole number in the unit bin [Row0Min + index, Row0Min + index + 1).
            (None, Some((m, _))) => index.map(|r| (m + r).ceil()),
            (None, None) => index,
        };
        let Some(v) = value.filter(|v| v.fract() == 0.0 && (0.0..MAX_CATEGORIES as f64).contains(v)) else {
            lost.odd += 1;
            continue;
        };
        let v = v as usize;
        if categories.get(v).is_some_and(|c| !c.is_empty()) {
            lost.again += 1;
            continue;
        }
        if categories.len() <= v {
            let grow = v + 1 - categories.len();
            if lost.slots + grow > MAX_RAT_SLOTS {
                lost.over += 1;
                continue;
            }
            lost.slots += grow;
            categories.resize(v + 1, String::new());
        }
        categories[v] = kept(name);
    }
    categories
}

/// The band descriptions a GeoTIFF holds itself, in GDAL's metadata tag (`GDAL_METADATA`, 42112),
/// where GDAL and rasterio write them: `None` when it names no band and loses nothing.
/// Its `sample` counts from 0. Class names may be dataset items named `CLASS_<value>`, as the
/// MapBiomas files on Source Cooperative have them: they name band 1's classes, however many
/// bands the file has.
///
/// ```xml
/// <GDALMetadata>
///   <Item name="DESCRIPTION" sample="0" role="description">NDVI 2024-06-01</Item>
///   <Item name="CLASS_3">Forest Formation</Item>
/// </GDALMetadata>
/// ```
///
/// A document that doesn't read as XML (or is too large to), or isn't GDAL's, names nothing: it
/// comes back with only [`Pam::lost`] saying so. An empty tag says nothing.
pub fn parse_gdal_metadata(text: &str) -> Option<Pam> {
    if text.trim().is_empty() {
        return None;
    }
    let opts = ParsingOptions { allow_dtd: false, nodes_limit: crate::io::XML_NODES, ..ParsingOptions::default() };
    let doc = match crate::xml::parse(text, opts) {
        Ok(doc) => doc,
        Err(e) => return Some(Pam::losing(format!("it doesn't read as XML ({e}): band and class names and wavelengths are left out"))),
    };
    let root = doc.root_element();
    if root.tag_name().name() != "GDALMetadata" {
        let name = root.tag_name().name().chars().take(40).collect::<String>();
        return Some(Pam::losing(format!(
            "it is a <{name}> document, not GDAL's <GDALMetadata>: band and class names and wavelengths are left out"
        )));
    }
    let mut pam = Pam::default();
    // What is left out or cut, counted.
    let (mut odd_bands, mut twice, mut cut) = (0usize, 0usize, 0usize);
    // An item's text, and whether it is longer than is kept (counted only once it is kept).
    let item_text = |item: roxmltree::Node| {
        let t = item.text().map(str::trim).filter(|d| !d.is_empty())?;
        Some((t.chars().take(MAX_NAME).collect::<String>(), t.chars().nth(MAX_NAME).is_some()))
    };
    // The bands described so far, so a long document isn't searched again for each item.
    let mut seen = std::collections::HashSet::new();
    for item in root.children().filter(|n| n.has_tag_name("Item") && n.attribute("role") == Some("description")) {
        let band = item
            .attribute("sample")
            .and_then(|v| v.trim().parse::<usize>().ok())
            .and_then(|s| s.checked_add(1))
            .filter(|n| *n <= MAX_BANDS);
        let Some((description, long)) = item_text(item) else { continue };
        let Some(band) = band else {
            odd_bands += 1;
            continue;
        };
        // A band's first description, if it is given twice.
        if seen.insert(band) {
            pam.bands.push(PamBand { band, description: Some(description), ..PamBand::default() });
            cut += usize::from(long);
        } else {
            twice += 1;
        }
    }
    let (odd_wavelengths, other_wavelengths, units_again) = read_wavelengths(root, &mut pam);
    // A band's scale and offset aren't applied: the values stay as stored. One that changes
    // nothing (a scale of 1, an offset of 0) isn't counted.
    let unscaled = root
        .children()
        .filter(|n| n.has_tag_name("Item"))
        .filter(|n| match n.attribute("role") {
            Some("scale") => n.text().and_then(|t| t.trim().parse::<f64>().ok()) != Some(1.0),
            Some("offset") => n.text().and_then(|t| t.trim().parse::<f64>().ok()) != Some(0.0),
            _ => false,
        })
        .count();
    if unscaled > 0 {
        pam.lost.push(format!("{unscaled} scale or offset item(s) not applied: the values are as stored"));
    }
    // Class names, by value: dataset items (no band, no role) named CLASS_ and a whole number. One
    // given for a band or a role isn't read, and is counted.
    let (mut classes, mut odd_classes): (Vec<String>, usize) = (Vec::new(), 0);
    let banded = root
        .children()
        .filter(|n| n.has_tag_name("Item") && (n.attribute("sample").is_some() || n.attribute("role").is_some()))
        .filter(|n| n.attribute("name").is_some_and(|name| name.starts_with("CLASS_")))
        .count();
    if banded > 0 {
        pam.lost.push(format!("{banded} CLASS_ item(s) given for a band or a role left out (only the file's own are read)"));
    }
    // Other band items (units, a band's own keys, a `DESCRIPTION` with no description role) aren't
    // read either, and are counted. Statistics (`STATISTICS_*`) aren't: they are worked out again
    // from the values.
    let unread = root
        .children()
        .filter(|n| n.has_tag_name("Item") && (n.attribute("sample").is_some() || n.attribute("role").is_some()))
        .filter(|n| !matches!(n.attribute("role"), Some("description" | "scale" | "offset")))
        .filter(|n| n.attribute("name").is_none_or(|name| !name.starts_with("CLASS_") && !name.starts_with("STATISTICS_")))
        // Wavelengths, a band's, are read (one that isn't a length is counted with them).
        .filter(|n| n.attribute("sample").is_none() || n.attribute("name").is_none_or(|name| !is_wavelength_item(name)))
        .count();
    if unread > 0 {
        pam.lost.push(format!("{unread} band item(s) not read: left out"));
    }
    for item in root.children().filter(|n| n.has_tag_name("Item") && n.attribute("sample").is_none() && n.attribute("role").is_none()) {
        // Other dataset items (AREA_OR_POINT, a file's own keys) aren't class names.
        let Some(value) = item.attribute("name").and_then(|n| n.strip_prefix("CLASS_")) else { continue };
        let Some((name, long)) = item_text(item) else { continue };
        // Digits only: `parse` would also take a sign (`CLASS_+5`).
        let digits = !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit());
        let Some(value) = value.parse::<usize>().ok().filter(|v| digits && *v < MAX_CATEGORIES) else {
            odd_classes += 1;
            continue;
        };
        if classes.len() <= value {
            classes.resize(value + 1, String::new());
        }
        match classes.get_mut(value).filter(|s| s.is_empty()) {
            Some(slot) => {
                *slot = name;
                cut += usize::from(long);
            }
            None => twice += 1,
        }
    }
    if odd_bands > 0 {
        pam.lost.push(format!("{odd_bands} band description(s) for no band from 1 to {MAX_BANDS} left out"));
    }
    if odd_classes > 0 {
        pam.lost.push(format!("{odd_classes} CLASS_ item(s) whose value isn't a whole number below {MAX_CATEGORIES} left out"));
    }
    if odd_wavelengths > 0 {
        pam.lost.push(format!(
            "{odd_wavelengths} wavelength item(s) that aren't a length from 1 nm to 1 m in nanometres or micrometres, or for no band from 1 to {MAX_BANDS}, left out"
        ));
    }
    if other_wavelengths > 0 {
        pam.lost.push(format!("{other_wavelengths} other wavelength item(s) for a band that has one left out (the first is kept)"));
    }
    if units_again > 0 {
        pam.lost.push(format!("{units_again} other wavelength unit item(s) for a band or the dataset left out (the first is kept)"));
    }
    if twice > 0 {
        pam.lost.push(format!("{twice} band or class name(s) given again left out (the first is kept)"));
    }
    if cut > 0 {
        pam.lost.push(crate::loss::Note::loss(
            crate::loss::LossCode::ChangedCut,
            format!("{cut} band or class name(s) cut to {MAX_NAME} characters"),
        ));
    }
    if !classes.is_empty() {
        match pam.bands.iter_mut().find(|b| b.band == 1) {
            Some(b) => b.categories = classes,
            None => pam.bands.push(PamBand { band: 1, categories: classes, ..PamBand::default() }),
        }
    }
    (!pam.bands.is_empty() || !pam.lost.is_empty()).then_some(pam)
}

/// Reads the bands' wavelengths of GDAL's metadata tag into `pam`: a band's (`sample`) first item
/// of [`WAVELENGTH_ITEMS`], in that order, in the unit of its band's `wavelength_units` item, else
/// the dataset's. A band's other wavelength items giving the same length are other names for it,
/// and aren't counted. Returns how many items were left out: not a length or for no band, and
/// another length for a band that has one, and units given again otherwise for a band or the
/// dataset (the first is read). A dataset's own item (no `sample`) is for no band.
fn read_wavelengths(root: roxmltree::Node<'_, '_>, pam: &mut Pam) -> (usize, usize, usize) {
    let named = |n: &roxmltree::Node<'_, '_>, name: &str| {
        n.has_tag_name("Item") && n.attribute("name").is_some_and(|a| a.eq_ignore_ascii_case(name))
    };
    let band_of = |n: &roxmltree::Node<'_, '_>| {
        n.attribute("sample").and_then(|v| v.trim().parse::<usize>().ok()).and_then(|s| s.checked_add(1)).filter(|n| *n <= MAX_BANDS)
    };
    // The units given, by band (0: the dataset's); units for no band are counted, and so are
    // other units given again.
    let (mut odd, mut other, mut units_again) = (0usize, 0usize, 0usize);
    let mut units: std::collections::HashMap<usize, &str> = std::collections::HashMap::new();
    for item in root.children().filter(|n| named(n, WAVELENGTH_UNITS)) {
        let at = if item.attribute("sample").is_none() { Some(0) } else { band_of(&item) };
        let Some(at) = at else {
            odd += 1;
            continue;
        };
        // An empty unit names none: the next one is read, as in the sidecar.
        if let Some(t) = item.text().filter(|t| !t.trim().is_empty()) {
            match units.entry(at) {
                std::collections::hash_map::Entry::Vacant(v) => {
                    v.insert(t);
                }
                std::collections::hash_map::Entry::Occupied(o) => {
                    units_again += usize::from(unit_of(o.get()) != unit_of(t));
                }
            }
        }
    }
    // Where each band already described is in `pam.bands`.
    let mut at: std::collections::HashMap<usize, usize> = pam.bands.iter().enumerate().map(|(i, b)| (b.band, i)).collect();
    for name in WAVELENGTH_ITEMS {
        // A dataset item (no `sample`) is for no band: GeoLibre doesn't read it either; counted.
        for item in root.children().filter(|n| named(n, name)) {
            let Some(band) = band_of(&item) else {
                odd += 1;
                continue;
            };
            let unit = units.get(&band).or_else(|| units.get(&0)).copied();
            let Some(nm) = wavelength_nm(name, item.text().unwrap_or_default(), unit) else {
                odd += 1;
                continue;
            };
            match at.get(&band).and_then(|i| pam.bands.get_mut(*i)) {
                Some(b) => other += usize::from(keep_first_wavelength(&mut b.wavelength, nm)),
                None => {
                    at.insert(band, pam.bands.len());
                    pam.bands.push(PamBand { band, wavelength: Some(nm), ..PamBand::default() });
                }
            }
        }
    }
    (odd, other, units_again)
}

/// Whether XML can't hold `c` (a control character other than a tab or a line break, or a
/// noncharacter): [`gdal_metadata`] writes it as a space.
pub fn xml_cannot_hold(c: char) -> bool {
    (c < ' ' && !matches!(c, '\t' | '\n' | '\r')) || matches!(c, '\u{FFFE}' | '\u{FFFF}')
}

/// GDAL's metadata document for the bands a GeoTIFF holds, as [`parse_gdal_metadata`] reads
/// it back: each named band's description (`sample` from 0, in the order given), and the first
/// band's classes as dataset items `CLASS_<value>`. `None` when there is nothing to say. Names
/// are written whole, in plain ASCII (other characters as XML references), as the tag's type is.
/// A character XML can't hold (a control character other than a tab or a line break) is written
/// as a space: names read from GDAL's metadata never hold one, since XML can't.
///
/// Each band's wavelength in `wavelengths` (nanometres, in the order of `descriptions`) is written
/// as GDAL's `CENTRAL_WAVELENGTH_UM`, in micrometres, in its `IMAGERY` domain.
pub fn gdal_metadata(descriptions: &[Option<&str>], classes: &[String], wavelengths: &[Option<f64>]) -> Option<String> {
    let mut items = String::new();
    let mut put = |open: String, name: &str| {
        items.push_str(&open);
        for c in name.chars() {
            match c {
                '&' => items.push_str("&amp;"),
                '<' => items.push_str("&lt;"),
                '>' => items.push_str("&gt;"),
                c if c.is_ascii() && !c.is_ascii_control() => items.push(c),
                c if xml_cannot_hold(c) => items.push(' '),
                c => items.push_str(&format!("&#{};", u32::from(c))),
            }
        }
        items.push_str("</Item>");
    };
    for (sample, d) in descriptions.iter().enumerate() {
        if let Some(d) = d.map(str::trim).filter(|d| !d.is_empty()) {
            put(format!("<Item name=\"DESCRIPTION\" sample=\"{sample}\" role=\"description\">"), d);
        }
    }
    for (value, name) in classes.iter().enumerate() {
        if !name.trim().is_empty() {
            put(format!("<Item name=\"CLASS_{value}\">"), name.trim());
        }
    }
    for (sample, nm) in wavelengths.iter().enumerate() {
        if let Some(nm) = nm.filter(|v| v.is_finite() && *v > 0.0) {
            items
                .push_str(&format!("<Item name=\"{CENTRAL_WAVELENGTH_UM}\" sample=\"{sample}\" domain=\"IMAGERY\">{}</Item>", nm / 1000.0));
        }
    }
    (!items.is_empty()).then(|| format!("<GDALMetadata>{items}</GDALMetadata>"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn band_and_class_names_written_are_read_back() {
        let classes = vec![String::new(), "Floresta & savana".into(), String::new(), "Formação <campestre>".into()];
        let xml = gdal_metadata(&[Some("classe_mapbiomas"), None], &classes, &[]).unwrap();
        assert!(xml.is_ascii(), "{xml}");
        let pam = parse_gdal_metadata(&xml).unwrap();
        assert_eq!(pam.band(1).unwrap().categories, classes);
        assert_eq!(pam.band(1).unwrap().description.as_deref(), Some("classe_mapbiomas"));
        assert!(pam.band(2).is_none() && pam.lost.is_empty(), "{pam:?}");
        // Nothing named, no document.
        assert_eq!(gdal_metadata(&[None, Some(" ")], &[String::new(), " ".into()], &[None, Some(f64::NAN)]), None);
        // A character XML can't hold is written as a space, so the document still reads.
        let xml = gdal_metadata(&[Some("a\u{1}b")], &[], &[]).unwrap();
        assert_eq!(parse_gdal_metadata(&xml).unwrap().band(1).unwrap().description.as_deref(), Some("a b"));
    }

    #[test]
    fn what_gdal_metadata_leaves_out_is_said() {
        // A clean document says nothing.
        let clean =
            parse_gdal_metadata(r#"<GDALMetadata><Item name="CLASS_1">Forest</Item><Item name="AREA_OR_POINT">Area</Item></GDALMetadata>"#);
        assert!(clean.unwrap().lost.is_empty());
        let long = "x".repeat(300);
        // The long name on an item left out is counted as left out, not as cut too.
        let xml = format!(
            r#"<GDALMetadata><Item name="CLASS_1">Forest</Item><Item name="CLASS_1">Woods</Item><Item name="CLASS_70000">Far</Item><Item name="CLASS_x">Odd</Item><Item name="CLASS_2">{long}</Item><Item name="DESCRIPTION" sample="x" role="description">{long}</Item></GDALMetadata>"#
        );
        let pam = parse_gdal_metadata(&xml).unwrap();
        assert_eq!(pam.band(1).unwrap().categories[1], "Forest");
        assert_eq!(pam.band(1).unwrap().categories[2].chars().count(), 256);
        let said = pam.lost.texts().collect::<Vec<_>>().join("; ");
        for want in
            ["1 band description(s) for no band", "2 CLASS_ item(s)", "1 band or class name(s) given again", "1 band or class name(s) cut"]
        {
            assert!(said.contains(want), "{want}: {said}");
        }
        // Only what is lost: a document of nothing but bad items still says so.
        assert!(!parse_gdal_metadata(r#"<GDALMetadata><Item name="CLASS_99999">Far</Item></GDALMetadata>"#).unwrap().lost.is_empty());
        // A band's units aren't read, and are said; its statistics are worked out again, and aren't.
        let units =
            parse_gdal_metadata(r#"<GDALMetadata><Item name="UNITTYPE" sample="0" role="unittype">m</Item></GDALMetadata>"#).unwrap();
        assert_eq!(units.lost, ["1 band item(s) not read: left out"]);
        assert!(parse_gdal_metadata(r#"<GDALMetadata><Item name="STATISTICS_MEAN" sample="0">4.5</Item></GDALMetadata>"#).is_none());
        // A signed value isn't a class's.
        let signed = parse_gdal_metadata(r#"<GDALMetadata><Item name="CLASS_+5">Water</Item></GDALMetadata>"#).unwrap();
        assert!(signed.bands.is_empty() && signed.lost.len() == 1, "{signed:?}");
        // A scale or offset that changes the values isn't applied, and is said; 1 and 0 aren't.
        let scaled = parse_gdal_metadata(
            r#"<GDALMetadata><Item name="SCALE" sample="0" role="scale">0.0001</Item><Item name="OFFSET" sample="0" role="offset">-0.1</Item></GDALMetadata>"#,
        )
        .unwrap();
        assert_eq!(scaled.lost, ["2 scale or offset item(s) not applied: the values are as stored"]);
        assert!(
            parse_gdal_metadata(
                r#"<GDALMetadata><Item name="SCALE" sample="0" role="scale">1</Item><Item name="OFFSET" sample="0" role="offset">0</Item></GDALMetadata>"#
            )
            .is_none()
        );
        // A CLASS_ item for a band isn't read, and is said.
        let banded = parse_gdal_metadata(r#"<GDALMetadata><Item name="CLASS_5" sample="0">Water</Item></GDALMetadata>"#).unwrap();
        assert!(
            banded.bands.is_empty() && banded.lost.len() == 1 && banded.lost[0].starts_with("1 CLASS_ item(s) given for a band"),
            "{banded:?}"
        );
    }

    #[test]
    fn reads_band_descriptions_nodata_and_statistics() {
        let xml = r#"<PAMDataset>
  <SRS>GEOGCS["WGS 84"]</SRS>
  <PAMRasterBand band="2"><Description>Green</Description></PAMRasterBand>
  <PAMRasterBand band="1">
    <Description> Blue </Description>
    <NoDataValue>-9999</NoDataValue>
    <Metadata domain="IMAGE_STRUCTURE"><MDI key="STATISTICS_MINIMUM">999</MDI></Metadata>
    <Metadata>
      <MDI key="STATISTICS_MINIMUM">1</MDI>
      <MDI key="STATISTICS_MAXIMUM">250.5</MDI>
      <MDI key="STATISTICS_MEAN">80</MDI>
      <MDI key="STATISTICS_STDDEV">nan</MDI>
    </Metadata>
  </PAMRasterBand>
  <PAMRasterBand band="0"><Description>none</Description></PAMRasterBand>
  <PAMRasterBand band="3"><NoDataValue>nan</NoDataValue><Description></Description></PAMRasterBand>
</PAMDataset>"#;
        let pam = parse(xml).unwrap();
        assert_eq!(pam.bands.len(), 3, "band 0 isn't a band");
        let b1 = pam.band(1).unwrap();
        assert_eq!(b1.description.as_deref(), Some("Blue"));
        assert_eq!(b1.nodata, Some(-9999.0));
        assert_eq!(b1.stats, Some(Stats { min: 1.0, max: 250.5, mean: Some(80.0), std_dev: None }));
        assert_eq!(pam.band(2).unwrap().description.as_deref(), Some("Green"));
        assert_eq!((pam.band(3).unwrap().description.as_ref(), pam.band(3).unwrap().nodata), (None, None));
        assert_eq!(pam.nodata(), Some(-9999.0));
        let mut differ = pam.clone();
        differ.bands.push(PamBand { band: 4, nodata: Some(0.0), ..Default::default() });
        assert_eq!(differ.nodata(), None, "bands disagree");
    }

    #[test]
    fn reads_class_names() {
        // As GDAL writes them (SetCategoryNames): one per value from 0, empty for a value with no
        // name.
        let xml = r#"<PAMDataset>
  <PAMRasterBand band="1">
    <CategoryNames>
      <Category>no data</Category>
      <Category> 0-5 % flat </Category>
      <Category></Category>
      <Category>over 100 % (over 45°)</Category>
      <Category/>
    </CategoryNames>
  </PAMRasterBand>
  <PAMRasterBand band="2"><CategoryNames><Category></Category></CategoryNames></PAMRasterBand>
  <PAMRasterBand band="3"><Description>none</Description></PAMRasterBand>
</PAMDataset>"#;
        let pam = parse(xml).unwrap();
        assert_eq!(pam.band(1).unwrap().categories, ["no data", "0-5 % flat", "", "over 100 % (over 45°)"]);
        assert!(pam.band(2).unwrap().categories.is_empty(), "no value is named");
        assert!(pam.band(3).unwrap().categories.is_empty());
        let long = format!(
            "<PAMDataset><PAMRasterBand band=\"1\"><CategoryNames><Category>{}</Category></CategoryNames></PAMRasterBand></PAMDataset>",
            "x".repeat(1000)
        );
        assert_eq!(parse(&long).unwrap().band(1).unwrap().categories[0].len(), 256);
        // What is cut or left out is said; a sidecar read whole says nothing.
        assert_eq!(
            parse(&long).unwrap().lost.iter().map(|n| (n.text.as_str(), n.loss)).collect::<Vec<_>>(),
            [("1 band or class name(s) cut to 256 characters", Some(crate::loss::LossCode::ChangedCut))]
        );
        assert!(pam.lost.is_empty(), "{:?}", pam.lost);
        let many = format!(
            r#"<PAMDataset><PAMRasterBand band="1"><CategoryNames>{}</CategoryNames></PAMRasterBand></PAMDataset>"#,
            (0..MAX_CATEGORIES + 2).map(|v| format!("<Category>c{v}</Category>")).collect::<String>()
        );
        let many = parse(&many).unwrap();
        let kept = &many.band(1).unwrap().categories;
        assert_eq!((kept.len(), kept[0].as_str()), (MAX_CATEGORIES, "c0"));
        assert_eq!(kept[MAX_CATEGORIES - 1], format!("c{}", MAX_CATEGORIES - 1));
        assert_eq!(
            many.lost.iter().map(|n| (n.text.clone(), n.loss)).collect::<Vec<_>>(),
            [(
                format!("2 class name(s) past the first {MAX_CATEGORIES} of a band left out"),
                Some(crate::loss::LossCode::DroppedOverLimit)
            )]
        );
        // A record for no band, a scale, an offset and a unit are said; a scale of 1 and an offset
        // of 0 aren't.
        let odd = parse(
            r#"<PAMDataset><PAMRasterBand band="0"><Description>x</Description></PAMRasterBand>
            <PAMRasterBand band="1"><Scale>0.01</Scale><Offset>-5</Offset><UnitType>m</UnitType></PAMRasterBand>
            <PAMRasterBand band="2"><Scale>1</Scale><Offset>0</Offset><UnitType></UnitType>
            <Metadata domain="x"><MDI key="SOURCE">y</MDI><MDI key="STATISTICS_MEAN">1</MDI></Metadata>
            <ColorTable><Entry c1="0" c2="0" c3="0" c4="255"/></ColorTable><ColorTable/></PAMRasterBand>
            <PAMRasterBand band="3"><ColorTable/><NoDataValue>-9.999,0</NoDataValue></PAMRasterBand>
            <PAMRasterBand band="4"><NoDataValue>nan</NoDataValue></PAMRasterBand>
            <PAMRasterBand band="1"><Description>again</Description></PAMRasterBand></PAMDataset>"#,
        )
        .unwrap();
        // A band given again keeps its first record.
        assert!(odd.bands.len() == 4 && odd.band(1).unwrap().description.is_none(), "{odd:?}");
        assert_eq!((odd.band(3).unwrap().nodata, odd.band(4).unwrap().nodata), (None, None));
        assert_eq!(
            odd.lost,
            [
                format!("1 band record(s) for no band from 1 to {MAX_BANDS} left out"),
                "1 band record(s) given again left out (the first is kept)".to_string(),
                "2 scale or offset(s) not applied: the values are as stored".to_string(),
                "1 band unit(s) not read: left out".to_string(),
                "1 band metadata item(s) not read: left out".to_string(),
                "1 band colour table(s) not read: left out (only the file's own is drawn)".to_string(),
                "1 no-data value(s) that aren't numbers left out: those pixels are read as values".to_string(),
            ]
        );
    }

    /// A band's raster attribute table, as GDAL writes it, from its attributes, fields and rows.
    fn table(attrs: &str, fields: &[(&str, u32, u32)], rows: &[&[&str]]) -> String {
        let fields: String = fields
            .iter()
            .enumerate()
            .map(|(i, (n, t, u))| format!(r#"<FieldDefn index="{i}"><Name>{n}</Name><Type>{t}</Type><Usage>{u}</Usage></FieldDefn>"#))
            .collect();
        let rows: String = rows
            .iter()
            .enumerate()
            .map(|(i, r)| format!(r#"<Row index="{i}">{}</Row>"#, r.iter().map(|f| format!("<F>{f}</F>")).collect::<String>()))
            .collect();
        format!(
            r#"<PAMDataset><PAMRasterBand band="1"><GDALRasterAttributeTable {attrs}>{fields}{rows}</GDALRasterAttributeTable></PAMRasterBand></PAMDataset>"#
        )
    }

    #[test]
    fn a_raster_attribute_table_names_classes() {
        let xml = r#"<PAMDataset><PAMRasterBand band="1">
  <GDALRasterAttributeTable Row0Min="0" BinSize="1" tableType="thematic">
    <FieldDefn index="0"><Name>Value</Name><Type>0</Type><Usage>5</Usage></FieldDefn>
    <FieldDefn index="1"><Name>Count</Name><Type>0</Type><Usage>1</Usage></FieldDefn>
    <FieldDefn index="2"><Name>Class_Name</Name><Type>2</Type><Usage>2</Usage></FieldDefn>
    <Row index="0"><F>1</F><F>100</F><F>Water</F></Row>
    <Row index="1"><F>3</F><F>250</F><F> Forest </F></Row>
  </GDALRasterAttributeTable></PAMRasterBand></PAMDataset>"#;
        let pam = parse(xml).unwrap();
        let b = pam.band(1).unwrap();
        assert_eq!(b.categories, ["", "Water", "", "Forest"]);
        assert!(b.from_table && pam.lost.is_empty(), "{:?}", pam.lost);
        // The name column is found by its usage, however it is called, else by a name.
        let odd = parse(&table("", &[("Value", 0, 5), ("Desc", 2, 2)], &[&["2", "Urban"]])).unwrap();
        assert_eq!(odd.band(1).unwrap().categories, ["", "", "Urban"]);
        let named = parse(&table("", &[("VALUE", 0, 0), ("CLASS_NAME", 2, 0)], &[&["1", "Crops"]])).unwrap();
        assert_eq!(named.band(1).unwrap().categories, ["", "Crops"]);
        // A table with no name column names nothing and loses nothing.
        let none = parse(&table("", &[("Value", 0, 5), ("Count", 0, 1), ("Red", 0, 6)], &[&["1", "5", "9"]])).unwrap();
        assert!(none.band(1).unwrap().categories.is_empty() && none.lost.is_empty(), "{none:?}");
        let number = parse(&table("", &[("Name", 0, 0)], &[&["1"]])).unwrap();
        assert!(number.band(1).unwrap().categories.is_empty() && number.lost.is_empty(), "{number:?}");
    }

    #[test]
    fn a_table_without_a_value_column_numbers_its_rows() {
        let fields = [("Class_Name", 2, 2), ("Count", 0, 1)];
        let rows: [&[&str]; 3] = [&["zero", "1"], &["", "2"], &["two", "3"]];
        let plain = parse(&table("", &fields, &rows)).unwrap();
        assert_eq!(plain.band(1).unwrap().categories, ["zero", "", "two"]);
        let binned = parse(&table(r#"Row0Min="10" BinSize="1""#, &fields, &rows)).unwrap();
        let c = &binned.band(1).unwrap().categories;
        assert_eq!((c.len(), c[10].as_str(), c[12].as_str()), (13, "zero", "two"));
        assert!(binned.lost.is_empty(), "{:?}", binned.lost);
    }

    #[test]
    fn a_table_that_cannot_be_read_as_names_is_said() {
        let bins_msg = "1 attribute table(s) with a bin or a field type or usage that isn't a number not read as class names";
        let text_msg = "1 attribute table(s) with text fields but no name column not read as class names";
        // A bin that isn't a number would number the rows without a notice.
        let bin = parse(&table(r#"Row0Min="x" BinSize="1""#, &[("Name", 2, 2)], &[&["zero"]])).unwrap();
        assert!(bin.band(1).unwrap().categories.is_empty());
        assert_eq!(bin.lost, [bins_msg]);
        let wide = parse(&table(r#"BinSize="wide""#, &[("Name", 2, 2)], &[&["zero"]])).unwrap();
        assert_eq!(wide.lost, [bins_msg]);
        // A usage that isn't a number may be the name column's.
        let usage = r#"<PAMDataset><PAMRasterBand band="1"><GDALRasterAttributeTable><FieldDefn index="0"><Name>Value</Name><Type>0</Type><Usage>5</Usage></FieldDefn><FieldDefn index="1"><Name>Desc</Name><Type>2</Type><Usage>two</Usage></FieldDefn><Row index="0"><F>1</F><F>one</F></Row></GDALRasterAttributeTable></PAMRasterBand></PAMDataset>"#;
        let usage = parse(usage).unwrap();
        assert!(usage.band(1).unwrap().categories.is_empty());
        assert_eq!(usage.lost, [bins_msg]);
        // A text field that no usage or name marks as names is said; a table with none isn't.
        let text = parse(&table("", &[("Value", 0, 5), ("Desc", 2, 0)], &[&["2", "Urban"]])).unwrap();
        assert!(text.band(1).unwrap().categories.is_empty());
        assert_eq!(text.lost, [text_msg]);
        let plain = parse(&table("", &[("Value", 0, 5), ("Count", 0, 1)], &[&["2", "7"]])).unwrap();
        assert!(plain.lost.is_empty(), "{:?}", plain.lost);
    }

    #[test]
    fn category_names_win_over_a_table() {
        let xml = r#"<PAMDataset><PAMRasterBand band="1">
<CategoryNames><Category>a</Category><Category>b</Category></CategoryNames>
<GDALRasterAttributeTable><FieldDefn index="0"><Name>Name</Name><Type>2</Type><Usage>2</Usage></FieldDefn>
<Row index="0"><F>x</F></Row><Row index="1"><F>y</F></Row><Row index="2"><F>z</F></Row></GDALRasterAttributeTable>
</PAMRasterBand></PAMDataset>"#;
        let pam = parse(xml).unwrap();
        assert_eq!(pam.band(1).unwrap().categories, ["a", "b"]);
        assert!(!pam.band(1).unwrap().from_table);
        assert_eq!(pam.lost, ["1 attribute table(s) of class names not read: the band's category names are used"]);
        // A table that names nothing isn't counted.
        let counts = xml.replace("<Type>2</Type><Usage>2</Usage>", "<Type>0</Type><Usage>1</Usage>");
        assert!(parse(&counts).unwrap().lost.is_empty());
    }

    #[test]
    fn a_tables_rows_keep_their_index_and_unit_bins_name_the_whole_number_in_them() {
        // Rows given out of order keep the values of their `index`.
        let xml = r#"<PAMDataset><PAMRasterBand band="1"><GDALRasterAttributeTable>
<FieldDefn index="0"><Name>Class_Name</Name><Type>2</Type><Usage>2</Usage></FieldDefn>
<Row index="1"><F>Forest</F></Row><Row index="0"><F>Water</F></Row></GDALRasterAttributeTable></PAMRasterBand></PAMDataset>"#;
        assert_eq!(parse(xml).unwrap().band(1).unwrap().categories, ["Water", "Forest"]);
        // A bin from -0.5 to 0.5 holds 0.
        let half = parse(&table(r#"Row0Min="-0.5" BinSize="1""#, &[("Name", 2, 2)], &[&["zero"], &["one"]])).unwrap();
        assert_eq!(half.band(1).unwrap().categories, ["zero", "one"]);
        assert!(half.lost.is_empty(), "{:?}", half.lost);
        // A value field wins over unit bins; wider bins are ranges even beside one.
        let valued = parse(&table(r#"Row0Min="0" BinSize="1""#, &[("Value", 0, 5), ("Name", 2, 2)], &[&["2", "two"]])).unwrap();
        assert_eq!(valued.band(1).unwrap().categories, ["", "", "two"]);
        let wide = parse(&table(r#"Row0Min="0" BinSize="10""#, &[("Value", 0, 5), ("Name", 2, 2)], &[&["2", "two"]])).unwrap();
        assert!(wide.band(1).unwrap().categories.is_empty());
        assert_eq!(wide.lost, ["1 attribute table(s) of value ranges not read as class names"]);
    }

    #[test]
    fn tables_hold_a_bounded_number_of_class_values() {
        // Each band's one row at the highest value would hold 65,536 names.
        let bands = MAX_RAT_SLOTS / MAX_CATEGORIES + 3;
        let band = |b: usize| {
            format!(
                r#"<PAMRasterBand band="{b}"><GDALRasterAttributeTable><FieldDefn index="0"><Name>Value</Name><Type>0</Type><Usage>5</Usage></FieldDefn><FieldDefn index="1"><Name>Name</Name><Type>2</Type><Usage>2</Usage></FieldDefn><Row index="0"><F>{}</F><F>top</F></Row></GDALRasterAttributeTable></PAMRasterBand>"#,
                MAX_CATEGORIES - 1
            )
        };
        let xml = format!("<PAMDataset>{}</PAMDataset>", (1..=bands).map(band).collect::<String>());
        let pam = parse(&xml).unwrap();
        let held: usize = pam.bands.iter().map(|b| b.categories.len()).sum();
        assert!(held <= MAX_RAT_SLOTS, "{held}");
        assert_eq!(
            pam.lost.iter().map(|n| (n.text.clone(), n.loss)).collect::<Vec<_>>(),
            [(
                format!("3 class name(s) of an attribute table past the sidecar's first {MAX_RAT_SLOTS} class values left out"),
                Some(crate::loss::LossCode::DroppedOverLimit)
            )]
        );
    }

    #[test]
    fn what_a_table_leaves_out_is_said() {
        let ranges = parse(&table("", &[("Min", 0, 3), ("Max", 0, 4), ("Name", 2, 2)], &[&["0", "10", "low"]])).unwrap();
        assert!(ranges.band(1).unwrap().categories.is_empty());
        assert_eq!(ranges.lost, ["1 attribute table(s) of value ranges not read as class names"]);
        // One bound alone is a range too.
        for usage in [3, 4] {
            let one = parse(&table("", &[("Bound", 0, usage), ("Name", 2, 2)], &[&["10", "Forest"]])).unwrap();
            assert!(one.band(1).unwrap().categories.is_empty(), "usage {usage}");
            assert_eq!(one.lost, ["1 attribute table(s) of value ranges not read as class names"], "usage {usage}");
        }
        let wide = parse(&table(r#"Row0Min="0" BinSize="10""#, &[("Name", 2, 2)], &[&["low"]])).unwrap();
        assert_eq!(wide.lost, ["1 attribute table(s) of value ranges not read as class names"]);
        let odd = parse(&table(
            "",
            &[("Value", 0, 5), ("Name", 2, 2)],
            &[
                &["-1", "neg"],
                &["1.5", "frac"],
                &["65536", "big"],
                &["x", "text"],
                &["", "blank"],
                &["65535", "last"],
                &["4", "four"],
                &["4", "again"],
            ],
        ))
        .unwrap();
        let c = &odd.band(1).unwrap().categories;
        assert_eq!((c.len(), c[4].as_str(), c[65_535].as_str()), (65_536, "four", "last"));
        assert_eq!(
            odd.lost,
            [
                format!(
                    "5 class name(s) of an attribute table for a value that isn't a whole number from 0 to {} left out",
                    MAX_CATEGORIES - 1
                ),
                "1 class name(s) of an attribute table given again for a value left out (the first is kept)".to_string(),
            ]
        );
        let long = parse(&table("", &[("Name", 2, 2)], &[&[&"x".repeat(300)]])).unwrap();
        assert_eq!((long.band(1).unwrap().categories[0].len(), long.lost.len()), (256, 1));
        let many = vec![&["n"][..]; MAX_RAT_ROWS + 3];
        let many = parse(&table("", &[("Name", 2, 2)], &many)).unwrap();
        assert!(
            many.lost.iter().any(|l| l.text == format!("3 attribute table row(s) past the first {MAX_RAT_ROWS} left out")
                && l.loss == Some(crate::loss::LossCode::DroppedOverLimit)),
            "{:?}",
            many.lost
        );
    }

    #[test]
    fn junk_is_not_a_sidecar() {
        // Each names nothing, and says why.
        for (text, said) in [
            ("", "doesn't read as XML"),
            ("not xml", "doesn't read as XML"),
            ("<GDALMetadata/>", "it is a <GDALMetadata> document, not GDAL's <PAMDataset>"),
            ("<PAMDataset><PAMRasterBand band=\"x\"/>", "doesn't read as XML"),
            ("<!DOCTYPE x><PAMDataset/>", "doesn't read as XML"),
        ] {
            let pam = parse(text).unwrap();
            assert!(pam.bands.is_empty() && pam.lost.len() == 1 && pam.lost[0].contains(said), "{text}: {pam:?}");
            assert!(pam.lost[0].ends_with("its statistics, no-data, band names and wavelengths are left out"), "{text}: {pam:?}");
        }
        let deep = format!("<PAMDataset>{}{}</PAMDataset>", "<a>".repeat(100_000), "</a>".repeat(100_000));
        let deep = parse(&deep).unwrap();
        assert!(deep.bands.is_empty() && deep.lost.len() == 1, "{deep:?}");
        // A band record with no number is a sidecar, which says the record is left out.
        assert_eq!(parse("<PAMDataset><PAMRasterBand band=\"x\"/></PAMDataset>").unwrap().lost.len(), 1);
    }

    #[test]
    fn a_sidecar_that_does_not_read_says_why() {
        let dir = std::env::temp_dir().join(format!("rustgis-pam-junk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tif = dir.join("x.tif");
        assert_eq!(read(&tif), None, "no sidecar, nothing said");
        // Not text: said.
        std::fs::write(dir.join("x.tif.aux.xml"), [0xff, 0xfe, 0x00]).unwrap();
        let junk = read(&tif).unwrap();
        assert!(junk.bands.is_empty() && junk.lost[0].starts_with("it isn't UTF-8 text"), "{junk:?}");
        // The other name, when it parses, is read instead, and says the first was left out.
        std::fs::write(
            dir.join("x.aux.xml"),
            r#"<PAMDataset><PAMRasterBand band="1"><Description>NIR</Description></PAMRasterBand></PAMDataset>"#,
        )
        .unwrap();
        let other = read(&tif).unwrap();
        assert_eq!(other.band(1).unwrap().description.as_deref(), Some("NIR"), "{other:?}");
        assert!(
            other.lost.len() == 1
                && other.lost[0].starts_with("x.tif.aux.xml: it isn't UTF-8 text")
                && other.lost[0].ends_with(": left out, x.aux.xml read instead"),
            "{other:?}"
        );
        // Neither reads: both are named, each with its reason.
        std::fs::write(dir.join("x.aux.xml"), "not xml").unwrap();
        let both = read(&tif).unwrap();
        assert!(
            both.bands.is_empty()
                && both.lost.len() == 1
                && both.lost[0].starts_with("x.tif.aux.xml: it isn't UTF-8 text")
                && both.lost[0].contains("; x.aux.xml: ")
                && both.lost[0].ends_with(": its statistics, no-data, band names and wavelengths are left out"),
            "{both:?}"
        );
        // A sidecar that parses first says nothing of the other.
        std::fs::write(dir.join("x.tif.aux.xml"), "<PAMDataset/>").unwrap();
        assert!(read(&tif).unwrap().lost.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reads_class_names_from_gdal_metadata() {
        let xml = r#"<GDALMetadata>
  <Item name="AREA_OR_POINT">Area</Item>
  <Item name="CLASS_3">Forest Formation</Item>
  <Item name="CLASS_15"> Pasture </Item>
  <Item name="CLASS_15">again</Item>
  <Item name="CLASS_x">not a value</Item>
  <Item name="CLASS_70000">past the last value</Item>
  <Item name="CLASS_4"></Item>
  <Item name="CLASS_5" sample="0">a band's own item</Item>
  <Item name="DESCRIPTION" sample="0" role="description">mapbiomas_class</Item>
</GDALMetadata>"#;
        let pam = parse_gdal_metadata(xml).unwrap();
        let b1 = pam.band(1).unwrap();
        assert_eq!(b1.description.as_deref(), Some("mapbiomas_class"));
        assert_eq!(b1.categories.len(), 16);
        assert_eq!((b1.categories[3].as_str(), b1.categories[15].as_str()), ("Forest Formation", "Pasture"));
        assert!(b1.categories[4].is_empty() && b1.categories[5].is_empty() && b1.categories[0].is_empty());
        // Classes alone describe band 1.
        let only = parse_gdal_metadata(r#"<GDALMetadata><Item name="CLASS_1">Forest</Item></GDALMetadata>"#).unwrap();
        assert_eq!(only.band(1).map(|b| b.categories.clone()), Some(vec![String::new(), "Forest".into()]));
        assert!(parse_gdal_metadata(r#"<GDALMetadata><Item name="AREA_OR_POINT">Area</Item></GDALMetadata>"#).is_none());
    }

    #[test]
    fn reads_band_descriptions_from_gdal_metadata() {
        let xml = r#"<GDALMetadata>
  <Item name="AREA_OR_POINT">Area</Item>
  <Item name="DESCRIPTION" sample="1" role="description"> NDVI 2024-07-01 </Item>
  <Item name="DESCRIPTION" sample="0" role="description">NDVI 2024-06-01</Item>
  <Item name="DESCRIPTION" sample="0" role="description">again</Item>
  <Item name="SCALE" sample="0" role="scale">0.0001</Item>
  <Item name="DESCRIPTION" sample="x" role="description">no band</Item>
  <Item name="DESCRIPTION" sample="18446744073709551615" role="description">past the last band</Item>
  <Item name="DESCRIPTION" sample="65536" role="description">past the last band</Item>
  <Item name="DESCRIPTION" sample="2" role="description"></Item>
</GDALMetadata>"#;
        let pam = parse_gdal_metadata(xml).unwrap();
        assert_eq!(pam.bands.len(), 2, "{pam:?}");
        assert_eq!(pam.band(1).unwrap().description.as_deref(), Some("NDVI 2024-06-01"));
        assert_eq!(pam.band(2).unwrap().description.as_deref(), Some("NDVI 2024-07-01"));
        assert_eq!(pam.nodata(), None);
        // A document naming nothing says nothing; one that doesn't read, or isn't GDAL's, says so.
        for text in ["", " ", "<GDALMetadata/>", "<GDALMetadata><Item name=\"A\">1</Item></GDALMetadata>"] {
            assert!(parse_gdal_metadata(text).is_none(), "{text}");
        }
        for (text, said) in [
            ("not xml", "doesn't read as XML"),
            ("<GDALMetadata><Item>", "doesn't read as XML"),
            ("<PAMDataset/>", "a <PAMDataset> document"),
        ] {
            let pam = parse_gdal_metadata(text).unwrap();
            assert!(pam.bands.is_empty() && pam.lost.len() == 1 && pam.lost[0].contains(said), "{text}: {pam:?}");
        }
    }

    /// Wavelengths in GDAL's metadata tag: GDAL's own in micrometres first, then `wavelength` and
    /// `central_wavelength` in the unit given (the band's, else the dataset's) or, with none, by
    /// their size; those that aren't a length, or are for no band, are said. They aren't counted
    /// as band items not read.
    #[test]
    fn reads_wavelengths_from_gdal_metadata() {
        let xml = r#"<GDALMetadata>
  <Item name="CENTRAL_WAVELENGTH_UM" sample="0" domain="IMAGERY">0.4825</Item>
  <Item name="wavelength" sample="0">999</Item>
  <Item name="wavelength" sample="1">0.5613</Item>
  <Item name="wavelength" sample="2"> 654.6 </Item>
  <Item name="WAVELENGTH" sample="3">865</Item>
  <Item name="wavelength_units" sample="3">Nanometers</Item>
  <Item name="central_wavelength" sample="4">1.6</Item>
  <Item name="wavelength" sample="5">far</Item>
  <Item name="wavelength" sample="6">2.2</Item>
  <Item name="wavelength_units" sample="6">furlongs</Item>
  <Item name="wavelength" sample="70000">1</Item>
  <Item name="wavelength_units" sample="70000">nm</Item>
  <Item name="wavelength">500</Item>
</GDALMetadata>"#;
        let pam = parse_gdal_metadata(xml).unwrap();
        let w = |b: usize| pam.band(b).and_then(|b| b.wavelength);
        assert_eq!(
            [w(1), w(2), w(3), w(4), w(5), w(6), w(7)],
            [Some(482.5), Some(561.3), Some(654.6), Some(865.0), Some(1600.0), None, None]
        );
        assert_eq!(pam.lost.len(), 2, "{:?}", pam.lost);
        assert!(pam.lost[0].starts_with("5 wavelength item(s)"), "{:?}", pam.lost);
        assert_eq!(pam.lost[1], "1 other wavelength item(s) for a band that has one left out (the first is kept)");
        // The same length under two items is one wavelength: nothing is said.
        let xml = r#"<GDALMetadata><Item name="CENTRAL_WAVELENGTH_UM" sample="0">0.665</Item><Item name="wavelength" sample="0">665</Item></GDALMetadata>"#;
        let pam = parse_gdal_metadata(xml).unwrap();
        assert_eq!(pam.band(1).and_then(|b| b.wavelength), Some(665.0));
        assert!(pam.lost.is_empty(), "{:?}", pam.lost);
        // A band's unit given again otherwise: the first is read, and the other said; the same
        // one again is nothing.
        let xml = r#"<GDALMetadata><Item name="wavelength_units" sample="0">nm</Item><Item name="wavelength_units" sample="0">um</Item><Item name="wavelength_units" sample="0"> NM </Item><Item name="wavelength" sample="0">1.6</Item></GDALMetadata>"#;
        let pam = parse_gdal_metadata(xml).unwrap();
        assert_eq!(pam.band(1).and_then(|b| b.wavelength), Some(1.6));
        assert_eq!(pam.lost, ["1 other wavelength unit item(s) for a band or the dataset left out (the first is kept)"]);
        // One unit in two spellings is one unit: nothing is said.
        let xml = r#"<GDALMetadata><Item name="wavelength_units" sample="0">nm</Item><Item name="wavelength_units" sample="0">nanometers</Item><Item name="wavelength_units">um</Item><Item name="wavelength_units">µm</Item><Item name="wavelength" sample="0">1.6</Item></GDALMetadata>"#;
        let pam = parse_gdal_metadata(xml).unwrap();
        assert_eq!(pam.band(1).and_then(|b| b.wavelength), Some(1.6));
        assert!(pam.lost.is_empty(), "{:?}", pam.lost);
        // A tag that doesn't read loses its wavelengths too, and says so.
        let broken = parse_gdal_metadata(r#"<GDALMetadata><Item name="wavelength" sample="0">665</Item>"#).unwrap();
        assert_eq!(broken.band(1).and_then(|b| b.wavelength), None);
        assert!(broken.lost.iter().any(|l| l.ends_with("band and class names and wavelengths are left out")), "{:?}", broken.lost);
        // A unit the dataset gives is a band's when it gives none; then a large value may be in µm.
        let xml =
            r#"<GDALMetadata><Item name="wavelength_units">micrometers</Item><Item name="wavelength" sample="0">120</Item></GDALMetadata>"#;
        let pam = parse_gdal_metadata(xml).unwrap();
        assert_eq!(pam.band(1).and_then(|b| b.wavelength), Some(120_000.0));
        assert!(pam.lost.is_empty(), "{:?}", pam.lost);
    }

    /// Wavelengths written (as GDAL's `CENTRAL_WAVELENGTH_UM`) are read back as they were.
    #[test]
    fn wavelengths_written_are_read_back() {
        let xml = gdal_metadata(&[None, None, Some("SWIR 2")], &[], &[Some(482.5), None, Some(2200.0)]).unwrap();
        assert!(xml.contains(r#"name="CENTRAL_WAVELENGTH_UM" sample="0" domain="IMAGERY">0.4825<"#), "{xml}");
        let pam = parse_gdal_metadata(&xml).unwrap();
        assert_eq!(pam.band(1).and_then(|b| b.wavelength), Some(482.5));
        assert!(pam.band(2).is_none());
        assert_eq!(pam.band(3).and_then(|b| b.wavelength), Some(2200.0));
        assert_eq!(pam.band(3).and_then(|b| b.description.as_deref()), Some("SWIR 2"));
        assert!(pam.lost.is_empty(), "{:?}", pam.lost);
    }

    /// Wavelengths whose nanometres overflow or round to zero aren't lengths: left out, and the
    /// band's next item is read.
    #[test]
    fn wavelengths_that_overflow_or_round_to_zero_are_left_out() {
        for v in ["1e300", "1e-300", "5e-324"] {
            for (item, units) in [(CENTRAL_WAVELENGTH_UM, None), ("wavelength", Some("micrometers")), ("wavelength", None)] {
                let w = wavelength_nm(item, v, units);
                assert!(w.is_none_or(|w| w.is_finite() && w > 0.0), "{item} {v} {units:?}: {w:?}");
            }
            assert_eq!(wavelength_nm(CENTRAL_WAVELENGTH_UM, v, None), None, "{v}");
        }
        // Lengths past 1 nm to 1 m are left out: written in micrometres a tiny one would round
        // to 0, and a huge one stretches the chart.
        for v in ["1e-7", "0.5", "1e300", "2e9"] {
            assert_eq!(wavelength_nm("wavelength", v, Some("nm")), None, "{v} nm");
        }
        assert_eq!(wavelength_nm("wavelength", "1", Some("nm")), Some(1.0));
        assert_eq!(wavelength_nm("wavelength", "1e9", Some("nm")), Some(1e9));
        assert_eq!(wavelength_nm("wavelength", "1e-7", Some("micrometers")), None, "0.0001 nm");
        let xml = r#"<GDALMetadata><Item name="CENTRAL_WAVELENGTH_UM" sample="0">1e300</Item><Item name="wavelength" sample="0">842</Item><Item name="wavelength_units" sample="1">nm</Item><Item name="wavelength" sample="1">1e-7</Item></GDALMetadata>"#;
        let pam = parse_gdal_metadata(xml).unwrap();
        assert_eq!(pam.band(1).and_then(|b| b.wavelength), Some(842.0));
        assert_eq!(pam.band(2).and_then(|b| b.wavelength), None);
        assert!(pam.lost[0].starts_with("2 wavelength item(s) that aren't a length from 1 nm to 1 m"), "{:?}", pam.lost);
    }

    /// A sidecar's band takes the first item that is a length, as the metadata tag's: one that
    /// isn't is said and the next is read; another length is said and left out.
    #[test]
    fn a_sidecar_band_takes_its_first_wavelength_that_is_a_length() {
        let pam = parse(
            r#"<PAMDataset>
  <PAMRasterBand band="1"><Metadata domain="IMAGERY"><MDI key="CENTRAL_WAVELENGTH_UM">1e300</MDI></Metadata><Metadata><MDI key="wavelength">842</MDI></Metadata></PAMRasterBand>
  <PAMRasterBand band="2"><Metadata domain="IMAGERY"><MDI key="CENTRAL_WAVELENGTH_UM">0.665</MDI></Metadata><Metadata><MDI key="wavelength">665</MDI><MDI key="central_wavelength">700</MDI></Metadata></PAMRasterBand>
</PAMDataset>"#,
        )
        .unwrap();
        let w = |b: usize| pam.band(b).and_then(|b| b.wavelength);
        assert_eq!([w(1), w(2)], [Some(842.0), Some(665.0)]);
        assert_eq!(pam.lost.len(), 2, "{:?}", pam.lost);
        // An empty item is one the file gave: said, as the metadata tag's is.
        let empty =
            parse(r#"<PAMDataset><PAMRasterBand band="1"><Metadata><MDI key="wavelength"/></Metadata></PAMRasterBand></PAMDataset>"#)
                .unwrap();
        assert_eq!(empty.band(1).and_then(|b| b.wavelength), None);
        assert!(empty.lost.iter().any(|l| l.starts_with("1 wavelength(s) that aren't")), "{:?}", empty.lost);
        // A band's unit given again otherwise: the first is read, and the other said.
        let twice = parse(r#"<PAMDataset><PAMRasterBand band="1"><Metadata><MDI key="wavelength_units">nm</MDI><MDI key="wavelength_units">micrometers</MDI><MDI key="wavelength">1.6</MDI></Metadata></PAMRasterBand></PAMDataset>"#).unwrap();
        assert_eq!(twice.band(1).and_then(|b| b.wavelength), Some(1.6));
        assert_eq!(twice.lost, ["1 other wavelength unit(s) for a band or the dataset left out (the first is kept)"]);
        // One unit in two spellings is one unit: nothing is said.
        let spelt = parse(r#"<PAMDataset><PAMRasterBand band="1"><Metadata><MDI key="wavelength_units">nm</MDI><MDI key="wavelength_units">Nanometres</MDI><MDI key="wavelength">1.6</MDI></Metadata></PAMRasterBand></PAMDataset>"#).unwrap();
        assert_eq!(spelt.band(1).and_then(|b| b.wavelength), Some(1.6));
        assert!(spelt.lost.is_empty(), "{:?}", spelt.lost);
        // An empty unit names none, in the sidecar and the metadata tag alike: the next is read.
        let sidecar = parse(r#"<PAMDataset><PAMRasterBand band="1"><Metadata><MDI key="wavelength_units"/><MDI key="wavelength_units">nm</MDI><MDI key="wavelength">1.6</MDI></Metadata></PAMRasterBand></PAMDataset>"#).unwrap();
        let tag = parse_gdal_metadata(r#"<GDALMetadata><Item name="wavelength_units" sample="0"> </Item><Item name="wavelength_units" sample="0">nm</Item><Item name="wavelength" sample="0">1.6</Item></GDALMetadata>"#).unwrap();
        for pam in [sidecar, tag] {
            assert_eq!(pam.band(1).and_then(|b| b.wavelength), Some(1.6));
            assert!(pam.lost.is_empty(), "{:?}", pam.lost);
        }
        // Lengths half a thousandth of a nanometre apart are two.
        let mut slot = None;
        assert!(!keep_first_wavelength(&mut slot, 665.0));
        assert!(!keep_first_wavelength(&mut slot, 0.665e3 + 1e-9));
        assert!(keep_first_wavelength(&mut slot, 665.0005));
        assert_eq!(slot, Some(665.0));
        assert!(pam.lost.iter().any(|l| l.starts_with("1 wavelength(s) that aren't")), "{:?}", pam.lost);
        assert!(
            pam.lost.iter().any(|l| l == "1 other wavelength(s) for a band that has one left out (the first is kept)"),
            "{:?}",
            pam.lost
        );
    }

    /// A sidecar's wavelengths, in GDAL's `IMAGERY` domain or ENVI's default one; one that isn't a
    /// length is said, and other band items still are.
    #[test]
    fn reads_wavelengths_from_a_sidecar() {
        let pam = parse(
            r#"<PAMDataset>
  <Metadata><MDI key="wavelength_units">Nanometers</MDI></Metadata>
  <PAMRasterBand band="1"><Metadata domain="IMAGERY"><MDI key="CENTRAL_WAVELENGTH_UM">0.665</MDI><MDI key="FWHM_UM">0.03</MDI></Metadata></PAMRasterBand>
  <PAMRasterBand band="2"><Metadata><MDI key="wavelength">842</MDI></Metadata></PAMRasterBand>
  <PAMRasterBand band="3"><Metadata><MDI key="wavelength">far</MDI></Metadata></PAMRasterBand>
</PAMDataset>"#,
        )
        .unwrap();
        let w = |b: usize| pam.band(b).and_then(|b| b.wavelength);
        assert_eq!([w(1), w(2), w(3)], [Some(665.0), Some(842.0), None]);
        assert_eq!(pam.lost.len(), 2, "{:?}", pam.lost);
        assert!(pam.lost.iter().any(|l| l.starts_with("1 band metadata item(s) not read")), "{:?}", pam.lost);
        assert!(pam.lost.iter().any(|l| l.starts_with("1 wavelength(s)")), "{:?}", pam.lost);
    }

    #[test]
    fn sidecar_names() {
        let p = sidecar_paths(Path::new("/d/scene.tif"));
        assert_eq!(p, vec![PathBuf::from("/d/scene.tif.aux.xml"), PathBuf::from("/d/scene.aux.xml")]);
    }
}
