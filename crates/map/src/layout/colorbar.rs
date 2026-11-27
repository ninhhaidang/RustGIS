//! The colorbar's ticks and the colour scale it shows.

use super::*;

/// Which side of a colorbar its tick labels sit on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TickSide {
    /// Below a horizontal bar, right of a vertical one.
    #[default]
    End,
    /// Above a horizontal bar, left of a vertical one.
    Start,
}

impl TickSide {
    /// A side by name: `below`/`right`/`end` or `above`/`left`/`start`.
    pub fn from_name(s: &str) -> Option<TickSide> {
        match s.trim().to_ascii_lowercase().as_str() {
            "below" | "right" | "end" | "bottom" => Some(TickSide::End),
            "above" | "left" | "start" | "top" => Some(TickSide::Start),
            _ => None,
        }
    }
}

/// Labelled values a colorbar shows at most.
pub const MAX_COLORBAR_TICKS: usize = 21;

pub(super) fn ticks() -> usize {
    5
}

/// What a colorbar draws for a raster layer.
#[derive(Clone, Debug, PartialEq)]
pub enum ColorScale {
    /// A continuous ramp from `lo` to `hi`, its colours evenly spaced over the values.
    Ramp { lo: f64, hi: f64, colors: Vec<Rgba> },
    /// Classes, each bounded by two of `edges` (one more than there are colours).
    Steps { edges: Vec<f64>, colors: Vec<Rgba> },
    /// A colour per value.
    Values(Vec<(f64, Rgba)>),
}

/// Most values a colorbar gives a block of their own (unique values, colour tables).
pub const MAX_COLORBAR_VALUES: usize = 64;

/// Colours sampled along a continuous colorbar.
pub(super) const RAMP_SAMPLES: usize = 256;

/// The raster layer a colorbar shows: `layer` by name, or the topmost raster the map shows (its
/// groups visible too) with a colour scale ([`color_scale`]) that isn't drawn by classes: a legend lists those (#1679), and a colorbar
/// of their codes would be the only key a land cover had. An RGB composite or a land cover over a
/// DEM leaves it to the DEM. A raster on a ramp (Stretch, a tinted hillshade) whose pixels aren't
/// read yet keeps its place: it is returned with no colour scale to show, rather than the one of
/// a raster under it.
pub fn colorbar_layer<'a>(map: &'a Map, layer: Option<&str>) -> Option<&'a Layer> {
    match layer {
        Some(n) => map.layer_by_name(n).filter(|l| l.is_raster()),
        None => map.layers.iter().filter(|l| map.shows(l)).find(|l| {
            l.raster.as_ref().is_some_and(|r| match r.data {
                None => matches!(r.style.renderer, RasterRenderer::Stretch { .. } | RasterRenderer::ShadedRelief { ramp: Some(_), .. }),
                Some(_) => !r.style.renderer.draws_classes() && color_scale(r).is_some(),
            })
        }),
    }
}

/// A raster layer's colour scale as the map draws it (stretch, enhancement and all), or `None`
/// when it has none: RGB, a hillshade without a tint, or pixels that couldn't be read.
pub fn color_scale(rl: &RasterLayer) -> Option<ColorScale> {
    let r = rl.data.as_ref()?;
    let mask = &rl.style.mask;
    // Coloured without the mask, so a hidden value leaves no gap in the bar.
    let style = RasterStyle { mask: RasterMask::default(), ..rl.style.clone() };
    let masked = rl.masked_stats();
    let view_stats = masked.as_deref().map(Vec::as_slice);
    let band = style.renderer.bands().first().copied().unwrap_or(0);
    let stats = view_stats.and_then(|s| s.first()).or_else(|| r.bands.get(band).map(|b| &b.stats))?;
    let colorizer = Colorizer::new(&style, r, view_stats);
    // A relief's bar is the colour of its flat ground: under the balanced light (#1977), flat
    // ground's own hillshade, which keeps the ramp's colour; under the plain light, full light.
    let shade = match style.renderer {
        RasterRenderer::ShadedRelief { altitude, balanced: true, .. } => {
            Some(crate::raster::hillshade(&[0.0; 9], 1.0, 1.0, 0.0, altitude, 1.0))
        }
        RasterRenderer::ShadedRelief { .. } => Some(255),
        _ => None,
    };
    let at = |v: f64| colorizer.color(&[v as f32], shade).map(Rgba);
    match &style.renderer {
        RasterRenderer::Stretch { .. } | RasterRenderer::ShadedRelief { ramp: Some(_), .. } => {
            let n = Normalizer::new(style.stretch, stats, &r.data_type);
            let (lo, hi) = (n.lo, n.hi);
            let colors = (0..RAMP_SAMPLES)
                .map(|i| at(lo + (hi - lo) * i as f64 / (RAMP_SAMPLES - 1) as f64).unwrap_or(Rgba::new(0, 0, 0, 0)))
                .collect();
            Some(ColorScale::Ramp { lo, hi, colors })
        }
        RasterRenderer::Classify { breaks, ramp, .. } => {
            let first = *breaks.first()?;
            let mut edges = vec![if stats.min.is_finite() { stats.min.min(first) } else { first }];
            edges.extend(breaks.iter().copied());
            // Each class's colour from a value inside it, so the enhancement applies as on the map.
            let colors = edges
                .windows(2)
                .enumerate()
                .map(|(i, w)| {
                    match w {
                        [a, b] if a < b => at((a + b) / 2.0),
                        [_, b] => at(*b),
                        _ => None,
                    }
                    .unwrap_or_else(|| ramp.pick(i, breaks.len()))
                })
                .collect();
            Some(ColorScale::Steps { edges, colors })
        }
        RasterRenderer::UniqueValues { values, .. } => {
            let v: Vec<(f64, Rgba)> = values
                .iter()
                .filter(|(v, _)| !mask.hides(*v as f32))
                .take(MAX_COLORBAR_VALUES)
                .map(|(v, c)| (*v, at(*v).unwrap_or(*c)))
                .collect();
            (!v.is_empty()).then_some(ColorScale::Values(v))
        }
        RasterRenderer::Colormap => {
            let cats = r.bands.first().and_then(|b| b.stats.categories.as_ref())?;
            let v: Vec<(f64, Rgba)> =
                cats.iter().filter(|v| !mask.hides(**v as f32)).filter_map(|v| at(*v).map(|c| (*v, c))).take(MAX_COLORBAR_VALUES).collect();
            (!v.is_empty()).then_some(ColorScale::Values(v))
        }
        RasterRenderer::Rgb { .. } | RasterRenderer::ShadedRelief { ramp: None, .. } => None,
    }
}
