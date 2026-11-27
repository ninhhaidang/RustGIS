//! The tokens a layout's texts hold: page, scale and a layer's statistics.

use super::*;

/// A scale as `{scale}` prints it: `1:6,300,000`, or `1:6.300.000` with a decimal comma.
pub fn scale_ratio_text(scale: f64, comma: bool) -> String {
    let ratio = rustgis_map::graphics::scale_ratio(scale);
    if comma { decimal(ratio, true) } else { ratio }
}

/// Expands layout text tokens: the page ones here, the map ones from the first frame.
pub fn expand(t: &str, layout: &Layout, info: &PageInfo, frame: Option<(&Map, rustgis_map::Camera)>) -> String {
    // The frame's scale, worked out once for both its forms below.
    let scale = frame.map(|(m, cam)| m.ground_res_at(&cam) / rustgis_map::layout::PAPER_M_PER_PT);
    // The layer statistics first, read from the text as written: a page or layout name put in
    // below isn't read as one.
    let t = statistics(t, layout.decimal_comma, frame.map(|(m, _)| m));
    // A layout with a decimal comma prints its scale with points between thousands: put in first,
    // so a page or layout name holding "{scale}" isn't read as the token.
    let t = match scale {
        Some(scale) if layout.decimal_comma => t.replace("{scale}", &scale_ratio_text(scale, true)),
        _ => t,
    };
    let t = t
        .replace("{page}", &info.page.max(1).to_string())
        .replace("{pages}", &info.pages.max(1).to_string())
        .replace("{pageName}", &info.page_name)
        .replace("{layout}", &layout.name);
    let now = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let v = match frame {
        Some((m, cam)) => rustgis_map::graphics::TokenValues {
            map: m.name.clone(),
            scale: scale.unwrap_or(0.0),
            crs: m.projection.label(),
            layers: m.layers.iter().filter(|l| !l.graphics).count(),
            center: m.to_lonlat(cam.center).unwrap_or_default(),
            now,
        },
        None => rustgis_map::graphics::TokenValues {
            map: String::new(),
            scale: 0.0,
            crs: String::new(),
            layers: 0,
            center: Coord::default(),
            now,
        },
    };
    rustgis_map::graphics::expand(&t, &v)
}

/// What the layer statistics in `text` (#2853) are missing, one line per token, each once: no map
/// frame, the first frame's map not in the project, no such layer in it, or what
/// [`rustgis_map::layout::layer_statistic`] finds missing (each drawn as written by [`expand`]); and
/// a figure over a layer read in place, which covers only the features loaded, has a line for that
/// as well.
pub fn statistic_notices(layout: &Layout, maps: &[Map], text: &str) -> Vec<String> {
    let map = layout.first_frame().and_then(|f| match &layout.element(f)?.kind {
        ElementKind::MapFrame { map, .. } => maps.get(map_index(maps, map)?),
        _ => None,
    });
    let no_map = match layout.first_frame() {
        Some(_) => "the map of the layout's first frame is not in the project",
        None => "the layout has no map frame whose map it would read",
    };
    notices_over(map.ok_or(no_map), "the map of the layout's first frame", text)
}

/// [`statistic_notices`] for a text whose figures are read from `map`, which the notices call
/// `whose`: a presentation map page's title, drawn over that page's own map.
pub fn map_statistic_notices(map: &Map, whose: &str, text: &str) -> Vec<String> {
    notices_over(Ok(map), whose, text)
}

/// The notices of `text`'s figures over `map`, or why there is none to read.
pub(super) fn notices_over(map: Result<&Map, &str>, whose: &str, text: &str) -> Vec<String> {
    use rustgis_map::layout::StatMissing;
    let mut said: Vec<String> = Vec::new();
    let tokens = distinct_stat_tokens(text, true);
    // Each layer's figures in one pass, whatever their number.
    let figures: Vec<Option<Option<(bool, Result<f64, StatMissing>)>>> = match map {
        Ok(m) => figures_of(m, &tokens).into_iter().map(Some).collect(),
        Err(_) => vec![None; tokens.len()],
    };
    for (t, found) in tokens.iter().zip(figures) {
        let (layer, field) = (t.layer, t.field.unwrap_or_default());
        // A layer read in place holds only what is loaded, so its figure is partial, and one with
        // no number yet gets some as more is loaded.
        if let Some(Some((true, Ok(_) | Err(StatMissing::NoValue)))) = found {
            said.push(format!("{} covers only the features of '{layer}' that are loaded: it is read in place", t.token));
        }
        let why = match found.map(|l| l.map(|(_, figure)| figure)) {
            None => map.err().unwrap_or_default().to_string(),
            Some(None) => format!("{whose} has no layer named '{layer}'"),
            Some(Some(Ok(_))) => continue,
            Some(Some(Err(StatMissing::Raster))) => format!("'{layer}' is a raster layer, which has no features"),
            Some(Some(Err(StatMissing::Field))) if t.field.is_none() => {
                format!("a {} takes a field: write {{{}:{layer}:field}}", t.stat.code(), t.stat.code())
            }
            Some(Some(Err(StatMissing::Field))) => format!("'{layer}' has no field named '{field}'"),
            Some(Some(Err(StatMissing::NotNumeric))) => format!("'{field}' of '{layer}' holds no numbers"),
            Some(Some(Err(StatMissing::NoValue))) => {
                format!(
                    "no feature of '{layer}' its definition query keeps has a number in '{field}', or the figure is past a number's range"
                )
            }
        };
        said.push(format!("{} shows as written: {why}", t.token));
    }
    said
}

/// `t` with its layer statistics (#2853) from `map`, the first frame's, written as a chart writes
/// its values ([`rustgis_map::layout::chart_value_text`]) with `comma` the decimal mark; a token
/// with no figure stays as written ([`statistic_notices`] says why).
pub(super) fn statistics(t: &str, comma: bool, map: Option<&Map>) -> String {
    let Some(m) = map else { return t.to_string() };
    let mut out = t.to_string();
    // `replace` below puts in every occurrence, so a repeated token is worked out once, and the
    // figures of one layer in one pass over it.
    let tokens = distinct_stat_tokens(t, false);
    for (s, figure) in tokens.iter().zip(figures_of(m, &tokens)) {
        if let Some((_, Ok(v))) = figure {
            out = out.replace(s.token, &decimal(rustgis_map::layout::chart_value_text(v, None), comma));
        }
    }
    out
}

/// The layer statistics of `texts` (#2853) as the page draws them from `map`, as (token, figure)
/// pairs for the tokens that have a figure, each token once: what a publication measures its texts
/// with, so a box is sized for the figures it will show, not for the tokens written.
pub fn statistic_figures(map: &Map, comma: bool, texts: &[&str]) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    for text in texts {
        let tokens = distinct_stat_tokens(text, false);
        for (s, figure) in tokens.iter().zip(figures_of(map, &tokens)) {
            if let Some((_, Ok(v))) = figure
                && !pairs.iter().any(|(token, _)| token == s.token)
            {
                pairs.push((s.token.to_string(), decimal(rustgis_map::layout::chart_value_text(v, None), comma)));
            }
        }
    }
    pairs
}

/// The statistic tokens of `text`, each once, in the order they come; with `gaps`, the figures
/// written without their field too.
pub(super) fn distinct_stat_tokens(text: &str, gaps: bool) -> Vec<rustgis_map::layout::StatToken<'_>> {
    let mut tokens: Vec<rustgis_map::layout::StatToken<'_>> = Vec::new();
    let read = if gaps { rustgis_map::layout::stat_tokens_and_gaps(text) } else { rustgis_map::layout::stat_tokens(text) };
    for t in read {
        if !tokens.iter().any(|seen| seen.token == t.token) {
            tokens.push(t);
        }
    }
    tokens
}

/// The figure of each of `tokens` in `map`, with whether its layer is read in place; none for a token
/// whose layer the map lacks. The figures of one layer are worked out in one pass over its rows
/// ([`rustgis_map::layout::layer_statistics`]), so a text with many figures costs a pass for each
/// layer, not for each figure.
pub(super) fn figures_of(
    map: &Map,
    tokens: &[rustgis_map::layout::StatToken<'_>],
) -> Vec<Option<(bool, Result<f64, rustgis_map::layout::StatMissing>)>> {
    let mut out = vec![None; tokens.len()];
    let mut done = vec![false; tokens.len()];
    for i in 0..tokens.len() {
        if done[i] {
            continue;
        }
        let same: Vec<usize> = (i..tokens.len()).filter(|j| !done[*j] && tokens[*j].layer.eq_ignore_ascii_case(tokens[i].layer)).collect();
        for j in &same {
            done[*j] = true;
        }
        let Some(layer) = map.layer_by_name(tokens[i].layer) else { continue };
        let wanted: Vec<_> = same.iter().map(|j| (tokens[*j].stat, tokens[*j].field)).collect();
        for (j, figure) in same.iter().zip(rustgis_map::layout::layer_statistics(layer, &wanted)) {
            out[*j] = Some((layer.is_streamed(), figure));
        }
    }
    out
}
