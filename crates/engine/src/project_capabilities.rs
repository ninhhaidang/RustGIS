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
        required.extend(COMMAND_CAPABILITIES.iter().filter_map(|&(name, uses)| self.uses_commands(uses).then_some(name)));
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
    ("layout-element-background", command_uses_element_background),
];

/// A layout command that gives an element a background: an older reader warns that it ignores
/// the key and makes the element without one. `null` only takes a background away, which an
/// older reader's elements never have.
fn command_uses_element_background(id: &str, params: &Value) -> bool {
    match id {
        "layout.addElement" | "layout.update" => params.get("background").is_some_and(|b| !b.is_null()),
        "script.run" => params.get("script").and_then(Value::as_str).is_some_and(|s| script_uses(s, command_uses_element_background)),
        _ => false,
    }
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

/// Inspect known command envelopes; unrelated data containing an areaUnit is not executable.
/// Blank unit choices use the km² default and need no capability.
fn command_uses_tabulate_unit(id: &str, params: &Value) -> bool {
    let non_default = |p: &Value| {
        p.get("areaUnit").and_then(Value::as_str).map(str::trim).is_some_and(|u| !u.is_empty() && !u.eq_ignore_ascii_case("km2"))
    };
    if crate::cmd::gp::tool_id(id) == Some("tabulateIntersection") {
        return non_default(params);
    }
    let tabulate = params.get("tool").and_then(Value::as_str).and_then(crate::cmd::gp::tool_id) == Some("tabulateIntersection");
    match id {
        "gp.run" if tabulate => crate::parameters::params_envelope(params.get("params")).is_ok_and(|p| non_default(&p)),
        // Batch ignores non-object shared params; binding areaUnit itself is refused by older tools.
        "gp.batch" if tabulate => params.get("params").filter(|p| p.is_object()).is_some_and(non_default),
        "script.run" => params.get("script").and_then(Value::as_str).is_some_and(|s| script_uses(s, command_uses_tabulate_unit)),
        _ => false,
    }
}

/// Use the executor's line grammar; comments/invalid lines are not executable commands, and
/// script_line refuses nested scripts, so this check cannot recurse through script.run.
fn script_uses(script: &str, uses: CommandCheck) -> bool {
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

/// The renderer capabilities a layer needs, in [`map_capabilities`]'s order.
fn layer_capabilities(layer: &Layer) -> Vec<&'static str> {
    [(relief_effects(layer), "shaded-relief-effects"), (relief_light(layer), "shaded-relief-light")]
        .into_iter()
        .filter_map(|(used, name)| used.then_some(name))
        .collect()
}

fn map_capabilities<'a>(maps: impl IntoIterator<Item = &'a Map>) -> Vec<&'static str> {
    let (mut block, mut water, mut relief, mut light, mut legends) = (false, false, false, false, false);
    for map in maps {
        if let Some(t) = &map.view.terrain {
            block |= t.block.is_some();
            water |= t.water.is_some();
        }
        relief |= map.layers.iter().any(relief_effects);
        light |= map.layers.iter().any(relief_light);
        legends |= map.tiles.iter().any(|tile| {
            tile.legends.iter().any(|legend| rustgis_data::login_address::inner(&legend.url).len() > rustgis_map::tiles::MAX_LEGEND_URL)
        });
    }
    [
        (block, "terrain-block"),
        (water, "terrain-water"),
        (relief, "shaded-relief-effects"),
        (light, "shaded-relief-light"),
        (legends, "normalized-legend-urls"),
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
    serde_json::to_vec(&Item { format: FORMAT, requires: map_capabilities([map]), value: map }).map_err(|e| e.to_string())
}

/// A layer file keeps the same renderer capabilities as the layer saved inside a project.
pub(crate) fn layer_file_bytes(layer: &Layer) -> Result<Vec<u8>, String> {
    serde_json::to_vec(&Item { format: FORMAT, requires: layer_capabilities(layer), value: layer }).map_err(|e| e.to_string())
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

    fn json_bytes(bytes: &[u8]) -> Value {
        serde_json::from_slice(bytes).unwrap()
    }

    /// All replayable project containers require the unit only when it differs from km².
    /// Selection of executable notebook cells follows the notebook runner's language boundary.
    #[test]
    fn tabulate_unit_capability_follows_saved_executables() {
        let mut fixture = Project::from_json(include_str!("../../../schema/fixtures/tabulate_intersection_unit.rgis")).unwrap();
        assert_eq!(fixture.models[0].steps[0].params["areaUnit"], "ac");
        assert_eq!(serde_json::to_value(&fixture).unwrap()["requires"], json!(["tabulate-intersection-unit"]));
        fixture.models.clear();
        let base = serde_json::to_value(&fixture).unwrap();
        for unit in [None, Some(""), Some(" \t\n "), Some("km2"), Some(" KM2 "), Some("ac")] {
            let mut params = json!({"zones":"Zones", "classes":"Classes"});
            if let Some(unit) = unit {
                params["areaUnit"] = json!(unit);
            }
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
                if unit == Some("ac") {
                    assert_eq!(saved["requires"], json!(["tabulate-intersection-unit"]), "{field}");
                    assert_eq!(saved["format"], 2);
                } else {
                    assert!(saved.get("requires").is_none(), "{field} {unit:?}");
                    assert_eq!(saved["format"], FORMAT);
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
            data: p.map().clone(),
        });
        p.maps[0].view.terrain = None;
        assert_eq!(serde_json::to_value(&p).unwrap()["requires"], json!(["terrain-water"]));
        p.offline_areas.clear();
        let clean = serde_json::to_value(&p).unwrap();
        assert!(clean.get("requires").is_none());
        assert!(json_bytes(&map_file_bytes(p.map()).unwrap()).get("requires").is_none());
        assert!(Project::from_json(include_str!("../../../schema/fixtures/terrain_water_v2.rgis")).is_ok());
        let mut unknown = saved;
        unknown["requires"] = json!(["future-terrain"]);
        assert!(Project::from_json(&unknown.to_string()).unwrap_err().contains("future-terrain"));
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
