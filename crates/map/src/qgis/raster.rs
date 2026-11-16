//! QGIS raster renderers (`<pipe><rasterrenderer>`) as a RustGIS [`RasterStyle`]: singleband
//! grey as a stretch from black to white, singleband pseudocolour as a stretch along its colour
//! map (interpolated), as classes (discrete) or as unique values (exact), paletted as unique
//! values, multiband colour as RGB, hillshade as shaded relief; each with its contrast
//! enhancement as the stretch, its brightness, contrast and gamma, its resampling, and the values
//! its transparency and user no-data hide as the mask.

use roxmltree::Node;

use super::{Report, child, color};
use crate::Rgba;
use crate::color::{CustomRamp, Ramp, RampKind};
use crate::raster::{MAX_UNIQUE_VALUES, RasterMask, RasterRenderer, RasterStyle, Resampling, Stretch};
use crate::symbology::ClassMethod;

/// The most bands a QGIS renderer is read naming (RustGIS's bands are checked against the file
/// when the style is applied).
const MAX_BAND: usize = 10_000;
/// The most colour map entries read.
const MAX_ITEMS: usize = 4096;

/// A raster's style from its layer's `<pipe>` (`node`: a project's `<maplayer>` or a style's
/// root), or `None` (noted) for a renderer RustGIS has no match for.
pub(super) fn read(node: Node, layer: &str, report: &mut Report) -> Option<RasterStyle> {
    let pipe = child(node, "pipe")?;
    let r = child(pipe, "rasterrenderer")?;
    let kind = r.attribute("type").unwrap_or("unknown");
    let notes = report.notes.len();
    let style = renderer_style(node, pipe, r, kind, layer, report);
    // A renderer dropped where nothing said why (a band that isn't a band number, a colour map
    // too long for a ramp or with no finite value) is noted here.
    if style.is_none() && report.notes.len() == notes {
        report.approximated(layer, format!("its raster colouring ({kind} renderer), which RustGIS couldn't read, drawn by its default"));
    }
    style
}

/// [`read`]'s style from the renderer `r` in `pipe`, noting what it drops.
fn renderer_style(node: Node, pipe: Node, r: Node, kind: &str, layer: &str, report: &mut Report) -> Option<RasterStyle> {
    let band = |k: &str| -> Option<usize> {
        let b = r.attribute(k)?.trim().parse::<usize>().ok()?;
        (1..=MAX_BAND).contains(&b).then(|| b - 1)
    };
    let mut mask = RasterMask::default();
    let (renderer, stretch) = match kind {
        "singlebandgray" => {
            let b = band("grayBand")?;
            let black_to_white = r.attribute("gradient") != Some("WhiteToBlack");
            let ramp = grey(black_to_white)?;
            (RasterRenderer::Stretch { band: b, ramp, invert: false }, stretch_of(r, child(r, "contrastEnhancement"), &mut mask))
        }
        "singlebandpseudocolor" => pseudocolor(r, band("band")?, &mut mask, layer, report)?,
        "paletted" => {
            let entries = child(r, "colorPalette")
                .map(|p| {
                    p.children()
                        .filter(|e| e.has_tag_name("paletteEntry"))
                        .take(MAX_ITEMS)
                        .filter_map(|e| {
                            let v = e.attribute("value")?.trim().parse::<f64>().ok().filter(|v| v.is_finite())?;
                            let c = color(e.attribute("color")?)?;
                            let a = e.attribute("alpha").and_then(|a| a.trim().parse::<u8>().ok()).unwrap_or(c.a());
                            Some((v, c.with_alpha(a)))
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            (unique(band("band")?, entries, layer, report)?, Stretch::None)
        }
        "multibandcolor" => {
            let (Some(red), Some(green), Some(blue)) = (band("redBand"), band("greenBand"), band("blueBand")) else {
                report.approximated(layer, "a multiband raster missing a red, green or blue band, drawn by RustGIS's default");
                return None;
            };
            let enh = ["redContrastEnhancement", "greenContrastEnhancement", "blueContrastEnhancement"].map(|t| child(r, t));
            let bounds: Vec<Option<[f64; 2]>> = enh.iter().map(|e| e.and_then(bounds_of)).collect();
            let stretch = match bounds.as_slice() {
                [Some(a), Some(b), Some(c)] => {
                    clip(enh[0], *a, &mut mask);
                    Stretch::custom_bands([*a, *b, *c])
                }
                _ => dynamic(r).unwrap_or(Stretch::None),
            };
            (RasterRenderer::Rgb { red, green, blue }, stretch)
        }
        "hillshade" => {
            let num = |k: &str, d: f64| r.attribute(k).and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite()).unwrap_or(d);
            if r.attribute("multidirection") == Some("1") {
                report.approximated(layer, "a hillshade lit from several directions, drawn lit from one");
            }
            let renderer = RasterRenderer::ShadedRelief {
                band: band("band")?,
                ramp: None,
                azimuth: num("azimuth", 315.0).rem_euclid(360.0),
                altitude: num("angle", 45.0).clamp(0.0, 90.0),
                z_factor: num("zfactor", 1.0).clamp(0.0001, 10_000.0),
                shadow: 0.0,
                sky: 0.0,
                fill: 0.0,
                rock: None,
                ambient: None,
                balanced: false,
            };
            (renderer, Stretch::None)
        }
        other => {
            report.skipped(layer, format!("its raster colouring ({other} renderer)"));
            return None;
        }
    };
    if r.attribute("alphaBand").and_then(|a| a.trim().parse::<i64>().ok()).is_some_and(|a| a > 0) {
        report.approximated(layer, "a raster band as its transparency, left out");
    }
    transparency(r, &mut mask, layer, report);
    no_data(node, &mut mask, layer, report);
    let mut style = RasterStyle {
        renderer,
        stretch,
        dra: false,
        resampling: resampling(pipe),
        brightness: 0.0,
        contrast: 0.0,
        gamma: 1.0,
        rotation: 0.0,
        mask,
    };
    if let Some(bc) = child(pipe, "brightnesscontrast") {
        let num = |k: &str| bc.attribute(k).and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite());
        // QGIS's brightness runs -255 to 255, RustGIS's -100 to 100; contrast is -100 to 100 in both.
        style.brightness = num("brightness").map_or(0.0, |b| (b / 2.55).clamp(-100.0, 100.0));
        style.contrast = num("contrast").map_or(0.0, |c| c.clamp(-100.0, 100.0));
        style.gamma = num("gamma").map_or(1.0, |g| g.clamp(0.1, 10.0));
    }
    if let Some(hs) = child(pipe, "huesaturation") {
        let set = |k: &str, off: &str| hs.attribute(k).is_some_and(|v| v.trim() != off);
        if set("saturation", "0") || set("grayscaleMode", "0") || set("colorizeOn", "0") || set("invertColors", "0") {
            report.approximated(layer, "raster hue and saturation settings (saturation, greyscale, colourise, invert), left out");
        }
    }
    style.dra =
        dynamic(r).is_some() && matches!(style.stretch, Stretch::MinMax | Stretch::PercentClip { .. } | Stretch::StandardDeviation { .. });
    Some(style)
}

/// Black to white (or white to black) as a two-stop ramp: QGIS's grey is the full range, where
/// RustGIS's Greys ramp stops short of both ends.
fn grey(black_to_white: bool) -> Option<Ramp> {
    let (lo, hi) =
        if black_to_white { (Rgba::rgb(0, 0, 0), Rgba::rgb(255, 255, 255)) } else { (Rgba::rgb(255, 255, 255), Rgba::rgb(0, 0, 0)) };
    CustomRamp::new("QGIS grey", RampKind::Sequential, vec![(Some(0.0), lo), (Some(1.0), hi)]).ok().map(|c| Ramp::Custom(Box::new(c)))
}

/// A singleband pseudocolour renderer's colour map: interpolated as a stretch along it, discrete
/// as classes (a value takes the first entry at or above it, as in QGIS), exact as unique values.
fn pseudocolor(r: Node, band: usize, mask: &mut RasterMask, layer: &str, report: &mut Report) -> Option<(RasterRenderer, Stretch)> {
    let shader = child(r, "rastershader").and_then(|s| child(s, "colorrampshader"))?;
    let mut items: Vec<(f64, Rgba)> = shader
        .children()
        .filter(|n| n.has_tag_name("item"))
        .take(MAX_ITEMS)
        .filter_map(|n| {
            let v = n.attribute("value")?.trim().parse::<f64>().ok().filter(|v| !v.is_nan())?;
            let c = color(n.attribute("color")?)?;
            let a = n.attribute("alpha").and_then(|a| a.trim().parse::<u8>().ok()).unwrap_or(c.a());
            Some((v, c.with_alpha(a)))
        })
        .collect();
    items.sort_by(|a, b| a.0.total_cmp(&b.0));
    if items.is_empty() {
        report.approximated(layer, "a pseudocolour raster without a colour map, drawn by RustGIS's default");
        return None;
    }
    let num = |k: &str| shader.attribute(k).and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite());
    // Values outside the map's range are left out when it clips.
    if shader.attribute("clip") == Some("1") {
        mask.min = num("minimumValue").or(items.first().map(|i| i.0).filter(|v| v.is_finite()));
        mask.max = num("maximumValue").or(items.last().map(|i| i.0).filter(|v| v.is_finite()));
    }
    match shader.attribute("colorRampType").unwrap_or("INTERPOLATED") {
        "EXACT" => Some((unique(band, items, layer, report)?, Stretch::None)),
        "DISCRETE" => {
            // The last entry is often "inf": every value above the one before.
            let breaks: Vec<f64> = items.iter().map(|i| i.0.clamp(f64::from(f32::MIN), f64::from(f32::MAX))).collect();
            let colors = items.iter().map(|i| (None, i.1)).collect();
            let ramp = CustomRamp::new("QGIS classes", RampKind::Qualitative, colors).ok()?;
            let classes = breaks.len();
            Some((
                RasterRenderer::Classify { band, method: ClassMethod::Manual, ramp: Ramp::Custom(Box::new(ramp)), breaks, classes },
                Stretch::None,
            ))
        }
        _ => {
            let finite: Vec<(f64, Rgba)> = items.into_iter().filter(|i| i.0.is_finite()).collect();
            let (lo, hi) = (finite.first()?.0, finite.last()?.0);
            let stops: Vec<(Option<f64>, Rgba)> = if hi > lo {
                finite.iter().map(|(v, c)| (Some(((v - lo) / (hi - lo)).clamp(0.0, 1.0)), *c)).collect()
            } else {
                let c = finite.first()?.1;
                vec![(Some(0.0), c), (Some(1.0), c)]
            };
            let ramp = CustomRamp::new("QGIS colour map", RampKind::Sequential, stops).ok()?;
            let (lo, hi) = if hi > lo { (lo, hi) } else { (lo, lo + 1.0) };
            Some((RasterRenderer::Stretch { band, ramp: Ramp::Custom(Box::new(ramp)), invert: false }, Stretch::custom(lo, hi)))
        }
    }
}

/// A colour per value, at most [`MAX_UNIQUE_VALUES`] (noted past them), sorted as RustGIS looks
/// them up; the values as the pixels hold them (in f32).
fn unique(band: usize, mut values: Vec<(f64, Rgba)>, layer: &str, report: &mut Report) -> Option<RasterRenderer> {
    values.retain(|v| v.0.is_finite());
    for v in &mut values {
        v.0 = f64::from(v.0 as f32);
    }
    values.sort_by(|a, b| a.0.total_cmp(&b.0));
    values.dedup_by(|a, b| a.0 == b.0);
    if values.is_empty() {
        return None;
    }
    if values.len() > MAX_UNIQUE_VALUES {
        report.skipped(layer, format!("raster values past the first {MAX_UNIQUE_VALUES} with a colour of their own"));
        values.truncate(MAX_UNIQUE_VALUES);
    }
    Some(RasterRenderer::UniqueValues { band, ramp: Ramp::Harbour, values })
}

/// A `<contrastEnhancement>`'s minimum and maximum, when it stretches between them.
fn bounds_of(e: Node) -> Option<[f64; 2]> {
    let algorithm = child(e, "algorithm").and_then(|a| a.text()).map(str::trim).unwrap_or("NoEnhancement");
    if algorithm == "NoEnhancement" || algorithm == "UserDefinedEnhancement" {
        return None;
    }
    let v = |t: &str| child(e, t).and_then(|n| n.text()).and_then(|t| t.trim().parse::<f64>().ok()).filter(|v| v.is_finite());
    let (lo, hi) = (v("minValue")?, v("maxValue")?);
    (hi > lo).then_some([lo, hi])
}

/// A clipping enhancement leaves out the values outside its bounds.
fn clip(e: Option<Node>, [lo, hi]: [f64; 2], mask: &mut RasterMask) {
    let algorithm = e.and_then(|e| child(e, "algorithm")).and_then(|a| a.text()).map(str::trim);
    if matches!(algorithm, Some("StretchAndClipToMinimumMaximum" | "ClipToMinimumMaximum")) {
        mask.min = Some(lo);
        mask.max = Some(hi);
    }
}

/// A single band's stretch: its enhancement's bounds, or none.
fn stretch_of(r: Node, e: Option<Node>, mask: &mut RasterMask) -> Stretch {
    match e.and_then(bounds_of) {
        Some(b) => {
            clip(e, b, mask);
            // Bounds worked out from the canvas follow the view: RustGIS's dynamic range.
            dynamic(r).unwrap_or(Stretch::custom(b[0], b[1]))
        }
        None => Stretch::None,
    }
}

/// The stretch QGIS works out again as the view changes (`<minMaxOrigin>` with a canvas extent),
/// as RustGIS's (its dynamic range adjustment then on).
fn dynamic(r: Node) -> Option<Stretch> {
    let o = child(r, "minMaxOrigin")?;
    let text = |t: &str| child(o, t).and_then(|n| n.text()).map(str::trim);
    if !matches!(text("extent"), Some("CurrentCanvas" | "UpdatedCanvas")) {
        return None;
    }
    let num = |t: &str| text(t).and_then(|v| v.parse::<f64>().ok()).filter(|v| v.is_finite());
    match text("limits")? {
        "MinMax" => Some(Stretch::MinMax),
        "CumulativeCut" => {
            let low = num("cumulativeCutLower").unwrap_or(0.02) * 100.0;
            let high = (1.0 - num("cumulativeCutUpper").unwrap_or(0.98)) * 100.0;
            Some(Stretch::PercentClip { low: low.clamp(0.0, 49.0), high: high.clamp(0.0, 49.0) })
        }
        "StdDev" => Some(Stretch::StandardDeviation { n: num("stdDevFactor").unwrap_or(2.0).clamp(0.1, 10.0) }),
        _ => None,
    }
}

/// Bilinear when QGIS smooths the raster (bilinear or cubic, zoomed in), else nearest.
fn resampling(pipe: Node) -> Resampling {
    let smooth = |m: Option<&str>| m.is_some_and(|m| m != "nearestNeighbour" && !m.is_empty());
    let provider = child(pipe, "provider").and_then(|p| child(p, "resampling"));
    let by_provider = provider.is_some_and(|p| p.attribute("enabled") == Some("true") && smooth(p.attribute("zoomedInResamplingMethod")));
    let by_resampler = child(pipe, "rasterresampler").is_some_and(|r| smooth(r.attribute("zoomedInResampler")));
    if by_provider || by_resampler { Resampling::Bilinear } else { Resampling::Nearest }
}

/// The values a layer's own no-data settings (`<noData>`, the user's on top of the file's) hide.
fn no_data(node: Node, mask: &mut RasterMask, layer: &str, report: &mut Report) {
    let Some(nd) = child(node, "noData") else { return };
    let mut ranges = false;
    for e in nd.descendants().filter(|e| e.has_tag_name("noDataRange")).take(MAX_ITEMS) {
        let num = |k: &str| e.attribute(k).and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite());
        match (num("min"), num("max")) {
            (Some(lo), Some(hi)) if lo == hi => mask.values.push(lo),
            _ => ranges = true,
        }
    }
    mask.values.sort_by(f64::total_cmp);
    mask.values.dedup();
    if ranges {
        report.approximated(layer, "ranges of raster values set as no data, left out");
    }
}

/// The values QGIS's raster transparency makes fully transparent, as mask values; ranges and
/// partial transparency are noted.
fn transparency(r: Node, mask: &mut RasterMask, layer: &str, report: &mut Report) {
    let Some(t) = child(r, "rasterTransparency") else { return };
    let mut other = false;
    for e in t.descendants().filter(|e| e.has_tag_name("pixelListEntry")).take(MAX_ITEMS) {
        let num = |k: &str| e.attribute(k).and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite());
        let full = num("percentTransparent").is_none_or(|p| p >= 100.0);
        match (num("min"), num("max")) {
            (Some(lo), Some(hi)) if lo == hi && full => mask.values.push(lo),
            _ => other = true,
        }
    }
    if t.descendants().any(|e| e.has_tag_name("threeValuePixelList") && e.children().any(|c| c.is_element())) {
        other = true;
    }
    mask.values.sort_by(f64::total_cmp);
    mask.values.dedup();
    if other {
        report.approximated(layer, "raster transparency for ranges of values or in part, left out");
    }
}
