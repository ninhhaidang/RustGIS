//! Required capabilities at saved-document boundaries. The derived Project fields stay the
//! serialization source of truth; the header is computed from the payload, never cached.

use rustgis_map::{Layer, Map, raster::RasterRenderer};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{FORMAT, ModelGraph, ModelNodeKind, ModelStep, Project};

impl Serialize for Project {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Document<'a> {
            #[serde(skip_serializing_if = "Vec::is_empty")]
            requires: Vec<&'static str>,
            #[serde(flatten, with = "Project")]
            project: &'a Project,
        }
        Document { requires: self.required_capabilities(), project: self }.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Project {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Project::deserialize(deserializer)
    }
}

impl Project {
    /// Required features of all saved maps, snapshots and replayable commands/scripts.
    pub(crate) fn required_capabilities(&self) -> Vec<&'static str> {
        let mut required = map_capabilities(self.maps.iter().chain(self.offline_areas.iter().map(|a| &a.data)));
        if !self.origin_id.is_empty() || self.field_origin.is_some() || self.offline_areas.iter().any(|area| !area.id.is_empty()) {
            required.push("field-origin");
        }
        // A return check an older reader would drop on saving, and then accept any copy.
        if self.field_origin.as_ref().is_some_and(|origin| !origin.generation.is_empty())
            || self.offline_areas.iter().any(|area| !area.generation.is_empty())
        {
            required.push("field-generation");
        }
        // A table saved in a coordinate system of its own, as a layer's is (map_capabilities).
        if !required.contains(&super::NATIVE_COORDINATES) && self.tables.iter().any(|t| held_natively(&t.dataset)) {
            required.push(super::NATIVE_COORDINATES);
        }
        // Saved symbology keeps its images too.
        if !required.contains(&"picture-markers") && self.styles.iter().any(|s| s.renderer.has_picture_markers()) {
            required.push("picture-markers");
        }
        let commands: Vec<&'static str> =
            COMMAND_CAPABILITIES.iter().filter_map(|&(name, uses)| self.uses_commands(uses).then_some(name)).collect();
        required.extend(commands.into_iter().filter(|name| !required.contains(name)).collect::<Vec<_>>());
        if self.tasks.iter().any(|task| task.steps.iter().any(|step| command_uses_table_export_order(&step.command, &step.params)))
            || self.gp_history.iter().any(|record| {
                command_uses_table_export_order(&record.id, &record.params)
                    || record.command.as_ref().is_some_and(|(id, params)| command_uses_table_export_order(id, params))
            })
            || self.notebooks.iter().any(|notebook| {
                notebook.language().is_none()
                    && notebook
                        .cells
                        .iter()
                        .any(|cell| cell.kind == crate::cmd::notebook::CellKind::Code && script_uses_table_export_order(&cell.source))
            })
        {
            required.push("table-export-order");
        }
        // Saved layouts as well as saved commands: an older reader keeps neither.
        if !required.contains(&"layout-element-background")
            && self.layouts.iter().any(|l| l.elements.iter().any(|e| e.background.is_some()))
        {
            required.push("layout-element-background");
        }
        if !required.contains(&"layout-element-ink") && self.layouts.iter().any(|l| l.elements.iter().any(|e| e.kind.ink().is_some())) {
            required.push("layout-element-ink");
        }
        if !required.contains(&"layout-element-wrap") && self.layouts.iter().any(|l| l.elements.iter().any(wraps)) {
            required.push("layout-element-wrap");
        }
        if !required.contains(&"layout-chart-data") && self.layouts.iter().any(|l| l.elements.iter().any(charts_data)) {
            required.push("layout-chart-data");
        }
        if !required.contains(&"layout-chart-look") && self.layouts.iter().any(|l| l.elements.iter().any(charts_look)) {
            required.push("layout-chart-look");
        }
        if !required.contains(&"layout-chart-classes") && self.layouts.iter().any(|l| l.elements.iter().any(charts_classes)) {
            required.push("layout-chart-classes");
        }
        if !required.contains(&"layout-chart-stack") && self.layouts.iter().any(|l| l.elements.iter().any(charts_stack)) {
            required.push("layout-chart-stack");
        }
        if !required.contains(&"layout-frame-border") && self.layouts.iter().any(|l| l.elements.iter().any(|e| e.kind.has_custom_border()))
        {
            required.push("layout-frame-border");
        }
        // A coordinate system whose PROJ definition, which an older reader reads, doesn't place
        // points as its WKT does (angles in grads, a datum transformation the definition can't
        // hold).
        struct Payload<'a>(&'a Project);
        impl Serialize for Payload<'_> {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                Project::serialize(self.0, serializer)
            }
        }
        if rustgis_data::raster::crs::saves_wkt(&Payload(self)) {
            required.push("crs-wkt");
        }
        required
    }

    /// Apply the same capability predicate to every replayable project container.
    fn uses_commands(&self, uses: CommandCheck) -> bool {
        self.models.iter().any(|m| model_uses(&m.steps, m.graph.as_ref(), uses))
            || self.tasks.iter().any(|t| t.steps.iter().any(|s| uses(&s.command, &s.params)))
            || self.gp_history.iter().any(|h| {
                uses(if h.id.is_empty() { &h.tool } else { &h.id }, &h.params)
                    || h.command.as_ref().is_some_and(|(id, params)| uses(id, params))
            })
            || self.notebooks.iter().any(|n| {
                n.language().is_none()
                    && n.cells.iter().any(|c| c.kind == crate::cmd::notebook::CellKind::Code && script_uses(&c.source, uses))
            })
    }
}

type CommandCheck = fn(&str, &Value) -> bool;
const COMMAND_CAPABILITIES: &[(&str, CommandCheck)] = &[
    ("slope-percent", command_uses_percent_slope),
    ("tabulate-intersection-unit", command_uses_tabulate_unit),
    ("tabulate-intersection-empty-zones", command_uses_tabulate_empty_zones),
    ("layout-element-background", command_uses_element_background),
    ("picture-markers", command_uses_picture_marker),
    ("graticule-mgrs", command_draws_mgrs_graticule),
    ("graticule-on-top", command_puts_graticule_on_top),
    ("graticule-color", command_colors_graticule),
    ("layout-element-ink", command_uses_element_ink),
    ("layout-element-wrap", command_uses_element_wrap),
    ("layout-chart-data", command_uses_chart_data),
    ("layout-chart-look", command_uses_chart_look),
    ("layout-chart-classes", command_uses_chart_classes),
    ("layout-chart-stack", command_uses_chart_stack),
    ("blank-paper", command_uses_blank_paper),
    ("layout-frame-border", command_uses_frame_border),
];

/// A saved `map.graticule` that switches to the MGRS grid, its type read as the command reads it:
/// an older reader refuses the type, so the command would fail there.
fn command_draws_mgrs_graticule(id: &str, params: &Value) -> bool {
    match id {
        "map.graticule" => params
            .get("type")
            .and_then(Value::as_str)
            .and_then(rustgis_map::graticule::GraticuleType::from_name)
            .is_some_and(|t| t == rustgis_map::graticule::GraticuleType::Mgrs),
        "script.run" => params.get("script").and_then(Value::as_str).is_some_and(|s| script_uses(s, command_draws_mgrs_graticule)),
        _ => false,
    }
}

/// A saved `map.graticule` that sets whether the grid draws over the layers, or puts the default
/// back with `null`: an older reader ignores the key, and without `on`, `type` or `crs` it toggles
/// the grid where this one keeps it as it is, so any value needs it.
fn command_puts_graticule_on_top(id: &str, params: &Value) -> bool {
    match id {
        "map.graticule" => params.get("onTop").is_some(),
        "script.run" => params.get("script").and_then(Value::as_str).is_some_and(|s| script_uses(s, command_puts_graticule_on_top)),
        _ => false,
    }
}

/// A saved `map.graticule` that sets the grid's color, or puts the palette's back with `null` or
/// `""`: an older reader ignores the key, and without `on`, `type` or `crs` it toggles the grid
/// where this one keeps it as it is, so any value needs it.
fn command_colors_graticule(id: &str, params: &Value) -> bool {
    match id {
        "map.graticule" => params.get("color").is_some(),
        "script.run" => params.get("script").and_then(Value::as_str).is_some_and(|s| script_uses(s, command_colors_graticule)),
        _ => false,
    }
}

/// A layout command that changes a map frame's border (#2557): an older reader warns that it
/// ignores `border` and draws the default one. `null` only gives the default back, which an older
/// reader's frames always have.
fn command_uses_frame_border(id: &str, params: &Value) -> bool {
    match id {
        "layout.update" => params.get("border").is_some_and(|b| !b.is_null()),
        "layout.import" => imported_layout_uses(params, "\"border_", |e| e.kind.has_custom_border()),
        "script.run" => params.get("script").and_then(Value::as_str).is_some_and(|s| script_uses(s, command_uses_frame_border)),
        _ => false,
    }
}

/// A `map.basemap` command that gives the blank basemap a paper colour (#2532): an older reader
/// ignores `color` and draws white. White, `null` or another basemap leaves no paper to keep.
fn command_uses_blank_paper(id: &str, params: &Value) -> bool {
    match id {
        "map.basemap" => {
            params.get("name").and_then(Value::as_str).is_some_and(|n| n.trim().eq_ignore_ascii_case("blank"))
                && params.get("color").and_then(Value::as_str).is_some_and(|c| !c.trim().eq_ignore_ascii_case("#FFFFFF"))
        }
        "script.run" => params.get("script").and_then(Value::as_str).is_some_and(|s| script_uses(s, command_uses_blank_paper)),
        _ => false,
    }
}

/// A layout command that gives an element a background: an older reader warns that it ignores
/// the key and makes the element without one. `null` only takes a background away, which an
/// older reader's elements never have.
fn command_uses_element_background(id: &str, params: &Value) -> bool {
    match id {
        "layout.addElement" | "layout.update" => params.get("background").is_some_and(|b| !b.is_null()),
        "layout.import" => imported_layout_uses(params, "\"background\"", |e| e.background.is_some()),
        "script.run" => params.get("script").and_then(Value::as_str).is_some_and(|s| script_uses(s, command_uses_element_background)),
        _ => false,
    }
}

/// A layout command that gives a legend, colorbar, table or chart an ink (#2257). Only an
/// imported layout's: an older reader refuses "color" on those elements in `layout.addElement`
/// and `layout.update`, so such a command fails there instead of losing the ink.
fn command_uses_element_ink(id: &str, params: &Value) -> bool {
    match id {
        "layout.import" => imported_layout_uses(params, "\"color\"", |e| e.kind.ink().is_some()),
        "script.run" => params.get("script").and_then(Value::as_str).is_some_and(|s| script_uses(s, command_uses_element_ink)),
        _ => false,
    }
}

/// Whether a layout element is a text that wraps to its box (#2346).
fn wraps(e: &rustgis_map::layout::Element) -> bool {
    matches!(e.kind, rustgis_map::layout::ElementKind::Text { wrap: true, .. })
}

/// A layout command that makes a text wrap (#2346): an older reader warns that it ignores
/// `wrap` and draws the text unwrapped. `"wrap": false` only keeps a text as an older reader's
/// texts always are.
fn command_uses_element_wrap(id: &str, params: &Value) -> bool {
    match id {
        "layout.addElement" | "layout.update" => params.get("wrap").and_then(crate::cmd::bool_value) == Some(true),
        "layout.import" => imported_layout_uses(params, "\"wrap\"", wraps),
        "script.run" => params.get("script").and_then(Value::as_str).is_some_and(|s| script_uses(s, command_uses_element_wrap)),
        _ => false,
    }
}

/// Whether a layout element is a chart grouped, ordered, cut or gathered as an older reader's
/// can't be (#2487): it would draw a bar per feature, the 20 largest.
fn charts_data(e: &rustgis_map::layout::Element) -> bool {
    e.kind.chart_uses_data_options()
}

/// A layout command that groups, orders or cuts a chart, or gathers its rest in an Other bar:
/// an older reader warns that it ignores those keys and draws a bar per feature, the 20 largest.
/// One that gives the defaults, or takes a group away, keeps a chart as an older reader draws it;
/// a label given to `layout.update`, which an older reader's update doesn't take, needs it too.
fn command_uses_chart_data(id: &str, params: &Value) -> bool {
    use rustgis_map::layout::{CHART_BARS, ChartOrder};
    use rustgis_map::report::StatKind;
    match id {
        "layout.addElement" | "layout.update" => {
            let text = |k: &str| params.get(k).and_then(Value::as_str).map(str::trim);
            // An older reader's update takes no chart label: it would say nothing and keep the old.
            (id == "layout.update" && params.get("label").is_some_and(|v| !v.is_null()))
                || text("group").is_some_and(|g| !g.is_empty())
                || text("statistic").and_then(StatKind::from_code).is_some_and(|s| s != StatKind::default())
                || text("order").and_then(ChartOrder::from_code).is_some_and(|o| o != ChartOrder::default())
                || params.get("bars").and_then(Value::as_u64).is_some_and(|n| n != CHART_BARS as u64)
                || params.get("other").and_then(crate::cmd::bool_value) == Some(true)
        }
        "layout.import" => ["\"group\"", "\"statistic\"", "\"order\"", "\"bars\"", "\"other\""]
            .iter()
            .any(|key| imported_layout_uses(params, key, charts_data)),
        "script.run" => params.get("script").and_then(Value::as_str).is_some_and(|s| script_uses(s, command_uses_chart_data)),
        _ => false,
    }
}

fn charts_look(e: &rustgis_map::layout::Element) -> bool {
    e.kind.chart_uses_look_options()
}

/// A layout command that draws a chart as columns, writes its values or their decimals, gives it
/// its own title or none, a bar colour, or labels against its bars (#2487): an older reader
/// ignores those keys and draws bars running across in the default blue, with the title it is
/// given. A `layout.update`'s `title` and `orientation` may be a legend's or a colorbar's, which
/// it can't tell apart, so only the keys a chart alone takes are read there; the layout saved
/// with the chart requires it anyway. Keys at their defaults keep a chart as an older reader
/// draws it.
fn command_uses_chart_look(id: &str, params: &Value) -> bool {
    let text = |k: &str| params.get(k).and_then(Value::as_str).map(|t| t.trim().to_ascii_lowercase());
    let only_chart = || {
        params.get("values").and_then(crate::cmd::bool_value) == Some(true)
            || params.get("decimals").is_some_and(|d| d.is_u64())
            || params.get("barColor").is_some_and(|c| !c.is_null())
            || text("labelAlign").as_deref() == Some("right")
    };
    match id {
        "layout.addElement" => {
            text("kind").as_deref() == Some("chart")
                && (only_chart() || text("orientation").as_deref() == Some("vertical") || params.get("title").is_some_and(|t| !t.is_null()))
        }
        "layout.update" => only_chart(),
        "layout.import" => ["\"orientation\"", "\"values\"", "\"decimals\"", "\"title\"", "\"bar_color\"", "\"label_align\""]
            .iter()
            .any(|key| imported_layout_uses(params, key, charts_look)),
        "script.run" => params.get("script").and_then(Value::as_str).is_some_and(|s| script_uses(s, command_uses_chart_look)),
        _ => false,
    }
}

fn charts_classes(e: &rustgis_map::layout::Element) -> bool {
    e.kind.chart_uses_classes()
}

/// A layout command that colours a chart's bars by its layer's classes (#2487 part 3): an older
/// reader ignores the key and draws every bar in the chart's bar colour. `false` keeps a chart
/// as an older reader draws it; only a chart takes the key, so a `layout.update` is read by it
/// alone.
fn command_uses_chart_classes(id: &str, params: &Value) -> bool {
    let classes = || params.get("classes").and_then(crate::cmd::bool_value) == Some(true);
    match id {
        "layout.addElement" => {
            params.get("kind").and_then(Value::as_str).is_some_and(|k| k.trim().eq_ignore_ascii_case("chart")) && classes()
        }
        "layout.update" => classes(),
        "layout.import" => imported_layout_uses(params, "\"classes\"", charts_classes),
        "script.run" => params.get("script").and_then(Value::as_str).is_some_and(|s| script_uses(s, command_uses_chart_classes)),
        _ => false,
    }
}

fn charts_stack(e: &rustgis_map::layout::Element) -> bool {
    e.kind.chart_uses_stack()
}

/// A layout command that stacks a chart's bars by a second field (#2487 part 3b): an older reader
/// ignores the key and draws each bar whole. `""` keeps a chart as an older reader draws it; only
/// a chart takes the key, so a `layout.update` is read by it alone.
fn command_uses_chart_stack(id: &str, params: &Value) -> bool {
    let stack = || params.get("stack").and_then(Value::as_str).is_some_and(|f| !f.trim().is_empty());
    match id {
        "layout.addElement" => {
            params.get("kind").and_then(Value::as_str).is_some_and(|k| k.trim().eq_ignore_ascii_case("chart")) && stack()
        }
        "layout.update" => stack(),
        "layout.import" => imported_layout_uses(params, "\"stack\"", charts_stack),
        "script.run" => params.get("script").and_then(Value::as_str).is_some_and(|s| script_uses(s, command_uses_chart_stack)),
        _ => false,
    }
}

/// Whether a `layout.import` carries its layout as `"text"` with an element `uses`: an older
/// reader reads it without the keys it doesn't know. One read from a `"path"` is a layout file,
/// which carries no format header yet (#2277). Every save asks, so a text without the feature's
/// `key` (a saved layout writes it only when set) isn't read through, unless it has a backslash:
/// JSON may spell the key with escapes (`"\u0063olor"`).
fn imported_layout_uses(params: &Value, key: &str, uses: fn(&rustgis_map::layout::Element) -> bool) -> bool {
    params
        .get("text")
        .and_then(Value::as_str)
        .filter(|t| t.contains(key) || t.contains('\\'))
        .and_then(|t| serde_json::from_str::<rustgis_map::layout::Layout>(t).ok())
        .is_some_and(|l| l.elements.iter().any(uses))
}

/// Require only executable payloads being saved: graph files omit steps, web tools omit graphs.
pub(crate) fn model_capabilities(steps: &[ModelStep], graph: Option<&ModelGraph>) -> Vec<&'static str> {
    COMMAND_CAPABILITIES.iter().filter_map(|&(name, uses)| model_uses(steps, graph, uses).then_some(name)).collect()
}

/// Traverse executable model nodes with the same predicate used for project commands.
fn model_uses(steps: &[ModelStep], graph: Option<&ModelGraph>, uses: CommandCheck) -> bool {
    steps.iter().any(|s| uses(&s.tool, &s.params))
        || graph.is_some_and(|g| g.nodes.iter().any(|n| n.kind == ModelNodeKind::Tool && uses(n.tool.as_deref().unwrap_or(""), &n.params)))
}

/// Inspect known command parameters, not arbitrary data that happens to contain unit names.
fn command_uses_percent_slope(id: &str, params: &serde_json::Value) -> bool {
    let percent = |p: &serde_json::Value| {
        p.get("function").and_then(serde_json::Value::as_str) == Some("slope")
            && p.get("units").and_then(serde_json::Value::as_str) == Some("percent")
    };
    match id {
        "raster.function" => percent(params),
        "raster.functionChain" => params.get("steps").and_then(serde_json::Value::as_array).is_some_and(|steps| steps.iter().any(percent)),
        "script.run" => {
            params.get("script").and_then(serde_json::Value::as_str).is_some_and(|s| script_uses(s, command_uses_percent_slope))
        }
        _ => false,
    }
}

/// Blank and km² unit choices need no capability.
fn command_uses_tabulate_unit(id: &str, params: &Value) -> bool {
    tabulate_uses(id, params, |p| {
        p.get("areaUnit").and_then(Value::as_str).map(str::trim).is_some_and(|u| !u.is_empty() && !u.eq_ignore_ascii_case("km2"))
    })
}

/// Only a run that requests zero rows needs the capability. Tool booleans also accept text.
fn command_uses_tabulate_empty_zones(id: &str, params: &Value) -> bool {
    tabulate_uses(id, params, |p| match p.get("includeEmptyZones") {
        Some(Value::Bool(on)) => *on,
        Some(Value::String(on)) => on.trim().eq_ignore_ascii_case("true"),
        _ => false,
    })
}

/// Tests Tabulate Intersection parameters in executable commands, including nested envelopes.
fn tabulate_uses(id: &str, params: &Value, uses: fn(&Value) -> bool) -> bool {
    if crate::cmd::gp::tool_id(id) == Some("tabulateIntersection") {
        return uses(params);
    }
    let tabulate = params.get("tool").and_then(Value::as_str).and_then(crate::cmd::gp::tool_id) == Some("tabulateIntersection");
    match id {
        "gp.run" if tabulate => crate::parameters::params_envelope(params.get("params")).is_ok_and(|p| uses(&p)),
        // Batch ignores non-object shared params; older tools refuse unknown binding parameters.
        "gp.batch" if tabulate => params.get("params").filter(|p| p.is_object()).is_some_and(uses),
        "script.run" => {
            params.get("script").and_then(Value::as_str).is_some_and(|s| script_uses(s, |id, params| tabulate_uses(id, params, uses)))
        }
        _ => false,
    }
}

/// `layer.marker`, a line stroke's markers or a dot fill given an image: an older reader would
/// leave its `image` out and keep the marker it had, or refuse `"image"` as a mark.
fn command_uses_picture_marker(id: &str, params: &Value) -> bool {
    let any = |list: &str, image: fn(&Value) -> Option<&Value>| {
        params.get(list).and_then(Value::as_array).is_some_and(|l| l.iter().any(|v| image(v).is_some_and(Value::is_string)))
    };
    match id {
        "layer.marker" => params.get("image").is_some_and(Value::is_string),
        "layer.lineStrokes" => any("strokes", |st| st.get("markers")?.get("image")),
        "layer.fillLayers" => any("fills", |f| f.get("image")),
        "script.run" => params.get("script").and_then(Value::as_str).is_some_and(|s| script_uses(s, command_uses_picture_marker)),
        _ => false,
    }
}

/// Use the executor's line grammar; comments/invalid lines are not executable commands, and
/// script_line refuses nested scripts, so this check cannot recurse through script.run.
fn script_uses(script: &str, uses: impl Fn(&str, &Value) -> bool) -> bool {
    script
        .lines()
        .take(crate::cmd::workflows::MAX_SCRIPT_LINES)
        .any(|line| crate::script_line(line).is_some_and(|parsed| parsed.is_ok_and(|(id, params)| uses(&id, &params))))
}

fn relief_effects(layer: &Layer) -> bool {
    layer.raster.as_ref().is_some_and(|r| {
        matches!(&r.style.renderer, RasterRenderer::ShadedRelief { shadow, sky, fill, rock, .. }
            if *shadow != 0.0 || *sky != 0.0 || *fill != 0.0 || rock.is_some())
    })
}

/// Whether a layer sets Shaded Relief's ambient or balanced light (#1977): newer than
/// `shaded-relief-effects`, so a reader that knows only that would drop them.
fn relief_light(layer: &Layer) -> bool {
    layer.raster.as_ref().is_some_and(
        |r| matches!(&r.style.renderer, RasterRenderer::ShadedRelief { ambient, balanced, .. } if ambient.is_some() || *balanced),
    )
}

/// Whether a layer is a window of a Zarr variable following the time slider (#1720): an older
/// reader would drop the window and read the whole map at each step.
fn zarr_window(layer: &Layer) -> bool {
    layer.raster.as_ref().and_then(|r| r.time_zarr.as_ref()).is_some_and(|z| z.window.is_some())
}

/// Whether a dataset is saved in a coordinate system of its own (`Dataset::held_in`): an older
/// reader would take its coordinates for longitude/latitude, and save it again without its
/// system (one with no features too, such as a template's layers).
fn held_natively(ds: &rustgis_data::Dataset) -> bool {
    !ds.windowed && ds.coord_crs() != rustgis_data::raster::Crs::LonLat
}

/// The capabilities a layer needs, in [`map_capabilities`]'s order.
fn layer_capabilities(layer: &Layer) -> Vec<&'static str> {
    [
        (relief_effects(layer), "shaded-relief-effects"),
        (relief_light(layer), "shaded-relief-light"),
        (layer.size_by.is_some(), super::SIZE_BY_CAPABILITY),
        (layer.has_picture_markers(), "picture-markers"),
        (zarr_window(layer), "zarr-window"),
        (held_natively(&layer.dataset), super::NATIVE_COORDINATES),
    ]
    .into_iter()
    .filter_map(|(used, name)| used.then_some(name))
    .collect()
}

fn map_capabilities<'a>(maps: impl IntoIterator<Item = &'a Map>) -> Vec<&'static str> {
    let (
        mut block,
        mut shape,
        mut water,
        mut shore,
        mut block_water,
        mut relief,
        mut light,
        mut legends,
        mut sized,
        mut pictures,
        mut zarr,
        mut mgrs,
        mut on_top,
        mut grid_color,
    ) = (false, false, false, false, false, false, false, false, false, false, false, false, false, false);
    let mut polygon = false;
    let (mut offline_area_id, mut paper, mut native) = (false, false, false);
    for map in maps {
        offline_area_id |= map.offline_area_id.is_some();
        // A blank basemap's paper colour (#2532), which an older reader would draw and save white.
        paper |= map.paper.is_some();
        // An older reader doesn't know the MGRS graticule (#1042 piece 8): it refuses the
        // project by name rather than failing to read the type.
        mgrs |= map.graticule_type == rustgis_map::graticule::GraticuleType::Mgrs;
        // Nor the graticule drawn over the layers or in a color of its own: it would save them away.
        on_top |= map.graticule_on_top;
        grid_color |= map.graticule_color.is_some();
        if let Some(t) = &map.view.terrain {
            block |= t.block.is_some();
            // A round block (#2318), which an older reader would draw and save as a box; one cut
            // to polygons, which a reader of round blocks only couldn't read.
            shape |= t.block.as_ref().is_some_and(|b| !b.shape.is_box());
            polygon |= t.block.as_ref().is_some_and(|b| matches!(b.shape, rustgis_map::view::BlockShape::Polygon { .. }));
            water |= t.water.is_some();
            // A shore band other than the default, which an older reader would save away.
            shore |= t.water.as_ref().is_some_and(|w| w.shore != rustgis_map::view::WATER_SHORE);
            // The water in a block's walls (#2317), which an older reader would save away.
            block_water |= t.block.as_ref().is_some_and(|b| b.water.is_some());
        }
        relief |= map.layers.iter().any(relief_effects);
        light |= map.layers.iter().any(relief_light);
        sized |= map.layers.iter().any(|layer| layer.size_by.is_some());
        pictures |= map.layers.iter().any(Layer::has_picture_markers);
        zarr |= map.layers.iter().any(zarr_window);
        native |= map.layers.iter().any(|layer| held_natively(&layer.dataset));
        legends |= map.tiles.iter().any(|tile| {
            tile.legends.iter().any(|legend| rustgis_data::login_address::inner(&legend.url).len() > rustgis_map::tiles::MAX_LEGEND_URL)
        });
    }
    [
        (block, "terrain-block"),
        (shape, "terrain-block-shape"),
        (polygon, "terrain-block-polygon"),
        (water, "terrain-water"),
        (shore, "terrain-water-shore"),
        (block_water, "terrain-block-water"),
        (relief, "shaded-relief-effects"),
        (light, "shaded-relief-light"),
        (legends, "normalized-legend-urls"),
        (sized, super::SIZE_BY_CAPABILITY),
        (pictures, "picture-markers"),
        (zarr, "zarr-window"),
        (offline_area_id, "offline-map-area-id"),
        (paper, "blank-paper"),
        (native, super::NATIVE_COORDINATES),
        (mgrs, "graticule-mgrs"),
        (on_top, "graticule-on-top"),
        (grid_color, "graticule-color"),
    ]
    .into_iter()
    .filter_map(|(used, name)| used.then_some(name))
    .collect()
}

#[derive(Serialize)]
struct Item<'a, T> {
    format: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    requires: Vec<&'static str>,
    #[serde(flatten)]
    value: &'a T,
}

/// A map file has its own header, independent of the project it came from.
pub(crate) fn map_file_bytes(map: &Map) -> Result<Vec<u8>, String> {
    let mut requires = map_capabilities([map]);
    if rustgis_data::raster::crs::saves_wkt(map) {
        requires.push("crs-wkt");
    }
    serde_json::to_vec(&Item { format: FORMAT, requires, value: map }).map_err(|e| e.to_string())
}

/// A layer file keeps the same renderer capabilities as the layer saved inside a project.
pub(crate) fn layer_file_bytes(layer: &Layer) -> Result<Vec<u8>, String> {
    let mut requires = layer_capabilities(layer);
    if rustgis_data::raster::crs::saves_wkt(layer) {
        requires.push("crs-wkt");
    }
    serde_json::to_vec(&Item { format: FORMAT, requires, value: layer }).map_err(|e| e.to_string())
}

/// Only executable export options require the capability, not fields in unrelated data.
/// Models and web tools run analysis tools, so cannot execute share.exportTable.
fn command_uses_table_export_order(id: &str, params: &serde_json::Value) -> bool {
    match id {
        "share.exportTable" => ["sort", "limit"].iter().any(|key| params.get(key).is_some_and(|value| !value.is_null())),
        "script.run" => params.get("script").and_then(serde_json::Value::as_str).is_some_and(script_uses_table_export_order),
        _ => false,
    }
}

/// Use the executor's bounded line grammar; nested script.run is refused by script_line,
/// so inspecting script commands cannot recurse through scripts.
fn script_uses_table_export_order(script: &str) -> bool {
    script.lines().take(crate::cmd::workflows::MAX_SCRIPT_LINES).any(|line| {
        crate::script_line(line).is_some_and(|parsed| parsed.is_ok_and(|(id, params)| command_uses_table_export_order(&id, &params)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    /// A coordinate system saved with its WKT requires `crs-wkt` only when its PROJ definition,
    /// which an older release reads, doesn't place points as the WKT does: one in grads (NTF
    /// (Paris), EPSG:4807 as GDAL writes it) does; the fixture's, whose definition does, doesn't,
    /// so an older release still opens it.
    #[test]
    fn a_system_only_its_wkt_places_requires_crs_wkt() {
        let mut p = Project::from_json(include_str!("../../../schema/fixtures/crs_with_wkt.rgis")).unwrap();
        assert!(!p.required_capabilities().contains(&"crs-wkt"));
        let grads = r#"GEOGCS["NTF (Paris)",DATUM["Nouvelle_Triangulation_Francaise_Paris",SPHEROID["Clarke 1880 (IGN)",6378249.2,293.466021293627,AUTHORITY["EPSG","7011"]],AUTHORITY["EPSG","6807"]],PRIMEM["Paris",2.33722917,AUTHORITY["EPSG","8903"]],UNIT["grad",0.0157079632679489,AUTHORITY["EPSG","9105"]]]"#;
        p.tables[0].crs = Some(rustgis_data::raster::wkt::crs_from_wkt(grads).unwrap());
        let saved = serde_json::to_value(&p).unwrap();
        assert_eq!(saved["requires"], json!(["crs-wkt"]));
        // Saved again, the same: finding out serializes the payload, not the project with its
        // header, so it doesn't ask again.
        assert_eq!(serde_json::to_value(&p).unwrap(), saved);
        // Read back, the same.
        let again = Project::from_json(&saved.to_string()).unwrap();
        assert_eq!(serde_json::to_value(&again).unwrap()["requires"], json!(["crs-wkt"]));
        let fixture = Project::from_json(include_str!("../../../schema/fixtures/crs_wkt_required.rgis")).unwrap();
        assert_eq!(serde_json::to_value(&fixture).unwrap()["requires"], json!(["crs-wkt"]));
    }

    fn json_bytes(bytes: &[u8]) -> Value {
        serde_json::from_slice(bytes).unwrap()
    }

    /// A saved MGRS graticule drawn on top in a color of its own (#1042 piece 8) reads back as it
    /// was, and saving names all three again; a map without them names none.
    #[test]
    fn graticule_mgrs_fixture_reads_its_grid() {
        use rustgis_map::graticule::GraticuleType;
        let fixture = Project::from_json(include_str!("../../../schema/fixtures/graticule_mgrs.rgis")).unwrap();
        let saved = serde_json::to_value(&fixture).unwrap();
        assert_eq!(saved["requires"], json!(["graticule-mgrs", "graticule-on-top", "graticule-color"]));
        let map = &fixture.maps[0];
        assert_eq!(map.graticule_type, GraticuleType::Mgrs);
        assert!(map.graticule && map.graticule_on_top);
        assert_eq!(map.graticule_color.map(|c| (c.r(), c.g(), c.b(), c.a())), Some((48, 48, 48, 153)));
        let reopened = Project::from_json(&saved.to_string()).unwrap();
        assert_eq!(serde_json::to_value(&reopened).unwrap(), saved);
        let mut plain = fixture;
        let map = &mut plain.maps[0];
        map.graticule_type = GraticuleType::Utm;
        map.graticule_on_top = false;
        map.graticule_color = None;
        assert!(serde_json::to_value(&plain).unwrap().get("requires").is_none());
    }

    /// A saved image marker reads back as the image it was, and saving names it again.
    #[test]
    fn picture_markers_fixture_reads_its_image() {
        let fixture = Project::from_json(include_str!("../../../schema/fixtures/picture_markers.rgis")).unwrap();
        let rustgis_map::Renderer::Single(symbol) = &fixture.styles[0].renderer else { panic!("a single symbol") };
        let rustgis_map::Marker::Picture(picture) = symbol.marker else { panic!("an image marker") };
        assert!(picture.base64().starts_with("iVBORw0KGgo") && !picture.is_dropped());
        assert_eq!(serde_json::to_value(&fixture).unwrap()["requires"], json!(["picture-markers"]));
    }

    /// All replayable project containers require non-default options only when requested.
    /// Selection of executable notebook cells follows the notebook runner's language boundary.
    #[test]
    fn tabulate_capabilities_follow_saved_executables() {
        let empty_zones = Project::from_json(include_str!("../../../schema/fixtures/tabulate_intersection_empty_zones.rgis")).unwrap();
        assert_eq!(empty_zones.models[0].steps[0].params["includeEmptyZones"], true);
        assert_eq!(serde_json::to_value(&empty_zones).unwrap()["requires"], json!(["tabulate-intersection-empty-zones"]));

        let mut fixture = Project::from_json(include_str!("../../../schema/fixtures/tabulate_intersection_unit.rgis")).unwrap();
        assert_eq!(fixture.models[0].steps[0].params["areaUnit"], "ac");
        assert_eq!(serde_json::to_value(&fixture).unwrap()["requires"], json!(["tabulate-intersection-unit"]));
        fixture.models.clear();
        let base = serde_json::to_value(&fixture).unwrap();
        for (option, expected) in [
            (json!({}), vec![]),
            (json!({"areaUnit":""}), vec![]),
            (json!({"areaUnit":" \t\n "}), vec![]),
            (json!({"areaUnit":"km2"}), vec![]),
            (json!({"areaUnit":" KM2 "}), vec![]),
            (json!({"areaUnit":"ac"}), vec!["tabulate-intersection-unit"]),
            (json!({"includeEmptyZones":false}), vec![]),
            (json!({"includeEmptyZones":" TRUE "}), vec!["tabulate-intersection-empty-zones"]),
            (json!({"areaUnit":"ac", "includeEmptyZones":true}), vec!["tabulate-intersection-unit", "tabulate-intersection-empty-zones"]),
        ] {
            let mut params = option;
            params["zones"] = json!("Zones");
            params["classes"] = json!("Classes");
            let run = json!({"tool":"tabulateIntersection", "params": params});
            let script = format!("# Tabulate\ngp.run= {run}");
            for (field, value) in [
                ("models", json!([{"name":"Steps", "steps":[{"tool":"tabulateIntersection", "params":params}]}])),
                (
                    "models",
                    json!([{"name":"Graph", "graph":{"nodes":[{"id":"t", "kind":"tool", "tool":"tabulateIntersection", "params":params}]}}]),
                ),
                ("tasks", json!([{"name":"Task", "steps":[{"title":"Tabulate", "command":"gp.run", "params":run}]}])),
                (
                    "tasks",
                    json!([{"name":"Alias", "steps":[{"title":"Tabulate", "command":"gp.run", "params":{"tool":"Tabulate Intersection", "params":params}}]}]),
                ),
                (
                    "tasks",
                    json!([{"name":"Batch", "steps":[{"title":"Tabulate", "command":"gp.batch", "params":{"tool":"tabulateIntersection", "inputs":["Zones"], "params":params}}]}]),
                ),
                ("tasks", json!([{"name":"Script", "steps":[{"title":"Tabulate", "command":"script.run", "params":{"script":script}}]}])),
                ("notebooks", json!([{"name":"Notebook", "cells":[{"kind":"code", "source":script}]}])),
                (
                    "gp_history",
                    json!([{"tool":"Tabulate Intersection", "id":"tabulateIntersection", "params":params, "ok":true, "messages":[], "millis":0}]),
                ),
                ("gp_history", json!([{"tool":"Tabulate Intersection", "params":params, "ok":true, "messages":[], "millis":0}])),
                (
                    "gp_history",
                    json!([{"tool":"Tabulate Intersection", "params":{}, "command":["gp.run",run], "ok":true, "messages":[], "millis":0}]),
                ),
                (
                    "tasks",
                    json!([{"name":"Envelope", "steps":[{"title":"Tabulate", "command":"gp.run", "params":{"tool":"tabulateIntersection", "params":params.to_string()}}]}]),
                ),
            ] {
                let mut document = base.clone();
                document[field] = value;
                let project = Project::from_json(&document.to_string()).unwrap();
                let saved = serde_json::to_value(&project).unwrap();
                if expected.is_empty() {
                    assert!(saved.get("requires").is_none(), "{field} {params}");
                    assert_eq!(saved["format"], FORMAT);
                } else {
                    assert_eq!(saved["requires"], json!(expected), "{field} {params}");
                    assert_eq!(saved["format"], 2);
                }
                assert_eq!(Project::from_json(&saved.to_string()).unwrap().required_capabilities(), project.required_capabilities());
            }
        }
        let mut document = base;
        let script = "gp.run {\"tool\":\"tabulateIntersection\",\"params\":{\"areaUnit\":\"ac\"}}";
        document["notebooks"] = json!([
            {"name":"Notes", "cells":[{"kind":"markdown", "source":script}]},
            {"name":"Foreign", "cells":[{"kind":"code", "source":script}], "jupyter":{"metadata":{"kernelspec":{"language":"python"}}}},
            {"name":"Comments", "cells":[{"kind":"code", "source":format!("# {script}\ngp.run {{invalid")}]}
        ]);
        let project = Project::from_json(&document.to_string()).unwrap();
        assert!(serde_json::to_value(&project).unwrap().get("requires").is_none());
        assert!(!command_uses_tabulate_unit("other", &json!({"areaUnit":"ac"})));
        assert!(!command_uses_tabulate_unit("tabulateIntersection", &json!({"areaUnit":null})));
    }

    /// Every saved executable container protects percent slope, while default/explicit degrees
    /// and non-executable notebook text stay readable without the capability.
    #[test]
    fn slope_percent_capability_follows_saved_executables() {
        let mut fixture = Project::from_json(include_str!("../../../schema/fixtures/slope_percent.rgis")).unwrap();
        assert_eq!(fixture.models[0].steps[0].params["steps"][0]["units"], "percent");
        assert_eq!(serde_json::to_value(&fixture).unwrap()["requires"], json!(["slope-percent"]));
        fixture.models.clear();
        let base = serde_json::to_value(&fixture).unwrap();
        for units in [None, Some("degrees"), Some("percent")] {
            let mut slope = json!({"function": "slope", "layer": "DEM"});
            if let Some(units) = units {
                slope["units"] = json!(units);
            }
            for (id, params) in [("raster.function", slope.clone()), ("raster.functionChain", json!({"layer": "DEM", "steps": [slope]}))] {
                let script = format!("# Slope\n{id}= {params}");
                for (field, value) in [
                    ("models", json!([{"name": "Steps", "steps": [{"tool": id, "params": params}]}])),
                    ("models", json!([{"name": "Graph", "graph": {"nodes": [{"id": "s", "kind": "tool", "tool": id, "params": params}]}}])),
                    ("tasks", json!([{"name": "Task", "steps": [{"title": "Slope", "command": id, "params": params}]}])),
                    (
                        "tasks",
                        json!([{"name": "Script", "steps": [{"title": "Slope", "command": "script.run", "params": {"script": script}}]}]),
                    ),
                    ("notebooks", json!([{"name": "Notebook", "cells": [{"kind": "code", "source": script}]}])),
                    (
                        "gp_history",
                        json!([{"tool": "Slope", "params": {}, "command": [id, params], "ok": true, "messages": [], "millis": 0}]),
                    ),
                ] {
                    let mut document = base.clone();
                    document[field] = value;
                    let project = Project::from_json(&document.to_string()).unwrap();
                    let saved = serde_json::to_value(&project).unwrap();
                    if units == Some("percent") {
                        assert_eq!(saved["requires"], json!(["slope-percent"]), "{id} {field}");
                    } else {
                        assert!(saved.get("requires").is_none(), "{id} {field} {units:?}");
                    }
                }
            }
        }
        let mut document = base;
        let script = "raster.function {\"function\":\"slope\",\"units\":\"percent\"}";
        document["notebooks"] = json!([
            {"name": "Notes", "cells": [{"kind": "markdown", "source": script}]},
            {"name": "Foreign", "cells": [{"kind": "code", "source": script}],
             "jupyter": {"metadata": {"kernelspec": {"language": "python"}}}}
        ]);
        let project = Project::from_json(&document.to_string()).unwrap();
        assert!(serde_json::to_value(&project).unwrap().get("requires").is_none());
    }

    /// A layout element's background is required where it is saved, so an older reader refuses
    /// the project instead of opening it and saving it again without the background.
    #[test]
    fn element_background_capability_follows_saved_layouts() {
        let fixture = Project::from_json(include_str!("../../../schema/fixtures/layout_element_background.rgis")).unwrap();
        let saved = serde_json::to_value(&fixture).unwrap();
        assert_eq!(saved["requires"], json!(["layout-element-background"]));
        let reopened = Project::from_json(&saved.to_string()).unwrap();
        let backgrounds = |p: &Project| p.layouts[0].elements.iter().map(|e| e.background).collect::<Vec<_>>();
        assert_eq!(backgrounds(&reopened), backgrounds(&fixture));
        assert!(backgrounds(&fixture)[1].is_some_and(|b| b.padding == 1.5));
        let mut clean = fixture;
        for e in &mut clean.layouts[0].elements {
            e.background = None;
        }
        assert!(serde_json::to_value(&clean).unwrap().get("requires").is_none());
        // A saved command that gives an element a background needs it too; one that takes it
        // away or sets none doesn't.
        let base = serde_json::to_value(&clean).unwrap();
        for (background, required) in
            [(Some(json!({"fill": "#FFFFFF"})), true), (Some(json!({})), true), (Some(Value::Null), false), (None, false)]
        {
            let mut params = json!({"element": 2, "x": 5});
            if let Some(b) = background {
                params["background"] = b;
            }
            let script = format!("# Panel\nlayout.update= {params}");
            for (field, value) in [
                ("tasks", json!([{"name": "Task", "steps": [{"title": "Panel", "command": "layout.update", "params": params}]}])),
                (
                    "tasks",
                    json!([{"name": "Script", "steps": [{"title": "Panel", "command": "script.run", "params": {"script": script}}]}]),
                ),
                ("notebooks", json!([{"name": "Notebook", "cells": [{"kind": "code", "source": script}]}])),
                (
                    "gp_history",
                    json!([{"tool": "Panel", "params": {}, "command": ["layout.addElement", params], "ok": true, "messages": [], "millis": 0}]),
                ),
            ] {
                let mut document = base.clone();
                document[field] = value;
                let saved = serde_json::to_value(Project::from_json(&document.to_string()).unwrap()).unwrap();
                if required {
                    assert_eq!(saved["requires"], json!(["layout-element-background"]), "{field} {params}");
                } else {
                    assert!(saved.get("requires").is_none(), "{field} {params}");
                }
            }
        }
        // Both a saved background and a saved command: listed once.
        let mut both = saved.clone();
        both["tasks"] = json!([{"name": "Task", "steps": [{"title": "Panel", "command": "layout.update", "params": {"element": 2, "background": {}}}]}]);
        let saved = serde_json::to_value(Project::from_json(&both.to_string()).unwrap()).unwrap();
        assert_eq!(saved["requires"], json!(["layout-element-background"]));
    }

    /// A legend's, colorbar's, table's or chart's ink is required where it is saved, so an older
    /// reader refuses the project instead of drawing them dark grey and saving them so (#2257).
    /// A saved command that sets one needs nothing: an older reader refuses its "color".
    #[test]
    fn element_ink_capability_follows_saved_layouts() {
        let fixture = Project::from_json(include_str!("../../../schema/fixtures/layout_element_ink.rgis")).unwrap();
        let saved = serde_json::to_value(&fixture).unwrap();
        assert_eq!(saved["requires"], json!(["layout-element-ink"]));
        let inks = |p: &Project| p.layouts[0].elements.iter().map(|e| e.kind.ink()).collect::<Vec<_>>();
        let reopened = Project::from_json(&saved.to_string()).unwrap();
        assert_eq!(inks(&reopened), inks(&fixture));
        assert_eq!(inks(&fixture).iter().filter(|i| i.is_some()).count(), 4, "{:?}", inks(&fixture));
        let mut clean = fixture;
        for e in &mut clean.layouts[0].elements {
            if let Some(ink) = e.kind.ink_mut() {
                *ink = None;
            }
        }
        let base = serde_json::to_value(&clean).unwrap();
        assert!(base.get("requires").is_none(), "default inks need nothing");
        let mut document = base.clone();
        document["tasks"] = json!([{"name": "Task", "steps": [{"title": "Ink", "command": "layout.update", "params": {"element": 2, "color": "#FFFFFF"}}]}]);
        assert!(serde_json::to_value(Project::from_json(&document.to_string()).unwrap()).unwrap().get("requires").is_none());
        // A layout imported from its text keeps unknown keys out in an older reader, so a saved
        // import of one with an ink or a background needs them; one without, or from a path, doesn't.
        let layout = &saved["layouts"][0];
        let mut plain = layout.clone();
        for e in plain["elements"].as_array_mut().unwrap() {
            if let Some(kind) = e["kind"].as_object_mut().and_then(|k| k.values_mut().next()).and_then(Value::as_object_mut) {
                kind.remove("color");
            }
        }
        let mut boxed = plain.clone();
        boxed["elements"][1]["background"] = json!({"fill": [255, 255, 255, 255]});
        for (params, required) in [
            (json!({"text": layout.to_string()}), json!(["layout-element-ink"])),
            // The same key spelled with an escape is the same key.
            (json!({"text": layout.to_string().replace("\"color\"", "\"\\u0063olor\"")}), json!(["layout-element-ink"])),
            (json!({"text": boxed.to_string()}), json!(["layout-element-background"])),
            (json!({"text": plain.to_string()}), Value::Null),
            (json!({"path": "inks.rgislayout"}), Value::Null),
            (json!({"text": "not a layout"}), Value::Null),
        ] {
            let script = format!("layout.import= {params}");
            for value in [
                json!([{"name": "Task", "steps": [{"title": "Import", "command": "layout.import", "params": params}]}]),
                json!([{"name": "Script", "steps": [{"title": "Import", "command": "script.run", "params": {"script": script}}]}]),
            ] {
                let mut document = base.clone();
                document["tasks"] = value;
                let saved = serde_json::to_value(Project::from_json(&document.to_string()).unwrap()).unwrap();
                assert_eq!(saved.get("requires").cloned().unwrap_or(Value::Null), required, "{params}");
            }
        }
    }

    /// A text that wraps to its box is required where it is saved (#2346), so an older reader
    /// refuses the project instead of drawing it on its own line breaks; so is a saved command
    /// that makes one wrap, which an older reader would run without it. Unwrapping needs nothing.
    #[test]
    fn element_wrap_capability_follows_saved_layouts_and_commands() {
        use rustgis_map::layout::ElementKind;
        let fixture = Project::from_json(include_str!("../../../schema/fixtures/layout_element_wrap.rgis")).unwrap();
        let saved = serde_json::to_value(&fixture).unwrap();
        assert_eq!(saved["requires"], json!(["layout-element-wrap"]));
        let wrapped = |p: &Project| p.layouts[0].elements.iter().filter(|e| matches!(e.kind, ElementKind::Text { wrap: true, .. })).count();
        assert_eq!(wrapped(&Project::from_json(&saved.to_string()).unwrap()), 1);
        let mut clean = fixture;
        for e in &mut clean.layouts[0].elements {
            if let ElementKind::Text { wrap, .. } = &mut e.kind {
                *wrap = false;
            }
        }
        let base = serde_json::to_value(&clean).unwrap();
        assert!(base.get("requires").is_none(), "unwrapped texts need nothing");
        assert!(!base["layouts"].to_string().contains("wrap"), "nor write it");
        let layout = &saved["layouts"][0];
        for (id, params, required) in [
            ("layout.addElement", json!({"kind": "text", "wrap": true}), true),
            ("layout.update", json!({"element": 2, "wrap": "on"}), true),
            ("layout.update", json!({"element": 2, "wrap": false}), false),
            ("layout.update", json!({"element": 2, "wrap": null}), false),
            ("layout.update", json!({"element": 2, "text": "wrap"}), false),
            ("layout.import", json!({"text": layout.to_string()}), true),
            ("layout.import", json!({"text": base["layouts"][0].to_string()}), false),
            ("layout.import", json!({"path": "notes.rgislayout"}), false),
        ] {
            let script = format!("{id}= {params}");
            for value in [
                json!([{"name": "Task", "steps": [{"title": "Note", "command": id, "params": params}]}]),
                json!([{"name": "Script", "steps": [{"title": "Note", "command": "script.run", "params": {"script": script}}]}]),
            ] {
                let mut document = base.clone();
                document["tasks"] = value;
                let saved = serde_json::to_value(Project::from_json(&document.to_string()).unwrap()).unwrap();
                let expected = if required { json!(["layout-element-wrap"]) } else { Value::Null };
                assert_eq!(saved.get("requires").cloned().unwrap_or(Value::Null), expected, "{id} {params}");
            }
        }
    }

    /// A chart grouped, ordered, cut or with an Other bar is required where it is saved, so an
    /// older reader refuses the project instead of drawing a bar per feature, the 20 largest; so
    /// is a saved command that makes one. Options at their defaults, or a group taken away, need
    /// nothing, nor are they written.
    #[test]
    fn chart_data_capability_follows_saved_layouts_and_commands() {
        use rustgis_map::layout::{CHART_BARS, ChartOrder, ElementKind};
        use rustgis_map::report::StatKind;
        let fixture = Project::from_json(include_str!("../../../schema/fixtures/layout_chart_data.rgis")).unwrap();
        let saved = serde_json::to_value(&fixture).unwrap();
        assert_eq!(saved["requires"], json!(["layout-chart-data"]));
        let reread = Project::from_json(&saved.to_string()).unwrap();
        assert!(matches!(
            &reread.layouts[0].elements[1].kind,
            ElementKind::Chart { group: Some(g), statistic: StatKind::Mean, order: ChartOrder::Category, bars: 5, other: true, .. } if g == "region"
        ));
        let mut clean = fixture;
        for e in &mut clean.layouts[0].elements {
            if let ElementKind::Chart { group, statistic, order, bars, other, .. } = &mut e.kind {
                (*group, *statistic, *order, *bars, *other) = (None, StatKind::default(), ChartOrder::default(), CHART_BARS, false);
            }
        }
        let base = serde_json::to_value(&clean).unwrap();
        assert!(base.get("requires").is_none(), "a plain chart needs nothing");
        for key in ["group", "statistic", "order", "bars", "other"] {
            assert!(!base["layouts"].to_string().contains(&format!("\"{key}\"")), "nor writes {key}");
        }
        let layout = &saved["layouts"][0];
        for (id, params, required) in [
            ("layout.addElement", json!({"kind": "chart", "group": "region"}), true),
            ("layout.update", json!({"element": 2, "statistic": "count"}), true),
            ("layout.update", json!({"element": 2, "order": "layer"}), true),
            ("layout.update", json!({"element": 2, "bars": 8}), true),
            ("layout.update", json!({"element": 2, "other": "on"}), true),
            ("layout.update", json!({"element": 2, "group": ""}), false),
            ("layout.update", json!({"element": 2, "statistic": "sum", "order": "descending", "bars": 20, "other": false}), false),
            ("layout.update", json!({"element": 2, "label": "name"}), true),
            ("layout.update", json!({"element": 2, "label": ""}), true),
            ("layout.addElement", json!({"kind": "chart", "label": "name"}), false),
            ("layout.import", json!({"text": layout.to_string()}), true),
            ("layout.import", json!({"text": base["layouts"][0].to_string()}), false),
            ("layout.import", json!({"path": "regions.rgislayout"}), false),
        ] {
            let script = format!("{id}= {params}");
            for value in [
                json!([{"name": "Task", "steps": [{"title": "Chart", "command": id, "params": params}]}]),
                json!([{"name": "Script", "steps": [{"title": "Chart", "command": "script.run", "params": {"script": script}}]}]),
            ] {
                let mut document = base.clone();
                document["tasks"] = value;
                let saved = serde_json::to_value(Project::from_json(&document.to_string()).unwrap()).unwrap();
                let expected = if required { json!(["layout-chart-data"]) } else { Value::Null };
                assert_eq!(saved.get("requires").cloned().unwrap_or(Value::Null), expected, "{id} {params}");
            }
        }
    }

    /// A chart drawn as columns, with its values, their decimals, its own title or none, a bar
    /// colour or labels against its bars is required where it is saved, so an older reader
    /// refuses the project instead of drawing it as bars running across in the default blue; so
    /// is a saved command that draws one so. Options at their defaults need nothing, nor are
    /// they written, and a legend's or colorbar's title or orientation needs nothing either.
    #[test]
    fn chart_look_capability_follows_saved_layouts_and_commands() {
        use rustgis_map::layout::{CHART_BARS, ChartOrder, ElementKind, Orientation, TextAlign};
        use rustgis_map::report::StatKind;
        let fixture = Project::from_json(include_str!("../../../schema/fixtures/layout_chart_look.rgis")).unwrap();
        let saved = serde_json::to_value(&fixture).unwrap();
        assert_eq!(saved["requires"], json!(["layout-chart-data", "layout-chart-look"]));
        let reread = Project::from_json(&saved.to_string()).unwrap();
        assert!(matches!(
            &reread.layouts[0].elements[1].kind,
            ElementKind::Chart {
                orientation: Orientation::Vertical,
                values: true,
                decimals: Some(1),
                title: Some(t),
                bar_color: Some(c),
                label_align: TextAlign::Right,
                ..
            } if t == "Mean people by region" && c.hex() == "#C2185B"
        ));
        let mut clean = fixture;
        for e in &mut clean.layouts[0].elements {
            if let ElementKind::Chart {
                group,
                statistic,
                order,
                bars,
                other,
                orientation,
                values,
                decimals,
                title,
                bar_color,
                label_align,
                ..
            } = &mut e.kind
            {
                (*group, *statistic, *order, *bars, *other) = (None, StatKind::default(), ChartOrder::default(), CHART_BARS, false);
                (*orientation, *values, *decimals, *title, *bar_color, *label_align) =
                    (Orientation::Horizontal, false, None, None, None, TextAlign::Left);
            }
        }
        let base = serde_json::to_value(&clean).unwrap();
        assert!(base.get("requires").is_none(), "a chart drawn as before needs nothing");
        for key in ["orientation", "values", "decimals", "title", "bar_color", "label_align"] {
            assert!(!base["layouts"].to_string().contains(&format!("\"{key}\"")), "nor writes {key}");
        }
        let mut document = base.clone();
        document["tasks"] = json!([{"name": "Task", "steps": [{"title": "Chart", "command": "layout.import", "params": {"text": saved["layouts"][0].to_string()}}]}]);
        let imported = serde_json::to_value(Project::from_json(&document.to_string()).unwrap()).unwrap();
        assert_eq!(imported["requires"], json!(["layout-chart-data", "layout-chart-look"]));
        for (id, params, required) in [
            ("layout.addElement", json!({"kind": "chart", "orientation": "vertical"}), true),
            ("layout.addElement", json!({"kind": "chart", "orientation": "horizontal"}), false),
            ("layout.addElement", json!({"kind": "chart", "title": ""}), true),
            ("layout.addElement", json!({"kind": "chart", "values": true}), true),
            ("layout.addElement", json!({"kind": "chart", "decimals": "auto"}), false),
            ("layout.addElement", json!({"kind": "colorbar", "orientation": "vertical", "title": "Height"}), false),
            ("layout.addElement", json!({"kind": "legend", "title": "Key"}), false),
            ("layout.update", json!({"element": 2, "decimals": 0}), true),
            ("layout.update", json!({"element": 2, "barColor": "#C2185B"}), true),
            ("layout.update", json!({"element": 2, "labelAlign": "right"}), true),
            ("layout.update", json!({"element": 2, "labelAlign": "left", "values": false, "barColor": null}), false),
            ("layout.update", json!({"element": 2, "title": "Key"}), false),
            ("layout.import", json!({"text": base["layouts"][0].to_string()}), false),
        ] {
            let script = format!("{id}= {params}");
            for value in [
                json!([{"name": "Task", "steps": [{"title": "Chart", "command": id, "params": params}]}]),
                json!([{"name": "Script", "steps": [{"title": "Chart", "command": "script.run", "params": {"script": script}}]}]),
            ] {
                let mut document = base.clone();
                document["tasks"] = value;
                let saved = serde_json::to_value(Project::from_json(&document.to_string()).unwrap()).unwrap();
                let expected = if required { json!(["layout-chart-look"]) } else { Value::Null };
                assert_eq!(saved.get("requires").cloned().unwrap_or(Value::Null), expected, "{id} {params}");
            }
        }
    }

    /// A chart coloured by its layer's classes (#2487 part 3) is required where it is saved, so an
    /// older reader refuses the project instead of drawing every bar in the chart's colour; so is
    /// a saved command that colours one. `false` needs nothing, nor is it written.
    #[test]
    fn chart_classes_capability_follows_saved_layouts_and_commands() {
        use rustgis_map::layout::ElementKind;
        let fixture = Project::from_json(include_str!("../../../schema/fixtures/layout_chart_classes.rgis")).unwrap();
        let saved = serde_json::to_value(&fixture).unwrap();
        assert_eq!(saved["requires"], json!(["layout-chart-classes"]));
        let reread = Project::from_json(&saved.to_string()).unwrap();
        assert!(matches!(&reread.layouts[0].elements[1].kind, ElementKind::Chart { classes: true, .. }));
        let mut clean = fixture;
        for e in &mut clean.layouts[0].elements {
            if let ElementKind::Chart { classes, .. } = &mut e.kind {
                *classes = false;
            }
        }
        let base = serde_json::to_value(&clean).unwrap();
        assert!(base.get("requires").is_none(), "a chart in one colour needs nothing");
        assert!(!base["layouts"].to_string().contains("\"classes\""), "nor writes classes");
        let mut document = base.clone();
        document["tasks"] = json!([{"name": "Task", "steps": [{"title": "Chart", "command": "layout.import", "params": {"text": saved["layouts"][0].to_string()}}]}]);
        let imported = serde_json::to_value(Project::from_json(&document.to_string()).unwrap()).unwrap();
        assert_eq!(imported["requires"], json!(["layout-chart-classes"]));
        for (id, params, required) in [
            ("layout.addElement", json!({"kind": "chart", "classes": true}), true),
            ("layout.addElement", json!({"kind": "chart", "classes": false}), false),
            ("layout.addElement", json!({"kind": "legend", "classes": true}), false),
            ("layout.update", json!({"element": 2, "classes": true}), true),
            ("layout.update", json!({"element": 2, "classes": false}), false),
            ("layout.import", json!({"text": base["layouts"][0].to_string()}), false),
        ] {
            let script = format!("{id}= {params}");
            for value in [
                json!([{"name": "Task", "steps": [{"title": "Chart", "command": id, "params": params}]}]),
                json!([{"name": "Script", "steps": [{"title": "Chart", "command": "script.run", "params": {"script": script}}]}]),
            ] {
                let mut document = base.clone();
                document["tasks"] = value;
                let saved = serde_json::to_value(Project::from_json(&document.to_string()).unwrap()).unwrap();
                let expected = if required { json!(["layout-chart-classes"]) } else { Value::Null };
                assert_eq!(saved.get("requires").cloned().unwrap_or(Value::Null), expected, "{id} {params}");
            }
        }
    }

    /// A chart stacked by a second field (#2487 part 3b) is required where it is saved, so an older
    /// reader refuses the project instead of drawing each bar whole; so is a saved command that
    /// stacks one. `""` needs nothing, nor is a chart without a stack field written with one.
    #[test]
    fn chart_stack_capability_follows_saved_layouts_and_commands() {
        use rustgis_map::layout::ElementKind;
        let fixture = Project::from_json(include_str!("../../../schema/fixtures/layout_chart_stack.rgis")).unwrap();
        let saved = serde_json::to_value(&fixture).unwrap();
        assert_eq!(saved["requires"], json!(["layout-chart-data", "layout-chart-stack"]));
        let reread = Project::from_json(&saved.to_string()).unwrap();
        assert_eq!(reread.layouts[0].elements[1].kind.chart_stack(), Some("kind"));
        let mut clean = fixture;
        for e in &mut clean.layouts[0].elements {
            if let ElementKind::Chart { group, stack, .. } = &mut e.kind {
                (*group, *stack) = (None, None);
            }
        }
        let base = serde_json::to_value(&clean).unwrap();
        assert!(base.get("requires").is_none(), "a chart with whole bars needs nothing");
        assert!(!base["layouts"].to_string().contains("\"stack\""), "nor writes stack");
        let mut document = base.clone();
        document["tasks"] = json!([{"name": "Task", "steps": [{"title": "Chart", "command": "layout.import", "params": {"text": saved["layouts"][0].to_string()}}]}]);
        let imported = serde_json::to_value(Project::from_json(&document.to_string()).unwrap()).unwrap();
        assert_eq!(imported["requires"], json!(["layout-chart-data", "layout-chart-stack"]));
        for (id, params, required) in [
            ("layout.addElement", json!({"kind": "chart", "stack": "kind"}), true),
            ("layout.addElement", json!({"kind": "chart", "stack": ""}), false),
            ("layout.addElement", json!({"kind": "legend", "stack": "kind"}), false),
            ("layout.update", json!({"element": 2, "stack": "kind"}), true),
            ("layout.update", json!({"element": 2, "stack": ""}), false),
            ("layout.update", json!({"element": 2, "stack": null}), false),
            ("layout.import", json!({"text": base["layouts"][0].to_string()}), false),
        ] {
            let script = format!("{id}= {params}");
            for value in [
                json!([{"name": "Task", "steps": [{"title": "Chart", "command": id, "params": params}]}]),
                json!([{"name": "Script", "steps": [{"title": "Chart", "command": "script.run", "params": {"script": script}}]}]),
            ] {
                let mut document = base.clone();
                document["tasks"] = value;
                let saved = serde_json::to_value(Project::from_json(&document.to_string()).unwrap()).unwrap();
                let expected = if required { json!(["layout-chart-stack"]) } else { Value::Null };
                assert_eq!(saved.get("requires").cloned().unwrap_or(Value::Null), expected, "{id} {params}");
            }
        }
    }

    #[test]
    fn field_identity_is_preserved_and_required_in_a_plain_saved_project() {
        let project = Project::from_json(include_str!("../../../schema/fixtures/field_origin.rgis")).unwrap();
        let saved = serde_json::to_value(&project).unwrap();
        assert_eq!(saved["requires"], json!(["field-origin"]));
        let reopened = Project::from_json(&saved.to_string()).unwrap();
        assert_eq!(reopened.origin_id, project.origin_id);
        assert_eq!(reopened.field_origin, project.field_origin);
        let mut invalid = saved.clone();
        invalid["field_origin"]["area"] = json!("not-an-id");
        assert!(Project::from_json(&invalid.to_string()).unwrap_err().contains("identity"));
        let mut clean = Project::new("Legacy");
        assert!(serde_json::to_value(&clean).unwrap().get("requires").is_none());
        clean.origin_id = "0123456789abcdef0123456789abcdef".into();
        assert_eq!(serde_json::to_value(&clean).unwrap()["requires"], json!(["field-origin"]));
    }

    /// A return check (a package's or an area's generation) is kept on saving and requires its own
    /// capability; an invalid one is refused, and an area never packaged has none and requires nothing.
    #[test]
    fn field_generation_is_preserved_and_required_and_must_be_an_identity() {
        let project = Project::from_json(include_str!("../../../schema/fixtures/field_generation.rgis")).unwrap();
        let saved = serde_json::to_value(&project).unwrap();
        assert_eq!(saved["requires"], json!(["field-origin", "field-generation"]));
        let reopened = Project::from_json(&saved.to_string()).unwrap();
        assert_eq!(reopened.field_origin, project.field_origin);
        assert_eq!(reopened.field_origin.as_ref().map(|o| o.generation.as_str()), Some("8899aabbccddeeff0011223344556677"));
        let mut invalid = saved.clone();
        invalid["field_origin"]["generation"] = json!("not-an-id");
        assert!(Project::from_json(&invalid.to_string()).unwrap_err().contains("identity"));
        // The same for an area's own, once it holds a snapshot of a map.
        let mut with_area = Project::from_json(include_str!("../../../schema/fixtures/field_origin.rgis")).unwrap();
        with_area.offline_areas.push(super::super::OfflineArea {
            id: "0123456789abcdef0123456789abcdef".into(),
            name: "Field".into(),
            map: "Map".into(),
            extent: rustgis_geom::Extent::new(0.0, 0.0, 1.0, 1.0),
            created: 0.0,
            synced: 0.0,
            generation: String::new(),
            data: with_area.map().clone(),
        });
        assert_eq!(serde_json::to_value(&with_area).unwrap()["requires"], json!(["field-origin"]));
        with_area.offline_areas[0].generation = "8899aabbccddeeff0011223344556677".into();
        let saved = serde_json::to_value(&with_area).unwrap();
        assert_eq!(saved["requires"], json!(["field-origin", "field-generation"]));
        let mut invalid = saved;
        invalid["offline_areas"][0]["generation"] = json!("nope");
        assert!(Project::from_json(&invalid.to_string()).unwrap_err().contains("identity"));
    }

    #[test]
    fn normalized_legend_capability_follows_maps_and_offline_snapshots() {
        let mut p = Project::from_json(include_str!("../../../schema/fixtures/normalized_legend.rgis")).unwrap();
        assert_eq!(p.map().tiles[0].legends.len(), 1);
        let url = p.map().tiles[0].legends[0].url.clone();
        assert!(url.len() > rustgis_map::tiles::MAX_LEGEND_URL);
        let saved = serde_json::to_value(&p).unwrap();
        assert_eq!(saved["requires"], json!(["normalized-legend-urls"]));
        assert_eq!(json_bytes(&map_file_bytes(p.map()).unwrap())["requires"], saved["requires"]);
        p.offline_areas.push(super::super::OfflineArea {
            id: String::new(),
            name: "Field".into(),
            map: "Map".into(),
            extent: rustgis_geom::Extent::new(0.0, 0.0, 1.0, 1.0),
            created: 0.0,
            synced: 0.0,
            generation: String::new(),
            data: p.map().clone(),
        });
        p.maps[0].tiles.clear();
        assert_eq!(serde_json::to_value(&p).unwrap()["requires"], json!(["normalized-legend-urls"]));
        let reopened = Project::from_json(&saved.to_string()).unwrap();
        assert_eq!(reopened.map().tiles[0].legends[0].url, url);
    }

    /// Legacy files still open; a save names terrain features an older format-2 reader loses.
    #[test]
    fn terrain_headers_follow_the_saved_payload_including_offline_snapshots() {
        let mut p = Project::from_json(include_str!("../../../schema/fixtures/terrain_water.rgis")).unwrap();
        let saved = serde_json::to_value(&p).unwrap();
        assert_eq!(saved["format"], 2);
        assert_eq!(saved["requires"], json!(["terrain-water"]));
        assert_eq!(json_bytes(&map_file_bytes(p.map()).unwrap())["requires"], saved["requires"]);
        let loaded = Project::from_json(&saved.to_string()).unwrap();
        assert_eq!(loaded.map().view.terrain, p.map().view.terrain);
        p.offline_areas.push(super::super::OfflineArea {
            id: String::new(),
            name: "Field".into(),
            map: "Map".into(),
            extent: rustgis_geom::Extent::new(0.0, 0.0, 1.0, 1.0),
            created: 0.0,
            synced: 0.0,
            generation: String::new(),
            data: p.map().clone(),
        });
        p.maps[0].view.terrain = None;
        assert_eq!(serde_json::to_value(&p).unwrap()["requires"], json!(["terrain-water"]));
        p.offline_areas.clear();
        let clean = serde_json::to_value(&p).unwrap();
        assert!(clean.get("requires").is_none());
        assert!(json_bytes(&map_file_bytes(p.map()).unwrap()).get("requires").is_none());
        assert!(Project::from_json(include_str!("../../../schema/fixtures/terrain_water_v2.rgis")).is_ok());
        // A shore band other than the default is named too (#1915); the default needs nothing more.
        let mut shore = Project::from_json(include_str!("../../../schema/fixtures/terrain_water_shore_v2.rgis")).unwrap();
        assert_eq!(serde_json::to_value(&shore).unwrap()["requires"], json!(["terrain-water", "terrain-water-shore"]));
        assert_eq!(shore.map().view.terrain.as_ref().and_then(|t| t.water.as_ref()).map(|w| w.shore), Some(0.0));
        shore.maps[0].view.terrain.as_mut().and_then(|t| t.water.as_mut()).unwrap().shore = rustgis_map::view::WATER_SHORE;
        assert_eq!(serde_json::to_value(&shore).unwrap()["requires"], json!(["terrain-water"]));
        let mut unknown = saved;
        unknown["requires"] = json!(["future-terrain"]);
        assert!(Project::from_json(&unknown.to_string()).unwrap_err().contains("future-terrain"));
    }

    /// A Zarr window following the time slider (#1720) is required by name in a project, a map
    /// file and a layer file: an older reader would drop it and read the whole map at each step.
    #[test]
    fn a_zarr_window_is_required_where_it_is_saved() {
        let mut p = Project::from_json(include_str!("../../../schema/fixtures/zarr_window.rgis")).unwrap();
        let layer = p.map().layers[0].clone();
        assert_eq!(
            layer.raster.as_ref().and_then(|r| r.time_zarr.as_ref()).and_then(|z| z.window),
            Some([1526419, 1162633, 1526643, 1162857])
        );
        let saved = serde_json::to_value(&p).unwrap();
        assert_eq!(saved["requires"], json!(["zarr-window"]));
        assert_eq!(json_bytes(&map_file_bytes(p.map()).unwrap())["requires"], saved["requires"]);
        assert_eq!(json_bytes(&layer_file_bytes(&layer).unwrap())["requires"], saved["requires"]);
        // The whole variable following the slider needs nothing new.
        if let Some(z) = p.maps[0].layers[0].raster.as_mut().and_then(|r| r.time_zarr.as_mut()) {
            z.window = None;
        }
        assert!(serde_json::to_value(&p).unwrap().get("requires").is_none());
        let mut unknown = saved;
        unknown["requires"] = json!(["zarr-window", "future-zarr"]);
        assert!(Project::from_json(&unknown.to_string()).unwrap_err().contains("future-zarr"));
    }

    /// A blank basemap's paper colour (#2532) is required by name in a project and a map file: an
    /// older reader would draw the paper white and save the colour away. White paper needs nothing.
    #[test]
    fn a_blank_paper_colour_is_required_where_it_is_saved() {
        let mut p = Project::from_json(include_str!("../../../schema/fixtures/blank_paper.rgis")).unwrap();
        assert_eq!(p.map().blank_paper(), Some(rustgis_map::Rgba::rgb(241, 234, 223)));
        let saved = serde_json::to_value(&p).unwrap();
        assert_eq!(saved["requires"], json!(["blank-paper"]));
        assert_eq!(saved["maps"][0]["paper"], json!([241, 234, 223, 255]));
        assert_eq!(json_bytes(&map_file_bytes(p.map()).unwrap())["requires"], saved["requires"]);
        p.maps[0].paper = None;
        let clean = serde_json::to_value(&p).unwrap();
        assert!(clean.get("requires").is_none());
        assert!(clean["maps"][0].get("paper").is_none());
        // A saved command that gives the blank basemap a colour needs it too; white, null or
        // another basemap doesn't.
        for (params, required) in [
            (json!({"name": "blank", "color": "#F1EADF"}), true),
            (json!({"name": "Blank", "color": "#101820"}), true),
            (json!({"name": "blank", "color": "#ffffff"}), false),
            (json!({"name": "blank", "color": null}), false),
            (json!({"name": "blank"}), false),
            (json!({"name": "paper", "color": "#F1EADF"}), false),
        ] {
            let script = format!("# Paper\nmap.basemap= {params}");
            for (field, value) in [
                ("tasks", json!([{"name": "Task", "steps": [{"title": "Paper", "command": "map.basemap", "params": params}]}])),
                (
                    "tasks",
                    json!([{"name": "Script", "steps": [{"title": "Paper", "command": "script.run", "params": {"script": script}}]}]),
                ),
                ("notebooks", json!([{"name": "Notebook", "cells": [{"kind": "code", "source": script}]}])),
            ] {
                let mut document = clean.clone();
                document[field] = value;
                let saved = serde_json::to_value(Project::from_json(&document.to_string()).unwrap()).unwrap();
                if required {
                    assert_eq!(saved["requires"], json!(["blank-paper"]), "{field} {params}");
                } else {
                    assert!(saved.get("requires").is_none(), "{field} {params}");
                }
            }
        }
        let mut unknown = saved;
        unknown["requires"] = json!(["blank-paper", "future-paper"]);
        assert!(Project::from_json(&unknown.to_string()).unwrap_err().contains("future-paper"));
    }

    /// A map frame's border other than the default (#2557) is required where it is saved, so an
    /// older reader refuses the project instead of drawing the default line and saving it so. The
    /// default border is not written and needs nothing.
    #[test]
    fn frame_border_capability_follows_saved_layouts_and_commands() {
        let fixture = Project::from_json(include_str!("../../../schema/fixtures/layout_frame_border.rgis")).unwrap();
        let saved = serde_json::to_value(&fixture).unwrap();
        assert_eq!(saved["requires"], json!(["layout-frame-border"]));
        let frames = &saved["layouts"][0]["elements"];
        assert!(frames[0]["kind"]["MapFrame"].get("border_color").is_none());
        assert!(frames[0]["kind"]["MapFrame"].get("border_width").is_none());
        assert_eq!(frames[1]["kind"]["MapFrame"]["border_width"], json!(0.0));
        assert_eq!(frames[2]["kind"]["MapFrame"]["border_color"], json!([200, 184, 154, 255]));
        let reopened = Project::from_json(&saved.to_string()).unwrap();
        assert_eq!(reopened.layouts, fixture.layouts);
        let mut clean = fixture;
        for e in &mut clean.layouts[0].elements {
            if let rustgis_map::layout::ElementKind::MapFrame { border_color, border_width, .. } = &mut e.kind {
                *border_color = rustgis_map::layout::FRAME_BORDER_COLOR;
                *border_width = rustgis_map::layout::FRAME_BORDER_WIDTH;
            }
        }
        let base = serde_json::to_value(&clean).unwrap();
        assert!(base.get("requires").is_none(), "default borders need nothing");
        // A saved update that changes a border needs it; null only gives the default back.
        for (params, required) in [
            (json!({"element": 1, "border": {"width": 0}}), true),
            (json!({"element": 1, "border": {"color": "#C8B89A"}}), true),
            (json!({"element": 1, "border": null}), false),
            (json!({"element": 1, "name": "Main"}), false),
        ] {
            let script = format!("layout.update {params}");
            for value in [
                json!([{"name": "Task", "steps": [{"title": "Border", "command": "layout.update", "params": params}]}]),
                json!([{"name": "Script", "steps": [{"title": "Border", "command": "script.run", "params": {"script": script}}]}]),
            ] {
                let mut document = base.clone();
                document["tasks"] = value;
                let saved = serde_json::to_value(Project::from_json(&document.to_string()).unwrap()).unwrap();
                if required {
                    assert_eq!(saved["requires"], json!(["layout-frame-border"]), "{params}");
                } else {
                    assert!(saved.get("requires").is_none(), "{params}");
                }
            }
        }
        // An imported layout's text with a changed border needs it; with the default, none.
        for (layout, required) in [(&saved["layouts"][0], true), (&base["layouts"][0], false)] {
            let params = json!({"text": layout.to_string()});
            let mut document = base.clone();
            document["tasks"] = json!([{"name": "Task", "steps": [{"title": "Import", "command": "layout.import", "params": params}]}]);
            let saved = serde_json::to_value(Project::from_json(&document.to_string()).unwrap()).unwrap();
            assert_eq!(saved.get("requires").is_some(), required, "{layout}");
        }
        let mut unknown = saved;
        unknown["requires"] = json!(["layout-frame-border", "future-border"]);
        assert!(Project::from_json(&unknown.to_string()).unwrap_err().contains("future-border"));
    }

    /// Nondefault relief effects survive project, map and layer files; ordinary relief needs none.
    #[test]
    fn relief_headers_cover_each_effect_and_clean_controls() {
        let raster: rustgis_map::raster::RasterLayer = serde_json::from_value(json!({
            "style": {"renderer": {"ShadedRelief": {"band": 0, "ramp": null, "azimuth": 315.0, "altitude": 45.0, "z_factor": 1.0}}}
        }))
        .unwrap();
        let clean = Layer::new_raster(1, "DEM", raster);
        for field in ["shadow", "sky", "fill", "rock", "ambient", "balanced"] {
            let mut value = serde_json::to_value(&clean).unwrap();
            value["raster"]["style"]["renderer"]["ShadedRelief"][field] = match field {
                "rock" => serde_json::to_value(rustgis_map::raster::Rock::DEFAULT).unwrap(),
                "balanced" => json!(true),
                _ => json!(0.4),
            };
            let layer: Layer = serde_json::from_value(value).unwrap();
            let mut p = Project::new("Relief");
            p.maps[0].layers.push(layer.clone());
            for document in [
                serde_json::to_value(&p).unwrap(),
                json_bytes(&map_file_bytes(p.map()).unwrap()),
                json_bytes(&layer_file_bytes(&layer).unwrap()),
            ] {
                let needs = if matches!(field, "ambient" | "balanced") { "shaded-relief-light" } else { "shaded-relief-effects" };
                assert_eq!(document["requires"], json!([needs]), "{field}: {document}");
            }
        }
        assert!(json_bytes(&layer_file_bytes(&clean).unwrap()).get("requires").is_none());
    }

    /// Tasks, scripts and command notebook cells need ordered-export support when replayed;
    /// default exports, unrelated parameters and notebook prose retain legacy compatibility.
    #[test]
    fn table_export_order_capability_follows_saved_commands() {
        let mut fixture = Project::from_json(include_str!("../../../schema/fixtures/table_export_order.rgis")).unwrap();
        assert_eq!(serde_json::to_value(&fixture).unwrap()["requires"], json!(["table-export-order"]));
        fixture.tasks.clear();
        let base = serde_json::to_value(&fixture).unwrap();
        for (params, required) in [
            (json!({"table":"ranked","path":"top.csv"}), false),
            (json!({"sort":null,"limit":null}), false),
            (json!({"sort":[{"field":"score","descending":true}]}), true),
            (json!({"limit":0}), true),
        ] {
            let script = format!("# Export\nshare.exportTable= {params}");
            for (field, value) in [
                ("tasks", json!([{"name":"Export","steps":[{"title":"Top","command":"share.exportTable","params":params}]}])),
                ("tasks", json!([{"name":"Script","steps":[{"title":"Top","command":"script.run","params":{"script":script}}]}])),
                ("notebooks", json!([{"name":"Commands","cells":[{"kind":"code","source":script}]}])),
                (
                    "gp_history",
                    json!([{"tool":"Export Table","params":{},"command":["share.exportTable",params],"ok":true,"messages":[],"millis":0}]),
                ),
            ] {
                let mut document = base.clone();
                document[field] = value;
                let project = Project::from_json(&document.to_string()).unwrap();
                let saved = serde_json::to_value(&project).unwrap();
                assert_eq!(saved.get("requires"), required.then(|| json!(["table-export-order"])).as_ref(), "{field} {params}");
                let reopened = Project::from_json(&saved.to_string()).unwrap();
                assert_eq!(serde_json::to_value(reopened).unwrap().get("requires"), saved.get("requires"));
            }
        }
        let script = "share.exportTable {\"limit\":1}";
        let mut document = base;
        document["tasks"] =
            json!([{"name":"Unrelated","steps":[{"title":"Other","command":"layer.addDataText","params":{"sort":[],"limit":1}}]}]);
        document["notebooks"] = json!([
            {"name":"Notes","cells":[{"kind":"markdown","source":script}]},
            {"name":"Comments","cells":[{"kind":"code","source":format!("# {script}")}]},
            {"name":"Foreign","cells":[{"kind":"code","source":script}],"jupyter":{"metadata":{"kernelspec":{"language":"python"}}}}
        ]);
        let project = Project::from_json(&document.to_string()).unwrap();
        assert!(serde_json::to_value(project).unwrap().get("requires").is_none());
        assert!(rustgis_analysis::resolve_published("share.exportTable").is_none(), "models and web tools execute analysis tools only");
    }
}
