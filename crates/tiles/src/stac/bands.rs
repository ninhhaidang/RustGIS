//! Which bands a STAC item offers, and which band combinations can be made from them.
//!
//! A *band* is one raster band of one GeoTIFF asset, found by its common name (`red`, `green`,
//! `nir`, `swir16`…) that catalogs give in `eo:bands` or STAC 1.1's `bands`, or, for an asset of
//! one band the catalog names none for, by the asset's key. Sentinel-2 and Landsat publish one
//! file per band, so a colour image is made from three files. Pure: no network, no files; the
//! item is hostile input (counts are bounded and nothing is indexed past its end).

use super::Item;

/// The most bands an item is taken to offer (its assets are already capped).
const MAX_BANDS: usize = 256;

/// One band of one asset's file.
#[derive(Clone, Debug, PartialEq)]
pub struct Band {
    /// The asset's key.
    pub asset: String,
    /// The asset's address.
    pub href: String,
    /// Its place in the file, from 1.
    pub index: u32,
    /// How many bands its file has, by the catalog (1 when it names none).
    pub in_file: u32,
    /// Its common name, lower case; empty when the catalog names none.
    pub common: String,
    /// A stored value `v` means `v · scale + offset` (1 and 0 when the catalog gives none).
    pub scale: f64,
    pub offset: f64,
    /// The stored value the catalog says is no data, when it names one.
    pub nodata: Option<f64>,
    /// Whether the catalog gave the scale (when not, the stored values are used as they are).
    pub scaled: bool,
    /// Whether the catalog's scale, offset or no-data for it is not a number (see
    /// [`BandInfo::unread`](super::BandInfo::unread)).
    pub unread: bool,
    /// Stored values the catalog says are codes for a failed retrieval (JAXA Earth's `dn.error`): not values.
    pub invalid: Vec<f64>,
    /// Whether the catalog names such codes but they could not be read whole: its values can't be told from them.
    pub codes_unread: bool,
}

/// A wanted name and the names that can stand in for it, best first.
const ALIASES: &[(&str, &[&str])] = &[
    ("nir", &["nir", "nir08", "nir09"]),
    ("swir16", &["swir16", "swir1"]),
    ("swir22", &["swir22", "swir2"]),
    ("rededge", &["rededge", "rededge1", "rededge2", "rededge3"]),
    ("coastal", &["coastal", "aerosol"]),
    ("lwir", &["lwir", "lwir11", "lwir12"]),
];

/// A band combination: three bands drawn as red, green and blue.
#[derive(Debug, PartialEq)]
pub struct Combination {
    /// A name for commands.
    pub id: &'static str,
    /// What a menu shows.
    pub name: &'static str,
    /// The band names drawn as red, green and blue.
    pub channels: [&'static str; 3],
}

/// The combinations known, in the order they are offered.
pub const COMBINATIONS: [Combination; 7] = [
    Combination { id: "trueColor", name: "True color", channels: ["red", "green", "blue"] },
    Combination { id: "colorInfrared", name: "Color infrared", channels: ["nir", "red", "green"] },
    Combination { id: "shortwaveInfrared", name: "Shortwave infrared", channels: ["swir22", "nir", "red"] },
    Combination { id: "falseColorUrban", name: "False color urban", channels: ["swir22", "swir16", "red"] },
    Combination { id: "agriculture", name: "Agriculture", channels: ["swir16", "nir", "blue"] },
    Combination { id: "geology", name: "Geology", channels: ["swir22", "swir16", "blue"] },
    Combination { id: "bathymetric", name: "Bathymetric", channels: ["red", "green", "coastal"] },
];

/// A spectral index: a ratio of the bands it names, made as one band of reflectance numbers. The
/// formulas are the Indices section's (`raster.index`); this says which bands each one reads.
#[derive(Debug, PartialEq)]
pub struct Index {
    /// A name for commands.
    pub id: &'static str,
    /// What a menu shows.
    pub name: &'static str,
    /// The band names it reads (see [`find`]), in the order they are read.
    pub channels: &'static [&'static str],
}

/// The indices known, in the order they are offered.
pub const INDICES: [Index; 6] = [
    Index { id: "ndvi", name: "NDVI (vegetation)", channels: &["nir", "red"] },
    Index { id: "ndwi", name: "NDWI (water)", channels: &["green", "nir"] },
    Index { id: "savi", name: "SAVI (vegetation over soil)", channels: &["nir", "red"] },
    Index { id: "nbr", name: "NBR (burn ratio)", channels: &["nir", "swir22"] },
    Index { id: "ndbi", name: "NDBI (built-up)", channels: &["swir16", "nir"] },
    Index { id: "evi", name: "EVI (enhanced vegetation)", channels: &["nir", "red", "blue"] },
];

/// The indices the item can make: those whose bands it has, in the order of [`INDICES`].
pub fn available_indices(bands: &[Band]) -> Vec<&'static Index> {
    INDICES.iter().filter(|i| i.channels.iter().all(|c| find(bands, c).is_some())).collect()
}

/// Every band of the item's GeoTIFF assets, asset by asset (as the item lists them) and in file
/// order. An asset the catalog describes no bands for is one band.
pub fn bands(item: &Item) -> Vec<Band> {
    let mut out = Vec::new();
    for asset in item.assets.iter().filter(|a| a.is_geotiff()) {
        let count = asset.bands.len().max(1);
        for i in 0..count {
            if out.len() >= MAX_BANDS {
                return out;
            }
            // An asset that names no bands has its first band's numbers on the asset itself.
            let info = asset.bands.get(i);
            let (scale, offset, nodata) = match info {
                Some(b) => (b.scale, b.offset, b.nodata),
                None if i == 0 => (asset.scale, asset.offset, asset.nodata),
                None => (None, None, None),
            };
            out.push(Band {
                asset: asset.key.clone(),
                href: asset.href.clone(),
                index: u32::try_from(i + 1).unwrap_or(u32::MAX),
                // The file's own count (the catalog's, which may be past the bands kept), for how many to read.
                in_file: u32::try_from(count.max(asset.declared_bands)).unwrap_or(u32::MAX),
                common: info.map(|b| b.common.clone()).unwrap_or_default(),
                scale: scale.unwrap_or(1.0),
                offset: offset.unwrap_or(0.0),
                nodata,
                scaled: scale.is_some(),
                unread: info.is_some_and(|b| b.unread) || (i == 0 && asset.je_unread) || asset.lists_unread,
                invalid: asset.invalid.clone(),
                codes_unread: asset.codes_unread,
            });
        }
    }
    out
}

/// The band `wanted` names (`red`, `nir`…): the first of its stand-ins (see [`ALIASES`]) that some
/// band has as its common name, else that an asset of exactly one band has as its key. A file of its
/// own comes first, whether the catalog names its band or it is known by its key, over a band of a
/// stack, and under any of the stand-ins before a band of a stack under any of them: Sentinel-2's
/// `visual` and `preview` are colour images whose bands are named red, green and blue too, and they
/// are not the band files (their values are 8-bit), so a colour image's `nir` does not win over a
/// file `nir08`. A key of an asset with several bands says nothing of which one is meant, so it is
/// not taken for one, nor is the key of an asset whose one band the catalog names something else
/// (an asset `red` of `nir` is not red).
pub fn find<'a>(bands: &'a [Band], wanted: &str) -> Option<&'a Band> {
    let wanted = wanted.trim().to_ascii_lowercase();
    let names: Vec<&str> = match ALIASES.iter().find(|(name, _)| *name == wanted) {
        Some((_, stand_ins)) => stand_ins.to_vec(),
        None => vec![wanted.as_str()],
    };
    // A file of its own, under the best stand-in that has one.
    let of_its_own = names.iter().find_map(|name| {
        let key_only = || {
            let mut of_asset = bands.iter().filter(|b| b.asset.eq_ignore_ascii_case(name));
            match (of_asset.next(), of_asset.next()) {
                (Some(only), None) if only.common.is_empty() && only.in_file == 1 => Some(only),
                _ => None,
            }
        };
        bands.iter().find(|b| b.common == *name && b.in_file == 1).or_else(key_only)
    });
    // Only then a band of a colour image.
    of_its_own.or_else(|| names.iter().find_map(|name| bands.iter().find(|b| b.common == *name)))
}

/// The band of the asset whose key is exactly `wanted` (any case), when that asset is a file of one
/// band and no other asset has that key: an explicit choice of a file, ahead of any name, for the
/// user who gives red, green and blue themselves (`B8A`, where `B08` and `B8A` are both `nir`).
pub fn find_key<'a>(bands: &'a [Band], wanted: &str) -> Option<&'a Band> {
    let wanted = wanted.trim();
    let mut of_asset = bands.iter().filter(|b| b.asset.eq_ignore_ascii_case(wanted));
    let only = match (of_asset.next(), of_asset.next()) {
        (Some(only), None) if only.in_file == 1 => only,
        _ => return None,
    };
    // A key that is itself a band name (`red`, `nir`…) is not taken for it when the catalog names the
    // file something else, as [`find`] doesn't; any other key (`B8A`) is the user's pick whatever the
    // catalog calls it.
    let lower = wanted.to_ascii_lowercase();
    let is_name = ALIASES.iter().any(|(name, stand_ins)| *name == lower || stand_ins.contains(&lower.as_str()))
        || ["coastal", "blue", "green", "red", "yellow", "pan", "cirrus", "nir", "swir16", "swir22", "lwir"].contains(&lower.as_str());
    let names_it_so = only.common == lower
        || ALIASES
            .iter()
            .any(|(name, stand_ins)| (*name == lower || stand_ins.contains(&lower.as_str())) && stand_ins.contains(&only.common.as_str()));
    (!is_name || only.common.is_empty() || names_it_so).then_some(only)
}

/// A note for an error about a band not found, when the item describes more bands than are kept
/// (an asset's past [`super::MAX_BANDS`], or the item's past this list's cap): the band may be one
/// of those left out. None when nothing was cut.
pub fn cut_note(item: &Item) -> Option<String> {
    if item.assets.iter().any(|a| a.bands_cut) {
        return Some(format!(
            "an asset's band list was cut (more than {} bands, or a band name over {} characters or not text): bands past {}, the characters of a name past {}, and a name that is not text are not read",
            super::MAX_BANDS,
            super::MAX_NAME,
            super::MAX_BANDS,
            super::MAX_NAME
        ));
    }
    let total: usize = item.assets.iter().filter(|a| a.is_geotiff()).map(|a| a.bands.len().max(1)).sum();
    if total > MAX_BANDS {
        return Some(format!("the item has more than {MAX_BANDS} bands and only the first {MAX_BANDS} are listed"));
    }
    let jp2 = jp2_only(item);
    (!jp2.is_empty())
        .then(|| format!("JPEG 2000 files hold bands no GeoTIFF of the item has ({}), and only GeoTIFF files are read", jp2.join(", ")))
}

/// The common names of bands that only a JPEG 2000 file of the item holds: files of other formats
/// are not read, so an item that mixes them with GeoTIFFs offers no combination or index that needs
/// one of these. A JPEG 2000 file whose band a GeoTIFF has too (Sentinel-2's `red-jp2` beside `red`)
/// costs nothing and is not named.
fn jp2_only(item: &Item) -> Vec<String> {
    let is_jp2 = |a: &super::Asset| {
        let path = crate::logins::inner(&a.href).split(['?', '#']).next().unwrap_or("").to_ascii_lowercase();
        let t = a.media_type.to_ascii_lowercase();
        !a.is_geotiff() && (t.contains("jp2") || t.contains("jpeg2000") || path.ends_with(".jp2"))
    };
    let read: std::collections::BTreeSet<&str> =
        item.assets.iter().filter(|a| a.is_geotiff()).flat_map(|a| a.bands.iter().map(|b| b.common.as_str())).collect();
    let mut names: Vec<String> = Vec::new();
    for b in item.assets.iter().filter(|a| is_jp2(a)).flat_map(|a| a.bands.iter()) {
        if !b.common.is_empty() && !read.contains(b.common.as_str()) && !names.contains(&b.common) {
            names.push(b.common.clone());
        }
    }
    names
}

/// The combinations the item can make, each with the bands found for its red, green and blue:
/// those whose three bands it has, in the order of [`COMBINATIONS`].
pub fn available(bands: &[Band]) -> Vec<(&'static Combination, [Band; 3])> {
    COMBINATIONS
        .iter()
        .filter_map(|c| {
            let [r, g, b] = c.channels;
            Some((c, [find(bands, r)?.clone(), find(bands, g)?.clone(), find(bands, b)?.clone()]))
        })
        .collect()
}

/// [`available`] without the bands found, for a menu that is drawn every frame: no band is cloned.
pub fn offered(bands: &[Band]) -> Vec<&'static Combination> {
    COMBINATIONS.iter().filter(|c| c.channels.iter().all(|channel| find(bands, channel).is_some())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stac::parse_items;
    use serde_json::json;

    /// An item with these assets (key, `eo:bands`/`raster:bands` JSON or null).
    fn item(assets: &[(&str, serde_json::Value)]) -> Item {
        let assets: serde_json::Map<String, serde_json::Value> = assets
            .iter()
            .map(|(key, extra)| {
                let mut a =
                    json!({"href": format!("https://x.org/{key}.tif"), "type": "image/tiff; application=geotiff; profile=cloud-optimized"});
                if let (Some(a), Some(e)) = (a.as_object_mut(), extra.as_object()) {
                    a.extend(e.clone());
                }
                (key.to_string(), a)
            })
            .collect();
        let body = json!({"type": "FeatureCollection", "features": [
            {"type": "Feature", "id": "i", "bbox": [0, 0, 1, 1], "properties": {"datetime": "2024-06-01T00:00:00Z"}, "assets": assets}
        ]})
        .to_string();
        parse_items(body.as_bytes(), "https://x.org/search").unwrap().0.remove(0)
    }

    fn named(common: &str) -> serde_json::Value {
        json!({"eo:bands": [{"common_name": common}], "raster:bands": [{"scale": 0.0001, "offset": -0.1, "nodata": 0}]})
    }

    /// A Sentinel-2 style item: a file a band, the names in `eo:bands`.
    fn sentinel() -> Item {
        item(&[
            ("blue", named("blue")),
            ("green", named("green")),
            ("red", named("red")),
            ("nir", named("nir")),
            ("nir08", named("nir08")),
            ("swir16", named("swir16")),
            ("swir22", named("swir22")),
            ("thumbnail", json!({"type": "image/png", "href": "https://x.org/thumbnail.png"})),
        ])
    }

    #[test]
    fn an_items_bands_come_from_its_geotiff_assets_with_their_numbers() {
        let b = bands(&sentinel());
        assert_eq!(b.len(), 7, "the thumbnail is not a band");
        let red = find(&b, "red").unwrap();
        assert_eq!((red.asset.as_str(), red.index, red.common.as_str()), ("red", 1, "red"));
        assert_eq!((red.scale, red.offset, red.nodata), (0.0001, -0.1, Some(0.0)));
    }

    #[test]
    fn a_name_is_found_by_its_stand_ins_best_first() {
        let b = bands(&sentinel());
        // Both `nir` and `nir08` are there: the wanted name's own is the best.
        assert_eq!(find(&b, "nir").unwrap().asset, "nir");
        // Landsat has only `nir08`: it stands in for `nir`.
        let landsat = bands(&item(&[("red", named("red")), ("nir08", named("nir08"))]));
        assert_eq!(find(&landsat, "nir").unwrap().asset, "nir08");
        assert_eq!(find(&landsat, " NIR ").unwrap().asset, "nir08", "case and spaces are not part of a name");
        let old = bands(&item(&[("swir1", named("swir1")), ("swir2", named("swir2"))]));
        assert_eq!((find(&old, "swir16").unwrap().asset.as_str(), find(&old, "swir22").unwrap().asset.as_str()), ("swir1", "swir2"));
        assert!(find(&b, "lwir").is_none());
    }

    /// Sentinel-2 items carry `visual` and `preview` colour images whose three bands are named red,
    /// green and blue; `preview` sorts before `red`, and it is a file of three, not the red band.
    #[test]
    fn a_band_in_a_file_of_its_own_is_taken_over_one_in_a_colour_image() {
        let colour = json!({"eo:bands": [{"common_name": "red"}, {"common_name": "green"}, {"common_name": "blue"}]});
        let b = bands(&item(&[("red", named("red")), ("preview", colour.clone()), ("visual", colour.clone())]));
        let red = find(&b, "red").unwrap();
        assert_eq!((red.asset.as_str(), red.index, red.in_file), ("red", 1, 1), "not preview's first band");
        // With no file of its own, the colour image's band is still found, by its place in it.
        let only = bands(&item(&[("visual", colour)]));
        let red = find(&only, "red").unwrap();
        assert_eq!((red.asset.as_str(), red.index, red.in_file), ("visual", 1, 3));
    }

    #[test]
    fn an_assets_key_is_not_its_band_when_the_catalog_names_the_band_something_else() {
        // An asset `red` whose one band is near infrared is not red; with no name it still is.
        let b = bands(&item(&[("red", named("nir")), ("green", json!({}))]));
        assert!(find(&b, "red").is_none(), "`red` holds the near infrared band");
        assert_eq!(find(&b, "nir").unwrap().asset, "red", "and it is found as what it is");
        assert_eq!(find(&b, "green").unwrap().asset, "green", "a band the catalog names none for goes by its key");
    }

    #[test]
    fn a_file_of_its_own_known_by_its_key_is_taken_over_a_band_of_a_colour_image() {
        // `red` is one band the catalog names none for; `visual` is a colour image with a band named red.
        let colour = json!({"eo:bands": [{"common_name": "red"}, {"common_name": "green"}, {"common_name": "blue"}]});
        let b = bands(&item(&[("red", json!({})), ("visual", colour)]));
        let red = find(&b, "red").unwrap();
        assert_eq!((red.asset.as_str(), red.in_file), ("red", 1), "the file of its own, not visual's first band");
    }

    #[test]
    fn a_file_of_its_own_under_any_stand_in_comes_before_a_colour_images_band() {
        // A colour image has a band named `nir`, and the item's one band file is `nir08`.
        let colour = json!({"eo:bands": [{"common_name": "nir"}, {"common_name": "red"}, {"common_name": "green"}]});
        let b = bands(&item(&[("visual", colour.clone()), ("nir08", named("nir08"))]));
        let nir = find(&b, "nir").unwrap();
        assert_eq!((nir.asset.as_str(), nir.in_file), ("nir08", 1), "the file, not the colour image's band");
        // With no file at all, the colour image's band is still found.
        let only = bands(&item(&[("visual", colour)]));
        assert_eq!(find(&only, "nir").unwrap().asset, "visual");
        // The wanted name's own file still comes before a stand-in's.
        let both = bands(&item(&[("nir08", named("nir08")), ("nir", named("nir"))]));
        assert_eq!(find(&both, "nir").unwrap().asset, "nir");
    }

    #[test]
    fn a_scale_that_is_not_a_number_is_marked_not_dropped_and_a_missing_one_is_known() {
        let odd =
            |extra: serde_json::Value| bands(&item(&[("red", json!({"eo:bands": [{"common_name": "red"}], "raster:bands": [extra]}))]));
        // A string for a number: not read (1 and 0 stand in), and marked.
        let b = odd(json!({"scale": "0.0001", "offset": -0.1}));
        assert_eq!((b[0].scale, b[0].unread, b[0].scaled), (1.0, true, false));
        assert!(odd(json!({"offset": {"v": 1}}))[0].unread, "an object is no number either");
        // "nan" is a no-data the raster extension allows: not read as a number, but not odd.
        assert!(!odd(json!({"scale": 0.0001, "nodata": "nan"}))[0].unread);
        assert!(odd(json!({"scale": 0.0001, "nodata": "zero"}))[0].unread);
        assert!(odd(json!({"scale": 0.0001, "raster:nodata": "0"}))[0].unread, "the prefixed no-data is checked too");
        assert!(!odd(json!({"scale": 0.0001, "raster:nodata": "nan"}))[0].unread);
        // A real scale is known; no scale is known as none.
        let (scaled, plain) = (odd(json!({"scale": 0.0001})), odd(json!({})));
        assert_eq!((scaled[0].scaled, scaled[0].unread, plain[0].scaled, plain[0].unread), (true, false, false, false));
        // The mark goes with the item to the command.
        let item = item(&[("red", json!({"eo:bands": [{"common_name": "red"}], "raster:bands": [{"scale": "0.0001"}]}))]);
        let again = crate::stac::item_from_json(&crate::stac::item_json(&item), None).unwrap();
        assert!(bands(&again)[0].unread, "still marked after the item is passed on");
    }

    #[test]
    fn only_the_indices_an_item_can_make_are_offered() {
        let ids = |i: &Item| available_indices(&bands(i)).iter().map(|x| x.id).collect::<Vec<_>>();
        assert_eq!(ids(&sentinel()), ["ndvi", "ndwi", "savi", "nbr", "ndbi", "evi"]);
        // Visible light only: no near infrared, so no index.
        assert!(ids(&item(&[("red", named("red")), ("green", named("green")), ("blue", named("blue"))])).is_empty());
        // Near infrared and red: the vegetation indices, not the ones that need green, blue or SWIR.
        assert_eq!(ids(&item(&[("red", named("red")), ("nir", named("nir"))])), ["ndvi", "savi"]);
        // Every index reads bands the combinations' vocabulary knows.
        assert!(INDICES.iter().all(|i| i.channels.len() >= 2 && i.channels.iter().all(|c| !c.is_empty())));
    }

    #[test]
    fn an_asset_with_no_names_is_known_by_its_key_only_when_it_is_one_band() {
        // Named by key alone: one band each.
        let by_key = bands(&item(&[("red", json!({})), ("green", json!({})), ("blue", json!({}))]));
        assert_eq!(find(&by_key, "green").unwrap().asset, "green");
        // A file of several bands, none named: its key doesn't say which one is red.
        let stack = bands(&item(&[("red", json!({"raster:bands": [{}, {}, {}]}))]));
        assert_eq!(stack.len(), 3);
        assert!(find(&stack, "red").is_none());
        // Named inside the file: each is found by its own name and number.
        let named_stack = bands(&item(&[(
            "analytic",
            json!({"eo:bands": [{"common_name": "blue"}, {"common_name": "green"}, {"common_name": "red"}, {"common_name": "nir"}]}),
        )]));
        assert_eq!((find(&named_stack, "red").unwrap().index, find(&named_stack, "nir").unwrap().index), (3, 4));
    }

    #[test]
    fn only_the_combinations_an_item_can_make_are_offered() {
        let all = available(&bands(&sentinel()));
        let ids: Vec<&str> = all.iter().map(|(c, _)| c.id).collect();
        assert_eq!(
            ids,
            ["trueColor", "colorInfrared", "shortwaveInfrared", "falseColorUrban", "agriculture", "geology"],
            "no coastal band: no bathymetric"
        );
        let (_, [r, g, b]) = &all[1];
        assert_eq!((r.asset.as_str(), g.asset.as_str(), b.asset.as_str()), ("nir", "red", "green"), "colour infrared is nir, red, green");
        // Only visible light: true color and nothing else.
        let visible = available(&bands(&item(&[("red", named("red")), ("green", named("green")), ("blue", named("blue"))])));
        assert_eq!(visible.iter().map(|(c, _)| c.id).collect::<Vec<_>>(), ["trueColor"]);
        assert!(available(&bands(&item(&[("red", named("red"))]))).is_empty());
        assert!(available(&[]).is_empty());
        // `offered` is the same list without cloning the bands.
        for scene in [sentinel(), item(&[("red", named("red")), ("nir", named("nir"))]), item(&[])] {
            let all = bands(&scene);
            assert_eq!(
                offered(&all).iter().map(|c| c.id).collect::<Vec<_>>(),
                available(&all).iter().map(|(c, _)| c.id).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn an_item_passed_on_as_json_keeps_the_bands_it_needs() {
        // What a search answers, handed to a command: the names and the scaling survive.
        let original = sentinel();
        let again = crate::stac::item_from_json(&crate::stac::item_json(&original), None).unwrap();
        assert_eq!(bands(&again), bands(&original));
        assert_eq!(available(&bands(&again)).len(), 6);
        // A file of three named bands keeps each one's name and place.
        let stack = item(&[("analytic", json!({"eo:bands": [{"common_name": "blue"}, {"common_name": "green"}, {"common_name": "red"}]}))]);
        let again = crate::stac::item_from_json(&crate::stac::item_json(&stack), None).unwrap();
        assert_eq!(find(&bands(&again), "red").map(|b| b.index), Some(3));
        // An asset that names no band but has a scale keeps it, and one with none writes no `bands`.
        let scaled = item(&[("red", json!({"raster:bands": [{"scale": 0.5, "offset": 1.0}]})), ("green", json!({}))]);
        let again = crate::stac::item_from_json(&crate::stac::item_json(&scaled), None).unwrap();
        let (red, green) = (find(&bands(&again), "red").unwrap().clone(), find(&bands(&again), "green").unwrap().clone());
        assert_eq!((red.scale, red.offset), (0.5, 1.0));
        assert_eq!((green.scale, green.offset), (1.0, 0.0));
        // The asset's own numbers survive the round trip too, not only its bands'.
        let asset = again.assets.iter().find(|a| a.key == "red").unwrap();
        assert_eq!((asset.scale, asset.offset), (Some(0.5), Some(1.0)));
        let written = crate::stac::item_json(&scaled);
        assert!(written["assets"]["green"].get("bands").is_none(), "nothing to say of a band the catalog describes none of");
    }

    #[test]
    fn the_unified_bands_of_stac_1_1_are_read_like_eo_bands() {
        let unified = json!({"bands": [{"eo:common_name": "Red", "raster:scale": 0.5, "raster:offset": 2.0, "nodata": 255}]});
        let b = bands(&item(&[("data", unified)]));
        let red = find(&b, "red").unwrap();
        assert_eq!((red.scale, red.offset, red.nodata), (0.5, 2.0, Some(255.0)));
    }

    #[test]
    fn a_truncated_multiband_file_is_not_taken_for_a_band_by_its_key() {
        // 255 bands before it, so the cap keeps only the first of the three bands of `red`.
        let filler = |n: usize| json!({"eo:bands": (0..n).map(|_| json!({})).collect::<Vec<_>>()});
        let list = bands(&item(&[("a", filler(64)), ("b", filler(64)), ("c", filler(64)), ("d", filler(63)), ("red", filler(3))]));
        assert_eq!(list.len(), 256);
        assert!(find(&list, "red").is_none(), "an unnamed file of three bands says nothing of which is red");
    }

    #[test]
    fn a_jaxa_scale_and_failed_retrieval_codes_reach_the_named_band_and_the_command() {
        let a = json!({"eo:bands": [{"common_name": "red"}], "je:rasters": {"dn2value": {"slope": 0.0001, "offset": 0.5}, "dn": {"error": [65535.0]}}});
        let it = item(&[("red", a)]);
        let check = |it: &Item| {
            let b = bands(it);
            let red = find(&b, "red").unwrap();
            assert_eq!((red.scale, red.offset, red.scaled), (0.0001, 0.5, true));
            assert_eq!(red.invalid, vec![65535.0]);
            assert!(!red.codes_unread);
        };
        check(&it);
        check(&crate::stac::item_from_json(&crate::stac::item_json(&it), None).unwrap());
        // Codes that couldn't be read whole stay unread through the command's copy of the item.
        let odd = item(&[("red", json!({"eo:bands": [{"common_name": "red"}], "je:rasters": {"dn": {"error": ["x"]}}}))]);
        assert!(bands(&odd)[0].codes_unread);
        let again = crate::stac::item_from_json(&crate::stac::item_json(&odd), None).unwrap();
        assert!(bands(&again)[0].codes_unread);
    }

    #[test]
    fn a_jaxa_number_given_as_text_marks_the_band_unread() {
        let jaxa = |named: bool, slope: serde_json::Value| {
            let mut a = json!({"je:rasters": {"dn2value": {"slope": slope}}});
            if named {
                a["eo:bands"] = json!([{"common_name": "red"}]);
            }
            item(&[("red", a)])
        };
        for named in [false, true] {
            let it = jaxa(named, json!("0.0001"));
            assert!(bands(&it)[0].unread, "text is no number (named: {named})");
            let again = crate::stac::item_from_json(&crate::stac::item_json(&it), None).unwrap();
            assert!(bands(&again)[0].unread, "it goes through the command's copy (named: {named})");
            assert!(!bands(&jaxa(named, json!(0.0001)))[0].unread, "a number is read");
        }
    }

    #[test]
    fn an_exact_asset_key_picks_its_file_and_a_jpeg_2000_only_band_is_said() {
        // B08 and B8A are both near infrared: the key B8A is that file, though `nir` is B08.
        let b = bands(&item(&[("B08", named("nir")), ("B8A", named("nir")), ("visual", json!({"eo:bands": [{}, {}, {}]}))]));
        assert_eq!(find_key(&b, "b8a").map(|b| b.asset.as_str()), Some("B8A"));
        assert_eq!(find_key(&b, "visual").map(|b| b.asset.as_str()), None, "a file of several bands is not chosen by its key");
        assert_eq!(find_key(&b, "nir").map(|b| b.asset.as_str()), None, "a name is not a key");
        // A JPEG 2000 file holding a band no GeoTIFF has is said; one that repeats a GeoTIFF's is not.
        let jp2 =
            |name: &str| json!({"href": format!("https://x.org/{name}.jp2"), "type": "image/jp2", "eo:bands": [{"common_name": name}]});
        let tiff = |name: &str| json!({"href": format!("https://x.org/{name}.tif"), "type": "image/tiff; application=geotiff", "eo:bands": [{"common_name": name}]});
        let mixed = item(&[("red", tiff("red")), ("nir-jp2", jp2("nir"))]);
        assert!(cut_note(&mixed).is_some_and(|n| n.contains("JPEG 2000") && n.contains("nir")), "{:?}", cut_note(&mixed));
        assert_eq!(cut_note(&item(&[("red", tiff("red")), ("red-jp2", jp2("red"))])), None);
        assert_eq!(cut_note(&item(&[("red", tiff("red")), ("nir", tiff("nir"))])), None);
    }

    #[test]
    fn a_band_list_that_is_not_a_list_marks_the_band_unread() {
        for key in ["raster:bands", "eo:bands", "bands"] {
            let mut a = json!({"eo:bands": [{"common_name": "red"}]});
            a[key] = json!({"scale": 0.0001});
            let it = item(&[("red", a)]);
            let b = bands(&it);
            assert!(b[0].unread, "{key} as an object is no list");
            let again = crate::stac::item_from_json(&crate::stac::item_json(&it), None).unwrap();
            assert!(bands(&again)[0].unread, "{key}: it goes through the command's copy");
        }
        assert!(!bands(&item(&[("red", json!({"eo:bands": [{"common_name": "red"}], "raster:bands": [{"scale": 0.0001}]}))]))[0].unread);
    }

    #[test]
    fn an_asset_level_name_and_numbers_reach_its_bands() {
        // The EO extension's name on the asset, for a file of one band with no list.
        let b = bands(&item(&[("B04", json!({"eo:common_name": "Red"}))]));
        assert_eq!(find(&b, "red").map(|b| b.asset.as_str()), Some("B04"));
        // STAC 1.1: numbers shared on the asset, a band's own overriding.
        let shared = item(&[(
            "data",
            json!({"raster:scale": 0.0001, "raster:offset": -0.1,
                "bands": [{"eo:common_name": "nir"}, {"eo:common_name": "red", "raster:scale": 0.0002}]}),
        )]);
        let check = |it: &Item| {
            let b = bands(it);
            let (nir, red) = (find(&b, "nir").unwrap(), find(&b, "red").unwrap());
            assert_eq!((nir.scale, nir.offset, nir.scaled), (0.0001, -0.1, true));
            assert_eq!((red.scale, red.offset), (0.0002, -0.1), "the band's own scale, the asset's offset");
        };
        check(&shared);
        check(&crate::stac::item_from_json(&crate::stac::item_json(&shared), None).unwrap());
    }

    #[test]
    fn an_asset_level_number_given_as_text_marks_every_band_unread() {
        let it = item(&[("data", json!({"raster:scale": "0.0001", "eo:bands": [{"common_name": "nir"}, {"common_name": "red"}]}))]);
        let again = crate::stac::item_from_json(&crate::stac::item_json(&it), None).unwrap();
        for it in [&it, &again] {
            let b = bands(it);
            assert_eq!(b.len(), 2);
            assert!(b.iter().all(|x| x.unread), "every band, directly and after the round trip");
        }
        // A real number there, and JAXA's `rg:unread` on the asset alone, do not.
        let ok = item(&[("data", json!({"raster:scale": 0.0001, "eo:bands": [{"common_name": "nir"}, {"common_name": "red"}]}))]);
        assert!(bands(&ok).iter().all(|x| !x.unread));
    }

    #[test]
    fn a_bands_own_declaration_stops_the_asset_defaults_even_a_special_no_data() {
        // The asset says 255, the band says "nan" (no no-data value): the band's declaration is its own.
        let it = item(&[(
            "data",
            json!({"nodata": 255, "raster:scale": 0.0001,
                "bands": [{"eo:common_name": "red", "nodata": "nan"}, {"eo:common_name": "nir"}]}),
        )]);
        let b = bands(&it);
        let (red, nir) = (find(&b, "red").unwrap(), find(&b, "nir").unwrap());
        assert_eq!((red.nodata, red.unread), (None, false), "not the asset's 255");
        assert_eq!((nir.nodata, nir.scale), (Some(255.0), 0.0001), "a band that declares none takes the asset's");
    }

    #[test]
    fn an_asset_default_given_as_text_marks_only_the_bands_that_would_take_it() {
        let it = item(&[(
            "data",
            json!({"raster:scale": "0.0001", "bands": [{"eo:common_name": "red", "raster:scale": 0.0002}, {"eo:common_name": "nir"}]}),
        )]);
        let b = bands(&it);
        let (red, nir) = (find(&b, "red").unwrap(), find(&b, "nir").unwrap());
        assert_eq!((red.scale, red.unread), (0.0002, false), "its own valid scale stands");
        assert!(nir.unread, "this one would take the unreadable default");
    }

    #[test]
    fn an_asset_with_no_band_list_and_a_text_number_is_unread() {
        let it = item(&[("red", json!({"raster:scale": "0.0001"}))]);
        let again = crate::stac::item_from_json(&crate::stac::item_json(&it), None).unwrap();
        for it in [&it, &again] {
            let b = bands(it);
            assert_eq!(b.len(), 1);
            assert!(b[0].unread, "the lone band would be used unscaled");
        }
        assert!(!bands(&item(&[("red", json!({"raster:scale": 0.0001}))]))[0].unread);
    }

    #[test]
    fn a_key_that_is_a_band_name_is_not_taken_when_the_catalog_names_the_file_otherwise() {
        // An asset `red` whose one band the catalog calls nir: neither lookup gives it for red.
        let odd = bands(&item(&[("red", named("nir")), ("green", named("green"))]));
        assert!(find(&odd, "red").is_none());
        assert!(find_key(&odd, "red").is_none(), "the key lookup agrees with the name lookup");
        assert_eq!(find_key(&odd, "green").map(|b| b.asset.as_str()), Some("green"));
        // A key that is no band name is the user's pick, whatever the catalog calls the file.
        let b8a = bands(&item(&[("B08", named("nir")), ("B8A", named("nir"))]));
        assert_eq!(find_key(&b8a, "b8a").map(|b| b.asset.as_str()), Some("B8A"));
        // A stand-in the catalog uses (an asset `nir` named nir08) still matches.
        let alias = bands(&item(&[("nir", named("nir08"))]));
        assert!(find_key(&alias, "nir").is_some());
    }

    #[test]
    fn an_asset_level_name_that_is_long_or_not_text_is_said() {
        let long = item(&[("B04", json!({"eo:common_name": "r".repeat(40)}))]);
        assert!(cut_note(&long).is_some_and(|n| n.contains("name over 32")), "{:?}", cut_note(&long));
        let number = item(&[("B04", json!({"eo:common_name": 7}))]);
        assert!(cut_note(&number).is_some_and(|n| n.contains("not text")), "{:?}", cut_note(&number));
        assert_eq!(cut_note(&item(&[("B04", json!({"eo:common_name": "red"}))])), None);
    }

    #[test]
    fn a_jaxa_default_unreadable_marks_the_first_band_only_if_it_inherits_that_field() {
        let item_with =
            |band: serde_json::Value| item(&[("red", json!({"je:rasters": {"dn2value": {"slope": "0.0001"}}, "eo:bands": [band]}))]);
        let check = |it: &Item, unread: bool, what: &str| {
            let again = crate::stac::item_from_json(&crate::stac::item_json(it), None).unwrap();
            for it in [it, &again] {
                assert_eq!(bands(it)[0].unread, unread, "{what}");
            }
        };
        // A valid scale of its own: the unreadable JAXA slope is never used.
        check(&item_with(json!({"common_name": "red", "raster:scale": 0.0002})), false, "own scale");
        // No scale of its own: it would take the slope, so it is refused.
        check(&item_with(json!({"common_name": "red"})), true, "inherits the slope");
    }

    #[test]
    fn a_declared_asset_number_is_not_refused_for_an_unused_jaxa_one() {
        // No band list: read as one band numbered from the asset. A valid asset scale means JAXA's
        // unreadable slope is never used.
        let check = |asset: serde_json::Value, unread: bool, what: &str| {
            let it = item(&[("red", asset)]);
            let again = crate::stac::item_from_json(&crate::stac::item_json(&it), None).unwrap();
            for it in [&it, &again] {
                assert_eq!(bands(it)[0].unread, unread, "{what}");
            }
        };
        check(json!({"scale": 0.0002, "je:rasters": {"dn2value": {"slope": "0.0001"}}}), false, "declared scale");
        check(json!({"je:rasters": {"dn2value": {"slope": "0.0001"}}}), true, "would use the slope");
    }

    #[test]
    fn a_band_name_that_is_not_text_is_said() {
        let odd = item(&[("x", json!({"eo:bands": [{"common_name": 7}]}))]);
        assert!(cut_note(&odd).is_some_and(|n| n.contains("not text")), "{:?}", cut_note(&odd));
    }

    #[test]
    fn hostile_band_lists_are_bounded_and_odd_numbers_are_not_taken() {
        // More bands than are kept, and a no-data that is not a number.
        let many: Vec<serde_json::Value> = (0..500).map(|_| json!({"common_name": "red"})).collect();
        let b = bands(&item(&[("big", json!({"eo:bands": many}))]));
        assert_eq!(b.len(), 64, "one asset's bands are cut");
        let nan = bands(&item(&[("red", json!({"eo:bands": [{"common_name": "red"}], "raster:bands": [{"nodata": "nan", "scale": 0}]}))]));
        assert_eq!(nan[0].nodata, None, "\"nan\" is not a number");
        assert_eq!(nan[0].scale, 0.0, "a stated scale is kept as stated");
        // The cut is said: for the asset, and again after the item has been passed to a command.
        let big = item(&[("big", json!({"eo:bands": (0..500).map(|_| json!({})).collect::<Vec<_>>()}))]);
        assert!(cut_note(&big).is_some_and(|n| n.contains("more than 64 bands")), "{:?}", cut_note(&big));
        let again = crate::stac::item_from_json(&crate::stac::item_json(&big), None).unwrap();
        assert!(cut_note(&again).is_some(), "the cut survives the item being passed on");
        // An item with more bands than the list keeps (six files of 64) says so too; a whole one says nothing.
        let sixty_four = json!({"eo:bands": (0..64).map(|_| json!({})).collect::<Vec<_>>()});
        let wide = item(&[
            ("a", sixty_four.clone()),
            ("b", sixty_four.clone()),
            ("c", sixty_four.clone()),
            ("d", sixty_four.clone()),
            ("e", sixty_four.clone()),
            ("f", sixty_four),
        ]);
        assert_eq!(bands(&wide).len(), 256, "the list stops at its cap");
        assert!(cut_note(&wide).is_some_and(|n| n.contains("more than 256 bands")), "{:?}", cut_note(&wide));
        assert_eq!(cut_note(&sentinel()), None);
        // A name cut to 32 characters is said, as bands past the cap are; a name that fits says nothing.
        let long = item(&[("x", json!({"eo:bands": [{"common_name": "r".repeat(40)}]}))]);
        assert!(bands(&long).iter().all(|b| b.common.chars().count() <= 32));
        assert!(cut_note(&long).is_some_and(|n| n.contains("name over 32")), "{:?}", cut_note(&long));
        assert_eq!(cut_note(&item(&[("x", json!({"eo:bands": [{"common_name": "r".repeat(32)}]}))])), None);
        // Nothing panics on junk.
        let junk = bands(&item(&[("x", json!({"eo:bands": [1, "a", null, {"common_name": 7}], "raster:bands": "no"}))]));
        assert_eq!(junk.len(), 4);
        assert!(find(&junk, "red").is_none());
    }
}
