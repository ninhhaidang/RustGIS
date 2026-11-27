//! The legend: its rows, columns, sizes and the wait for service legend pictures.

use super::*;

/// Each visible layer's legend for what it draws at a scale denominator (#207), by layer id; a
/// layer that draws nothing there isn't listed. The scale is the one the renderer draws the frame
/// at (the one its layers' scale ranges and display filters see), not the paper scale `{scale}`
/// prints, so the legend follows what the frame actually shows.
pub(super) fn shown_legends(map: &Map, scale: f64, show_no_data: bool, env: &Env) -> Vec<(u32, Vec<LegendEntry>)> {
    map.layers
        .iter()
        .filter(|l| map.draws_at(l, scale) && legend_lists(l))
        .filter_map(|l| layer_shown(l, scale, show_no_data, env).map(|entries| (l.id, entries)))
        .collect()
}

/// Whether a legend can list a layer: a vector layer, by its symbols, or a raster drawn by classes
/// (a colour table, unique values, class breaks), by its classes (#1679). Not a graphics layer,
/// nor a raster on a ramp or in RGB bands, which a colorbar keys or nothing does.
pub fn legend_lists(l: &rustgis_map::Layer) -> bool {
    !l.graphics && l.raster.as_ref().is_none_or(|r| r.style.renderer.draws_classes())
}

/// What a layer a legend lists draws at 1:`scale` ([`rustgis_map::Layer::shown_legend`]); for a
/// classified raster, the classes it holds ([`raster_classes`]). `None` when it draws nothing
/// there.
pub(super) fn layer_shown(l: &rustgis_map::Layer, scale: f64, show_no_data: bool, env: &Env) -> Option<Vec<LegendEntry>> {
    if l.is_raster() {
        return raster_classes(l, false).filter(|classes| l.in_scale_range(scale) && !classes.is_empty());
    }
    let mut entries = l.shown_legend_with(scale, env)?;
    if show_no_data {
        entries.extend(l.renderer.no_data_legend(&l.dataset, Some(&l.drawn_rows_with(scale, env))));
    }
    Some(entries)
}

/// A classified raster's classes as legend rows (#1679), as the Layers pane draws them
/// ([`rustgis_map::RasterLayer::legend_classes`]): a filled swatch with a thin outline, and its
/// label. `None` for a raster on a ramp or in RGB bands, and for a vector layer.
pub(super) fn raster_classes(l: &rustgis_map::Layer, all_classes: bool) -> Option<Vec<LegendEntry>> {
    let classes = l.raster.as_ref()?.legend_classes(all_classes)?;
    let symbol = |fill| rustgis_map::Symbol { fill, stroke: RASTER_SWATCH_EDGE, stroke_width: 0.5, size: 0.0, marker: Default::default() };
    Some(classes.into_iter().map(|(fill, label)| LegendEntry::plain(label, symbol(fill))).collect())
}

/// Names only the source whose read stopped this page, leaving unrelated failures generic.
pub(super) fn failed_source_error(map: &Map, failed_source: Option<u64>) -> Option<String> {
    let id = failed_source?;
    let layer = map.layers.iter().find(|layer| layer.instance_id() == id)?;
    layer.source.as_ref()?.read_error().map(|why| format!("{}: {why}", layer.name))
}

/// Complete-source legends retain one minimal value per used class, never its geometry.
/// A failed traversal returns its live layer identity for the page's source-error report.
pub(super) fn complete_shown_legends(
    map: &mut Map,
    scale: f64,
    show_no_data: bool,
    failed_source: &mut Option<u64>,
    env: &Env,
) -> Option<Vec<(u32, Vec<LegendEntry>)>> {
    *failed_source = None;
    if !map.layers.iter().any(|l| l.source.as_ref().and_then(|s| s.chunk_source()).is_some()) {
        return Some(shown_legends(map, scale, show_no_data, env));
    }
    let style_env = env.at_scale(Some(scale));
    let wanted: Vec<usize> =
        map.layers.iter().enumerate().filter(|(_, l)| map.draws_at(l, scale) && legend_lists(l)).map(|(i, _)| i).collect();
    let mut out = Vec::new();
    for index in wanted {
        let layer = map.layers.get_mut(index)?;
        let Some(source) = layer.source.as_ref().and_then(|s| s.chunk_source()) else {
            if let Some(entries) = layer_shown(layer, scale, show_no_data, env) {
                out.push((layer.id, entries));
            }
            continue;
        };
        let unique = match &layer.renderer {
            rustgis_map::Renderer::Unique { field, classes, .. } => Some((field, classes)),
            _ => None,
        };
        // By expression: a feature of each class seen is kept whole, for the legend to class again.
        let by_expression = layer.renderer.expression().is_some();
        let fi = unique.filter(|_| !by_expression).and_then(|(field, _)| layer.dataset.field_index(field));
        let fields = if by_expression {
            layer.dataset.fields.clone()
        } else {
            unique
                .filter(|_| fi.is_some())
                .map_or_else(Vec::new, |(field, _)| vec![rustgis_data::Field::new(field, rustgis_data::FieldType::Text)])
        };
        let mut sample = rustgis_data::Dataset::new("legend", layer.geometry_type(), fields);
        let mut used = std::collections::HashSet::new();
        let mut any = false;
        let mut no_data = None;
        let mut chunk = layer.clone();
        chunk.source = None;
        // Its chunks come in the source's own system.
        chunk.dataset.held_in = source.coord_crs();
        chunk.clear_cache();
        let result = crate::visit_complete(source.as_ref(), &mut |features| {
            chunk.dataset.features = features.into();
            let rows = chunk.drawn_rows_with(scale, env);
            if show_no_data && no_data.is_none() {
                no_data = chunk.renderer.no_data_legend(&chunk.dataset, Some(&rows));
            }
            let classer = by_expression.then(|| chunk.renderer.classer_with(&chunk.dataset, style_env.clone()));
            for row in rows.iter().enumerate().filter_map(|(row, keep)| keep.then_some(row)) {
                any = true;
                if let Some(c) = &classer {
                    let key = chunk.renderer.unique_class(&chunk.dataset, row, c).unwrap_or(usize::MAX);
                    // Kept with its id ($id), without its geometry.
                    if used.insert(key)
                        && let Some(f) = chunk.dataset.features.get(row)
                    {
                        let mut f = f.clone();
                        f.geometry = None;
                        sample.features.push(f);
                    }
                } else if let Some((_, classes)) = unique {
                    let value = fi.map(|fi| chunk.dataset.value(row, fi).to_string());
                    let key = value.as_ref().and_then(|v| classes.iter().position(|(k, _)| k == v)).unwrap_or(classes.len());
                    if used.insert(key) {
                        sample.push(None, value.map_or_else(Vec::new, |v| vec![rustgis_data::Value::Text(v)]));
                    }
                }
            }
            chunk.dataset.features = Vec::new().into();
            Ok(())
        });
        if let Err(e) = result {
            if let Some(source) = &mut layer.source {
                source.error = Some(e.to_string());
            }
            *failed_source = Some(layer.instance_id());
            return None;
        }
        if let Some(source) = &mut layer.source {
            source.clear_error();
        }
        if any {
            // (A sample of the classes seen: classed again at this scale, by expression.)
            let mut entries = layer.with_size_legend(layer.renderer.legend_with(&sample, &[], style_env.clone()));
            entries.extend(no_data);
            out.push((layer.id, entries));
        }
    }
    Some(out)
}

/// What a legend leaves out: `<all other values>`, and layers by name; the labels it shows for
/// layers instead of their names; the scale denominator whose legend pictures a WMS or WMTS
/// layer shows; and how many times its default size its text is ([`legend_size`]).
#[derive(Clone, Copy)]
pub(super) struct LegendOptions<'a> {
    pub(super) hide_other: bool,
    pub(super) show_no_data: bool,
    pub(super) exclude: &'a [String],
    pub(super) labels: &'a BTreeMap<String, String>,
    pub(super) scale: f64,
    pub(super) size: f32,
}

/// How many times the default a legend of `text_size` points draws its text, rows, swatches and
/// gaps (#2581): 1 at [`rustgis_map::layout::LEGEND_TEXT_PT`], as legends have always been drawn.
/// A point symbol keeps the size the map draws it at, so a legend of sizes still matches the map.
pub(super) fn legend_size(text_size: f64) -> f32 {
    layout_text_scale(text_size, rustgis_map::layout::LEGEND_TEXT_PT) as f32
}

impl<'a> LegendOptions<'a> {
    /// What the legend shows for the layer named `name`: its label (by name, any case), or the
    /// name when it has none (#1654).
    pub(super) fn shown(&self, name: &'a str) -> &'a str {
        let label = self.labels.get(name).or_else(|| self.labels.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, l)| l));
        label.map(String::as_str).filter(|l| !l.trim().is_empty()).unwrap_or(name)
    }
}

/// A row of a legend: a layer drawn in one symbol (its swatch and name), a layer's name over its
/// classes, or one of them, or a WMS or WMTS layer's legend picture (under its name). The name is
/// the one the legend shows: the layer's label, if the legend gives it one.
pub(super) enum LegendRow<'a> {
    Plain(&'a rustgis_map::Layer, &'a str),
    Head(&'a str),
    Class(&'a rustgis_map::Layer, LegendEntry),
    Picture(std::sync::Arc<rustgis_tiles::cache::Payload>),
}

impl LegendRow<'_> {
    /// The side of the square its point symbol takes at 1:`scale`, in points, as far as its marks
    /// reach ([`point_marks::reach`]); 0 for any other row. No larger than a legend picture.
    pub(super) fn symbol_side(&self, scale: f64) -> f32 {
        let (LegendRow::Plain(l, _) | LegendRow::Class(l, _)) = self else { return 0.0 };
        if l.geometry_type() != GeometryType::Point {
            return 0.0;
        }
        let (s, strokes) = match self {
            LegendRow::Class(_, en) => (en.symbol, row_strokes(l, en)),
            _ => (l.renderer.symbol_for(&l.dataset, 0), l.line_strokes.as_slice()),
        };
        (2.0 * point_marks::reach(&s, strokes) * l.size_factor(scale)).min(MAX_LEGEND_SIDE as f32)
    }

    /// Its height at 1:`scale` in a legend `size` times the default ([`legend_size`]), in points:
    /// a picture's a pixel a point at the default size, as the Layers pane shows it; a row grows to
    /// hold its point symbol.
    pub(super) fn height(&self, scale: f64, size: f32) -> f32 {
        match self {
            LegendRow::Picture(p) => picture_size(p).1 * size,
            _ => (LEGEND_ROW * size).max(self.symbol_side(scale)),
        }
    }

    /// Its width beside a column of swatches `column` points wide ([`swatch_column`]) in a legend
    /// `size` times the default, in points.
    pub(super) fn width(&self, column: f32, size: f32) -> f32 {
        match self {
            LegendRow::Plain(_, name) => column + LEGEND_INDENT * size + text::measure(name, LEGEND_NAME_PT * size, false),
            LegendRow::Head(name) => text::measure(name, LEGEND_NAME_PT * size, true),
            LegendRow::Class(_, en) => column + LEGEND_LABEL_INDENT * size + text::measure(&en.label, LEGEND_LABEL_PT * size, false),
            LegendRow::Picture(p) => (LEGEND_INDENT + picture_size(p).0) * size,
        }
    }
}

/// Rows at 1:`scale` in a legend `size` times the default laid into at most `columns` columns
/// `limit` points high: the row each column starts at, and whether they all fit. A layer's name
/// stays with the row under it; a row taller than `limit` has a column to itself, and the last
/// column takes what is left.
pub(super) fn flow(rows: &[LegendRow], columns: usize, limit: f32, (scale, size): (f64, f32)) -> (Vec<usize>, bool) {
    // A thousandth of a row's slack, as the rows have when drawn.
    let limit = limit + LEGEND_ROW * size * 1e-3;
    let (mut starts, mut used, mut fits, mut i) = (vec![0], 0.0f32, true, 0);
    while i < rows.len() {
        let end = if matches!(rows[i], LegendRow::Head(_)) { (i + 2).min(rows.len()) } else { i + 1 };
        let part: f32 = rows[i..end].iter().map(|row| row.height(scale, size)).sum();
        if used > 0.0 && used + part > limit && starts.len() < columns {
            starts.push(i);
            used = 0.0;
        }
        used += part;
        fits &= used <= limit;
        i = end;
    }
    (starts, fits)
}

/// The rows at 1:`scale` in a legend `size` times the default laid into at most `columns` columns
/// as evenly as they go: where each starts ([`flow`]), and the height of the tallest, in points.
pub(super) fn balanced(rows: &[LegendRow], columns: usize, at: (f64, f32)) -> (Vec<usize>, f32) {
    let (scale, size) = at;
    let total: f32 = rows.iter().map(|row| row.height(scale, size)).sum();
    let (mut low, mut high) = (total / columns as f32, total);
    for _ in 0..24 {
        let middle = (low + high) / 2.0;
        if flow(rows, columns, middle, at).1 {
            high = middle;
        } else {
            low = middle;
        }
    }
    let starts = flow(rows, columns, high, at).0;
    let tallest = column_parts(rows, &starts).map(|part| part.iter().map(|row| row.height(scale, size)).sum()).fold(0.0f32, f32::max);
    (starts, tallest)
}

/// The rows of each column, from where they start.
pub(super) fn column_parts<'r, 'a>(rows: &'r [LegendRow<'a>], starts: &'r [usize]) -> impl Iterator<Item = &'r [LegendRow<'a>]> {
    starts.iter().enumerate().map(|(k, &start)| &rows[start..starts.get(k + 1).copied().unwrap_or(rows.len())])
}

/// The widest of a column's `rows` at 1:`scale` in a legend `size` times the default, in points.
pub(super) fn widest(rows: &[LegendRow], at: (f64, f32)) -> f32 {
    let column = swatch_column(rows, at);
    rows.iter().map(|row| row.width(column, at.1)).fold(0.0, f32::max)
}

/// The stacked strokes a legend's class row draws: its class's own, else its layer's, as
/// [`rustgis_map::Layer::strokes`] finds them for a feature.
pub(super) fn row_strokes<'a>(l: &'a rustgis_map::Layer, en: &'a LegendEntry) -> &'a [rustgis_map::LineStroke] {
    if en.strokes.is_empty() { &l.line_strokes } else { &en.strokes }
}

/// The stacked fills a legend's class row draws: its class's own, else its layer's, as
/// [`rustgis_map::Layer::fills`] finds them for a feature.
pub(super) fn row_fills<'a>(l: &'a rustgis_map::Layer, en: &'a LegendEntry) -> &'a [rustgis_map::fills::FillLayer] {
    if en.fills.is_empty() { &l.fill_layers } else { &en.fills }
}

/// The width of a legend's column of swatches at 1:`scale` in a legend `size` times the default,
/// in points: it grows to hold the widest point symbol, so the labels line up.
pub(super) fn swatch_column(rows: &[LegendRow], (scale, size): (f64, f32)) -> f32 {
    rows.iter().map(|row| row.symbol_side(scale)).fold(LEGEND_SWATCH * size, f32::max)
}

/// A legend picture's size in points (a pixel a point).
pub(super) fn picture_size(p: &rustgis_tiles::cache::Payload) -> (f32, f32) {
    match p {
        rustgis_tiles::cache::Payload::Image(pm) => (pm.width() as f32, pm.height() as f32),
        _ => (0.0, 0.0),
    }
}

/// Whether a legend lists a WMS or WMTS layer: drawn, and with a legend.
pub(super) fn tile_listed(t: &rustgis_map::TileLayer) -> bool {
    t.draws() && t.opacity > 0.0 && !t.legends.is_empty()
}

/// A WMS or WMTS layer's legend picture for the map at 1:`scale`, as the Layers pane shows it:
/// the first of its legends there that is a picture. `None` while the one to show is still on
/// its way (`pending` then set), or when none can be shown. The addresses come from a service or
/// a project, so they are checked as the layer's own are.
pub(super) fn tile_legend(
    t: &rustgis_map::TileLayer,
    scale: f64,
    pending: &mut bool,
) -> Option<std::sync::Arc<rustgis_tiles::cache::Payload>> {
    use rustgis_tiles::cache::{self, Payload, State, Want};
    for l in rustgis_data::ogc::legends_at(&t.legends, scale) {
        if rustgis_tiles::address::check(&l.url, Some(&t.url)).is_err() {
            continue;
        }
        match cache::get(&l.url, Want::Image) {
            // No larger than the Layers pane shows one.
            State::Ready(p) if matches!(&*p, Payload::Image(pm) if pm.width() <= MAX_LEGEND_SIDE && pm.height() <= MAX_LEGEND_SIDE) => {
                return Some(p);
            }
            State::Pending => {
                *pending = true;
                return None;
            }
            // The next, if there is one.
            _ => {}
        }
    }
    None
}

/// Whether a legend picture `map`'s WMS and WMTS layers show at 1:`scale` is still on its way
/// (asking for those not yet asked for); those `exclude` names aren't listed, so aren't waited for.
pub fn tile_legends_pending(map: &Map, scale: f64, exclude: &[String]) -> bool {
    let mut pending = false;
    for t in map.tiles.iter().filter(|t| tile_listed(t) && !exclude.iter().any(|n| n.eq_ignore_ascii_case(&t.name))) {
        let _ = tile_legend(t, scale, &mut pending);
    }
    pending
}

/// Whether an element lies at least partly on `layout`'s page. The page's drawing and the redraw
/// gate ([`legends_pending`]) share this one test, in millimetres, so no drawing resolution can make
/// them disagree about a legend at the page's edge.
pub(super) fn on_page(layout: &Layout, e: &rustgis_map::layout::Element) -> bool {
    e.x + e.w > 0.0 && e.y + e.h > 0.0 && e.x < layout.width && e.y < layout.height
}

/// Whether a legend on the page `layout` lists a WMS or WMTS layer whose legend picture is still
/// on its way, at its frame's scale. The interactive page is drawn again once it lands. A legend
/// wholly off the page shows none of its pictures, so it is not waited for.
pub fn legends_pending(layout: &Layout, maps: &[Map]) -> bool {
    layout.elements.iter().filter(|e| e.visible && on_page(layout, e)).any(|e| {
        let ElementKind::Legend { frame, exclude, .. } = &e.kind else { return false };
        let Some(ElementKind::MapFrame { map, center, scale, bearing, pitch, .. }) = layout.element(*frame).map(|f| &f.kind) else {
            return false;
        };
        let Some(m) = maps.iter().find(|m| m.name.eq_ignore_ascii_case(map)) else { return false };
        let cam = frame_camera(m, *center, *scale, *bearing, *pitch);
        tile_legends_pending(m, m.ground_res_at(&cam) / rustgis_geom::PIXEL_M, exclude)
    })
}

/// Refuses an export stopped during a legend wait, or that drew a legend whose picture was still
/// on its way: it stays refused if the picture lands afterwards, as the file would still lack it.
/// The browser cannot wait; it asks the caller to export again once the pictures land.
pub(crate) fn finish_legends() -> Result<(), String> {
    rustgis_data::progress::check().map_err(|e| e.to_string())?;
    if crate::cancel::requested() {
        return Err(rustgis_data::DataError::Cancelled.to_string());
    }
    if LEGEND_MISSING.get() {
        return Err(
            "legend pictures were still on their way when the export stopped waiting for them; export again once they arrive".into()
        );
    }
    Ok(())
}

/// How long an export waits, natively, for all the legend pictures it shows.
pub(super) const LEGEND_WAIT: web_time::Duration = web_time::Duration::from_secs(30);

thread_local! {
    /// While an export draws on this thread: `Some`, holding the deadline for its legend pictures
    /// once its first legend has waited. Every page, element and fallback picture of that export
    /// shares it.
    pub(super) static LEGEND_BUDGET: std::cell::Cell<Option<Option<web_time::Instant>>> = const { std::cell::Cell::new(None) };
    /// Whether a legend of the export drawing on this thread was drawn without a picture.
    pub(super) static LEGEND_MISSING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// A shorter [`LEGEND_WAIT`] for a test to reach.
    #[cfg(test)]
    pub(super) static TEST_LEGEND_WAIT: std::cell::Cell<Option<web_time::Duration>> = const { std::cell::Cell::new(None) };
}

/// Ends the budget [`legend_budget`] opened when dropped, even by a panic.
pub(crate) struct LegendBudget(bool);

impl Drop for LegendBudget {
    fn drop(&mut self) {
        if self.0 {
            LEGEND_BUDGET.set(None);
            LEGEND_MISSING.set(false);
        }
    }
}

/// Opens one legend-wait budget for the export drawing on this thread, unless one is open (an
/// export of several pages or elements, each drawn through its own call, shares it).
pub(crate) fn legend_budget() -> LegendBudget {
    let opened = LEGEND_BUDGET.get().is_none();
    if opened {
        LEGEND_BUDGET.set(Some(None));
        LEGEND_MISSING.set(false);
    }
    LegendBudget(opened)
}

/// When the open budget's legend wait ends, started by the first legend that waits.
pub(super) fn legend_deadline() -> web_time::Instant {
    #[cfg(test)]
    let wait = TEST_LEGEND_WAIT.get().unwrap_or(LEGEND_WAIT);
    #[cfg(not(test))]
    let wait = LEGEND_WAIT;
    let fresh = web_time::Instant::now() + wait;
    match LEGEND_BUDGET.get() {
        Some(Some(deadline)) => deadline,
        Some(None) => {
            LEGEND_BUDGET.set(Some(Some(fresh)));
            fresh
        }
        None => fresh,
    }
}

/// Waits (natively, until `deadline`) for the legend pictures a legend of `map` at 1:`scale`
/// shows, as a frame waits for its tiles: a page needs them all. Only for those: not for the
/// cache's other fetches, nor for a layer the legend leaves out (`exclude`). `None` when the
/// run was cancelled meanwhile.
pub(super) fn wait_for_tile_legends(map: &Map, scale: f64, exclude: &[String], deadline: web_time::Instant) -> Option<()> {
    // In the browser, which can't wait, the page draws what has arrived.
    while cfg!(not(target_arch = "wasm32")) && web_time::Instant::now() < deadline && tile_legends_pending(map, scale, exclude) {
        if crate::cancel::requested() || rustgis_data::progress::check().is_err() {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    Some(())
}

/// The widest and tallest legend picture shown, in pixels, as in the Layers pane, and the largest
/// side of a legend's point symbol, in points.
pub(super) const MAX_LEGEND_SIDE: u32 = 2048;

/// The outline of a raster class's swatch in a legend, as the Layers pane draws one around it.
pub(super) const RASTER_SWATCH_EDGE: Rgba = Rgba::rgb(0x80, 0x80, 0x80);
/// A legend row's height, in points.
pub(super) const LEGEND_ROW: f32 = 15.0;
/// A legend's title, layers' names and classes' labels, in points, at its default text size; the
/// room its title takes, in rows; and how far a class's swatch and a name's or label's text sit
/// in, in points. Measuring and drawing a legend both take them, times its size ([`legend_size`]).
pub(super) const LEGEND_TITLE_PT: f32 = 12.0;
pub(super) const LEGEND_NAME_PT: f32 = 9.0;
pub(super) const LEGEND_LABEL_PT: f32 = 8.5;
pub(super) const LEGEND_TITLE_ROWS: f32 = 1.3;
pub(super) const LEGEND_INDENT: f32 = 6.0;
pub(super) const LEGEND_LABEL_INDENT: f32 = 12.0;
/// A legend swatch's width, in points.
pub(super) const LEGEND_SWATCH: f32 = 16.0;
/// The room between a legend's columns, in points.
pub(super) const LEGEND_COLUMN_GAP: f32 = 10.0;

/// The rows of the legend of a frame's map. With `drawn` (from [`shown_legends`]) it shows what the
/// frame draws: only the layers listed there, with their classes. Without, every class of every
/// visible layer. `opts` leaves out `<all other values>` and the layers it names, either way.
pub(super) fn legend_rows<'a>(map: &'a Map, drawn: Option<&[(u32, Vec<LegendEntry>)]>, opts: &LegendOptions<'a>) -> Vec<LegendRow<'a>> {
    legend_rows_missing(map, drawn, opts).0
}

/// [`legend_rows`], and whether a WMS or WMTS legend picture was still on its way when the lookup
/// that makes its rows was made (the row is then left out).
pub(super) fn legend_rows_missing<'a>(
    map: &'a Map,
    drawn: Option<&[(u32, Vec<LegendEntry>)]>,
    opts: &LegendOptions<'a>,
) -> (Vec<LegendRow<'a>>, bool) {
    let mut missing = false;
    let excluded = |name: &str| opts.exclude.iter().any(|n| n.eq_ignore_ascii_case(name));
    let mut rows = Vec::new();
    // In the order the map draws them and the Layers pane lists them, top first.
    for item in map.stack() {
        let l = match item {
            rustgis_map::Drawn::Layer(i) => match map.layers.get(i) {
                Some(l) => l,
                None => continue,
            },
            // A WMS or WMTS layer's legend is its service's picture, whatever the frame draws of it.
            rustgis_map::Drawn::Tile(i) => {
                if let Some(t) = map.tiles.get(i).filter(|t| tile_listed(t) && !excluded(&t.name))
                    && let Some(p) = tile_legend(t, opts.scale, &mut missing)
                {
                    rows.push(LegendRow::Head(opts.shown(&t.name)));
                    rows.push(LegendRow::Picture(p));
                }
                continue;
            }
        };
        if !(map.shows(l) && legend_lists(l) && !excluded(&l.name)) {
            continue;
        }
        let mut entries = match drawn {
            Some(d) => match d.iter().find(|(id, _)| *id == l.id) {
                Some((_, entries)) => entries.clone(),
                None => continue,
            },
            // A classified raster's every class (#1679).
            None if l.is_raster() => raster_classes(l, true).unwrap_or_default(),
            None => l.with_size_legend(l.renderer.legend()),
        };
        if drawn.is_none() && opts.show_no_data {
            entries.extend(l.renderer.no_data_legend(&l.dataset, None));
        }
        // The fallback row only (a class that happens to bear its name stays).
        if opts.hide_other && entries.iter().any(|e| e.fallback) {
            entries.retain(|e| !e.fallback);
            // Nothing left to show for this layer: leave it out rather than draw its plain swatch
            // (unless a diagram lists its categories below).
            if entries.is_empty() && l.diagram.is_none() {
                continue;
            }
        }
        // A layer with a diagram lists its categories under its classes.
        if let Some(d) = &l.diagram {
            entries.extend(d.legend(l.geometry_type()));
        }
        if entries.is_empty() {
            // A Unique Values layer that lists nothing draws nothing visible (#207); a raster has
            // no symbol of its own to show.
            if !l.is_raster() && !matches!(l.renderer, rustgis_map::Renderer::Unique { .. }) {
                rows.push(LegendRow::Plain(l, opts.shown(&l.name)));
            }
            continue;
        }
        rows.push(LegendRow::Head(opts.shown(&l.name)));
        rows.extend(entries.into_iter().map(|en| LegendRow::Class(l, en)));
    }
    (rows, missing)
}

/// The legend of a frame's map: its title, and its rows ([`legend_rows`]) under it in `columns`
/// columns, each as wide as its widest row. As even as they go in the height the box leaves; in
/// less, each is filled in turn and the last rows are left out. In a box too narrow they close up,
/// so that none is left out for it: long labels run into the next, and the last column's past the
/// box, as a long label does in one column.
pub(super) fn legend_missing(
    pm: &mut Canvas,
    r: Rect,
    map: &Map,
    (title, columns): (&str, usize),
    drawn: Option<&[(u32, Vec<LegendEntry>)]>,
    opts: LegendOptions,
    ink: Rgba,
    ppp: f64,
) -> bool {
    let ppp = ppp as f32;
    let at = (opts.scale, opts.size);
    let mut y = r.y();
    if !title.is_empty() {
        let line = LEGEND_ROW * opts.size * ppp;
        text::text_at(pm, title, r.x(), y + line / 2.0, LEGEND_TITLE_PT * opts.size * 1.33 * ppp, ink, true, text::Align::Left, false);
        y += line * LEGEND_TITLE_ROWS;
    }
    let (rows, missing) = legend_rows_missing(map, drawn, &opts);
    let columns = columns.clamp(1, MAX_LEGEND_COLUMNS);
    let starts = flow(&rows, columns, balanced(&rows, columns, at).1.min((r.bottom() - y) / ppp), at).0;
    let gap = LEGEND_COLUMN_GAP * opts.size;
    let across = (column_parts(&rows, &starts).map(|part| widest(part, at)).sum::<f32>() + gap * (starts.len() - 1) as f32) * ppp;
    let close_up = if across > r.width() { r.width() / across } else { 1.0 };
    let mut x = r.x();
    for (k, part) in column_parts(&rows, &starts).enumerate() {
        // The last has the rest of the box's width.
        let right = if k + 1 < starts.len() { x + widest(part, at) * ppp * close_up } else { r.right() };
        let Some(column) = Rect::from_ltrb(x, y, right, r.bottom()) else { break };
        // The room left of it that holds nothing: none at the box's edge, the gap between columns
        // as drawn after it.
        let lead = if k == 0 { 0.0 } else { gap * ppp * close_up };
        legend_column(pm, column, part, (columns > 1, lead), map, &opts, ink, ppp);
        x = right + gap * ppp * close_up;
    }
    missing
}

/// [`legend_missing`], for a legend whose missing pictures nobody asks about.
#[cfg(test)]
pub(super) fn legend(
    pm: &mut Canvas,
    r: Rect,
    map: &Map,
    title_columns: (&str, usize),
    drawn: Option<&[(u32, Vec<LegendEntry>)]>,
    opts: LegendOptions,
    ppp: f64,
) {
    legend_missing(pm, r, map, title_columns, drawn, opts, INK, ppp);
}

/// A column of a legend: its rows from the top of `r`, as many as fit. With `name_with_class`, a
/// layer's name only with room for the class under it, as [`flow`] keeps them together. `lead`
/// pixels left of it hold nothing, which its swatches may reach into.
pub(super) fn legend_column(
    pm: &mut Canvas,
    r: Rect,
    rows: &[LegendRow],
    (name_with_class, lead): (bool, f32),
    map: &Map,
    opts: &LegendOptions,
    dark: Rgba,
    ppp: f32,
) {
    let mut y = r.y();
    let size = opts.size;
    let line = LEGEND_ROW * size * ppp;
    let sw = swatch_column(rows, (opts.scale, size)) * ppp;
    // A line's or polygon's swatch, across the column of swatches.
    let across = LEGEND_SWATCH * size * ppp;
    let mut rows = rows.iter().peekable();
    while let Some(row) = rows.next() {
        if let LegendRow::Picture(p) = row {
            match &**p {
                rustgis_tiles::cache::Payload::Image(pic) => match legend_picture(pm, r, y, pic, ppp * size) {
                    Some((h, true)) => y += h,
                    // Cut at the bottom, or no room for it.
                    _ => break,
                },
                _ => continue,
            }
            continue;
        }
        // The rectangle's own bottom, which a sum of its top and height can miss by a rounding,
        // and a thousandth of a row's slack: a legend sized to its rows keeps the last one.
        let height = row.height(opts.scale, size) * ppp;
        if y + height > r.bottom() + line * 1e-3 {
            break;
        }
        // A WMS or WMTS layer's name only with room for some of its picture under it.
        if let (LegendRow::Head(_), Some(LegendRow::Picture(p))) = (&row, rows.peek())
            && let rustgis_tiles::cache::Payload::Image(pic) = &**p
            && y + line + picture_scale(r, pic, ppp * size) > r.bottom() + line * 1e-3
        {
            break;
        }
        if let (true, LegendRow::Head(_), Some(next)) = (name_with_class, &row, rows.peek())
            && matches!(next, LegendRow::Class(..))
            && y + height + next.height(opts.scale, size) * ppp > r.bottom() + line * 1e-3
        {
            break;
        }
        match row {
            LegendRow::Plain(l, name) => {
                let s = l.renderer.symbol_for(&l.dataset, 0);
                let sym = (&s, l.line_strokes.as_slice(), l.fill_layers.as_slice());
                // Its room: left of the column, and the gap before its name.
                let room = (lead, LEGEND_INDENT * size * ppp);
                swatch(pm, (r.x(), y, sw, height), l, sym, map.layer_opacity(l), opts.scale, (ppp, across, room));
                let (x, pt) = (r.x() + sw + LEGEND_INDENT * size * ppp, LEGEND_NAME_PT * size * 1.33 * ppp);
                text::text_at(pm, name, x, y + height / 2.0, pt, dark, false, text::Align::Left, false);
            }
            LegendRow::Head(name) => {
                text::text_at(pm, name, r.x(), y + line / 2.0, LEGEND_NAME_PT * size * 1.33 * ppp, dark, true, text::Align::Left, false);
            }
            LegendRow::Class(l, en) => {
                swatch(
                    pm,
                    (r.x() + LEGEND_INDENT * size * ppp, y, sw, height),
                    l,
                    (&en.symbol, row_strokes(l, en), row_fills(l, en)),
                    map.layer_opacity(l),
                    opts.scale,
                    // Its room: its indent and what lies left of the column, and the gap before its label.
                    (ppp, across, (lead + LEGEND_INDENT * size * ppp, (LEGEND_LABEL_INDENT - LEGEND_INDENT) * size * ppp)),
                );
                text::text_at(
                    pm,
                    &en.label,
                    r.x() + sw + LEGEND_LABEL_INDENT * size * ppp,
                    y + height / 2.0,
                    LEGEND_LABEL_PT * size * 1.33 * ppp,
                    dark,
                    false,
                    text::Align::Left,
                    false,
                );
            }
            LegendRow::Picture(_) => {}
        }
        y += height;
    }
}

/// The pixels a legend picture's pixel takes in the legend `r`: a pixel a point, narrowed to fit
/// across after its indent.
pub(super) fn picture_scale(r: Rect, pic: &Pixmap, ppp: f32) -> f32 {
    ppp.min((r.right() - r.x() - 6.0 * ppp).max(0.0) / pic.width().max(1) as f32)
}

/// Draws a WMS or WMTS layer's legend picture at `y` in the legend `r`, indented as a layer's
/// classes, a pixel a point, narrowed to fit across ([`picture_scale`]). One taller than the room
/// left is cut at the bottom. The height drawn and whether it all fitted; `None` when none of it
/// did.
pub(super) fn legend_picture(pm: &mut Canvas, r: Rect, y: f32, pic: &Pixmap, ppp: f32) -> Option<(f32, bool)> {
    let x = r.x() + 6.0 * ppp;
    let k = picture_scale(r, pic, ppp);
    let room = r.bottom() - y;
    // The source rows that fit, a thousandth of a row's slack, as the rows have.
    let rows = if k > 0.0 { ((room + LEGEND_ROW * ppp * 1e-3) / k).floor().min(pic.height() as f32) } else { 0.0 };
    if rows < 1.0 {
        return None;
    }
    let shown;
    let src = if (rows as u32) < pic.height() {
        shown = tiny_skia::IntRect::from_xywh(0, 0, pic.width(), rows as u32).and_then(|c| pic.clone_rect(c))?;
        &shown
    } else {
        pic
    };
    pm.draw_pixmap(
        0,
        0,
        src.as_ref(),
        &PixmapPaint { quality: tiny_skia::FilterQuality::Bilinear, ..PixmapPaint::default() },
        Transform::from_row(k, 0.0, 0.0, k, x, y),
        None,
    );
    Some((rows * k, (rows as u32) == pic.height()))
}

/// The size a legend with no title takes in one column, in two and so on, in mm (width, height):
/// its rows as [`legend`] draws them, the classes a frame shows at the renderer's scale denominator
/// `scale` (as the layout finds it for a frame, a complete source read through as [`render`] reads
/// it), or every class with `all_classes`. For sizing a legend to its content. `None` when a
/// complete source can't be read, as [`render`] then draws no page. A WMS or WMTS layer's legend
/// picture still on its way isn't waited for, nor counted ([`tile_legends_pending`] says whether
/// one is). Its layers' rows are measured by the `labels` it shows for them, else their names.
pub fn legend_sizes_mm(
    map: &mut Map,
    scale: f64,
    all_classes: bool,
    hide_other: bool,
    (exclude, labels): (&[String], &BTreeMap<String, String>),
) -> Option<[(f64, f64); MAX_LEGEND_COLUMNS]> {
    let text_size = rustgis_map::layout::LEGEND_TEXT_PT;
    legend_sizes_with_no_data_mm(map, scale, all_classes, hide_other, (false, text_size), (exclude, labels))
}

/// Sizes of an optional no-data legend whose text is `text_size` points (#2581), using the same
/// rows and filters as the drawing.
pub fn legend_sizes_with_no_data_mm(
    map: &mut Map,
    scale: f64,
    all_classes: bool,
    hide_other: bool,
    (show_no_data, text_size): (bool, f64),
    (exclude, labels): (&[String], &BTreeMap<String, String>),
) -> Option<[(f64, f64); MAX_LEGEND_COLUMNS]> {
    let drawn = if all_classes { None } else { Some(complete_shown_legends(map, scale, show_no_data, &mut None, &Env::default())?) };
    let size = legend_size(text_size);
    let rows = legend_rows(map, drawn.as_deref(), &LegendOptions { show_no_data, hide_other, exclude, labels, scale, size });
    let mm = 25.4 / 96.0;
    Some(std::array::from_fn(|i| {
        let (starts, height) = balanced(&rows, i + 1, (scale, size));
        let gap = LEGEND_COLUMN_GAP * size * (starts.len() - 1) as f32;
        let width: f32 = column_parts(&rows, &starts).map(|part| widest(part, (scale, size))).sum::<f32>() + gap;
        (f64::from(width) * mm, f64::from(height) * mm)
    }))
}

/// The height in mm [`legend`] gives a title above its rows, which [`legend_sizes_mm`] leaves out,
/// in a legend whose text is `text_size` points (#2581).
pub fn legend_title_mm(text_size: f64) -> f64 {
    f64::from(LEGEND_ROW * legend_size(text_size)) * f64::from(LEGEND_TITLE_ROWS) * 25.4 / 96.0
}
