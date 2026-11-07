//! The Style and Data tabs for raster layers: Rendering (symbology, stretch, DRA, resampling, band
//! combination), Enhancement (brightness, contrast, gamma), plus raster info and export.

use std::path::Path;
use std::sync::Arc;

use rustgis_data::raster::{self, Raster};
use rustgis_geom::Extent;
use rustgis_map::raster::{
    Normalizer, RasterLayer, RasterMask, RasterRenderer, RasterStyle, Resampling, Stretch, classify_masked, unique_values,
};
use rustgis_map::{ClassMethod, Ramp, RasterDefaults};
use serde_json::{Value, json};

use super::{b, f, ps};
use crate::{CmdResult, CommandSpec as C, Session};

/// Largest file opened from its contents (a browser upload): 512 MiB, or 256 MiB in the browser.
#[cfg(not(target_arch = "wasm32"))]
pub const MAX_UPLOAD_BYTES: usize = 512 << 20;
#[cfg(target_arch = "wasm32")]
pub const MAX_UPLOAD_BYTES: usize = 256 << 20;

pub(crate) const JPEG2000_WEB_LIMITATION: &str = "JPEG 2000 opens in the desktop app, through GDAL; the web app reads GeoTIFFs";

pub(crate) fn need_raster(s: &Session) -> Option<&'static str> {
    match s.active_layer.and_then(|id| s.map().layer(id)) {
        Some(l) if l.raster.as_ref().is_some_and(|r| r.data.is_some()) => None,
        Some(l) if l.is_raster() => Some("the raster's data couldn't be read"),
        _ => Some("select a raster layer in the Layers pane first"),
    }
}

pub fn commands() -> Vec<C> {
    vec![
        C::new("layer.addDataBytes", "Add Data (from file contents)", "Home › Add", add_bytes).roles(&[]).effects(crate::policy::Effects::NONE)
            .params(r#"{"fileName": "name.tif|.tiff|.asc|.geojson|.csv|.zip|.kml|.kmz|.gpx|.parquet|.fgb|.gpkg|.mbtiles|.jp2|.j2k (JPEG 2000: desktop, through GDAL)", "data": base64, "zoom"?: bool, "layer"?: "one layer of a zip, KML or GPX by name", "name"?: text, "x"?, "y"?, "wkt"?, "crs"?, "delimiter"?, "paperSpace"?, "stream"?, "as"? (as layer.addData)}"#),
        C::new("raster.symbology", "Symbology", "Style › Rendering", symbology).when(need_raster).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r#"{"layer"?, "type": "stretch"|"rgb"|"classify"|"unique"|"colormap"|"shadedRelief", "band"?: 1.., "ramp"?, "invert"?: bool, "classes"?: 1..32, "method"?, "azimuth"?, "altitude"?, "zFactor"?, "grey"?: bool (shadedRelief without its colour ramp)}"#,
        ),
        C::new("raster.stretch", "Stretch Type", "Style › Rendering", stretch).inputs(stretch_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
        .when(need_raster)
        .params(r#"{"layer"?, "type": "none"|"minMax"|"custom"|"percentClip"|"standardDeviation"|"histogramEqualize", "low"?: %, "high"?: %, "n"?: deviations, "min"?, "max"?: custom bounds in the data's units, a number for every band or [red, green, blue] on an RGB layer, null for a band's own (a missing one is the range now drawn; the layer's own on one band), "like"?: raster layer (instead of type: that layer's stretch as fixed bounds, so tiles match at their joins)}"#),
        C::new("raster.dra", "DRA", "Style › Rendering", |s, p| {
            let on = b(p, "on");
            with(s, p, |rl, _| {
                rl.style.dra = on.unwrap_or(!rl.style.dra);
                Ok(())
            })
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .when(need_raster)
        .params(r#"{"layer"?, "on"?: bool}"#),
        C::new("raster.mask", "Mask Values", "Style › Rendering", mask).roles(&[]).effects(crate::policy::Effects::NONE).when(need_raster).params(
            r#"{"layer"?, "values"?: [numbers] or "0, 254, 255" (drawn as no data; [] for none), "min"?, "max"?: hide values outside this range (null for no bound), "inside"?: bool (hide those inside it instead), "clear"?: bool}"#,
        ),
        C::new("raster.resampling", "Resampling Type", "Style › Rendering", |s, p| {
            let name = ps(p, "type").ok_or("type is required")?;
            let r = Resampling::from_name(name).ok_or_else(|| format!("unknown resampling '{name}'"))?;
            with(s, p, |rl, _| {
                rl.style.resampling = r;
                Ok(())
            })
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .when(need_raster)
        .params(r#"{"layer"?, "type": "nearest"|"bilinear"}"#),
        C::new("raster.bands", "Band Combination", "Style › Rendering", bands).roles(&[]).effects(crate::policy::Effects::NONE)
            .when(need_raster)
            .params(r#"{"layer"?, "red"?: 1.., "green"?: 1.., "blue"?: 1.., "preset"?: "natural"|"colorInfrared"}"#),
        C::new("raster.enhance", "Enhancement", "Style › Enhancement", |s, p| {
            let (br, co, ga) = (f(p, "brightness"), f(p, "contrast"), f(p, "gamma"));
            with(s, p, |rl, _| {
                if let Some(v) = br {
                    rl.style.brightness = v.clamp(-100.0, 100.0);
                }
                if let Some(v) = co {
                    rl.style.contrast = v.clamp(-100.0, 100.0);
                }
                if let Some(v) = ga {
                    rl.style.gamma = v.clamp(0.1, 10.0);
                }
                Ok(())
            })
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .when(need_raster)
        .params(r#"{"layer"?, "brightness"?: -100..100, "contrast"?: -100..100, "gamma"?: 0.1..10}"#),
        C::new("raster.setDefaultStyle", "Save as Default Raster Style", "Style › Rendering", set_default_style).roles(&[]).effects(crate::policy::Effects::NONE).view().params(
            r#"{"fromLayer"?: bool (the active or named layer's colour ramp stretch; the settings apply to every raster added later that would be a grey stretch, opened files and analysis results alike), "layer"?, "ramp"?, "invert"?: bool, "stretch"?: "none"|"minMax"|"percentClip"|"standardDeviation"|"histogramEqualize", "low"?, "high"?: %, "n"?: deviations, "resampling"?: "nearest"|"bilinear", "reset"?: bool (back to the black-to-white grey stretch)}"#,
        ),
        C::new("raster.info", "Raster Information", "Data › Information", info).view().when(need_raster).roles(&[]).effects(crate::policy::Effects::NONE).params(r#"{"layer"?}"#),
        C::new("share.exportRaster", "Export Raster", "Data › Information", |s, p| super::exports::write_here(s, p, "share.exportRaster")).view().when(need_raster).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files)).params(
            r#"{"layer"?, "path": "*.tif", "band"?: 1.. (rasters that aren't colour ones; a colour raster writes its colour bands), "cog"?: bool (a cloud-optimised GeoTIFF: 512 px tiles, Deflate, overviews; default false, an untiled GeoTIFF)}"#,
        ),
    ]
}

/// The raster layer named by `p` (default: active layer) and its data.
fn get(s: &Session, p: &Value) -> Result<(u32, Arc<Raster>), String> {
    let id = s.layer_id(p)?;
    let l = s.map().layer(id).ok_or("no such layer")?;
    let rl = l.raster.as_ref().ok_or_else(|| format!("{} isn't a raster layer", l.name))?;
    let data = rl.data.clone().ok_or_else(|| format!("{}: the raster's data couldn't be read", l.name))?;
    Ok((id, data))
}

/// A `min`/`max` parameter: one number for every band, or three (red, green, blue) on an RGB
/// layer, where `null` leaves a band's own.
#[derive(Clone, Copy)]
enum Bound {
    Unset,
    All(f64),
    Each([Option<f64>; 3]),
}

impl Bound {
    fn at(self, channel: usize) -> Option<f64> {
        match self {
            Bound::Unset => None,
            Bound::All(v) => Some(v),
            Bound::Each(v) => v.get(channel).copied().flatten(),
        }
    }
}

fn bound(p: &Value, k: &str) -> Result<Bound, String> {
    let Some(v) = p.get(k).filter(|v| !v.is_null()) else { return Ok(Bound::Unset) };
    let num = |v: &Value| v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())).filter(|x: &f64| x.is_finite());
    let Some(a) = v.as_array() else { return num(v).map(Bound::All).ok_or_else(|| format!("{k} must be a finite number")) };
    if a.len() != 3 {
        return Err(format!("{k} for each band is [red, green, blue], not {} values", a.len()));
    }
    let mut each = [None; 3];
    for (slot, x) in each.iter_mut().zip(a) {
        if !x.is_null() {
            *slot = Some(num(x).ok_or_else(|| format!("{k} must be finite numbers or null"))?);
        }
    }
    Ok(Bound::Each(each))
}

/// The value range channel `i` (the only one on a single band) is drawn over now, without the
/// pixels the mask hides.
fn channel_range(rl: &RasterLayer, r: &Raster, i: usize) -> Option<(f64, f64)> {
    let band = *rl.style.renderer.bands().get(i)?;
    let masked = rl.masked_stats();
    let stats = masked.as_ref().and_then(|m| m.get(i)).or_else(|| r.bands.get(band).map(|b| &b.stats))?;
    let n = Normalizer::for_channel(rl.style.stretch, i, stats, &r.data_type);
    Some((n.lo, n.hi))
}

/// The lowest and highest values of `band`, without the pixels the mask hides.
fn band_extent(rl: &RasterLayer, r: &Raster, band: usize) -> Option<(f64, f64)> {
    if rl.style.mask.is_empty() {
        let s = &r.bands.get(band)?.stats;
        return Some((s.min, s.max));
    }
    let s = rl.style.mask.stats(r, band);
    Some((s.min, s.max))
}

fn valid_range(lo: f64, hi: f64) -> bool {
    lo.is_finite() && hi.is_finite() && lo < hi
}

/// A raster layer's stretch as fixed bounds (one pair per channel on an RGB layer), for another
/// layer to share.
fn like_stretch(s: &Session, name: &str) -> Result<Stretch, String> {
    let rl = s.map().layer_by_name(name).and_then(|l| l.raster.as_ref()).ok_or_else(|| format!("no raster layer '{name}'"))?;
    let r = rl.data.as_ref().ok_or_else(|| format!("{name}: the raster's data couldn't be read"))?;
    let channels = rl.style.renderer.bands().len().min(3);
    let mut pairs = [[0.0; 2]; 3];
    for (i, pair) in pairs.iter_mut().enumerate().take(channels) {
        let (lo, hi) = channel_range(rl, r, i).ok_or_else(|| format!("{name} has no bands"))?;
        if !valid_range(lo, hi) {
            return Err(format!("{name} has no range of values to share"));
        }
        *pair = [lo, hi];
    }
    Ok(if channels == 3 { Stretch::custom_bands(pairs) } else { Stretch::custom(pairs[0][0], pairs[0][1]) })
}

fn stretch(s: &mut Session, p: &Value) -> CmdResult {
    if let Some(like) = like_name(p)? {
        let st = like_stretch(s, like)?;
        return with(s, p, |rl, _| {
            rl.style.stretch = st;
            Ok(())
        });
    }
    let code = ps(p, "type").ok_or("type is required")?;
    // A bound that was given but isn't a finite number is refused, not replaced by the layer's own.
    let (min_b, max_b) = (bound(p, "min")?, bound(p, "max")?);
    let custom = code.trim().eq_ignore_ascii_case("custom");
    with(s, p, |rl, r| {
        let rgb = matches!(rl.style.renderer, RasterRenderer::Rgb { .. });
        let each = matches!(min_b, Bound::Each(_)) || matches!(max_b, Bound::Each(_));
        if custom && each && !rgb {
            return Err("a bound for each band needs an RGB symbology".into());
        }
        // On an RGB layer a bound that isn't a number for every band starts from the range each
        // band is drawn over now.
        if custom && rgb && !matches!((min_b, max_b), (Bound::All(_), Bound::All(_))) {
            let mut pairs = [[0.0; 2]; 3];
            for (i, pair) in pairs.iter_mut().enumerate() {
                let (own_lo, own_hi) = channel_range(rl, r, i).ok_or("the raster has no bands")?;
                let (given_lo, given_hi) = (min_b.at(i), max_b.at(i));
                let (mut lo, mut hi) = (given_lo.unwrap_or(own_lo), given_hi.unwrap_or(own_hi));
                if given_lo.is_none() && given_hi.is_none() && lo >= hi {
                    let pad = (lo.abs() * 1e-6).max(0.5);
                    (lo, hi) = (lo - pad, hi + pad);
                }
                if !valid_range(lo, hi) {
                    return Err(format!("{} min {lo} must be a finite number less than max {hi}", ["red", "green", "blue"][i]));
                }
                *pair = [lo, hi];
            }
            rl.style.stretch = Stretch::custom_bands(pairs);
            return Ok(());
        }
        let (min, max) = (min_b.at(0).filter(|_| !each), max_b.at(0).filter(|_| !each));
        // Only a custom stretch reads the statistics: those of the band being drawn.
        let band = rl.style.renderer.bands().first().copied().unwrap_or(0);
        let stats = r.bands.get(band).map(|b| &b.stats);
        if custom && stats.is_none() {
            return Err("the raster has no bands".into());
        }
        let mut st = Stretch::from_code(code, f(p, "low"), f(p, "high"), f(p, "n"), min, max, stats)
            .ok_or_else(|| format!("unknown stretch '{code}'"))?;
        // A band with a single value has no range of its own: open one around it to edit from.
        if let Stretch::Custom { min: lo, max: hi, .. } = st
            && min.is_none()
            && max.is_none()
            && lo >= hi
        {
            // Padded by the value's magnitude too, or a large value would absorb ±0.5.
            let pad = (lo.abs() * 1e-6).max(0.5);
            st = Stretch::custom(lo - pad, hi + pad);
        }
        // Statistics can be NaN or infinite (a band of nodata), which `min >= max` alone misses.
        if let Stretch::Custom { min, max, .. } = st
            && !valid_range(min, max)
        {
            return Err(format!("min {min} must be a finite number less than max {max}"));
        }
        rl.style.stretch = st;
        Ok(())
    })
}

/// Applies `f` to the raster layer named by `p`.
fn with(s: &mut Session, p: &Value, f: impl FnOnce(&mut RasterLayer, &Raster) -> Result<(), String>) -> CmdResult {
    let (id, data) = get(s, p)?;
    let rl = s.map_mut().layer_mut(id).and_then(|l| l.raster.as_mut()).ok_or("no such layer")?;
    f(rl, &data)?;
    Ok(Value::Null)
}

/// A 1-based band parameter as a 0-based index inside the raster.
fn band(p: &Value, k: &str, r: &Raster) -> Result<Option<usize>, String> {
    match f(p, k) {
        None => Ok(None),
        Some(v) if v >= 1.0 && v <= r.band_count() as f64 => Ok(Some(v as usize - 1)),
        Some(v) => Err(format!("{k} {v}: the raster has bands 1–{}", r.band_count())),
    }
}

/// Adds a picture laid over the map (a KML ground overlay), kept in the project as a GeoTIFF.
/// `zoom` is the adding command's `"zoom"`, as for the layers beside it.
pub(crate) fn add_overlay(s: &mut Session, name: &str, r: raster::Raster, zoom: Option<bool>) -> CmdResult {
    let tif = raster::geotiff::write(&r, 0).map_err(|e| e.to_string())?;
    add_raster_after(s, name, RasterLayer::from_bytes(r, format!("{name}.tif"), tif), zoom)
}

/// Sets how rasters added from now on are drawn when they hold a single band of continuous values
/// (`Session::raster_defaults`): from a layer's colour ramp stretch, from parameters, or both (the
/// parameters win). With none, it returns the settings as they are.
fn set_default_style(s: &mut Session, p: &Value) -> CmdResult {
    let mut d = if b(p, "reset") == Some(true) { RasterDefaults::default() } else { s.raster_defaults.clone() };
    if b(p, "fromLayer") == Some(true) {
        let (id, _) = get(s, p)?;
        let l = s.map().layer(id).ok_or("no such layer")?;
        let style = &l.raster.as_ref().ok_or_else(|| format!("{} isn't a raster layer", l.name))?.style;
        let RasterRenderer::Stretch { ramp, invert, .. } = &style.renderer else {
            return Err(format!("{}: only a Stretch symbology can be the default (it has {})", l.name, style.renderer.label()));
        };
        if matches!(style.stretch, Stretch::Custom { .. }) {
            return Err(format!("{}: a custom stretch is in this raster's own units; pick another stretch type first", l.name));
        }
        d = RasterDefaults { ramp: ramp.clone(), invert: *invert, stretch: style.stretch, resampling: style.resampling };
    }
    if let Some(name) = ps(p, "ramp") {
        d.ramp = s.ramp(name)?;
    }
    if let Some(v) = b(p, "invert") {
        d.invert = v;
    }
    if let Some(code) = ps(p, "stretch") {
        if code.trim().eq_ignore_ascii_case("custom") {
            return Err("a custom stretch is in one raster's own units, so it can't be the default".into());
        }
        d.stretch = Stretch::from_code(code, f(p, "low"), f(p, "high"), f(p, "n"), None, None, None)
            .ok_or_else(|| format!("unknown stretch '{code}'"))?;
    }
    if let Some(name) = ps(p, "resampling") {
        d.resampling = Resampling::from_name(name).ok_or_else(|| format!("unknown resampling '{name}'"))?;
    }
    s.raster_defaults = d.sanitized();
    Ok(default_style_json(&s.raster_defaults))
}

/// The default raster style as `raster.setDefaultStyle` takes it.
pub fn default_style_json(d: &RasterDefaults) -> Value {
    let mut v =
        json!({"ramp": d.ramp.name(), "invert": d.invert, "stretch": d.stretch.code(), "resampling": format!("{:?}", d.resampling)});
    match d.stretch {
        Stretch::PercentClip { low, high } => {
            v["low"] = json!(low);
            v["high"] = json!(high);
        }
        Stretch::StandardDeviation { n } => v["n"] = json!(n),
        _ => {}
    }
    v
}

/// Adds a raster as a new layer, selects it and returns its id. A raster drawn the built-in way
/// for a single band of continuous values takes the session's default raster style instead:
/// opened files and analysis results (a slope, an NDVI) alike, since every producer adds here.
pub fn add_raster(s: &mut Session, name: &str, layer: RasterLayer) -> CmdResult {
    add_raster_after(s, name, layer, None)
}

/// [`add_raster`], with the adding command's `"zoom"`: `Some(true)` frames what was added,
/// `Some(false)` keeps the view, and `None` frames only the first content on an empty map.
pub(crate) fn add_raster_after(s: &mut Session, name: &str, mut layer: RasterLayer, zoom: Option<bool>) -> CmdResult {
    // A scan has no coordinates: it is laid over what the view shows, and says so.
    let mut laid = None;
    if let Some(d) = layer.data.as_mut().filter(|d| !d.georeferenced) {
        let bbox = scan_box(s, d.width, d.height);
        Arc::make_mut(d).place(bbox).map_err(|e| format!("{name}: {e}"))?;
        layer.placement = Some(bbox);
        laid = Some(bbox);
    }
    if let Some(d) = layer.data.clone()
        && s.raster_defaults != RasterDefaults::default()
        && layer.style == RasterStyle::default_for(&d)
    {
        layer.style = RasterStyle::default_with(&d, &s.raster_defaults);
    }
    let mut unique = name.to_string();
    let mut i = 1;
    while s.map().layer_by_name(&unique).is_some() {
        i += 1;
        unique = format!("{name} ({i})");
    }
    let (w, h, n, overview) = layer.data.as_ref().map(|d| (d.width, d.height, d.band_count(), d.overview)).unwrap_or_default();
    // What reading it left out (a colour table, names), said once the layer has its name.
    let rounded = layer.data.as_deref().and_then(rounded_count);
    // Said here for pixels added with the layer; for those landing later, by `notice_landed_rasters`.
    let lost = layer.lost_unsaid();
    let unsaid = layer.pixels_unsaid();
    // Read in place, the map shows the full image as it draws, but statistics and tools read the
    // overview held: said so.
    let held = unsaid.filter(|_| layer.in_place).and_then(|d| crate::in_place_overview(&d));
    let in_place = layer.in_place;
    let empty_before = super::map_is_empty(s);
    let id = s.map_mut().add_raster(unique.clone(), layer);
    s.active_layer = Some(id);
    // Read in place, its own notice says the overview it holds (`held`); the result names the full
    // size all the same.
    if let Some((fw, fh)) = overview.filter(|_| !in_place) {
        s.notify(format!("{unique}: showing a {w} × {h} overview of the {fw} × {fh} raster"), false);
    }
    for l in lost {
        s.notify(format!("{unique}: {l}"), false);
    }
    if let Some(held) = held {
        s.notify(format!("{unique}: {held}"), false);
    }
    if let Some(count) = rounded {
        let held = "pixels are held as 32-bit floats, exact for whole numbers up to 16,777,216 and to about 7 significant digits";
        s.notify(format!("{unique}: {count} rounded as the file was read: {held}, so an export writes the rounded values"), false);
    }
    if zoom.unwrap_or(empty_before)
        && let Some(e) = s.map().layer(id).map(|l| l.extent())
    {
        s.zoom_to(&e);
    }
    let mut v = json!({"layer": id, "width": w, "height": h, "bands": n});
    if let Some(bbox) = laid {
        let note = "it has no coordinates, so it is drawn over the current view until it is georeferenced";
        s.notify(format!("{unique}: {note}"), false);
        v["georeferenced"] = json!(false);
        v["placedOver"] = json!(bbox);
        v["note"] = json!(note);
    }
    // Callers that only read the result (the MCP server, scripts) learn about the overview too.
    if let (Some(o), Some((fw, fh))) = (v.as_object_mut(), overview) {
        o.insert("overviewOf".into(), json!([fw, fh]));
    }
    Ok(v)
}

/// How many of a raster's values changed when read as 32-bit floats, as notices say it: "3 values
/// were", and by band ("8 values (band 1: 3, band 2: 5) were") when it has more than one.
/// `None` when none did.
fn rounded_count(r: &Raster) -> Option<String> {
    let total: u64 = r.rounded.iter().sum();
    if total == 0 {
        return None;
    }
    let by_band: Vec<String> = r.rounded.iter().enumerate().filter(|(_, n)| **n > 0).map(|(i, n)| format!("band {}: {n}", i + 1)).collect();
    let (values, verb) = values_were(total);
    Some(if r.band_count() > 1 { format!("{values} ({}) {verb}", by_band.join(", ")) } else { format!("{values} {verb}") })
}

/// "1 value" and "was", or "`n` values" and "were".
fn values_were(n: u64) -> (String, &'static str) {
    if n == 1 { ("1 value".into(), "was") } else { (format!("{n} values"), "were") }
}

/// The longitude/latitude box (west, south, east, north) a scan of `w` × `h` pixels is laid over:
/// the largest one in the view, centred, at the picture's shape and 90% of the view's size.
fn scan_box(s: &Session, w: u32, h: u32) -> [f64; 4] {
    let mut view = super::workspace::view_extent(s);
    if view.is_empty() || view.width() <= 0.0 || view.height() <= 0.0 {
        view = Extent::new(-30.0, -20.0, 30.0, 20.0);
    }
    let c = view.center();
    // A degree of latitude is longer than one of longitude by 1 / cos(latitude) on the map.
    let squash = c.y.to_radians().cos().abs().max(0.05);
    let (aw, ah) = (f64::from(w.max(1)), f64::from(h.max(1)));
    let across = (view.width() * 0.9).min(view.height() * 0.9 * aw / (ah * squash));
    let up = across * ah * squash / aw;
    [c.x - across / 2.0, c.y - up / 2.0, c.x + across / 2.0, c.y + up / 2.0]
}

/// Whether `"as": "scan"` was given for a TIFF, PNG or JPEG named `name`.
pub(crate) fn wants_scan(p: &Value, name: &str) -> Result<bool, String> {
    if !as_scan(p)? {
        return Ok(false);
    }
    let ext = Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if matches!(ext.as_str(), "tif" | "tiff" | "png" | "jpg" | "jpeg") {
        Ok(true)
    } else {
        Err(format!("{name}: \"as\": \"scan\" opens a TIFF, PNG or JPEG"))
    }
}

/// Whether `"as"` asks for a scan, before any file is known: absent or null is no, `"scan"` is
/// yes, and anything else (another word, a number) is refused rather than ignored.
pub(crate) fn as_scan(p: &Value) -> Result<bool, String> {
    match p.get("as") {
        None | Some(Value::Null) => Ok(false),
        Some(Value::String(s)) if s == "scan" => Ok(true),
        Some(other) => Err(format!("unknown \"as\": {other} (\"scan\")")),
    }
}

/// Says what reading several rasters left out, each `(raster, line)`: one notice a kind (the same
/// line), naming the rasters that share it (three, then a count), as AGENTS.md's Never silent asks.
///
/// Lines that differ only in their numbers ("1 scale…", "2 scale…") are one kind. When the
/// numbers are the same for every raster the kind is said once; when they differ, each raster's
/// own line is said (three rasters or fewer), or the range they give, for each of them ("4
/// rasters (a, b, c, …), each: 1–2 scale…"): never summed, since a size limit isn't a count.
pub(crate) fn say_losses(s: &mut Session, losses: Vec<(String, String)>) {
    // A line's kind: its text with each number taken out, and those numbers. A number is a run of
    // digits standing as a word of its own: one inside a name (`scene2024.tif`) stays text.
    fn kind_of(line: &str) -> (Vec<&str>, Vec<u64>) {
        let apart = |c: Option<char>| c.is_none_or(|c| c.is_whitespace() || matches!(c, '(' | ')' | ',' | ';' | ':'));
        let (mut parts, mut numbers, mut from) = (Vec::new(), Vec::new(), 0);
        let bytes = line.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i].is_ascii_digit() {
                let start = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                let before = line[..start].chars().next_back();
                let after = line[i..].chars().next();
                if !(apart(before) && apart(after)) {
                    continue;
                }
                match line[start..i].parse::<u64>() {
                    Ok(n) => {
                        parts.push(&line[from..start]);
                        numbers.push(n);
                        from = i;
                    }
                    Err(_) => continue,
                }
            } else {
                i += 1;
            }
        }
        parts.push(&line[from..]);
        (parts, numbers)
    }
    // Each kind: its text parts, each number's lowest and highest, and the rasters with their
    // own lines.
    type Kind = (Vec<String>, Vec<(u64, u64)>, Vec<(String, String)>);
    let mut kinds: Vec<Kind> = Vec::new();
    for (name, line) in losses {
        let (parts, numbers) = kind_of(&line);
        let parts_owned: Vec<String> = parts.iter().map(|p| p.to_string()).collect();
        match kinds.iter_mut().find(|(p, n, _)| *p == parts_owned && n.len() == numbers.len()) {
            Some((_, ranges, named)) => {
                for (r, n) in ranges.iter_mut().zip(&numbers) {
                    *r = (r.0.min(*n), r.1.max(*n));
                }
                named.push((name, line));
            }
            None => kinds.push((parts_owned, numbers.iter().map(|n| (*n, *n)).collect(), vec![(name, line)])),
        }
    }
    for (parts, ranges, named) in kinds {
        let differ = ranges.iter().any(|(lo, hi)| lo != hi);
        // A few rasters whose numbers differ: each its own, so none is lost in a range.
        if differ && named.len() <= 3 {
            for (name, line) in named {
                s.notify(format!("{name}: {line}"), false);
            }
            continue;
        }
        let names: Vec<String> = named.into_iter().map(|(name, _)| name).collect();
        let mut line = String::new();
        for (i, part) in parts.iter().enumerate() {
            line.push_str(part);
            if let Some((lo, hi)) = ranges.get(i) {
                line.push_str(&if lo == hi { lo.to_string() } else { format!("{lo}–{hi}") });
            }
        }
        let who = match names.as_slice() {
            [one] => one.clone(),
            _ => {
                let more = if names.len() > 3 { ", …" } else { "" };
                format!("{} rasters ({}{more})", names.len(), names[..names.len().min(3)].join(", "))
            }
        };
        let each = if differ { ", each" } else { "" };
        s.notify(format!("{who}{each}: {line}"), false);
    }
}

/// Opens a raster file by path.
pub fn add_path(s: &mut Session, path: &str, name: Option<&str>, scan: bool) -> CmdResult {
    let mut r = match super::reads::take(s, &read_key(path)) {
        Some(r) => r,
        None => read_raster(path),
    }?;
    if scan {
        r = r.into_scan().map_err(|e| format!("{path}: {e}"))?;
    }
    let name = name.map(str::to_string).unwrap_or_else(|| r.name.clone());
    // Absolute, so the project finds it again from anywhere (`absolute`, unlike `canonicalize`,
    // gives plain paths on Windows rather than `\\?\` ones).
    let full = std::path::absolute(path).map(|p| p.display().to_string()).unwrap_or_else(|_| path.to_string());
    add_raster(s, &name, RasterLayer::from_path(r, full))
}

/// The raster at `path`, read whole.
pub(crate) fn read_raster(path: &str) -> Result<Raster, String> {
    raster::read_path(Path::new(path)).map_err(|e| format!("{path}: {e}"))
}

/// The key [`add_path`]'s read of `path` is taken by (see [`super::reads`]).
pub(crate) fn read_key(path: &str) -> String {
    format!("raster:{path}")
}

/// A raster GDAL converts and the conversion it read.
#[cfg(not(target_arch = "wasm32"))]
type GdalRead = Result<(rustgis_data::raster::gdal::Converted, Raster), String>;

/// The raster GDAL converts from `path` (`layer` its subdataset, `crs` a coordinate system the
/// file doesn't say), with the conversion.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn read_gdal(path: &str, layer: Option<&str>, crs: Option<&str>) -> GdalRead {
    use rustgis_data::raster::gdal;
    let c = gdal::convert(Path::new(path), layer, crs).map_err(|e| format!("{path}: {e}"))?;
    let r = raster::read_converted(Path::new(path), &c).map_err(|e| format!("{path}: {e}"))?;
    Ok((c, r))
}

/// The key [`add_gdal_path`]'s read of `path` is taken by (see [`super::reads`]).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn gdal_key(path: &str) -> String {
    format!("gdal:{path}")
}

/// Adds a raster GDAL reads (converted once to a GeoTIFF in the cache): `"layer"` picks a
/// subdataset (a NetCDF variable) and `"crs"` gives a coordinate system the file doesn't say.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn add_gdal_path(s: &mut Session, p: &Value, path: &str) -> CmdResult {
    let text = |k: &str| ps(p, k).map(str::trim).filter(|v| !v.is_empty());
    let (c, r) = match super::reads::take::<GdalRead>(s, &gdal_key(path)) {
        Some(read) => read,
        None => read_gdal(path, text("layer"), text("crs")),
    }?;
    if c.assumed_lonlat {
        s.notify(format!("{path} doesn't say its coordinate system; its coordinates look like longitude/latitude, so it's read as WGS 84 (give \"crs\" if not)"), false);
    }
    if !c.others.is_empty() {
        let shown: Vec<&str> = c.others.iter().take(20).map(String::as_str).collect();
        s.notify(format!("{path} has more variables ({}): add one with \"layer\"", shown.join(", ")), false);
    }
    let name = text("name").map(str::to_string).unwrap_or_else(|| r.name.clone());
    // A project names the file itself, with the subdataset and coordinate system chosen, and
    // converts it again (from the cache, while it is there) when it opens.
    let full = std::path::absolute(path).map(|p| p.display().to_string()).unwrap_or_else(|_| path.to_string());
    let mut rl = RasterLayer::from_path(r, full);
    rl.subdataset = c.subdataset.clone();
    rl.crs = text("crs").map(str::to_string);
    add_raster(s, &name, rl)
}

/// Adds an uploaded or downloaded JPEG 2000 raster, converted by GDAL to a GeoTIFF that the project
/// keeps (it opens again without GDAL or the source file, as an uploaded GeoTIFF does).
#[cfg(not(target_arch = "wasm32"))]
fn add_jpeg2000_bytes(s: &mut Session, p: &Value, name: &str, bytes: Vec<u8>, zoom: Option<bool>) -> CmdResult {
    let crs = ps(p, "crs").map(str::trim).filter(|c| !c.is_empty());
    let c = raster::gdal::convert_jpeg2000(bytes, crs, MAX_UPLOAD_BYTES as u64).map_err(|e| format!("{name}: {e}"))?;
    let stem = Path::new(name).file_stem().and_then(|s| s.to_str()).unwrap_or("raster");
    let file_name = format!("{stem}.tif");
    let r = raster::read_bytes(&file_name, &c.tif).map_err(|e| format!("{name}: {e}"))?;
    if c.assumed_lonlat {
        s.notify(format!("{name} doesn't say its coordinate system; its coordinates look like longitude/latitude, so it's read as WGS 84 (give \"crs\" if not)"), false);
    }
    let title = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).map(str::to_string).unwrap_or_else(|| r.name.clone());
    add_raster_after(s, &title, RasterLayer::from_bytes(r, file_name, c.tif), zoom)
}

fn add_bytes(s: &mut Session, p: &Value) -> CmdResult {
    use base64::Engine;
    let name = ps(p, "fileName").ok_or("fileName is required")?.to_string();
    let data = ps(p, "data").ok_or("data is required")?;
    // Base64 is 4 characters per 3 bytes; refuse files past the upload limit before decoding.
    if data.len() / 4 * 3 > MAX_UPLOAD_BYTES {
        return Err(format!("{name} is too large to open"));
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(data.as_bytes()).map_err(|e| format!("data: {e}"))?;
    add_named_bytes(s, p, name, bytes, b(p, "zoom"))
}

/// Adds a file's contents (an upload, a download) as its name's extension says.
pub(crate) fn add_named_bytes(s: &mut Session, p: &Value, name: String, bytes: Vec<u8>, zoom: Option<bool>) -> CmdResult {
    // A geotagged photo (an upload, a drop): a point in the photo layer, so several uploads
    // gather in one.
    let scan = wants_scan(p, &name)?;
    if !scan && rustgis_data::photo::is_photo_name(&name) {
        let layer = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).unwrap_or(super::layer::PHOTOS).to_string();
        return super::layer::add_photos_to(s, &layer, zoom, |reader| reader.add(&name, &bytes));
    }
    // JPEG 2000 (an upload or a download): through GDAL, on the desktop.
    if raster::is_jpeg2000_name(&name) {
        #[cfg(not(target_arch = "wasm32"))]
        return add_jpeg2000_bytes(s, p, &name, bytes, zoom);
        #[cfg(target_arch = "wasm32")]
        return Err(JPEG2000_WEB_LIMITATION.into());
    }
    if raster::is_raster_name(&name) {
        let mut r = raster::read_bytes(&name, &bytes).map_err(|e| format!("{name}: {e}"))?;
        if scan {
            r = r.into_scan().map_err(|e| format!("{name}: {e}"))?;
        }
        let title = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).map(str::to_string).unwrap_or_else(|| r.name.clone());
        return add_raster_after(s, &title, RasterLayer::from_bytes(r, name, bytes), zoom);
    }
    // An MBTiles archive: held in memory and drawn as a tile layer.
    if rustgis_data::mbtiles::is_mbtiles_name(&name) {
        let url = crate::render::rustgis_tiles::mbtiles::register(&name, &bytes)?;
        let mut q = json!({"url": url, "kind": "mbtiles", "top": true});
        if let Some(n) = ps(p, "name") {
            q["name"] = json!(n);
        }
        return super::tiles::add(s, &q);
    }
    // A spreadsheet's sheet.
    if rustgis_data::sheets::is_sheet_name(&name) {
        return super::layer::add_sheet(s, p, &name, &bytes, zoom);
    }
    // Files of several layers: zipped Shapefiles, KML/KMZ, GPX, GeoRSS, LandXML, CityJSON.
    if rustgis_data::io::is_multi_layer_name(&name) {
        return super::layer::add_layers(s, p, &name, &bytes, zoom);
    }
    // GeoParquet and FlatGeobuf uploads (binary vector files).
    if rustgis_data::source::is_source_name(&name) {
        return super::layer::add_source(s, p, super::layer::Origin::Bytes(&name, std::sync::Arc::new(bytes)), zoom);
    }
    let text = String::from_utf8(bytes).map_err(|_| format!("{name} is not a text file"))?;
    // The delimited-text and naming parameters carry over (not the data, already decoded).
    let mut q = json!({"fileName": name, "text": text});
    for k in ["name", "x", "y", "wkt", "crs", "delimiter", "layer", "paperSpace"] {
        if let Some(v) = p.get(k) {
            q[k] = v.clone();
        }
    }
    super::layer::add_data_text_after(s, &q, zoom)
}

fn symbology(s: &mut Session, p: &Value) -> CmdResult {
    let (id, r) = get(s, p)?;
    let kind = ps(p, "type").unwrap_or("stretch").to_string();
    // A custom ramp deleted from the project is still found on the raster that uses it.
    let ramp = ps(p, "ramp")
        .map(|n| {
            s.ramp(n).or_else(|e| {
                s.map()
                    .layer(id)
                    .and_then(|l| l.raster.as_ref())
                    .and_then(|rl| rl.style.renderer.ramp())
                    .filter(|r| r.name().eq_ignore_ascii_case(n.trim()))
                    .ok_or(e)
            })
        })
        .transpose()?;
    let bnd = band(p, "band", &r)?;
    with(s, p, |rl, r| {
        let cur_band = rl.style.renderer.bands().first().copied().unwrap_or(0);
        let bi = bnd.unwrap_or(cur_band).min(r.band_count().saturating_sub(1));
        let cur_ramp = rl.style.renderer.ramp();
        rl.style.renderer = match kind.to_ascii_lowercase().replace([' ', '_', '-'], "").as_str() {
            "stretch" => {
                let ramp = ramp.or(cur_ramp.filter(|r| !r.is_qualitative())).unwrap_or_else(Ramp::gray);
                // Left out, a stretch keeps its own inversion (a band change); a new one isn't inverted.
                let cur_invert = matches!(rl.style.renderer, RasterRenderer::Stretch { invert: true, .. });
                let invert = b(p, "invert").unwrap_or(cur_invert);
                RasterRenderer::Stretch { band: bi, ramp, invert }
            }
            "rgb" => {
                if r.band_count() < 3 {
                    return Err("RGB needs a raster with at least three bands".into());
                }
                // Keep a band combination already chosen.
                match rl.style.renderer {
                    RasterRenderer::Rgb { red, green, blue } => RasterRenderer::Rgb { red, green, blue },
                    _ => RasterRenderer::Rgb { red: 0, green: 1, blue: 2 },
                }
            }
            "classify" => {
                let classes = f(p, "classes").map(|c| c.clamp(1.0, 32.0) as usize).unwrap_or(5);
                let method = ps(p, "method").and_then(ClassMethod::from_name).unwrap_or_default();
                if method == ClassMethod::Manual {
                    return Err("manual breaks are for Graduated Colors; a raster's classes are worked out".into());
                }
                let ramp = ramp.or(cur_ramp.filter(|r| !r.is_qualitative())).unwrap_or(Ramp::Ember);
                classify_masked(r, bi, classes, method, ramp, &rl.style.mask)
            }
            "unique" | "uniquevalues" => {
                if r.bands.get(bi).and_then(|b| b.stats.categories.as_ref()).is_none() {
                    return Err("Unique Values needs whole-number pixels with at most 256 distinct values".into());
                }
                unique_values(r, bi, ramp.unwrap_or(Ramp::Harbour))
            }
            "colormap" => {
                if !matches!(r.color, raster::ColorHint::Palette(_)) {
                    return Err("the raster has no colour table".into());
                }
                RasterRenderer::Colormap
            }
            "shadedrelief" | "hillshade" => RasterRenderer::ShadedRelief {
                band: bi,
                ramp: ramp.or(Some(Ramp::Terrain)).filter(|_| !b(p, "grey").unwrap_or(false)),
                azimuth: f(p, "azimuth").unwrap_or(315.0).rem_euclid(360.0),
                altitude: f(p, "altitude").unwrap_or(45.0).clamp(0.0, 90.0),
                z_factor: f(p, "zFactor").unwrap_or(1.0).clamp(0.001, 1000.0),
            },
            other => return Err(format!("unknown raster symbology '{other}'")),
        };
        if matches!(rl.style.renderer, RasterRenderer::Colormap | RasterRenderer::UniqueValues { .. } | RasterRenderer::Classify { .. }) {
            rl.style.stretch = Stretch::None;
        } else if rl.style.stretch == Stretch::None && r.data_type != "UInt8" {
            rl.style.stretch = Stretch::default();
        }
        Ok(())
    })
}

/// Most values a mask lists.
const MAX_MASK_VALUES: usize = 256;

/// The values of `raster.mask`'s `values`: a list of numbers, or numbers in text.
fn mask_values(v: &Value) -> Result<Vec<f64>, String> {
    let num = |v: &Value| v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())).filter(|x: &f64| x.is_finite());
    let mut out = match v {
        Value::Null => Vec::new(),
        Value::Array(a) => a.iter().map(|x| num(x).ok_or_else(|| format!("mask value {x} isn't a number"))).collect::<Result<_, _>>()?,
        Value::String(t) => t
            .split([',', ';', ' ', '\t', '\n'])
            .filter(|x| !x.trim().is_empty())
            .map(|x| {
                x.trim().parse::<f64>().ok().filter(|x| x.is_finite()).ok_or_else(|| format!("mask value '{}' isn't a number", x.trim()))
            })
            .collect::<Result<_, _>>()?,
        Value::Number(_) => vec![num(v).ok_or("mask values must be finite numbers")?],
        _ => return Err("values must be a list of numbers".into()),
    };
    if out.len() > MAX_MASK_VALUES {
        return Err(format!("a mask lists at most {MAX_MASK_VALUES} values"));
    }
    out.sort_by(f64::total_cmp);
    out.dedup();
    Ok(out)
}

/// Sets which pixel values a raster layer hides: values drawn as no data, and a range.
fn mask(s: &mut Session, p: &Value) -> CmdResult {
    let values = p.get("values").map(mask_values).transpose()?;
    // A bound given as null (or empty text) is removed; one given as anything but a number is refused.
    let bound = |k: &str| -> Result<Option<Option<f64>>, String> {
        match p.get(k) {
            None => Ok(None),
            Some(Value::Null) => Ok(Some(None)),
            Some(Value::String(t)) if t.trim().is_empty() => Ok(Some(None)),
            Some(_) => f(p, k).map(|x| Some(Some(x))).ok_or_else(|| format!("{k} must be a finite number")),
        }
    };
    let (min, max) = (bound("min")?, bound("max")?);
    let (inside, clear) = (b(p, "inside"), b(p, "clear").unwrap_or(false));
    with(s, p, |rl, r| {
        let mut m = if clear { RasterMask::default() } else { rl.style.mask.clone() };
        if let Some(v) = values {
            m.values = v;
        }
        if let Some(v) = min {
            m.min = v;
        }
        if let Some(v) = max {
            m.max = v;
        }
        if let Some(v) = inside {
            m.inside = v;
        }
        if let (Some(lo), Some(hi)) = (m.min, m.max)
            && lo > hi
        {
            return Err(format!("min {lo} must not be more than max {hi}"));
        }
        // Class breaks worked out from the pixels shown are worked out again; breaks edited by
        // hand are kept.
        if let RasterRenderer::Classify { band, method, ramp, breaks, classes } = &rl.style.renderer {
            let n = if *classes > 0 { *classes } else { breaks.len().max(1) };
            let auto = classify_masked(r, *band, n, *method, ramp.clone(), &rl.style.mask);
            if matches!(&auto, RasterRenderer::Classify { breaks: b, .. } if b == breaks) {
                rl.style.renderer = classify_masked(r, *band, n, *method, ramp.clone(), &m);
            }
        }
        rl.style.mask = m;
        Ok(())
    })
}

fn bands(s: &mut Session, p: &Value) -> CmdResult {
    let (_, r) = get(s, p)?;
    if r.band_count() < 3 {
        return Err("band combinations need a raster with at least three bands".into());
    }
    let (red, green, blue) = match ps(p, "preset").map(|v| v.to_ascii_lowercase().replace([' ', '_', '-'], "")) {
        Some(v) if v == "natural" || v == "naturalcolor" => (0, 1, 2),
        Some(v) if v == "colorinfrared" || v == "cir" => {
            if r.band_count() < 4 {
                return Err("Color Infrared needs a near-infrared band 4".into());
            }
            (3, 0, 1)
        }
        Some(v) => return Err(format!("unknown band combination '{v}'")),
        None => {
            let cur = match get(s, p)
                .ok()
                .and_then(|(id, _)| s.map().layer(id).and_then(|l| l.raster.as_ref()).map(|rl| rl.style.renderer.clone()))
            {
                Some(RasterRenderer::Rgb { red, green, blue }) => (red, green, blue),
                _ => (0, 1, 2),
            };
            (band(p, "red", &r)?.unwrap_or(cur.0), band(p, "green", &r)?.unwrap_or(cur.1), band(p, "blue", &r)?.unwrap_or(cur.2))
        }
    };
    with(s, p, |rl, r| {
        let before = match rl.style.renderer {
            RasterRenderer::Rgb { red, green, blue } => Some([red, green, blue]),
            _ => None,
        };
        // The range each channel is drawn over, read before the bands change.
        let drawn = before.filter(|_| matches!(rl.style.stretch, Stretch::Custom { .. })).and_then(|b| {
            let ranges: Vec<_> = (0..3).filter_map(|i| channel_range(rl, r, i)).collect();
            (ranges.len() == 3).then_some((b, ranges))
        });
        rl.style.renderer = RasterRenderer::Rgb { red, green, blue };
        // A channel moved to another band is redrawn over that band's range; the others keep theirs.
        if let Some((old, ranges)) = drawn {
            let new = [red, green, blue];
            if old != new {
                let mut pairs = [[0.0; 2]; 3];
                for (i, pair) in pairs.iter_mut().enumerate() {
                    let kept = ranges.get(i).copied().unwrap_or((0.0, 1.0));
                    let (lo, hi) = new
                        .get(i)
                        .filter(|b| old.get(i) != Some(*b))
                        .and_then(|b| band_extent(rl, r, *b))
                        .filter(|(lo, hi)| valid_range(*lo, *hi))
                        .unwrap_or(kept);
                    *pair = [lo, hi];
                }
                rl.style.stretch = Stretch::custom_bands(pairs);
            }
        }
        Ok(())
    })
}

fn info(s: &mut Session, p: &Value) -> CmdResult {
    let (id, r) = get(s, p)?;
    let rl = s.map().layer(id).and_then(|l| l.raster.as_ref()).ok_or("no layer")?;
    let (cx, cy) = r.cell_size();
    Ok(json!({
        "name": r.name,
        "source": rl.path.clone().or(rl.file_name.clone()),
        "width": r.width, "height": r.height,
        "overviewOf": r.overview.map(|(w, h)| json!([w, h])),
        "bands": r.bands.iter().map(|b| json!({
            "name": b.name, "min": b.stats.min, "max": b.stats.max, "mean": b.stats.mean, "stdDev": b.stats.std_dev,
            "validPixels": b.stats.count, "distinctValues": b.stats.categories.as_ref().map(Vec::len),
        })).collect::<Vec<_>>(),
        "dataType": r.data_type,
        "georeferenced": r.georeferenced,
        "noData": r.nodata,
        "crs": r.crs.label(),
        "cellSize": [cx, cy],
        "extent": {"west": r.extent().min_x, "south": r.extent().min_y, "east": r.extent().max_x, "north": r.extent().max_y},
        "renderer": rl.style.renderer.label(),
        "stretch": rl.style.stretch.label(),
        "mask": (!rl.style.mask.is_empty()).then(|| rl.style.mask.label()),
    }))
}

/// GeoTIFF bytes of a raster layer (band is 1-based; RGB rasters keep their colour bands; `cog`
/// makes it a cloud-optimised one).
pub fn geotiff_bytes(s: &Session, p: &Value) -> Result<(String, Vec<u8>), String> {
    let (_, r) = get(s, p)?;
    let bi = band(p, "band", &r)?.unwrap_or(0);
    let bytes = if b(p, "cog") == Some(true) { raster::cogwrite::write(&r, bi) } else { raster::geotiff::write(&r, bi) };
    Ok((r.name.clone(), bytes.map_err(|e| e.to_string())?))
}

/// `share.exportRaster`'s inputs, and the work that writes the raster as a GeoTIFF (a
/// cloud-optimised one with `cog`).
pub(crate) fn export_work(s: &Session, p: &Value) -> Result<super::exports::Work, String> {
    let path = ps(p, "path").ok_or("path is required")?.to_string();
    let (_, r) = get(s, p)?;
    let bi = band(p, "band", &r)?.unwrap_or(0);
    let cog = b(p, "cog") == Some(true);
    Ok(Box::new(move || write_geotiff(&r, bi, cog, &path)))
}

/// Writes raster `r` (band `bi`, or its colour bands) to `path`, and says what the file holds.
fn write_geotiff(r: &Raster, bi: usize, cog: bool, path: &str) -> Result<super::exports::Wrote, String> {
    let cog = if cog {
        // Tiles go to the file as they are made: the whole file is never held in memory.
        Some(raster::cogwrite::write_file(r, bi, Path::new(path)).map_err(|e| format!("{path}: {e}"))?)
    } else {
        let bytes = raster::geotiff::write(r, bi).map_err(|e| e.to_string())?;
        rustgis_data::io::write_atomic(Path::new(path), &bytes).map_err(|e| format!("{path}: {e}"))?;
        None
    };
    let mut note = match r.overview {
        Some((fw, fh)) => format!(" (the {} × {} overview of the {fw} × {fh} raster)", r.width, r.height),
        None => String::new(),
    };
    // Values changed when the file was read are written as they are held: those of the bands the
    // writer writes, as it chooses them.
    let bands = if cog.is_some() {
        raster::cogwrite::written_bands(r, bi).map_err(|e| e.to_string())?
    } else {
        raster::geotiff::written_bands(r, bi)
    };
    let written: u64 = bands.iter().filter_map(|b| r.rounded.get(*b)).sum();
    if written > 0 {
        let (values, verb) = values_were(written);
        note.push_str(&format!("; {values} in it {verb} rounded to 32-bit floats when {} was read", r.name));
    }
    // The bands the writer leaves out (it writes `band`, or a colour raster's colour bands, which
    // `band` doesn't change) are said, names and all.
    if bands.len() < r.bands.len() {
        let which = if bands == [bi] { "give \"band\" for another" } else { "its colour bands" };
        note.push_str(&format!("; {} of its {} bands written ({which})", bands.len(), r.bands.len()));
    }
    // The file keeps the first written band's class names only (as dataset `CLASS_` items).
    let unnamed: Vec<String> =
        bands.iter().skip(1).filter(|b| r.bands.get(**b).is_some_and(|b| !b.classes.is_empty())).map(|b| (b + 1).to_string()).collect();
    if !unnamed.is_empty() {
        note.push_str(&format!("; the class names of band(s) {} aren't written: a GeoTIFF keeps the first band's", unnamed.join(", ")));
    }
    let spaced = raster::gdal_metadata_changed(r, &bands);
    if spaced > 0 {
        note.push_str(&format!("; {spaced} band or class name(s) held control characters, written as spaces"));
    }
    let mut v = match r.overview {
        Some((fw, fh)) => json!({"path": path, "width": r.width, "height": r.height, "overviewOf": [fw, fh]}),
        None => json!({"path": path}),
    };
    if let (Some(o), Some(c)) = (v.as_object_mut(), cog) {
        o.insert("cog".into(), json!({"overviews": c.levels.len().saturating_sub(1), "bigTiff": c.big, "bytes": c.bytes}));
    }
    Ok(super::exports::Wrote {
        answer: v,
        notes: vec![format!("Exported {path}{note}")],
        written_path: Some(path.to_string()),
        then: None,
        #[cfg(not(target_arch = "wasm32"))]
        owner: None,
    })
}

/// Pixel values of the topmost visible raster under a longitude/latitude point.
pub fn identify(s: &Session, lon: f64, lat: f64) -> Option<Value> {
    let scale = s.scale();
    let c = rustgis_geom::Coord::new(lon, lat);
    s.map().layers.iter().filter(|l| s.map().draws_at(l, scale)).find_map(|l| {
        let r = l.raster.as_ref()?.data.as_ref()?;
        let (col, row) = r.pixel_at(c)?;
        let vals: Vec<Option<f32>> = (0..r.band_count()).map(|b| r.value(b, col, row)).collect();
        if vals.iter().all(Option::is_none) {
            return None;
        }
        let mut attrs = serde_json::Map::new();
        for (b, v) in r.bands.iter().zip(&vals) {
            attrs.insert(b.name.clone(), json!(v.map(|v| rustgis_map::symbology::fmt_num(v as f64)).unwrap_or_else(|| "NoData".into())));
        }
        attrs.insert("Pixel".into(), json!(format!("{col}, {row}")));
        Some(json!({"layer": l.name, "layerId": l.id, "raster": true, "attributes": attrs}))
    })
}

/// Binds the primary raster and optional actual like raster without copying pixel data.
fn stretch_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let s = input;
    let mut inputs = super::Inputs::single_layer(s, p)?;
    if let Some(name) = like_name(p)? {
        inputs.extend(super::Inputs::named(s.map(), [name])?);
    }
    Ok(inputs)
}

/// Selects only the literal like raster consumed by stretch.
fn like_name(p: &Value) -> Result<Option<&str>, String> {
    p.get("like").filter(|v| !v.is_null()).map(|v| v.as_str().ok_or_else(|| "like is a raster layer's name".to_string())).transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rasters that leave out the same thing are said together; when their numbers differ, each
    /// is said with its own (a few) or the range is said for each of them (many), never a sum.
    #[test]
    fn losses_are_said_by_kind_and_per_raster_when_they_differ() {
        let said = |losses: &[(&str, &str)]| {
            let mut s = Session::new(crate::Project::default());
            let mark = s.notice_mark();
            say_losses(&mut s, losses.iter().map(|(n, l)| (n.to_string(), l.to_string())).collect());
            s.notices_since(mark).iter().map(|n| n.text.clone()).collect::<Vec<_>>()
        };
        let cut = |n: u32| format!("{n} band or class name(s) cut to 256 characters");
        let (one, two, three) = (cut(1), cut(2), cut(3));
        assert_eq!(said(&[("a", &two), ("b", &two)]), [format!("2 rasters (a, b): {two}")]);
        assert_eq!(said(&[("a", &one), ("b", &three)]), [format!("a: {one}"), format!("b: {three}")]);
        assert_eq!(
            said(&[("a", &one), ("b", &three), ("c", &two), ("d", &one)]),
            ["4 rasters (a, b, c, …), each: 1–3 band or class name(s) cut to 256 characters"]
        );
    }
}
