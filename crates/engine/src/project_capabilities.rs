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
        // A package's identity and the packages already brought back: an older reader would drop
        // them on saving, and then apply a package's changes twice.
        if self.field_origin.as_ref().is_some_and(|origin| !origin.package.is_empty() || !origin.ids.is_empty())
            || self.offline_areas.iter().any(|area| !area.returned.is_empty() || !area.ids.is_empty())
        {
            required.push("field-package");
        }
        if self.offline_areas.iter().any(|area| !area.conflicts.is_empty()) {
            required.push(super::FIELD_CONFLICTS);
        }
        // A table saved in a coordinate system of its own, as a layer's is (map_capabilities).
        if !required.contains(&super::NATIVE_COORDINATES) && self.tables.iter().any(|t| held_natively(&t.dataset)) {
            required.push(super::NATIVE_COORDINATES);
        }
        if self.coordinate_system.is_some() || self.crs_from_first_layer {
            required.push(super::PROJECT_CRS);
        }
        // Saved symbology keeps its images too.
        if !required.contains(&"picture-markers") && self.styles.iter().any(|s| s.renderer.has_picture_markers()) {
            required.push("picture-markers");
        }
        if !required.contains(&"svg-markers") && self.styles.iter().any(|s| s.renderer.any_picture(svg_picture)) {
            required.push("svg-markers");
        }
        // And its labels' weights (#2803): an older reader would apply the style without them.
        if !required.contains(&"label-weight")
            && self.styles.iter().any(|s| s.labels.in_order().any(|c| c.weight != rustgis_map::symbology::DEFAULT_LABEL_WEIGHT))
        {
            required.push("label-weight");
        }
        // And their clearances (#2828), for the same reason.
        if !required.contains(&"label-clearance")
            && self.styles.iter().any(|s| s.labels.in_order().any(|c| c.clearance != rustgis_map::symbology::DEFAULT_LABEL_CLEARANCE))
        {
            required.push("label-clearance");
        }
        // And whether they remove duplicates (#2847), for the same reason.
        if !required.contains(&"label-duplicates") && self.styles.iter().any(|s| s.labels.in_order().any(|c| c.remove_duplicates.is_some()))
        {
            required.push("label-duplicates");
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
        for name in layout_capabilities(&self.layouts) {
            if !required.contains(&name) {
                required.push(name);
            }
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
            || self.tasks.iter().any(|t| t.steps.iter().any(|s| command_uses(&uses, &s.command, &s.params)))
            || self.gp_history.iter().any(|h| {
                command_uses(&uses, if h.id.is_empty() { &h.tool } else { &h.id }, &h.params)
                    || h.command.as_ref().is_some_and(|(id, params)| command_uses(&uses, id, params))
            })
            || self.notebooks.iter().any(|n| {
                n.language().is_none()
                    && n.cells.iter().any(|c| c.kind == crate::cmd::notebook::CellKind::Code && script_uses(&c.source, uses))
            })
    }
}

/// What a task saved on its own needs: what a project saves for its tasks' steps (a `model.run`
/// given its graph inline, and a script line, included), in the order its list has them.
pub(crate) fn task_capabilities(steps: &[super::TaskStep]) -> Vec<&'static str> {
    let mut required: Vec<&'static str> = COMMAND_CAPABILITIES
        .iter()
        .filter_map(|&(name, uses)| steps.iter().any(|s| command_uses(&uses, &s.command, &s.params)).then_some(name))
        .collect();
    if steps.iter().any(|s| command_uses_table_export_order(&s.command, &s.params)) {
        required.push("table-export-order");
    }
    required
}

type CommandCheck = fn(&str, &Value) -> bool;
const COMMAND_CAPABILITIES: &[(&str, CommandCheck)] = &[
    ("slope-percent", command_uses_percent_slope),
    ("tabulate-intersection-unit", command_uses_tabulate_unit),
    ("tabulate-intersection-empty-zones", command_uses_tabulate_empty_zones),
    ("layout-element-background", command_uses_element_background),
    ("picture-markers", command_uses_picture_marker),
    ("svg-markers", command_uses_svg_marker),
    ("graticule-mgrs", command_draws_mgrs_graticule),
    ("graticule-on-top", command_puts_graticule_on_top),
    ("graticule-color", command_colors_graticule),
    ("layout-element-ink", command_uses_element_ink),
    ("layout-element-wrap", command_uses_element_wrap),
    ("layout-text-block", command_uses_text_block),
    ("layout-chart-data", command_uses_chart_data),
    ("layout-chart-look", command_uses_chart_look),
    ("layout-chart-classes", command_uses_chart_classes),
    ("layout-chart-stack", command_uses_chart_stack),
    ("layout-chart-facing", command_uses_chart_facing),
    ("layout-chart-outline", command_uses_chart_outline),
    ("blank-paper", command_uses_blank_paper),
    ("layout-frame-border", command_uses_frame_border),
    ("layout-text-size", command_uses_text_size),
    ("layout-scale-bar-details", command_uses_scale_bar_details),
    ("dggs-system", command_uses_dggs_system),
    ("label-weight", command_weighs_labels),
    ("label-clearance", command_clears_labels),
    ("label-duplicates", command_removes_duplicate_labels),
    ("tile-resampling", command_resamples_tiles),
    ("layout-page-color", command_colours_the_page),
];

/// A saved `labels.placement` that has a class remove duplicates, read as the command reads it
/// (#2847): an older reader runs it without the key, saying nothing, so every label would show.
/// False or null is what an older reader keeps.
fn command_removes_duplicate_labels(id: &str, params: &Value) -> bool {
    match id {
        "labels.placement" => params.get("removeDuplicates").and_then(crate::cmd::labels::number).is_some(),
        "script.run" => params.get("script").and_then(Value::as_str).is_some_and(|s| script_uses(s, command_removes_duplicate_labels)),
        _ => false,
    }
}

/// A saved `map.addTiles` or `map.setTiles` drawing a tile layer's pictures nearest (#2804), read as
/// the command reads it: an older reader runs it without the key, saying nothing, so the layer would
/// stay blended. `bilinear` is what an older reader keeps.
fn command_resamples_tiles(id: &str, params: &Value) -> bool {
    match id {
        "map.addTiles" | "map.setTiles" => {
            crate::cmd::tiles::resampling(params).ok().flatten() == Some(rustgis_map::raster::Resampling::Nearest)
        }
        "script.run" => params.get("script").and_then(Value::as_str).is_some_and(|s| script_uses(s, command_resamples_tiles)),
        _ => false,
    }
}

/// A saved `labels.placement` that gives a class a clearance other than 2, read as the command
/// reads it (#2828): an older reader runs it without the key, saying nothing, so the labels would
/// keep 2 points apart as before. 2 or null is what an older reader keeps.
fn command_clears_labels(id: &str, params: &Value) -> bool {
    match id {
        "labels.placement" => params
            .get("clearance")
            .and_then(crate::cmd::labels::number)
            .is_some_and(|c| rustgis_map::symbology::label_clearance(c) != rustgis_map::symbology::DEFAULT_LABEL_CLEARANCE),
        "script.run" => params.get("script").and_then(Value::as_str).is_some_and(|s| script_uses(s, command_clears_labels)),
        _ => false,
    }
}

/// A saved `labels.placement` that gives a class a weight other than 5, read as the command reads
/// it (#2803): an older reader runs it without the key, saying nothing, so the labels would compete
/// as before. 5 or null is what an older reader keeps.
fn command_weighs_labels(id: &str, params: &Value) -> bool {
    match id {
        "labels.placement" => params
            .get("weight")
            .and_then(crate::cmd::labels::number)
            .is_some_and(|w| rustgis_map::symbology::label_weight(w) != rustgis_map::symbology::DEFAULT_LABEL_WEIGHT),
        "script.run" => params.get("script").and_then(Value::as_str).is_some_and(|s| script_uses(s, command_weighs_labels)),
        _ => false,
    }
}

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

/// A layout command that sizes a legend's text, a scale bar's labels or a grid's labels other
/// than their default (#2581): an older reader warns that it ignores `textSize` or `labelSize`
/// and draws them at the default. `null` or the element's default (a legend's 9 points, a scale
/// bar's or a grid's 8) need nothing. A saved `layout.update` isn't told which element it
/// changes, so any `textSize` but `null` requires it there, as for the other layout keys; a
/// grid's size is one of its `"grid"` options there.
fn command_uses_text_size(id: &str, params: &Value) -> bool {
    use rustgis_map::layout::{GRID_LABEL_PT, LEGEND_TEXT_PT, SCALE_BAR_TEXT_PT};
    let sized = |p: &Value, key: &str, default: f64| p.get(key).is_some_and(|v| !v.is_null() && v.as_f64() != Some(default));
    let kind = |k: &str| params.get("kind").and_then(Value::as_str).is_some_and(|v| v.trim().eq_ignore_ascii_case(k));
    match id {
        "layout.addElement" => {
            (kind("legend") && sized(params, "textSize", LEGEND_TEXT_PT))
                || (kind("scaleBar") && sized(params, "textSize", SCALE_BAR_TEXT_PT))
                || (kind("grid") && sized(params, "labelSize", GRID_LABEL_PT))
        }
        "layout.update" => {
            params.get("textSize").is_some_and(|v| !v.is_null()) || params.get("grid").is_some_and(|g| sized(g, "labelSize", GRID_LABEL_PT))
        }
        "layout.import" => imported_layout_uses(params, &["\"text_size\"", "\"label_size\""], |e| e.kind.has_custom_text_size()),
        "script.run" => params.get("script").and_then(Value::as_str).is_some_and(|s| script_uses(s, command_uses_text_size)),
        _ => false,
    }
}

/// A layout command that changes a map frame's border (#2557): an older reader warns that it
/// ignores `border` and draws the default one. `null` only gives the default back, which an older
/// reader's frames always have.
fn command_uses_frame_border(id: &str, params: &Value) -> bool {
    match id {
        "layout.update" => params.get("border").is_some_and(|b| !b.is_null()),
        "layout.import" => imported_layout_uses(params, &["\"border_"], |e| e.kind.has_custom_border()),
        "script.run" => params.get("script").and_then(Value::as_str).is_some_and(|s| script_uses(s, command_uses_frame_border)),
        _ => false,
    }
}

/// A `layout.page` command that gives the page a colour other than white (#2855), read as the
/// command reads it: an older reader ignores `color` and opens the page white. A `layout.import`
/// whose layout has one needs it too.
fn command_colours_the_page(id: &str, params: &Value) -> bool {
    match id {
        "layout.page" => crate::cmd::opaque_color(params, "color")
            .is_ok_and(|c| c.flatten().is_some_and(|c| c != rustgis_map::Rgba::rgb(0xFF, 0xFF, 0xFF))),
        "layout.import" => imported_layout(params, &["\"paper\""]).is_some_and(|mut l| {
            l.clamp();
            l.paper.is_some()
        }),
        "script.run" => params.get("script").and_then(Value::as_str).is_some_and(|s| script_uses(s, command_colours_the_page)),
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
        "layout.import" => imported_layout_uses(params, &["\"background\""], |e| e.background.is_some()),
        "script.run" => params.get("script").and_then(Value::as_str).is_some_and(|s| script_uses(s, command_uses_element_background)),
        _ => false,
    }
}

/// A layout command that gives a legend, colorbar, table or chart an ink (#2257). Only an
/// imported layout's: an older reader refuses "color" on those elements in `layout.addElement`
/// and `layout.update`, so such a command fails there instead of losing the ink.
fn command_uses_element_ink(id: &str, params: &Value) -> bool {
    match id {
        "layout.import" => imported_layout_uses(params, &["\"color\""], |e| e.kind.ink().is_some()),
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
        "layout.import" => imported_layout_uses(params, &["\"wrap\""], wraps),
        "script.run" => params.get("script").and_then(Value::as_str).is_some_and(|s| script_uses(s, command_uses_element_wrap)),
        _ => false,
    }
}

/// Whether a layout element is a text placed in the middle or at the bottom of its box, spaced
/// other than at the usual line spacing, or drawn in upper or lower case, once brought into range:
/// an older reader would draw it at the top, at the usual spacing and as typed.
fn text_blocks(e: &rustgis_map::layout::Element) -> bool {
    e.kind.text_uses_block()
}

/// A layout command that gives a text a block alignment, line spacing or case off its default:
/// an older reader ignores `valign`, `lineSpacing` and `case` and draws the text at the top, at
/// the usual spacing and as typed. Each is read on its own by the command's own reader
/// ([`crate::cmd::layout::text_block_settings`]), so the defaults (`"top"`, `1` or `"1"`, `"none"`,
/// in any case) and `null`, which leaves a text as it is, keep a text as an older reader draws it.
/// A value the command refuses on a text needs nothing either: the command changes nothing. The
/// keys are read here alone, so a block key given to another kind counts too, though its command
/// refuses it: an over-count that asks for the capability sooner, never a missing one. A
/// `layout.import` requires it for such a text once its layout is read into range, so a name it
/// doesn't know or a spacing that isn't a number, read as the default, needs nothing.
fn command_uses_text_block(id: &str, params: &Value) -> bool {
    use rustgis_map::layout::{LINE_SPACING, TextCase, TextVAlign};
    match id {
        "layout.addElement" | "layout.update" => {
            let usual = (TextVAlign::Top, LINE_SPACING, TextCase::None);
            // Each key on its own, so one the command refuses doesn't hide another one set.
            ["valign", "lineSpacing", "case"].into_iter().any(|k| {
                let one = Value::Object(params.get(k).into_iter().map(|v| (k.to_string(), v.clone())).collect());
                crate::cmd::layout::text_block_settings(&one, usual).is_ok_and(|set| set != usual)
            })
        }
        "layout.import" => imported_layout_uses(params, &["\"valign\"", "\"line_spacing\"", "\"case\""], text_blocks),
        "script.run" => params.get("script").and_then(Value::as_str).is_some_and(|s| script_uses(s, command_uses_text_block)),
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
        "layout.import" => imported_layout_uses(params, &["\"group\"", "\"statistic\"", "\"order\"", "\"bars\"", "\"other\""], charts_data),
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
        "layout.import" => imported_layout_uses(
            params,
            &["\"orientation\"", "\"values\"", "\"decimals\"", "\"title\"", "\"bar_color\"", "\"label_align\""],
            charts_look,
        ),
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
        "layout.import" => imported_layout_uses(params, &["\"classes\""], charts_classes),
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
        "layout.import" => imported_layout_uses(params, &["\"stack\""], charts_stack),
        "script.run" => params.get("script").and_then(Value::as_str).is_some_and(|s| script_uses(s, command_uses_chart_stack)),
        _ => false,
    }
}

/// Whether a layout element is a scale bar with details an older reader's can't draw: it would
/// draw a plain bar and save it again without them.
fn bar_details(e: &rustgis_map::layout::Element) -> bool {
    e.kind.scale_bar_uses_details()
}

/// A layout command that gives a scale bar a detail off its default: an older reader ignores the
/// keys and draws a plain bar. Giving the defaults, `"decimals": "auto"`, or `"unitLabel": null`
/// (the abbreviation an older reader's bar prints), keeps a bar as an older reader draws it.
fn command_uses_scale_bar_details(id: &str, params: &Value) -> bool {
    use rustgis_map::layout::{ScaleBarFit, ScaleLabelPosition, UnitLabelPosition};
    // A fixed number of decimals, 0 included, not "auto". A chart takes decimals too: a
    // `layout.addElement` says which kind it adds, but a `layout.update` doesn't say what it
    // changes, so a chart's decimals there require this as well, as a scale bar's require
    // `layout-chart-look`; the layout saved with the element requires what it holds anyway.
    let fixed_decimals = |id: &str| {
        crate::cmd::uint(params, "decimals").is_some()
            && (id == "layout.update"
                || params.get("kind").and_then(Value::as_str).is_some_and(|k| k.trim().eq_ignore_ascii_case("scaleBar")))
    };
    match id {
        "layout.addElement" | "layout.update" => {
            let num = |k: &str| crate::cmd::f(params, k);
            let name = |k: &str| params.get(k).and_then(Value::as_str);
            num("subdivisions").is_some_and(|n| n >= 2.0)
                || num("divisionsBeforeZero").is_some_and(|n| n >= 1.0)
                || name("labelPosition").and_then(ScaleLabelPosition::from_name).is_some_and(|v| v != ScaleLabelPosition::default())
                || params.get("unitLabel").is_some_and(Value::is_string)
                || name("unitLabelPosition").and_then(UnitLabelPosition::from_name).is_some_and(|v| v != UnitLabelPosition::default())
                || fixed_decimals(id)
                || name("fit").and_then(ScaleBarFit::from_name).is_some_and(|v| v != ScaleBarFit::default())
        }
        "layout.import" => imported_layout_uses(
            params,
            &[
                "\"subdivisions\"",
                "\"divisions_before_zero\"",
                "\"label_position\"",
                "\"unit_label\"",
                "\"unit_label_position\"",
                "\"decimals\"",
                "\"fit\"",
            ],
            bar_details,
        ),
        "script.run" => params.get("script").and_then(Value::as_str).is_some_and(|s| script_uses(s, command_uses_scale_bar_details)),
        _ => false,
    }
}

fn charts_facing(e: &rustgis_map::layout::Element) -> bool {
    e.kind.chart_uses_facing()
}

/// A layout command that mirrors a chart, gives it a fixed maximum or hides its labels (#2487
/// part 4): an older reader ignores those keys and draws the chart the usual way round, on its
/// bars' own scale, labelled. `false`, `null` and `"auto"` keep a chart as an older reader draws
/// it; only a chart takes the keys, so a `layout.update` is read by them alone.
fn command_uses_chart_facing(id: &str, params: &Value) -> bool {
    let facing = || {
        let on = |k: &str| params.get(k).and_then(crate::cmd::bool_value) == Some(true);
        on("mirror")
            || on("hideLabels")
            || params.get("max").and_then(Value::as_f64).and_then(|m| rustgis_map::layout::chart_max(Some(m))).is_some()
    };
    match id {
        "layout.addElement" => {
            params.get("kind").and_then(Value::as_str).is_some_and(|k| k.trim().eq_ignore_ascii_case("chart")) && facing()
        }
        "layout.update" => facing(),
        "layout.import" => imported_layout_uses(params, &["\"mirror\"", "\"max\"", "\"hide_labels\""], charts_facing),
        "script.run" => params.get("script").and_then(Value::as_str).is_some_and(|s| script_uses(s, command_uses_chart_facing)),
        _ => false,
    }
}

fn charts_outlined(e: &rustgis_map::layout::Element) -> bool {
    e.kind.chart_uses_outline()
}

/// A layout command that draws a line round a chart's bars (#2487 part 4b): an object without a
/// width (the outline's, or the 0.5 points it is first given) or with one above 0. Null, or a
/// width of 0, draws none, so it needs nothing.
fn command_uses_chart_outline(id: &str, params: &Value) -> bool {
    let outlined = || {
        // The width read as the command reads it, a number or a number in text.
        params.get("barOutline").filter(|o| o.is_object()).is_some_and(|o| match o.get("width") {
            None | Some(Value::Null) => true,
            Some(_) => crate::cmd::f(o, "width").is_some_and(|w| w > 0.0),
        })
    };
    match id {
        "layout.addElement" => {
            params.get("kind").and_then(Value::as_str).is_some_and(|k| k.trim().eq_ignore_ascii_case("chart")) && outlined()
        }
        "layout.update" => outlined(),
        "layout.import" => imported_layout_uses(params, &["\"bar_outline\""], charts_outlined),
        "script.run" => params.get("script").and_then(Value::as_str).is_some_and(|s| script_uses(s, command_uses_chart_outline)),
        _ => false,
    }
}

/// Whether a `layout.import` carries its layout as `"text"` with an element `uses`: an older
/// reader reads it without the keys it doesn't know. One read from a `"path"` is a layout file,
/// whose own header requires what it uses (#2277). Every save asks, so a text with none of the
/// feature's `keys` (a saved layout writes them only when set) isn't read through, unless it has a
/// backslash: JSON may spell a key with escapes (`"\u0063olor"`). It is read once, whatever its
/// keys.
fn imported_layout_uses(params: &Value, keys: &[&str], uses: fn(&rustgis_map::layout::Element) -> bool) -> bool {
    imported_layout(params, keys).is_some_and(|l| l.elements.iter().any(uses))
}

/// A `layout.import`'s layout, read only when its text names one of `keys` (or escapes, which
/// could hide one).
fn imported_layout(params: &Value, keys: &[&str]) -> Option<rustgis_map::layout::Layout> {
    params
        .get("text")
        .and_then(Value::as_str)
        .filter(|t| keys.iter().any(|key| t.contains(key)) || t.contains('\\'))
        .and_then(|t| serde_json::from_str::<rustgis_map::layout::Layout>(t).ok())
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
    tool_uses(&["tabulateIntersection"], id, params, uses)
}

/// Generate Tessellation and Aggregate Points to Grid were A5's tools, and keep A5's ids: an
/// older reader would leave out a `system` other than A5's and make A5 cells, warning only that
/// it ignored the key. Blank or `A5` needs nothing, by the tools' own rule (`System::is_a5`).
fn command_uses_dggs_system(id: &str, params: &Value) -> bool {
    tool_uses(&["a5Grid", "a5Aggregate"], id, params, |p| match p.get("system") {
        None | Some(Value::Null) => false,
        Some(Value::String(s)) => !rustgis_analysis::dggs::System::is_a5(s),
        Some(_) => true,
    })
}

/// Whether a saved command runs one of `tools` with parameters `uses` matches: as a tool step,
/// through `gp.run` or `gp.batch`, or in a script.
fn tool_uses(tools: &[&str], id: &str, params: &Value, uses: fn(&Value) -> bool) -> bool {
    let named = |t: Option<&str>| t.is_some_and(|t| tools.contains(&t));
    if named(crate::cmd::gp::tool_id(id)) {
        return uses(params);
    }
    let tool = named(params.get("tool").and_then(Value::as_str).and_then(crate::cmd::gp::tool_id));
    match id {
        "gp.run" if tool => crate::parameters::params_envelope(params.get("params")).is_ok_and(|p| uses(&p)),
        // Batch ignores non-object shared params; older tools refuse unknown binding parameters.
        "gp.batch" if tool => params.get("params").filter(|p| p.is_object()).is_some_and(uses),
        "script.run" => {
            params.get("script").and_then(Value::as_str).is_some_and(|s| script_uses(s, |id, params| tool_uses(tools, id, params, uses)))
        }
        _ => false,
    }
}

/// `layer.marker`, a line stroke's markers or a dot fill given an image: an older reader would
/// leave its `image` out and keep the marker it had, or refuse `"image"` as a mark.
fn command_uses_picture_marker(id: &str, params: &Value) -> bool {
    command_sets_marker_image(id, params, |_| true)
}

/// [`command_uses_picture_marker`] with an SVG (#2266 part E1b): a reader of PNG and JPEG markers
/// only would refuse the command.
fn command_uses_svg_marker(id: &str, params: &Value) -> bool {
    command_sets_marker_image(id, params, rustgis_render::picture_marker_is_svg)
}

/// Whether command `id` gives a marker an image (base64) that `image` holds true for.
fn command_sets_marker_image(id: &str, params: &Value, image: fn(&str) -> bool) -> bool {
    let any = |list: &str, at: fn(&Value) -> Option<&Value>| {
        params.get(list).and_then(Value::as_array).is_some_and(|l| l.iter().any(|v| at(v).and_then(Value::as_str).is_some_and(image)))
    };
    match id {
        "layer.marker" => params.get("image").and_then(Value::as_str).is_some_and(image),
        "layer.lineStrokes" => any("strokes", |st| st.get("markers")?.get("image")),
        "layer.fillLayers" => any("fills", |f| f.get("image")),
        "script.run" => params
            .get("script")
            .and_then(Value::as_str)
            .is_some_and(|s| script_uses(s, |id, params| command_sets_marker_image(id, params, image))),
        _ => false,
    }
}

/// Whether marker image `p` is an SVG ([`command_uses_svg_marker`]'s reason, for saved symbology).
fn svg_picture(p: rustgis_map::Picture) -> bool {
    rustgis_render::picture_marker_is_svg(p.base64())
}

/// Use the executor's line grammar; comments/invalid lines are not executable commands, and
/// script_line refuses nested scripts, so this check cannot recurse through script.run.
fn script_uses(script: &str, uses: impl Fn(&str, &Value) -> bool) -> bool {
    script
        .lines()
        .take(crate::cmd::workflows::MAX_SCRIPT_LINES)
        .any(|line| crate::script_line(line).is_some_and(|parsed| parsed.is_ok_and(|(id, params)| command_uses(&uses, &id, &params))))
}

/// `uses` on a saved command and, for a `model.run` given its graph inline, on each of the graph's
/// tool nodes as on a saved model's steps (#2758): that graph is in no saved model, so nothing
/// else reads it, and an older reader would run its nodes with the option dropped.
fn command_uses(uses: &impl Fn(&str, &Value) -> bool, id: &str, params: &Value) -> bool {
    uses(id, params) || (id == "model.run" && inline_graph_uses(params, uses))
}

/// A graph read as the run reads it (`model::graph_from_json_counted`): an object with `nodes`, at
/// most `MAX_NODES` nodes and `MAX_EDGES` connections (a bigger one is refused there, so needs
/// nothing), and only a node of kind `tool` runs, with `params` that aren't an object dropped (the
/// checks read nothing from one). Read in place, without a copy, so a node the run drops (an empty
/// or repeated id, an unreadable `x`, a `layer` that isn't text) is read here all the same: that
/// only over-requires, which is safe, and a node it can't use never hides the others.
///
/// No recursion: a node is checked by the leaf check only. A node runs a tool (`model::tool_of`),
/// so one naming a command, `model.run` included, fails there and is read as the id no check
/// matches; and one naming `script.run` is skipped, as its check is the one that reads into a
/// script, which could hold another graph.
fn inline_graph_uses(params: &Value, uses: &impl Fn(&str, &Value) -> bool) -> bool {
    let Some(graph) = params.get("graph") else { return false };
    let few = |key: &str, max: usize| graph.get(key).and_then(Value::as_array).is_none_or(|list| list.len() <= max);
    few("edges", crate::model::MAX_EDGES)
        && graph.get("nodes").and_then(Value::as_array).filter(|nodes| nodes.len() <= crate::model::MAX_NODES).is_some_and(|nodes| {
            nodes.iter().any(|node| {
                let tool = node.get("tool").and_then(Value::as_str).unwrap_or("");
                node.get("kind").and_then(Value::as_str) == Some("tool")
                    && tool != "script.run"
                    && uses(tool, node.get("params").unwrap_or(&Value::Null))
            })
        })
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

/// Whether a layer's labels follow a guide line layer (#2262): an older reader would drop the
/// guide, and place and save them as without it.
fn label_guide(layer: &Layer) -> bool {
    layer.labels.in_order().any(|c| c.guide.is_some())
}

/// Whether a layer's label classes weigh other than the default (#2803): an older reader would
/// drop the weights, and place and save the labels as without them.
fn label_weight(layer: &Layer) -> bool {
    layer.labels.in_order().any(|c| c.weight != rustgis_map::symbology::DEFAULT_LABEL_WEIGHT)
}

/// Whether a layer's label classes keep other than the default room free (#2828): an older reader
/// would drop the clearances, and place and save the labels as without them.
fn label_clearance(layer: &Layer) -> bool {
    layer.labels.in_order().any(|c| c.clearance != rustgis_map::symbology::DEFAULT_LABEL_CLEARANCE)
}

/// Whether a layer's label classes remove duplicates (#2847): an older reader would drop the
/// setting, and place and save every label.
fn removes_duplicate_labels(layer: &Layer) -> bool {
    layer.labels.in_order().any(|c| c.remove_duplicates.is_some())
}

/// The capabilities a layer needs, in [`map_capabilities`]'s order.
fn layer_capabilities(layer: &Layer) -> Vec<&'static str> {
    [
        (relief_effects(layer), "shaded-relief-effects"),
        (relief_light(layer), "shaded-relief-light"),
        (layer.size_by.is_some(), super::SIZE_BY_CAPABILITY),
        (layer.has_picture_markers(), "picture-markers"),
        (layer.any_picture(svg_picture), "svg-markers"),
        (zarr_window(layer), "zarr-window"),
        (label_guide(layer), "label-guide"),
        (label_weight(layer), "label-weight"),
        (label_clearance(layer), "label-clearance"),
        (removes_duplicate_labels(layer), "label-duplicates"),
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
        mut walls_layer,
        mut relief,
        mut light,
        mut legends,
        mut sized,
        mut pictures,
        mut zarr,
        mut mgrs,
        mut on_top,
        mut grid_color,
    ) = (false, false, false, false, false, false, false, false, false, false, false, false, false, false, false);
    let (mut polygon, mut svg) = (false, false);
    let (mut offline_area_id, mut paper, mut native, mut guided, mut weighed, mut cleared, mut deduped) =
        (false, false, false, false, false, false, false);
    let mut crisp = false;
    for map in maps {
        offline_area_id |= map.offline_area_id.is_some();
        // A blank basemap's paper colour (#2532), which an older reader would draw and save white.
        paper |= map.paper.is_some();
        guided |= map.layers.iter().any(label_guide);
        weighed |= map.layers.iter().any(label_weight);
        cleared |= map.layers.iter().any(label_clearance);
        deduped |= map.layers.iter().any(removes_duplicate_labels);
        // A tile layer drawn nearest (#2804), which an older reader would draw and save blended.
        crisp |= map.tiles.iter().any(|t| t.resampling != rustgis_map::raster::Resampling::Bilinear);
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
            // The layer a block's walls are coloured by (#2173), which an older reader would save away.
            walls_layer |= t.block.as_ref().is_some_and(|b| b.walls_layer.is_some());
        }
        relief |= map.layers.iter().any(relief_effects);
        light |= map.layers.iter().any(relief_light);
        sized |= map.layers.iter().any(|layer| layer.size_by.is_some());
        pictures |= map.layers.iter().any(Layer::has_picture_markers);
        svg |= map.layers.iter().any(|layer| layer.any_picture(svg_picture));
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
        (walls_layer, "terrain-block-walls-layer"),
        (relief, "shaded-relief-effects"),
        (light, "shaded-relief-light"),
        (legends, "normalized-legend-urls"),
        (sized, super::SIZE_BY_CAPABILITY),
        (pictures, "picture-markers"),
        (svg, "svg-markers"),
        (zarr, "zarr-window"),
        (guided, "label-guide"),
        (weighed, "label-weight"),
        (cleared, "label-clearance"),
        (deduped, "label-duplicates"),
        (crisp, "tile-resampling"),
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

/// A layout file has its own header, as a map file has: what its elements use, as the project
/// holding it requires for them (#2277). Indented, as layout files have always been written. No
/// `crs-wkt`: a layout holds no coordinate system, only a measured grid's as text.
pub fn layout_file_bytes(layout: &rustgis_map::layout::Layout) -> Result<Vec<u8>, String> {
    let requires = layout_capabilities(std::slice::from_ref(layout));
    serde_json::to_vec_pretty(&Item { format: FORMAT, requires, value: layout }).map_err(|e| e.to_string())
}

/// What the elements of `layouts` use that an older reader would draw without and save again
/// without: the project requires it for its layouts, and a layout file for its own.
fn layout_capabilities(layouts: &[rustgis_map::layout::Layout]) -> Vec<&'static str> {
    let elements = || layouts.iter().flat_map(|l| &l.elements);
    [
        (elements().any(|e| e.background.is_some()), "layout-element-background"),
        (elements().any(|e| e.kind.ink().is_some()), "layout-element-ink"),
        (elements().any(wraps), "layout-element-wrap"),
        (elements().any(text_blocks), "layout-text-block"),
        (elements().any(charts_data), "layout-chart-data"),
        (elements().any(charts_look), "layout-chart-look"),
        (elements().any(charts_classes), "layout-chart-classes"),
        (elements().any(charts_stack), "layout-chart-stack"),
        (elements().any(charts_facing), "layout-chart-facing"),
        (elements().any(charts_outlined), "layout-chart-outline"),
        (elements().any(|e| e.kind.has_custom_border()), "layout-frame-border"),
        (elements().any(|e| e.kind.has_custom_text_size()), "layout-text-size"),
        (elements().any(bar_details), "layout-scale-bar-details"),
        (layouts.iter().any(|l| l.paper.is_some()), "layout-page-color"),
    ]
    .into_iter()
    .filter_map(|(used, name)| used.then_some(name))
    .collect()
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

    /// A tile layer drawn nearest (#2804) reads back as it was and saving names `tile-resampling`;
    /// one drawn blended, as an older reader would, names nothing. So does a saved step setting it.
    #[test]
    fn tile_resampling_fixture_reads_its_setting() {
        use rustgis_map::raster::Resampling;
        let fixture = Project::from_json(include_str!("../../../schema/fixtures/tile_resampling.rgis")).unwrap();
        let saved = serde_json::to_value(&fixture).unwrap();
        assert_eq!(saved["requires"], json!(["tile-resampling"]));
        assert_eq!(fixture.maps[0].tiles[0].resampling, Resampling::Nearest);
        let reopened = Project::from_json(&saved.to_string()).unwrap();
        assert_eq!(serde_json::to_value(&reopened).unwrap(), saved);
        let mut plain = fixture;
        plain.maps[0].tiles[0].resampling = Resampling::Bilinear;
        assert!(serde_json::to_value(&plain).unwrap().get("requires").is_none());
        assert!(command_resamples_tiles("map.setTiles", &json!({"index": 0, "resampling": "nearest"})));
        assert!(!command_resamples_tiles("map.setTiles", &json!({"index": 0, "resampling": "bilinear"})));
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

    /// A block diagram's walls layer reads back as it was and saving names it in `requires`; a
    /// block without one doesn't (#2173).
    #[test]
    fn terrain_block_walls_layer_fixture_requires_it() {
        let fixture = Project::from_json(include_str!("../../../schema/fixtures/terrain_block_walls_layer_v2.rgis")).unwrap();
        let block = fixture.maps[0].view.terrain.as_ref().and_then(|t| t.block.as_ref());
        assert_eq!(block.and_then(|b| b.walls_layer.as_deref()), Some("geologia"));
        let saved = serde_json::to_value(&fixture).unwrap();
        assert_eq!(saved["requires"], json!(["terrain-block", "terrain-block-walls-layer"]));
        let mut plain = fixture;
        if let Some(b) = plain.maps[0].view.terrain.as_mut().and_then(|t| t.block.as_mut()) {
            b.walls_layer = None;
        }
        assert_eq!(serde_json::to_value(&plain).unwrap()["requires"], json!(["terrain-block"]));
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

    /// A grid system other than A5 on A5's tools requires `dggs-system` wherever a run is saved;
    /// A5, blank or none needs nothing, as such a run means the same to an older reader.
    #[test]
    fn a_grid_system_other_than_a5_is_required() {
        for (params, needed) in [
            (json!({"resolution": 5}), false),
            (json!({"system": "A5"}), false),
            (json!({"system": " a5 "}), false),
            (json!({"system": ""}), false),
            (json!({"system": null}), false),
            (json!({"system": "H3"}), true),
            (json!({"system": "Hexagon"}), true),
            (json!({"system": 5}), true),
        ] {
            for tool in ["a5Grid", "a5Aggregate"] {
                assert_eq!(command_uses_dggs_system(tool, &params), needed, "{tool} {params}");
                let run = json!({"tool": tool, "params": params});
                assert_eq!(command_uses_dggs_system("gp.run", &run), needed, "gp.run {params}");
                assert_eq!(command_uses_dggs_system("gp.batch", &run), needed, "gp.batch {params}");
                let script = json!({"script": format!("gp.run {run}")});
                assert_eq!(command_uses_dggs_system("script.run", &script), needed, "script {params}");
                let steps = vec![ModelStep { tool: tool.into(), params: params.clone() }];
                assert_eq!(model_capabilities(&steps, None).contains(&"dggs-system"), needed, "model {params}");
            }
            assert!(!command_uses_dggs_system("buffer", &params));
        }
        let fixture = Project::from_json(include_str!("../../../schema/fixtures/dggs_system.rgis")).unwrap();
        assert_eq!(fixture.models[0].steps[0].params["system"], "H3");
        assert_eq!(serde_json::to_value(&fixture).unwrap()["requires"], json!(["dggs-system"]));
    }

    /// A saved command that sets an SVG as a marker's image requires `svg-markers` as well as
    /// `picture-markers`, in every replayable container: a reader of PNG and JPEG markers only
    /// would refuse the command when it replays. A PNG requires `picture-markers` alone, and an
    /// image that is neither is refused by the command anyway (#2266 part E1b).
    #[test]
    fn a_saved_command_setting_an_svg_marker_requires_svg_markers() {
        use base64::Engine;
        let svg = base64::engine::general_purpose::STANDARD.encode(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 8 4"/>"#);
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNgYGD4DwABBAEAwS2OUAAAAABJRU5ErkJggg==";
        let base = serde_json::to_value(Project::default()).unwrap();
        for (image, expected) in [(png.to_string(), vec!["picture-markers"]), (svg, vec!["picture-markers", "svg-markers"])] {
            for (command, params) in [
                ("layer.marker", json!({"layer": "Cities", "image": image})),
                ("layer.lineStrokes", json!({"layer": "Rivers", "strokes": [{"width": 1}, {"markers": {"image": image}}]})),
                ("layer.fillLayers", json!({"layer": "Lakes", "fills": [{"pattern": "dots", "image": image}]})),
            ] {
                let script = format!("# Icons\n{command} {params}");
                for (field, value) in [
                    ("tasks", json!([{"name": "Task", "steps": [{"title": "Icon", "command": command, "params": params}]}])),
                    (
                        "tasks",
                        json!([{"name": "Script", "steps": [{"title": "Icon", "command": "script.run", "params": {"script": script}}]}]),
                    ),
                    ("notebooks", json!([{"name": "Notebook", "cells": [{"kind": "code", "source": script}]}])),
                    (
                        "gp_history",
                        json!([{"tool": "Marker", "params": {}, "command": [command, params], "ok": true, "messages": [], "millis": 0}]),
                    ),
                ] {
                    let mut document = base.clone();
                    document[field] = value;
                    let project = Project::from_json(&document.to_string()).unwrap();
                    assert_eq!(serde_json::to_value(&project).unwrap()["requires"], json!(expected), "{field} {command} {image}");
                }
            }
        }
    }

    /// A `model.run` command given `graph` inline, as a saved task step, a script line in a task, a
    /// notebook cell and a recorded command: what a project saves as `requires` for each (`None`:
    /// no header).
    fn saved_requires_of_inline_graph(run: &Value) -> Vec<Option<Value>> {
        let script = format!("# Run\nmodel.run {run}");
        let base = serde_json::to_value(Project::default()).unwrap();
        [
            ("tasks", json!([{"name": "Task", "steps": [{"title": "Run", "command": "model.run", "params": run}]}])),
            ("tasks", json!([{"name": "Script", "steps": [{"title": "Run", "command": "script.run", "params": {"script": script}}]}])),
            ("notebooks", json!([{"name": "Notebook", "cells": [{"kind": "code", "source": script}]}])),
            ("gp_history", json!([{"tool": "Run", "params": {}, "command": ["model.run", run], "ok": true, "messages": [], "millis": 0}])),
        ]
        .into_iter()
        .map(|(field, value)| {
            let mut document = base.clone();
            document[field] = value;
            let project = Project::from_json(&document.to_string()).unwrap();
            serde_json::to_value(&project).unwrap().get("requires").cloned()
        })
        .collect()
    }

    fn graph_of_one_tool(tool: &str, params: Value) -> Value {
        json!({"graph": {"nodes": [{"id": "n", "kind": "tool", "tool": tool, "params": params}]}})
    }

    /// A saved command that runs a graph given inline requires what its tool nodes would as
    /// saved steps, wherever the command is saved (#2758); one that needs nothing, none.
    #[test]
    fn an_inline_model_graph_requires_what_its_tool_nodes_do() {
        for (capability, tool, needs, needs_not) in [
            ("dggs-system", "a5Grid", json!({"system": "H3"}), json!({"system": "A5"})),
            ("dggs-system", "a5Aggregate", json!({"system": "Hexagon"}), json!({})),
            ("dggs-system", "A5 Grid", json!({"system": "S2"}), json!({"system": null})),
            ("tabulate-intersection-unit", "tabulateIntersection", json!({"areaUnit": "ac"}), json!({"areaUnit": "km2"})),
            (
                "tabulate-intersection-empty-zones",
                "tabulateIntersection",
                json!({"includeEmptyZones": true}),
                json!({"includeEmptyZones": false}),
            ),
            // As for a saved model's graph (`slope_percent_capability_follows_saved_executables`);
            // no tool node runs a raster function, so this is parity, not a reachable run.
            ("slope-percent", "raster.function", json!({"function": "slope", "units": "percent"}), json!({"function": "slope"})),
        ] {
            for (index, saved) in saved_requires_of_inline_graph(&graph_of_one_tool(tool, needs.clone())).into_iter().enumerate() {
                assert_eq!(saved, Some(json!([capability])), "{tool} {needs} in container {index}");
            }
            for (index, saved) in saved_requires_of_inline_graph(&graph_of_one_tool(tool, needs_not.clone())).into_iter().enumerate() {
                assert_eq!(saved, None, "{tool} {needs_not} in container {index}");
            }
        }
    }

    /// A node of an inline graph is checked as a step of a saved model is, by every capability: one
    /// sample for each entry of `COMMAND_CAPABILITIES`, each requiring exactly its own name, so an
    /// entry added later without a sample fails here. The walk takes a node's id and parameters as
    /// they come, as a saved model's does, so every entry is reachable; only the tool-based ones
    /// (`tabulate*`, `dggs-system`) are also a node a run can run, the rest are parity.
    #[test]
    fn an_inline_graph_node_is_checked_as_a_saved_step_is() {
        let ink_layout = {
            let fixture = Project::from_json(include_str!("../../../schema/fixtures/layout_element_ink.rgis")).unwrap();
            serde_json::to_value(&fixture).unwrap()["layouts"][0].to_string()
        };
        let svg = {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD.encode(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 8 4"/>"#)
        };
        let chart = json!({"kind": "chart"});
        let samples: Vec<(&[&str], &str, Value)> = vec![
            (&["slope-percent"], "raster.function", json!({"function": "slope", "units": "percent"})),
            (&["slope-percent"], "raster.functionChain", json!({"steps": [{"function": "slope", "units": "percent"}]})),
            (&["tabulate-intersection-unit"], "tabulateIntersection", json!({"areaUnit": "ac"})),
            (&["tabulate-intersection-empty-zones"], "tabulateIntersection", json!({"includeEmptyZones": true})),
            (&["layout-element-background"], "layout.update", json!({"element": 1, "background": {}})),
            (&["picture-markers"], "layer.marker", json!({"image": "iVBORw0KGgo"})),
            (&["graticule-mgrs"], "map.graticule", json!({"type": "mgrs"})),
            (&["graticule-on-top"], "map.graticule", json!({"onTop": true})),
            (&["graticule-color"], "map.graticule", json!({"color": "#000000"})),
            (&["layout-element-ink"], "layout.import", json!({"text": ink_layout})),
            (&["layout-element-wrap"], "layout.addElement", json!({"kind": "text", "wrap": true})),
            (&["layout-text-block"], "layout.update", json!({"element": 1, "lineSpacing": 1.5})),
            (&["layout-chart-data"], "layout.addElement", json!({"kind": "chart", "group": "Region"})),
            (&["layout-chart-look"], "layout.update", json!({"element": 1, "values": true})),
            (&["layout-chart-classes"], "layout.update", json!({"element": 1, "classes": true})),
            (&["layout-chart-stack"], "layout.update", json!({"element": 1, "stack": "Kind"})),
            (&["layout-chart-facing"], "layout.update", json!({"element": 1, "mirror": true})),
            (&["layout-chart-outline"], "layout.update", json!({"element": 1, "barOutline": {"width": 1}})),
            (&["layout-scale-bar-details"], "layout.update", json!({"element": 1, "subdivisions": 2})),
            (&["blank-paper"], "map.basemap", json!({"name": "blank", "color": "#112233"})),
            (&["layout-frame-border"], "layout.update", json!({"element": 1, "border": {}})),
            (&["layout-text-size"], "layout.update", json!({"element": 1, "textSize": 12})),
            (&["dggs-system"], "a5Grid", json!({"system": "H3"})),
            (&["dggs-system"], "a5Aggregate", json!({"system": "S2"})),
            (&["label-weight"], "labels.placement", json!({"weight": 9})),
            (&["label-clearance"], "labels.placement", json!({"clearance": 9})),
            (&["label-duplicates"], "labels.placement", json!({"removeDuplicates": 20})),
            (&["tile-resampling"], "map.addTiles", json!({"resampling": "nearest"})),
            (&["layout-page-color"], "layout.page", json!({"color": "#0B1B2B"})),
            // An SVG image is a picture marker as well, in a node as in a saved command.
            (&["picture-markers", "svg-markers"], "layer.marker", json!({"image": svg})),
            (&[], "buffer", json!({"input": "Cities", "distance": 1})),
            (&[], "layout.addElement", chart),
        ];
        let mut covered = std::collections::BTreeSet::new();
        for (capability, id, params) in samples {
            let step = [ModelStep { tool: id.into(), params: params.clone() }];
            let names: Vec<&str> =
                COMMAND_CAPABILITIES.iter().filter_map(|&(name, uses)| model_uses(&step, None, uses).then_some(name)).collect();
            assert_eq!(names, capability, "the sample {id} {params} requires exactly its own capability");
            covered.extend(capability.iter().copied());
            for (index, saved) in saved_requires_of_inline_graph(&graph_of_one_tool(id, params.clone())).into_iter().enumerate() {
                // The documents are otherwise empty, so the header names only what the node needs.
                assert_eq!(saved, (!capability.is_empty()).then(|| json!(capability)), "{id} {params} in container {index}");
            }
        }
        let missing: Vec<&str> = COMMAND_CAPABILITIES.iter().map(|&(name, _)| name).filter(|name| !covered.contains(name)).collect();
        assert!(missing.is_empty(), "add a sample for each command capability, missing: {missing:?}");
    }

    /// An inline graph is read as the run reads it: only an object with `nodes`, only nodes of kind
    /// `tool` with an object for `params`; one it can't read it drops, not the rest.
    #[test]
    fn an_inline_graph_is_read_as_the_run_reads_it() {
        let h3 = json!({"system": "H3"});
        let node = |extra: Value| {
            let mut n = json!({"id": "n", "kind": "tool", "tool": "a5Grid", "params": h3});
            for (k, v) in extra.as_object().into_iter().flatten() {
                n[k] = v.clone();
            }
            n
        };
        let none = |run: Value, why: &str| {
            assert!(saved_requires_of_inline_graph(&run).iter().all(Option::is_none), "{why}");
        };
        let needs = |run: Value, why: &str| {
            assert!(saved_requires_of_inline_graph(&run).iter().all(|r| r == &Some(json!(["dggs-system"]))), "{why}");
        };
        let graph = |nodes: Vec<Value>| json!({"graph": {"nodes": nodes}});
        none(json!({"graph": graph(vec![node(json!({}))]).get("graph").map(Value::to_string)}), "a graph as text is refused by the run");
        none(json!({"graph": null, "name": "Saved"}), "no graph: the named model is read as a saved model");
        none(json!({"graph": []}), "not an object");
        none(json!({"graph": {"nodes": "text"}}), "nodes not a list");
        none(graph(vec![node(json!({"kind": null}))]), "no kind");
        none(graph(vec![node(json!({"kind": "input"}))]), "an input node runs no tool");
        none(graph(vec![node(json!({"kind": "output"}))]), "an output node runs no tool");
        none(graph(vec![node(json!({"kind": "Tool"}))]), "kind is lowercase");
        none(graph(vec![node(json!({"tool": 5}))]), "tool not text");
        none(graph(vec![node(json!({"params": h3.to_string()}))]), "params as text are dropped");
        none(graph(vec![node(json!({"params": [h3]}))]), "params as a list are dropped");
        none(graph(vec![node(json!({"params": null}))]), "no params");
        none(graph(vec![node(json!({"tool": "buffer"}))]), "another tool");
        needs(graph(vec![node(json!({}))]), "the plain case");
        needs(graph(vec![json!(5), json!({"id": 3}), json!(null), node(json!({}))]), "the run drops a node it can't read, not the rest");
        needs(graph(vec![node(json!({"tool": "A5 Grid"}))]), "a tool by its former name");
        needs(json!({"graph": {"nodes": [node(json!({}))], "edges": "junk"}}), "edges are not read");
    }

    /// A graph past the run's limits is refused there, so needs nothing; one no bigger is read to
    /// its last node. Nested and cyclic graphs are read one level, without recursion.
    #[test]
    fn a_hostile_inline_graph_is_read_without_panic_or_hang() {
        use crate::model::{MAX_EDGES, MAX_NODES};
        let junk = || json!({"id": "j", "kind": "tool", "tool": "buffer", "params": {}});
        let h3 = || json!({"id": "h", "kind": "tool", "tool": "a5Grid", "params": {"system": "H3"}});
        let nodes_with_last = |count: usize, last: Value| {
            let mut nodes = vec![junk(); count - 1];
            nodes.push(last);
            json!({"graph": {"nodes": nodes}})
        };
        let all = |run: Value, expected: Option<Value>, why: &str| {
            for (index, saved) in saved_requires_of_inline_graph(&run).into_iter().enumerate() {
                assert_eq!(saved, expected, "{why} (container {index})");
            }
        };
        // A notebook cell is cut at `MAX_SOURCE` on load: the task and history containers cover the oversized graphs.
        all(nodes_with_last(MAX_NODES, h3()), Some(json!(["dggs-system"])), "as many nodes as a run takes, the last one needing it");
        all(nodes_with_last(MAX_NODES + 1, h3()), None, "one more is refused");
        all(nodes_with_last(100_000, h3()), None, "far more is refused");
        let mut edges = nodes_with_last(2, h3());
        edges["graph"]["edges"] = json!(vec![json!({"from": "j", "to": "h", "toPort": "input"}); MAX_EDGES + 1]);
        all(edges, None, "more connections than a run takes");
        // A node that is itself a `model.run`, nested to the depth the project's JSON allows (128).
        let mut nested = graph_of_one_tool("a5Grid", json!({"system": "H3"}));
        for _ in 0..24 {
            nested = json!({"graph": {"nodes": [{"id": "m", "kind": "tool", "tool": "model.run", "params": nested}]}});
        }
        all(nested, None, "a node is a tool, not a model run: nothing inside it is read");
        // A node that is a script holding a graph holding a script, as deep as its escaping allows.
        let mut cycle = graph_of_one_tool("a5Grid", json!({"system": "H3"}));
        for _ in 0..12 {
            let script = format!("model.run {cycle}");
            cycle = json!({"graph": {"nodes": [{"id": "s", "kind": "tool", "tool": "script.run", "params": {"script": script}}]}});
        }
        all(cycle, None, "a script node is not read into");
        // A script of as many graph lines as a script takes.
        let line = format!("model.run {}", graph_of_one_tool("buffer", json!({})));
        let script = vec![line; crate::cmd::workflows::MAX_SCRIPT_LINES].join("\n");
        let base = serde_json::to_value(Project::default()).unwrap();
        let mut document = base;
        document["tasks"] = json!([{"name": "T", "steps": [{"title": "Run", "command": "script.run", "params": {"script": script}}]}]);
        let project = Project::from_json(&document.to_string()).unwrap();
        assert!(serde_json::to_value(&project).unwrap().get("requires").is_none());
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
                // An update's decimals may be a scale bar's too, which it can't tell apart.
                let bar_too = id == "layout.update" && params.get("decimals").is_some_and(Value::is_u64);
                let expected = match (required, bar_too) {
                    (true, true) => json!(["layout-chart-look", "layout-scale-bar-details"]),
                    (true, false) => json!(["layout-chart-look"]),
                    _ => Value::Null,
                };
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

    /// A text with a block setting is required where it is saved, so an older reader refuses the
    /// project instead of drawing it at the top, at the usual spacing, as typed, and saving it again
    /// without them; so is a saved command that sets one. The defaults, null and values read as
    /// them need nothing.
    #[test]
    fn text_block_capability_follows_saved_layouts_and_commands() {
        use rustgis_map::layout::ElementKind;
        let fixture = Project::from_json(include_str!("../../../schema/fixtures/layout_text_block.rgis")).unwrap();
        let saved = serde_json::to_value(&fixture).unwrap();
        assert_eq!(saved["requires"], json!(["layout-text-block"]));
        let reread = Project::from_json(&saved.to_string()).unwrap();
        assert!(reread.layouts[0].elements[1].kind.text_uses_block());
        assert!(!reread.layouts[0].elements[2].kind.text_uses_block());
        let mut clean = fixture;
        for e in &mut clean.layouts[0].elements {
            if let ElementKind::Text { text, size, color, bold, .. } = &e.kind {
                e.kind = ElementKind::text(text.clone(), *size, *color, *bold);
            }
        }
        let base = serde_json::to_value(&clean).unwrap();
        assert!(base.get("requires").is_none(), "a plain text needs nothing");
        for key in ["valign", "line_spacing", "\"case\""] {
            assert!(!base["layouts"].to_string().contains(key), "nor writes {key}");
        }
        // A name it doesn't know and a spacing that isn't a number read as the defaults.
        let mut damaged = base["layouts"][0].clone();
        damaged["elements"][1]["kind"]["Text"]["valign"] = json!("sideways");
        damaged["elements"][1]["kind"]["Text"]["line_spacing"] = json!("x");
        for (id, params, required) in [
            ("layout.addElement", json!({"kind": "text", "valign": "middle"}), true),
            ("layout.addElement", json!({"kind": "dynamicText", "case": "upper"}), true),
            ("layout.update", json!({"element": 2, "lineSpacing": 1.5}), true),
            ("layout.update", json!({"element": 2, "lineSpacing": "0.75"}), true),
            ("layout.update", json!({"element": 2, "valign": "top", "lineSpacing": 1, "case": "none"}), false),
            ("layout.update", json!({"element": 2, "valign": " Top ", "lineSpacing": "1", "case": "NONE"}), false),
            ("layout.update", json!({"element": 2, "lineSpacing": 1.0}), false),
            ("layout.update", json!({"element": 2, "valign": null, "lineSpacing": null, "case": null}), false),
            ("layout.update", json!({"element": 2, "wrap": false}), false),
            ("layout.import", json!({"text": saved["layouts"][0].to_string()}), true),
            ("layout.import", json!({"text": base["layouts"][0].to_string()}), false),
            ("layout.import", json!({"text": damaged.to_string()}), false),
            ("layout.import", json!({"path": "texts.rgislayout"}), false),
        ] {
            let script = format!("{id}= {params}");
            for value in [
                json!([{"name": "Task", "steps": [{"title": "Text", "command": id, "params": params}]}]),
                json!([{"name": "Script", "steps": [{"title": "Text", "command": "script.run", "params": {"script": script}}]}]),
            ] {
                let mut document = base.clone();
                document["tasks"] = value;
                let saved = serde_json::to_value(Project::from_json(&document.to_string()).unwrap()).unwrap();
                let requires = saved.get("requires").and_then(Value::as_array).cloned().unwrap_or_default();
                assert_eq!(requires.contains(&json!("layout-text-block")), required, "{id} {params}");
                assert!(requires.iter().all(|r| r == "layout-text-block"), "{id} {params}: {requires:?}");
            }
        }
    }

    /// A scale bar with details is required where it is saved, so an older reader refuses the project
    /// instead of drawing a plain bar and saving it again without them; so is a saved command that
    /// sets one. The defaults, or a unit label taken back to its abbreviation, need nothing.
    #[test]
    fn scale_bar_details_capability_follows_saved_layouts_and_commands() {
        use rustgis_map::layout::ElementKind;
        let fixture = Project::from_json(include_str!("../../../schema/fixtures/layout_scale_bar_details.rgis")).unwrap();
        let saved = serde_json::to_value(&fixture).unwrap();
        // Its sized bar's text size is the text size capability's, as for any scale bar.
        assert_eq!(saved["requires"], json!(["layout-text-size", "layout-scale-bar-details"]));
        let reread = Project::from_json(&saved.to_string()).unwrap();
        assert!(reread.layouts[0].elements[1].kind.scale_bar_uses_details());
        assert!(!reread.layouts[0].elements[2].kind.scale_bar_uses_details());
        let mut clean = fixture;
        for e in &mut clean.layouts[0].elements {
            if let ElementKind::ScaleBar { frame, units, divisions, style, color, fill, .. } = &e.kind {
                let mut plain = ElementKind::scale_bar(*frame);
                if let ElementKind::ScaleBar { units: u, divisions: d, style: st, color: c, fill: f, .. } = &mut plain {
                    (*u, *d, *st, *c, *f) = (*units, *divisions, *style, *color, *fill);
                }
                e.kind = plain;
            }
        }
        let base = serde_json::to_value(&clean).unwrap();
        assert!(base.get("requires").is_none(), "a plain bar needs nothing");
        for key in [
            "subdivisions",
            "divisions_before_zero",
            "label_position",
            "unit_label",
            "unit_label_position",
            "decimals",
            "text_size",
            "\"fit\"",
        ] {
            assert!(!base["layouts"].to_string().contains(key), "nor writes {key}");
        }
        let layout = &saved["layouts"][0];
        let mut position_only = base["layouts"][0].clone();
        position_only["elements"][1]["kind"]["ScaleBar"]["unit_label_position"] = json!("afterBar");
        // One subdivision clamps to none, so a text whose only detail it is needs nothing.
        let mut one_subdivision = base["layouts"][0].clone();
        one_subdivision["elements"][1]["kind"]["ScaleBar"]["subdivisions"] = json!(1);
        for (id, params, required) in [
            ("layout.addElement", json!({"kind": "scaleBar", "subdivisions": 4}), true),
            ("layout.update", json!({"element": 2, "labelPosition": "below"}), true),
            ("layout.update", json!({"element": 2, "unitLabel": ""}), true),
            ("layout.update", json!({"element": 2, "fit": "adjustWidth"}), true),
            // A text size is no detail: the text size capability covers it.
            ("layout.update", json!({"element": 2, "textSize": 9}), false),
            ("layout.addElement", json!({"kind": "scaleBar", "textSize": 12}), false),
            ("layout.update", json!({"element": 2, "unitLabel": null}), false),
            // Fixed decimals, none too, are a detail; "auto" isn't. An update's may be a chart's,
            // which it can't tell apart; an addElement's for a chart is the chart's alone.
            ("layout.addElement", json!({"kind": "scaleBar", "decimals": 0}), true),
            ("layout.addElement", json!({"kind": "scaleBar", "decimals": "auto"}), false),
            ("layout.addElement", json!({"kind": "chart", "decimals": 2}), false),
            ("layout.update", json!({"element": 2, "decimals": 2}), true),
            ("layout.update", json!({"element": 2, "decimals": "2"}), true),
            ("layout.update", json!({"element": 2, "decimals": 2.0}), true),
            ("layout.update", json!({"element": 2, "decimals": "auto"}), false),
            (
                "layout.update",
                json!({"element": 2, "subdivisions": 1, "divisionsBeforeZero": 0, "labelPosition": "above", "unitLabelPosition": "afterLabels", "decimals": "auto", "textSize": null, "fit": "adjustDivisionValue"}),
                false,
            ),
            ("layout.update", json!({"element": 2, "divisions": 3}), false),
            ("layout.import", json!({"text": layout.to_string()}), true),
            ("layout.import", json!({"text": base["layouts"][0].to_string()}), false),
            ("layout.import", json!({"text": position_only.to_string()}), true),
            ("layout.import", json!({"text": one_subdivision.to_string()}), false),
            ("layout.import", json!({"path": "bars.rgislayout"}), false),
        ] {
            let script = format!("{id}= {params}");
            for value in [
                json!([{"name": "Task", "steps": [{"title": "Bar", "command": id, "params": params}]}]),
                json!([{"name": "Script", "steps": [{"title": "Bar", "command": "script.run", "params": {"script": script}}]}]),
            ] {
                let mut document = base.clone();
                document["tasks"] = value;
                let saved = serde_json::to_value(Project::from_json(&document.to_string()).unwrap()).unwrap();
                // Decimals a chart takes too may require its capability as well, and a text size
                // its own.
                let requires = saved.get("requires").and_then(Value::as_array).cloned().unwrap_or_default();
                assert_eq!(requires.contains(&json!("layout-scale-bar-details")), required, "{id} {params}");
                assert!(
                    requires
                        .iter()
                        .all(|r| ["layout-scale-bar-details", "layout-chart-look", "layout-text-size"].contains(&r.as_str().unwrap_or(""))),
                    "{id} {params}"
                );
            }
        }
    }

    /// A chart that faces another (#2487 part 4: mirrored, a fixed maximum, labels hidden) is
    /// required where it is saved, so an older reader refuses the project instead of drawing it
    /// the usual way round, on its own scale, labelled; so is a saved command that sets one. Each
    /// at its default (`false`, `null`, `"auto"`) needs nothing, nor is written.
    #[test]
    fn chart_facing_capability_follows_saved_layouts_and_commands() {
        use rustgis_map::layout::ElementKind;
        let fixture = Project::from_json(include_str!("../../../schema/fixtures/layout_chart_facing.rgis")).unwrap();
        let saved = serde_json::to_value(&fixture).unwrap();
        assert_eq!(saved["requires"], json!(["layout-chart-facing"]));
        let mut clean = fixture;
        for e in &mut clean.layouts[0].elements {
            if let ElementKind::Chart { mirror, max, hide_labels, .. } = &mut e.kind {
                (*mirror, *max, *hide_labels) = (false, None, false);
            }
        }
        let base = serde_json::to_value(&clean).unwrap();
        assert!(base.get("requires").is_none(), "a chart drawn the usual way needs nothing");
        for key in ["\"mirror\"", "\"max\"", "\"hide_labels\""] {
            assert!(!base["layouts"].to_string().contains(key), "nor writes {key}");
        }
        // Each one alone requires it.
        for key in ["mirror", "max", "hide_labels"] {
            let mut one = base.clone();
            one["layouts"][0]["elements"][1]["kind"]["Chart"][key] = if key == "max" { json!(100.0) } else { json!(true) };
            let saved = serde_json::to_value(Project::from_json(&one.to_string()).unwrap()).unwrap();
            assert_eq!(saved["requires"], json!(["layout-chart-facing"]), "{key}");
        }
        // A hand-edited maximum not above 0 is let go on read (and said), so it is neither written
        // back nor required.
        let mut kept = base.clone();
        kept["layouts"][0]["elements"][1]["kind"]["Chart"]["max"] = json!(-5.0);
        let saved_kept = serde_json::to_value(Project::from_json(&kept.to_string()).unwrap()).unwrap();
        assert!(saved_kept.get("requires").is_none());
        assert!(saved_kept["layouts"][0]["elements"][1]["kind"]["Chart"].get("max").is_none());
        let mut document = base.clone();
        document["tasks"] = json!([{"name": "Task", "steps": [{"title": "Chart", "command": "layout.import", "params": {"text": saved["layouts"][0].to_string()}}]}]);
        let imported = serde_json::to_value(Project::from_json(&document.to_string()).unwrap()).unwrap();
        assert_eq!(imported["requires"], json!(["layout-chart-facing"]));
        for (id, params, required) in [
            ("layout.addElement", json!({"kind": "chart", "mirror": true}), true),
            ("layout.addElement", json!({"kind": "chart", "max": 100}), true),
            ("layout.addElement", json!({"kind": "chart", "hideLabels": true}), true),
            ("layout.addElement", json!({"kind": "chart", "mirror": false, "max": null, "hideLabels": false}), false),
            ("layout.addElement", json!({"kind": "chart", "max": "auto"}), false),
            ("layout.addElement", json!({"kind": "legend", "mirror": true}), false),
            ("layout.update", json!({"element": 2, "mirror": true}), true),
            ("layout.update", json!({"element": 2, "max": 50}), true),
            ("layout.update", json!({"element": 2, "hideLabels": true}), true),
            ("layout.update", json!({"element": 2, "mirror": false, "max": null, "hideLabels": false}), false),
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
                let expected = if required { json!(["layout-chart-facing"]) } else { Value::Null };
                assert_eq!(saved.get("requires").cloned().unwrap_or(Value::Null), expected, "{id} {params}");
            }
        }
    }

    /// A line round a chart's bars (#2487 part 4b) requires `layout-chart-outline`, in a saved
    /// layout and in the commands that give it one; none, null or a width of 0 need nothing.
    #[test]
    fn chart_outline_capability_follows_saved_layouts_and_commands() {
        use rustgis_map::layout::ElementKind;
        let fixture = Project::from_json(include_str!("../../../schema/fixtures/layout_chart_outline.rgis")).unwrap();
        let saved = serde_json::to_value(&fixture).unwrap();
        assert_eq!(saved["requires"], json!(["layout-chart-outline"]));
        let mut clean = fixture;
        for e in &mut clean.layouts[0].elements {
            if let ElementKind::Chart { bar_outline, .. } = &mut e.kind {
                *bar_outline = None;
            }
        }
        let base = serde_json::to_value(&clean).unwrap();
        assert!(base.get("requires").is_none(), "a chart without an outline needs nothing");
        assert!(!base["layouts"].to_string().contains("\"bar_outline\""), "nor writes it");
        // One of no width, or below 0 (a hand-edited file), is let go on read: it needs nothing
        // and isn't written back.
        for width in [0.0, -1.0] {
            let mut zero = base.clone();
            zero["layouts"][0]["elements"][1]["kind"]["Chart"]["bar_outline"] = json!({"color": [0, 0, 0, 255], "width": width});
            let saved_zero = serde_json::to_value(Project::from_json(&zero.to_string()).unwrap()).unwrap();
            assert!(saved_zero.get("requires").is_none(), "{width}");
            assert!(!saved_zero["layouts"].to_string().contains("\"bar_outline\""), "{width}");
        }
        let mut document = base.clone();
        document["tasks"] = json!([{"name": "Task", "steps": [{"title": "Chart", "command": "layout.import", "params": {"text": saved["layouts"][0].to_string()}}]}]);
        let imported = serde_json::to_value(Project::from_json(&document.to_string()).unwrap()).unwrap();
        assert_eq!(imported["requires"], json!(["layout-chart-outline"]));
        for (id, params, required) in [
            ("layout.addElement", json!({"kind": "chart", "barOutline": {}}), true),
            ("layout.addElement", json!({"kind": "chart", "barOutline": {"color": "#000000", "width": 1}}), true),
            ("layout.addElement", json!({"kind": "chart", "barOutline": {"width": 0}}), false),
            ("layout.addElement", json!({"kind": "chart", "barOutline": null}), false),
            ("layout.addElement", json!({"kind": "legend", "barOutline": {}}), false),
            ("layout.update", json!({"element": 2, "barOutline": {"color": "#C2185B"}}), true),
            ("layout.update", json!({"element": 2, "barOutline": {"width": 0}}), false),
            ("layout.update", json!({"element": 2, "barOutline": {"width": "2"}}), true),
            ("layout.update", json!({"element": 2, "barOutline": {"width": "0"}}), false),
            ("layout.update", json!({"element": 2, "barOutline": null}), false),
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
                let expected = if required { json!(["layout-chart-outline"]) } else { Value::Null };
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
            returned: Vec::new(),
            ids: Default::default(),
            conflicts: Default::default(),
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

    /// The versions an offline area's conflicts kept are saved again as they were read, one of each
    /// kind, and require their own capability, which goes once none is left.
    #[test]
    fn offline_area_conflicts_are_preserved_and_required() {
        use crate::project::ConflictKind;
        let project = Project::from_json(include_str!("../../../schema/fixtures/field_conflicts.rgis")).unwrap();
        let kept = &project.offline_areas[0].conflicts;
        let kinds: Vec<ConflictKind> = kept.iter().map(|c| c.kind).collect();
        assert_eq!(kinds, [ConflictKind::Replaced, ConflictKind::Deleted, ConflictKind::Kept, ConflictKind::Dropped]);
        assert_eq!(kept[0].values[1], ("pop_max".to_string(), rustgis_data::Value::Int(1200)));
        assert_eq!(kept[0].geometry, Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(12.5, 41.9))));
        // A shape that lost with none: the feature had no shape.
        assert!(kept[1].shape && kept[1].geometry.is_none());
        assert!(!kept[3].shape && kept[3].package == "a1b2c3d4e5f60718293a4b5c6d7e8f90");
        let saved = serde_json::to_value(&project).unwrap();
        assert!(saved["requires"].as_array().unwrap().contains(&json!("field-conflicts")), "{}", saved["requires"]);
        assert_eq!(Project::from_json(&saved.to_string()).unwrap().offline_areas[0].conflicts, *kept);
        // An Undo step's copy of the project shares each one, and so does a list changed after it.
        let mut copy = project.clone();
        copy.offline_areas[0].conflicts.remove(0);
        assert!(std::sync::Arc::ptr_eq(&copy.offline_areas[0].conflicts[0], &project.offline_areas[0].conflicts[1]));
        let mut none = project.clone();
        none.offline_areas[0].conflicts = Default::default();
        let saved = serde_json::to_value(&none).unwrap();
        assert!(!saved["requires"].as_array().unwrap().contains(&json!("field-conflicts")), "{}", saved["requires"]);
        // A kind this version doesn't know is refused, not read as another.
        let mut strange = serde_json::to_value(&project).unwrap();
        strange["offline_areas"][0]["conflicts"][0]["kind"] = json!("merged");
        assert!(Project::from_json(&strange.to_string()).is_err());
    }

    /// A package's identity, and an area's list of packages brought back, are kept on saving and
    /// require their own capability; an invalid one is refused.
    #[test]
    fn field_package_ids_are_preserved_and_required_and_must_be_identities() {
        let project = Project::from_json(include_str!("../../../schema/fixtures/field_package.rgis")).unwrap();
        let saved = serde_json::to_value(&project).unwrap();
        assert_eq!(saved["requires"], json!(["field-origin", "field-generation", "field-package"]));
        let reopened = Project::from_json(&saved.to_string()).unwrap();
        assert_eq!(reopened.field_origin.as_ref().map(|o| o.package.as_str()), Some("a1b2c3d4e5f60718293a4b5c6d7e8f90"));
        assert_eq!(reopened.field_origin.as_ref().map(|o| o.ids.clone()), Some([(1, 120)].into()));
        let mut invalid = saved.clone();
        invalid["field_origin"]["package"] = json!("not-an-id");
        assert!(Project::from_json(&invalid.to_string()).unwrap_err().contains("identity"));
        // Hostile highest ids, whose next id no GeoPackage or PostGIS table could hold, are refused
        // too, from the largest such a feature can have (`i64::MAX`) up; the one below it is kept.
        for bad in [json!(u64::MAX), json!(u64::MAX - 1), json!(i64::MAX.unsigned_abs() + 1), json!(i64::MAX.unsigned_abs())] {
            let mut invalid = saved.clone();
            invalid["field_origin"]["ids"] = json!({"1": bad});
            assert!(Project::from_json(&invalid.to_string()).unwrap_err().contains("identity"), "{bad}");
        }
        // Floors on their own, with no package identity, require the capability too, and are kept.
        let mut floors = Project::from_json(include_str!("../../../schema/fixtures/field_origin.rgis")).unwrap();
        floors.field_origin.as_mut().unwrap().ids = [(1, 7)].into();
        let kept = serde_json::to_value(&floors).unwrap();
        assert!(kept["requires"].as_array().unwrap().contains(&json!("field-package")), "{}", kept["requires"]);
        assert_eq!(Project::from_json(&kept.to_string()).unwrap().field_origin.unwrap().ids, floors.field_origin.as_ref().unwrap().ids);
        let mut largest = saved.clone();
        largest["field_origin"]["ids"] = json!({"1": i64::MAX.unsigned_abs() - 1});
        assert!(Project::from_json(&largest.to_string()).is_ok());
        let mut with_area = Project::from_json(include_str!("../../../schema/fixtures/field_origin.rgis")).unwrap();
        with_area.offline_areas.push(super::super::OfflineArea {
            id: "0123456789abcdef0123456789abcdef".into(),
            name: "Field".into(),
            map: "Map".into(),
            extent: rustgis_geom::Extent::new(0.0, 0.0, 1.0, 1.0),
            created: 0.0,
            synced: 0.0,
            generation: String::new(),
            returned: vec!["a1b2c3d4e5f60718293a4b5c6d7e8f90".into()],
            ids: Default::default(),
            conflicts: Default::default(),
            data: with_area.map().clone(),
        });
        let saved = serde_json::to_value(&with_area).unwrap();
        assert_eq!(saved["requires"], json!(["field-origin", "field-package"]));
        assert_eq!(Project::from_json(&saved.to_string()).unwrap().offline_areas[0].returned, with_area.offline_areas[0].returned);
        // The ids an area's packages gave require it too, on their own.
        let mut only_ids = with_area.clone();
        only_ids.offline_areas[0].returned.clear();
        only_ids.offline_areas[0].ids = [(1, 120)].into();
        assert_eq!(serde_json::to_value(&only_ids).unwrap()["requires"], json!(["field-origin", "field-package"]));
        // The same bound for an area's: refused from `i64::MAX` up, the one below it kept.
        for bad in [json!(u64::MAX), json!(i64::MAX.unsigned_abs())] {
            let mut hostile = saved.clone();
            hostile["offline_areas"][0]["ids"] = json!({"1": bad});
            assert!(Project::from_json(&hostile.to_string()).unwrap_err().contains("identity"), "{bad}");
        }
        let mut largest = saved.clone();
        largest["offline_areas"][0]["ids"] = json!({"1": i64::MAX.unsigned_abs() - 1});
        assert!(Project::from_json(&largest.to_string()).is_ok());
        let mut invalid = saved;
        invalid["offline_areas"][0]["returned"] = json!(["nope"]);
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
            returned: Vec::new(),
            ids: Default::default(),
            conflicts: Default::default(),
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
            returned: Vec::new(),
            ids: Default::default(),
            conflicts: Default::default(),
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

    /// A project's coordinate system and its first-layer option (#2032 C3a) are required by name:
    /// an older reader would save the project again without them. A project with neither needs
    /// nothing, and one listing a capability this release lacks is still refused by name.
    #[test]
    fn a_project_coordinate_system_is_required_where_it_is_saved() {
        let text = include_str!("../../../schema/fixtures/project_crs.rgis");
        let mut p = Project::from_json(text).unwrap();
        assert_eq!(p.coordinate_system, Some(rustgis_data::raster::Crs::Epsg(2056)));
        assert!(p.crs_from_first_layer);
        let saved = serde_json::to_value(&p).unwrap();
        assert_eq!(saved["requires"], json!(["project-crs"]));
        assert_eq!(saved["coordinate_system"], json!({"Epsg": 2056}));
        p.coordinate_system = None;
        p.crs_from_first_layer = false;
        let clean = serde_json::to_value(&p).unwrap();
        assert!(clean.get("requires").is_none() && clean.get("coordinate_system").is_none() && clean.get("crs_from_first_layer").is_none());
        let mut newer: Value = serde_json::from_str(text).unwrap();
        newer["requires"] = json!(["project-crs", "future-crs"]);
        assert!(Project::from_json(&newer.to_string()).unwrap_err().contains("future-crs"));
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

    /// A layout's page colour (#2855) is required where it is saved, so an older reader refuses
    /// the project instead of opening the page white and saving it so. A white page is not
    /// written and needs nothing; neither do a saved layout.page that gives white or null.
    #[test]
    fn page_color_capability_follows_saved_layouts_and_commands() {
        let fixture = Project::from_json(include_str!("../../../schema/fixtures/layout_page_color.rgis")).unwrap();
        let saved = serde_json::to_value(&fixture).unwrap();
        assert_eq!(saved["requires"], json!(["layout-page-color"]));
        assert_eq!(saved["layouts"][0]["paper"], json!([11, 27, 43, 255]));
        let mut clean = fixture;
        clean.layouts[0].paper = None;
        let base = serde_json::to_value(&clean).unwrap();
        assert!(base.get("requires").is_none() && base["layouts"][0].get("paper").is_none(), "a white page needs nothing");
        for (params, required) in [
            (json!({"color": "#0B1B2B"}), true),
            (json!({"color": " #0b1b2b "}), true),
            (json!({"color": "#FFFFFF"}), false),
            (json!({"color": "#ffffff"}), false),
            (json!({"color": null}), false),
            (json!({"orientation": "portrait"}), false),
        ] {
            let script = format!("layout.page {params}");
            for value in [
                json!([{"name": "Task", "steps": [{"title": "Page", "command": "layout.page", "params": params}]}]),
                json!([{"name": "Script", "steps": [{"title": "Page", "command": "script.run", "params": {"script": script}}]}]),
            ] {
                let mut document = base.clone();
                document["tasks"] = value;
                let saved = serde_json::to_value(Project::from_json(&document.to_string()).unwrap()).unwrap();
                let expected = if required { json!(["layout-page-color"]) } else { Value::Null };
                assert_eq!(saved.get("requires").cloned().unwrap_or(Value::Null), expected, "{params}");
            }
        }
        // An imported layout's text with a page colour needs it; a white one, none.
        for (layout, required) in [(&saved["layouts"][0], true), (&base["layouts"][0], false)] {
            let mut document = base.clone();
            document["tasks"] = json!([{"name": "Task", "steps": [{"title": "Import", "command": "layout.import", "params": {"text": layout.to_string()}}]}]);
            let saved = serde_json::to_value(Project::from_json(&document.to_string()).unwrap()).unwrap();
            assert_eq!(saved.get("requires").is_some(), required, "{layout}");
        }
    }

    /// A legend's, scale bar's or grid's text size other than its default (#2581) is required
    /// where it is saved, so an older reader refuses the project instead of drawing it at the
    /// default and saving it so. Default sizes are not written and need nothing.
    #[test]
    fn text_size_capability_follows_saved_layouts_and_commands() {
        use rustgis_map::layout::{ElementKind, GRID_LABEL_PT, LEGEND_TEXT_PT, SCALE_BAR_TEXT_PT};
        let legends = Project::from_json(include_str!("../../../schema/fixtures/layout_legend_text.rgis")).unwrap();
        let bars = Project::from_json(include_str!("../../../schema/fixtures/layout_scale_grid_text.rgis")).unwrap();
        for fixture in [&legends, &bars] {
            let saved = serde_json::to_value(fixture).unwrap();
            assert_eq!(saved["requires"], json!(["layout-text-size"]));
            assert_eq!(Project::from_json(&saved.to_string()).unwrap().layouts, fixture.layouts);
        }
        let elements = &serde_json::to_value(&legends).unwrap()["layouts"][0]["elements"];
        assert!(elements[1]["kind"]["Legend"].get("text_size").is_none());
        assert_eq!(elements[2]["kind"]["Legend"]["text_size"], json!(18.0));
        let elements = &serde_json::to_value(&bars).unwrap()["layouts"][0]["elements"];
        assert_eq!(elements[0]["kind"]["MapFrame"]["grid"]["label_size"], json!(14.0));
        assert!(elements[1]["kind"]["ScaleBar"].get("text_size").is_none());
        assert_eq!(elements[2]["kind"]["ScaleBar"]["text_size"], json!(16.0));
        // Each size alone needs it; all at their defaults, nothing.
        let sized = |grid: bool, bar: bool| {
            let mut p = bars.clone();
            for e in &mut p.layouts[0].elements {
                match &mut e.kind {
                    ElementKind::MapFrame { grid: Some(g), .. } if !grid => g.label_size = GRID_LABEL_PT,
                    ElementKind::ScaleBar { text_size, .. } if !bar => *text_size = SCALE_BAR_TEXT_PT,
                    _ => {}
                }
            }
            serde_json::to_value(&p).unwrap()
        };
        assert_eq!(sized(true, false)["requires"], json!(["layout-text-size"]), "a grid's size alone");
        assert_eq!(sized(false, true)["requires"], json!(["layout-text-size"]), "a scale bar's size alone");
        let mut clean = legends;
        for e in &mut clean.layouts[0].elements {
            if let ElementKind::Legend { text_size, .. } = &mut e.kind {
                *text_size = LEGEND_TEXT_PT;
            }
        }
        assert!(serde_json::to_value(&clean).unwrap().get("requires").is_none(), "a legend at its default");
        let base = sized(false, false);
        assert!(base.get("requires").is_none(), "default sizes need nothing");
        // A saved command that sizes one needs it; null or the element's default only give the
        // default. An update isn't told which element it sizes: any textSize but null needs it.
        for (id, params, required) in [
            ("layout.update", json!({"element": 2, "textSize": 18}), true),
            ("layout.update", json!({"element": 2, "textSize": 9}), true),
            ("layout.update", json!({"element": 3, "textSize": 8}), true),
            ("layout.update", json!({"element": 2, "textSize": null}), false),
            ("layout.update", json!({"element": 2, "title": "Key"}), false),
            ("layout.update", json!({"element": 1, "grid": {"labelSize": 12}}), true),
            ("layout.update", json!({"element": 1, "grid": {"labelSize": 8}}), false),
            ("layout.update", json!({"element": 1, "grid": {"labelSize": null}}), false),
            ("layout.update", json!({"element": 1, "grid": false}), false),
            ("layout.addElement", json!({"kind": "legend", "textSize": 6}), true),
            ("layout.addElement", json!({"kind": "legend", "textSize": 9}), false),
            ("layout.addElement", json!({"kind": "legend", "textSize": 8}), true),
            ("layout.addElement", json!({"kind": "legend"}), false),
            ("layout.addElement", json!({"kind": "scaleBar", "textSize": 12}), true),
            ("layout.addElement", json!({"kind": "scaleBar", "textSize": 8}), false),
            ("layout.addElement", json!({"kind": "scaleBar"}), false),
            ("layout.addElement", json!({"kind": "grid", "labelSize": 12}), true),
            ("layout.addElement", json!({"kind": "grid", "labelSize": null}), false),
            ("layout.addElement", json!({"kind": "text", "text": "textSize", "size": 18}), false),
        ] {
            let script = format!("{id} {params}");
            for value in [
                json!([{"name": "Task", "steps": [{"title": "Size", "command": id, "params": params}]}]),
                json!([{"name": "Script", "steps": [{"title": "Size", "command": "script.run", "params": {"script": script}}]}]),
            ] {
                let mut document = base.clone();
                document["tasks"] = value;
                let saved = serde_json::to_value(Project::from_json(&document.to_string()).unwrap()).unwrap();
                let expected = if required { json!(["layout-text-size"]) } else { Value::Null };
                assert_eq!(saved["requires"], expected, "{id} {params}");
            }
        }
        // An imported layout's text with a sized element needs it; with the defaults, none.
        let legend_layout = serde_json::to_value(
            Project::from_json(include_str!("../../../schema/fixtures/layout_legend_text.rgis")).unwrap(),
        )
        .unwrap()["layouts"][0]
            .clone();
        for (layout, required) in [
            (legend_layout, true),
            (sized(true, false)["layouts"][0].clone(), true),
            (sized(false, true)["layouts"][0].clone(), true),
            (base["layouts"][0].clone(), false),
        ] {
            let params = json!({"text": layout.to_string()});
            let mut document = base.clone();
            document["tasks"] = json!([{"name": "Task", "steps": [{"title": "Import", "command": "layout.import", "params": params}]}]);
            let saved = serde_json::to_value(Project::from_json(&document.to_string()).unwrap()).unwrap();
            assert_eq!(saved.get("requires").is_some(), required, "{layout}");
        }
        let mut unknown = serde_json::to_value(&bars).unwrap();
        unknown["requires"] = json!(["layout-text-size", "future-legend"]);
        assert!(Project::from_json(&unknown.to_string()).unwrap_err().contains("future-legend"));
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
