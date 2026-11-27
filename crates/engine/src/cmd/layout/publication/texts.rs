//! A publication layout's texts, and the figures they print.

use super::*;

/// A figure's text size, in points at its printed size: journals ask for 6 to 8.
pub(in crate::cmd::layout) const FIGURE_PT: f64 = 7.0;

/// The graticule steps a publication layout picks from, in degrees.
pub(in crate::cmd::layout) const GRID_STEPS: &[f64] = &[0.01, 0.02, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0];

/// The strips a frame's graticule labels take inside its edges on an A4 sheet, in mm: beside the
/// left and right edges (a latitude such as `10°S` across), and along the top and bottom ones. A
/// sheet scales them with its labels, by its short side (#2581).
pub(in crate::cmd::layout) const LABEL_ROOM_SIDE: f64 = 9.0;
pub(in crate::cmd::layout) const LABEL_ROOM_END: f64 = 6.0;

/// A scale denominator rounded up to two significant figures (1:1,234,567 to 1:1,300,000), so the
/// extent fitted at it still fits.
pub(crate) fn round_scale(scale: f64) -> f64 {
    if !(scale.is_finite() && scale > 0.0) {
        return scale;
    }
    let unit = 10f64.powf(scale.log10().floor() - 1.0);
    ((scale / unit) - 1e-9).ceil() * unit
}

/// The graticule step for an extent: the smallest that draws at most five lines across its longer
/// side.
pub(crate) fn grid_step(e: &Extent) -> f64 {
    let span = e.width().max(e.height());
    GRID_STEPS.iter().copied().find(|step| span / step <= 5.0).unwrap_or(30.0)
}

/// A graticule step as printed: degrees, else minutes, else seconds of arc.
pub(crate) fn step_text(step: f64) -> String {
    let minutes = step * 60.0;
    if step >= 1.0 {
        format!("{step}°")
    } else if (minutes - minutes.round()).abs() < 1e-9 {
        format!("{}′", minutes.round())
    } else {
        // Minutes and seconds (1′12″), or seconds alone under a minute.
        let seconds = (step * 3600.0).round();
        let (m, sec) = ((seconds / 60.0).floor(), seconds % 60.0);
        if m > 0.0 { format!("{m}′{sec}″") } else { format!("{sec}″") }
    }
}

/// The map's projection and datum for a layout's texts: an EPSG name's two halves (`SIRGAS 2000 /
/// UTM zone 23S`), else the projection's name and the datum its definition names (a preset's
/// `+datum=NAD83`), WGS 84 when it names none.
pub(in crate::cmd::layout) fn projection_and_datum(s: &Session) -> (String, String) {
    let projection = &s.map().projection;
    let label = projection.label();
    if let Some((datum, name)) = label.split_once(" / ") {
        return (name.trim().to_string(), datum.trim().to_string());
    }
    // A UTM zone's EPSG code, named: its PROJ definition gives only an ellipsoid (SIRGAS 2000's
    // zones have +ellps=GRS80), and its label only the code.
    let code = label.strip_prefix("EPSG:").and_then(|n| n.parse::<u32>().ok());
    if let Some((datum, zone, north)) = code.and_then(rustgis_data::raster::wkt::utm_of_code) {
        return (format!("UTM zone {zone}{}", if north { "N" } else { "S" }), datum.to_string());
    }
    let datum = rustgis_map::projection::lookup(&projection.code()).ok().and_then(|(def, _)| datum_of(&def));
    (label, datum.unwrap_or_else(|| "WGS 84".to_string()))
}

/// The datum a PROJ definition names: its `+datum`, else its `+ellps`, else a sphere for `+R`.
pub(crate) fn datum_of(def: &str) -> Option<String> {
    let param = |k: &str| def.split_whitespace().find_map(|t| t.strip_prefix(k).map(str::to_string));
    if let Some(d) = param("+datum=") {
        return Some(if d == "WGS84" { "WGS 84".into() } else { d });
    }
    if let Some(e) = param("+ellps=") {
        return Some(match e.as_str() {
            "WGS84" => "WGS 84".into(),
            "GRS80" => "GRS 1980 ellipsoid".into(),
            _ => format!("{e} ellipsoid"),
        });
    }
    param("+R=").map(|_| "a sphere".into())
}

/// Texts given as one or a list (up to 50), each of at most [`MAX_PUBLICATION_TEXT`] characters: a
/// longer one is refused, not cut.
pub(in crate::cmd::layout) fn texts_of(p: &Value, k: &str) -> Result<Option<Vec<String>>, String> {
    let within = |t: &str| crate::cmd::within(k, "a publication layout's text", t, MAX_PUBLICATION_TEXT);
    match p.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(t)) => Ok(Some(vec![within(t)?])),
        Some(Value::Array(a)) if a.len() <= 50 => a
            .iter()
            .map(|v| v.as_str().ok_or_else(|| format!("{k} is a text or a list of texts")).and_then(within))
            .collect::<Result<_, _>>()
            .map(Some),
        Some(_) => Err(format!("{k} is a text or a list of up to 50 texts")),
    }
}

/// The extent's height over its width on the active map's projection, along its edges.
pub(in crate::cmd::layout) fn aspect(s: &Session, e: &Extent) -> Option<f64> {
    let ext = projected_extent(s, e);
    (ext.width() > 0.0 && ext.height() > 0.0).then(|| ext.height() / ext.width())
}

/// A lon/lat extent's bounds on the map's projection, from points along its edges.
pub(in crate::cmd::layout) fn projected_extent(s: &Session, e: &Extent) -> Extent {
    let flat = s.map().flat();
    let mut ext = Extent::EMPTY;
    for i in 0..=32 {
        let t = f64::from(i) / 32.0;
        let (x, y) = (e.min_x + e.width() * t, e.min_y + e.height() * t);
        for c in [Coord::new(x, e.min_y), Coord::new(x, e.max_y), Coord::new(e.min_x, y), Coord::new(e.max_x, y)] {
            if let Some(q) = flat.forward_checked(c) {
                ext.expand(q);
            }
        }
    }
    ext
}

/// `t` in capitals, its `{tokens}` as they are: the layout fills a token (`{date}`) when it draws
/// only in its own case. A token is a closed `{name}` of letters and digits, or a layer statistic
/// (`{count:Cities}`, #2853); a lone brace is text.
pub(crate) fn capitals(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    let mut rest = t;
    while let Some(c) = rest.chars().next() {
        let token = (c == '{')
            .then(|| {
                let closed = rest.find('}').filter(|&end| end > 1 && rest[1..end].chars().all(|c| c.is_ascii_alphanumeric()));
                closed.or_else(|| {
                    let statistic = rustgis_map::layout::stat_tokens(&rest[..=rest.find('}')?]).into_iter().next()?;
                    rest.starts_with(statistic.token).then(|| statistic.token.len() - 1)
                })
            })
            .flatten();
        match token {
            Some(end) => {
                out.push_str(&rest[..=end]);
                rest = &rest[end + 1..];
            }
            None => {
                out.extend(c.to_uppercase());
                rest = &rest[c.len_utf8()..];
            }
        }
    }
    out
}

/// A publication text broken to its box's width.
#[derive(Default)]
pub(crate) struct Broken {
    /// What its element holds.
    pub(in crate::cmd::layout) text: String,
    /// The lines it is drawn in, which size its box.
    pub(in crate::cmd::layout) lines: Vec<String>,
    /// Whether the element wraps to its box as it is drawn.
    pub(in crate::cmd::layout) wrap: bool,
}

impl From<Vec<String>> for Broken {
    /// Lines the element holds as they are drawn.
    fn from(lines: Vec<String>) -> Self {
        Self { text: lines.join("\n"), lines, wrap: false }
    }
}

/// A text element in a publication layout's style: upright, sans, in `align`.
pub(in crate::cmd::layout) fn pub_text(text: Broken, size: f64, color: Rgba, bold: bool, align: TextAlign) -> ElementKind {
    pub_text_in(text, size, color, bold, align, FontFamily::Sans)
}

/// [`pub_text`] in `font`.
pub(in crate::cmd::layout) fn pub_text_in(
    broken: Broken,
    size: f64,
    color: Rgba,
    bold: bool,
    align: TextAlign,
    font: FontFamily,
) -> ElementKind {
    ElementKind::Text {
        text: broken.text,
        size,
        color,
        bold,
        italic: false,
        align,
        letter_spacing: 0.0,
        font,
        font_name: None,
        rotation: 0.0,
        wrap: broken.wrap,
        valign: TextVAlign::Top,
        line_spacing: LINE_SPACING,
        case: TextCase::None,
    }
}
