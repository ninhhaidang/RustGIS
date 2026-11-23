//! The tools the model calls, and what they do to the session.
//!
//! Every RustGIS action is a command, so a handful of tools reach all of them: the model finds a
//! command or geoprocessing tool by searching, then runs it by id. Runs go through
//! [`Session::run`], so each is one undo step and is restored if it fails. Commands that reach
//! beyond the project (files, the network, the user's own automation) need the user's approval
//! first ([`gate`]).

use rustgis_engine::policy::{Call, Outcome, Permit, Scope};
use rustgis_engine::{Session, analysis, project};
use serde_json::{Value, json};

use crate::provider::{ToolCall, ToolDef, ToolImage};

/// Default per-tool token estimate, matching Codex's default output budget.
pub const DEFAULT_OUTPUT_TOKENS: usize = 10_000;
/// Absolute per-tool ceiling, even when the context window is very large.
pub const MAX_OUTPUT_TOKENS: usize = 24_000;
/// Default byte budget: like Codex, estimate one token per four UTF-8 bytes.
pub const MAX_RESULT: usize = DEFAULT_OUTPUT_TOKENS * 4;

/// Convert a requested token estimate to the shared, clamped serialized byte budget.
pub fn output_bytes(tokens: usize) -> usize {
    tokens.clamp(1, MAX_OUTPUT_TOKENS) * 4
}

/// Shared tool definitions for the in-app assistant and MCP clients.
pub fn defs() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "list_layers",
            description: "Everything about the open project in one call: its path and unsaved changes, the view (centre, lon/lat extent, scale, projection), the active layer, its layers in table-of-contents order, top first (id, name, group, type, source, lon/lat extent, feature count, selection, definition query, renderer, labels, fields, visibility), its web tile layers (XYZ, WMS, vector tiles; index 0 on top, each with the layer it's drawn over, changed with map.setTiles), basemap, tables and layouts with their map frames.",
            schema: json!({
                "type": "object",
                "properties": {
                    "name_filter": {"type": "string", "description": "Only the layers and tables whose name holds this (ignoring case)"},
                    "include_fields": {"type": "boolean", "description": "List each layer's fields (default true); false keeps a large project short"}
                }
            }),
        },
        ToolDef {
            name: "view_map",
            description: "The map as it looks now, as a PNG image: look after changing it. The current view, at a size of your choosing (default 1024 x 640), or a layout's page; region draws part of either at full detail, to read small text.",
            schema: json!({
                "type": "object",
                "properties": {
                    "width": {"type": "integer", "description": "Pixels (64 to 2048)"},
                    "height": {"type": "integer", "description": "Pixels (64 to 2048); with a layout or a region, the picture fits width (and height when given) in their shape"},
                    "layout": {"type": "string", "description": "A layout's name: its page (a map series' current page) instead of the map view"},
                    "region": {
                        "type": "array", "items": {"type": "number"}, "minItems": 4, "maxItems": 4,
                        "description": "A closer look, drawn at full detail (up to 600 dpi on a page): [x, y, w, h] in mm from the page's top left with a layout, else [west, south, east, north] in degrees on the map (with a small margin)"
                    }
                }
            }),
        },
        ToolDef {
            name: "search_commands",
            description: "Search RustGIS commands (map navigation, layers, symbology, labels, selection, editing, layouts, export…) by words. Returns each command's id, label, ribbon location and parameters.",
            schema: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Words to look for, e.g. \"basemap\", \"label field\", \"zoom layer\""},
                    "limit": {"type": "integer", "description": "At most this many (default 12)"}
                },
                "required": ["query"]
            }),
        },
        ToolDef {
            name: "run_command",
            description: "Run a RustGIS command by id with JSON parameters (see search_commands for ids and parameters). Project edits are undoable. Source publication and other external file writes cannot be undone. Commands that read or write files, use the network or replace the project ask the user first. A notice that reports lost data with a loss code is also in the result's losses, as {code, kind, text, action}; for an assumed.* code, ask the user rather than guess.",
            schema: json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "Command id, e.g. map.goToXY"},
                    "params": {"type": "object", "description": "The command's parameters"}
                },
                "required": ["command"]
            }),
        },
        ToolDef {
            name: "search_tools",
            description: "Search geoprocessing tools (buffer, clip, dissolve, spatial join, statistics, raster analysis…) by words. Returns each tool's id, name, toolbox and summary.",
            schema: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Words to look for"},
                    "limit": {"type": "integer", "description": "At most this many (default 12)"}
                },
                "required": ["query"]
            }),
        },
        ToolDef {
            name: "describe_tool",
            description: "A geoprocessing tool's parameters: name, kind, whether required, default and help.",
            schema: json!({
                "type": "object",
                "properties": {"tool": {"type": "string", "description": "Tool id"}},
                "required": ["tool"]
            }),
        },
        ToolDef {
            name: "run_tool",
            description: "Run a geoprocessing tool. Its output is added to the map as a new layer, or to the project as a standalone table (for example Summary Statistics), whose name the result gives (use it as the input of a next step; read a table with read_features). One undo step. As for run_command, a notice that reports lost data with a loss code is also in the result's losses, as {code, kind, text, action}.",
            schema: json!({
                "type": "object",
                "properties": {
                    "tool": {"type": "string", "description": "Tool id"},
                    "params": {"type": "object", "description": "The tool's parameters by name; layers by name"}
                },
                "required": ["tool"]
            }),
        },
        ToolDef {
            name: "read_features",
            description: "Read a vector layer's or a standalone table's attribute rows, optionally only those matching a where clause (SQL-like: pop_max > 1000000 AND continent = 'Asia'). Give layer or table. Returns the number matching and up to `limit` rows, each with its row number (_row; a layer's for select.rows) and a layer's location (_lon, _lat).",
            schema: json!({
                "type": "object",
                "properties": {
                    "layer": {"type": "string", "description": "Layer name or id (or a standalone table's name, when no layer has it)"},
                    "table": {"type": "string", "description": "A standalone table's name or id (an id as text, \"7\"), such as a tool's output table (read instead of layer)"},
                    "where": {"type": "string", "description": "A where clause; empty for all rows"},
                    "fields": {"type": "array", "items": {"type": "string"}, "description": "Only these fields (default all)"},
                    "order_by": {"type": "string", "description": "Sort by this field"},
                    "descending": {"type": "boolean"},
                    "selected": {"type": "boolean", "description": "Only the selected rows"},
                    "limit": {"type": "integer", "description": "At most this many rows (default 20, at most 200)"}
                }
            }),
        },
        ToolDef {
            name: "search_docs",
            description: "Search the RustGIS documentation (user guide, command line, plugins, installation…) by words. Returns the best matching sections: page, section, address and a snippet; read_doc reads one.",
            schema: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Words to look for, e.g. \"graduated symbols\", \"vector tiles\""},
                    "limit": {"type": "integer", "description": "At most this many (default 8)"}
                },
                "required": ["query"]
            }),
        },
        ToolDef {
            name: "read_doc",
            description: "Read a RustGIS documentation page as Markdown, or one section of it (page paths come from search_docs). Long pages read best one section at a time. Results are budgeted; continue at next_offset when present.",
            schema: json!({
                "type": "object",
                "properties": {
                    "page": {"type": "string", "description": "Page path, e.g. user-guide/cartography.md"},
                    "section": {"type": "string", "description": "A section heading (default the whole page)"},
                    "offset": {"type": "integer", "minimum": 1, "description": "First line, 1-based (default 1; use next_offset to continue)"},
                    "limit": {"type": "integer", "description": "Maximum lines (default 2000)"}
                },
                "required": ["page"]
            }),
        },
    ]
}

/// The command a call runs and its parameters (`gp.run` for a tool), if it runs one.
/// Use execution's strict envelope parser for policy too: malformed input has no runnable target.
pub fn target(call: &ToolCall) -> Option<(String, Value)> {
    match call.name.as_str() {
        "run_command" => Some((str_arg(&call.args, "command").unwrap_or_default(), params_arg(&call.args).ok()?)),
        "run_tool" => Some((
            "gp.run".to_string(),
            json!({"tool": str_arg(&call.args, "tool").unwrap_or_default(), "params": params_arg(&call.args).ok()?}),
        )),
        _ => None,
    }
}

/// Whether a successfully executed tool explicitly chose a layer, even if it was already active.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn selects_layer(name: &str, args: &Value) -> bool {
    match name {
        "run_command" => args.get("command").and_then(Value::as_str).map(str::trim) == Some("layer.select"),
        "run" => args.get("script").and_then(Value::as_str).is_some_and(|script| {
            rustgis_engine::cmd::workflows::script_commands(script).is_ok_and(|lines| lines.iter().any(|(_, id, _)| id == "layer.select"))
        }),
        _ => false,
    }
}

/// The executor's strict envelope is the only input to engine planning.
pub fn policy_call(call: &ToolCall) -> Result<Option<Call>, String> {
    let c = match call.name.as_str() {
        "run_command" => Some(Call::command(&str_arg(&call.args, "command").ok_or("command is required")?, params_arg(&call.args)?)),
        "run_tool" => Some(Call::tool(&str_arg(&call.args, "tool").ok_or("tool is required")?, params_arg(&call.args)?)),
        "run" => Some(Call::script(call.args.get("script").and_then(Value::as_str).ok_or("script is required")?)),
        "snapshot" | "view_map" => {
            let files = call.name == "snapshot" && call.args.get("save_path").and_then(Value::as_str).is_some_and(|p| !p.trim().is_empty());
            let effects = if files {
                rustgis_engine::policy::Effects::NONE.with(rustgis_engine::policy::Effect::Files)
            } else {
                rustgis_engine::policy::Effects::NONE
            };
            Some(Call::adapter(if call.name == "snapshot" { "snapshot" } else { "view_map" }, effects, call.args.clone()))
        }
        _ => None,
    };
    Ok(c.map(|c| c.with_request(&call.id)))
}
/// A mapping of the engine outcome for callers that display the reason only.
pub fn gate(s: &Session, call: &ToolCall) -> Option<String> {
    let call = match policy_call(call) {
        Ok(Some(call)) => call,
        Ok(None) => return None,
        Err(error) => return Some(error),
    };
    let scope = Scope::assistant(0, std::sync::Arc::new(Live::default()));
    match s.review(&scope, &call) {
        Outcome::Allowed => None,
        Outcome::NeedsApproval(p) => Some(p.review().why.clone()),
        Outcome::Refused(e) => Some(e),
    }
}
/// A revocable assistant lifetime. A stopped lifetime is never re-enabled.
pub(crate) struct Live(pub(crate) std::sync::atomic::AtomicBool);
impl Default for Live {
    fn default() -> Self {
        Self(std::sync::atomic::AtomicBool::new(true))
    }
}
impl rustgis_engine::policy::Revocation for Live {
    fn allows(&self, _: rustgis_engine::policy::Effects) -> bool {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}
/// A one-line description of a call for the transcript and the approval prompt.
pub fn summary(call: &ToolCall) -> String {
    let short = |v: Value| {
        let t = v.to_string();
        if t.chars().count() > 300 { format!("{}…", t.chars().take(300).collect::<String>()) } else { t }
    };
    match call.name.as_str() {
        "run_command" => format!("{} {}", str_arg(&call.args, "command").unwrap_or_default(), short(obj_arg(&call.args, "params"))),
        "run_tool" => format!(
            "gp.run {}",
            short(json!({"tool": str_arg(&call.args, "tool").unwrap_or_default(), "params": obj_arg(&call.args, "params")}))
        ),
        "delegate" => format!("Delegated: {}", str_arg(&call.args, "task").unwrap_or_default().chars().take(300).collect::<String>()),
        other => format!("{other} {}", short(call.args.clone())),
    }
}

/// The largest map image `view_map` draws (pixels a side).
pub(crate) const MAX_VIEW: f64 = 2048.0;

/// The most a `view_map` picture may weigh: 3.75 MiB of PNG, 5 MiB once base64-encoded, half the
/// 10 MB an image may be on Anthropic's API (5 MB through Amazon Bedrock) and under OpenAI's and
/// Gemini's limits. A satellite basemap weighs about 3.9 MB at 2048 × 1229, some 6.5 MB at 2048 × 2048.
pub(crate) const MAX_PNG_BYTES: usize = 5 * 1024 * 1024 / 4 * 3;

/// Draws at `w` × `h` and, while the picture weighs more than `max` bytes, again smaller (both
/// sides by the square root of the ratio, with a margin), down to 64 pixels a side. A picture a
/// provider refuses for its size would otherwise stay among the newest two and fail every later
/// request. Returns the picture and the size it was drawn at.
pub(crate) fn draw_within(
    mut w: f64,
    mut h: f64,
    max: usize,
    mut draw: impl FnMut(f64, f64) -> Result<Vec<u8>, String>,
) -> Result<(Vec<u8>, f64, f64), String> {
    loop {
        let png = draw(w, h)?;
        if png.len() <= max || w.min(h) <= 64.0 {
            return Ok((png, w, h));
        }
        let k = (max as f64 / png.len() as f64).sqrt() * 0.9;
        (w, h) = ((w * k).round().max(64.0), (h * k).round().max(64.0));
    }
}

/// `view_map`: the current view drawn offscreen at the size asked (default 1024 × 640), and a
/// caption naming it. It goes through its adapter, so the policy sees what the picture draws.
///
/// It draws on the thread that runs the assistant's calls, which on the desktop is the window's:
/// the agent runs every call there (`run_command` and `run_tool` too, through
/// `Session::run_scoped`), and a call's result must be in the conversation before the next
/// request goes out. The drawing is one pass over the features in view, capped at 2048 pixels a
/// side, with no Cancel; a layer of about 476,000 polygons read in place takes some 12 s at the
/// default size, one of 120,000 some 2 s. Moving the assistant's calls off the window is a change
/// to the agent's loop of its own.
pub fn view_map(s: &mut Session, call: &ToolCall, scope: Scope, permit: Option<Permit>) -> Result<(String, ToolImage), String> {
    let engine = policy_call(call)?.ok_or("missing view adapter")?;
    let owned = s.defer(scope, &engine, permit)?;
    s.finish_deferred(owned, |s| draw_view(s, &call.args, MAX_PNG_BYTES))?
}

/// The most dots per inch `view_map` draws a layout at, as Export Layout's most.
const MAX_LAYOUT_DPI: f64 = 600.0;

/// Draws what `view_map` is asked for (the view, a layout's page, a region of either), in a
/// picture of at most `max` bytes. A picture drawn smaller than asked says so in its caption, and
/// what drawing it said (texts cut off, layers drawn without their cast shadows) follows it.
pub(crate) fn draw_view(s: &mut Session, a: &Value, max: usize) -> Result<(String, ToolImage), String> {
    let mark = s.notice_mark();
    let size = |k: &str| a.get(k).and_then(Value::as_f64).filter(|v| v.is_finite()).map(|v| v.clamp(64.0, MAX_VIEW).round());
    let region = match a.get("region") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_array()
                .filter(|v| v.len() == 4)
                .map(|v| v.iter().filter_map(Value::as_f64).filter(|x| x.is_finite()).collect::<Vec<_>>())
                .and_then(|n| <[f64; 4]>::try_from(n).ok())
                .ok_or("region must be four numbers: [x, y, w, h] in mm with a layout, [west, south, east, north] in degrees on the map")?,
        ),
    };
    let asked = (size("width"), size("height"));
    let (mut caption, png, w, h) = if a.get("layout").is_some_and(|v| !v.is_null()) {
        draw_layout(s, a, region, asked, max)?
    } else {
        draw_map(s, region, asked, max)?
    };
    // Each once: a picture drawn again smaller says it all again.
    let mut said: Vec<String> = Vec::new();
    for text in s.notices_since(mark).iter().map(|n| public_text(n.text.clone())) {
        if !said.contains(&text) {
            said.push(text);
        }
    }
    if !said.is_empty() {
        caption.push_str(&format!(" Drawing it said: {}", said.join(" | ")));
    }
    Ok((caption, ToolImage { png, width: w, height: h }))
}

/// The size a PNG says it is, from its header.
fn png_size(png: &[u8]) -> (u32, u32) {
    let dim = |at: usize| png.get(at..at + 4).and_then(|b| <[u8; 4]>::try_from(b).ok()).map_or(0, u32::from_be_bytes);
    (dim(16), dim(20))
}

/// A picture of `view_map`: its caption, its PNG and its size in pixels.
type Drawn = (String, Vec<u8>, u32, u32);

/// `view_map` of the map view, or of the part of the map in `region` (lon/lat) fitted in the
/// asked size as Zoom To fits an extent, with its margin; the view is left as it was.
fn draw_map(s: &mut Session, region: Option<[f64; 4]>, asked: (Option<f64>, Option<f64>), max: usize) -> Result<Drawn, String> {
    let (mut asked_w, mut asked_h) = (asked.0.unwrap_or(1024.0), asked.1.unwrap_or(640.0));
    let fitted = match region {
        None => None,
        Some([west, south, east, north]) => {
            let lon = -180.0..=180.0;
            let lat = -90.0..=90.0;
            if !(lon.contains(&west) && lon.contains(&east) && lat.contains(&south) && lat.contains(&north))
                || west >= east
                || south >= north
            {
                return Err(format!(
                    "region [{west}, {south}, {east}, {north}] must be [west, south, east, north] in degrees, west < east and south < north (one across the 180° meridian is not drawn)"
                ));
            }
            if s.map().projection.is_globe() {
                return Err("region needs a flat map, and this one is a globe: give it a flat projection (map.projection), or look at the whole view".into());
            }
            let e = rustgis_engine::geom::Extent { min_x: west, min_y: south, max_x: east, max_y: north };
            // Without a height, the picture takes the region's shape on the map.
            let m = s.map().flat().extent(&e);
            if asked.1.is_none() && m.width() > 0.0 && m.height() > 0.0 {
                asked_h = (asked_w * m.height() / m.width()).round();
                if asked_h > MAX_VIEW {
                    asked_w = (asked_w * MAX_VIEW / asked_h).round().max(64.0);
                }
                asked_h = asked_h.clamp(64.0, MAX_VIEW);
            }
            Some(e)
        }
    };
    let (camera, view_size) = (s.map().camera, s.view_size);
    let drawn = draw_within(asked_w, asked_h, max, |w, h| {
        if let Some(e) = &fitted {
            // Fitted at the picture's own size, which the image then keeps.
            s.view_size = (w, h);
            s.map_mut().camera = camera;
            if !s.map_mut().fit_lonlat(e, w, h) {
                return Err("the region is outside what the map's projection shows".to_string());
            }
        }
        rustgis_engine::cmd::share::map_png(s, Some(w), Some(h), None)
    });
    s.map_mut().camera = camera;
    s.view_size = view_size;
    let (png, w, h) = drawn?;
    let m = s.map();
    let c = m.center_lonlat();
    let smaller = if (w, h) != (asked_w, asked_h) {
        format!(", smaller than the {asked_w:.0} x {asked_h:.0} asked, to fit the {:.2} MB a picture may weigh", max as f64 / 1e6)
    } else {
        String::new()
    };
    let shown = match region {
        Some([west, south, east, north]) => format!("region {west}, {south}, {east}, {north} (west, south, east, north)"),
        None => format!("centre {:.4}, {:.4} (lon, lat), scale 1:{:.0}", c.x, c.y, m.scale()),
    };
    let caption = format!("Map \"{}\", {} layers, {shown}, {w:.0} x {h:.0} px{smaller}.", m.name, m.layers.len());
    Ok((caption, png, w as u32, h as u32))
}

/// `view_map` of a layout's page, or of the part of it in `region` (mm), drawn at the dpi that
/// fits the asked width (and height when given), at most [`MAX_LAYOUT_DPI`]. A region is drawn
/// at that dpi rather than cut out of the whole page, and the frames it cuts are drawn whole, as
/// on the page: a close look at a large frame costs what exporting the page at that dpi does.
fn draw_layout(
    s: &mut Session,
    a: &Value,
    region: Option<[f64; 4]>,
    asked: (Option<f64>, Option<f64>),
    max: usize,
) -> Result<Drawn, String> {
    let li = rustgis_engine::cmd::layout::named(s, a)?;
    let l = s.project.layouts.get(li).ok_or("no layout")?;
    let (name, pw, ph) = (l.name.clone(), l.width, l.height);
    // A region is cut to the page, and the cut said.
    let (part, cut) = match region {
        None => ([0.0, 0.0, pw, ph], false),
        Some([x, y, w, h]) => {
            let (x0, y0, x1, y1) = (x.max(0.0), y.max(0.0), (x + w).min(pw), (y + h).min(ph));
            if w <= 0.0 || h <= 0.0 || x1 <= x0 || y1 <= y0 {
                return Err(format!(
                    "region [{x}, {y}, {w}, {h}] is not on the page: give [x, y, w, h] in mm from its top left, inside its {pw:.0} x {ph:.0} mm"
                ));
            }
            ([x0, y0, x1 - x0, y1 - y0], (x0, y0, x1, y1) != (x, y, x + w, y + h))
        }
    };
    let (asked_w, asked_h) = (asked.0.unwrap_or(1024.0), asked.1.unwrap_or(MAX_VIEW));
    // A region's edges are rounded outward to whole pixels, up to one more each side.
    let edge = if region.is_some() { 2.0 } else { 0.0 };
    let dpi_for = |w: f64, h: f64| (25.4 * ((w - edge) / part[2]).min((h - edge) / part[3])).min(MAX_LAYOUT_DPI);
    let (png, w, h) =
        draw_within(asked_w, asked_h, max, |w, h| rustgis_engine::cmd::layout::layout_png(s, li, dpi_for(w, h), region.map(|_| part)))?;
    let (pxw, pxh) = png_size(&png);
    // A page drawn at its least resolution (a quarter of the screen's) can still be larger than
    // a picture is sent at: refused, rather than sent too large.
    if f64::from(pxw.max(pxh)) > MAX_VIEW {
        return Err(format!(
            "the page, {pw:.0} x {ph:.0} mm, is {pxw} x {pxh} px drawn at its least, more than the {MAX_VIEW:.0} a side view_map draws: give a region"
        ));
    }
    let dpi = f64::from(pxw) * 25.4 / part[2];
    let shown = match region {
        Some(_) => format!(
            ", region {:.1}, {:.1}, {:.1}, {:.1} mm (x, y, w, h){}",
            part[0],
            part[1],
            part[2],
            part[3],
            if cut { ", cut to the page" } else { "" }
        ),
        None => String::new(),
    };
    let smaller = if (w, h) != (asked_w, asked_h) {
        format!(", smaller than asked, to fit the {:.2} MB a picture may weigh", max as f64 / 1e6)
    } else if dpi_for(w, h) >= MAX_LAYOUT_DPI {
        format!(", at the {MAX_LAYOUT_DPI:.0} dpi a page is drawn at the most")
    } else {
        String::new()
    };
    let caption = format!("Layout \"{name}\", page {pw:.0} x {ph:.0} mm{shown}, {pxw} x {pxh} px at {dpi:.0} dpi{smaller}.");
    Ok((caption, png, pxw, pxh))
}

/// Runs a call; `Err` is a result the model is told failed.
pub fn run(s: &mut Session, call: &ToolCall, scope: Scope, permit: Option<Permit>) -> Result<String, String> {
    run_with_budget(s, call, DEFAULT_OUTPUT_TOKENS, scope, permit)
}

/// Runs a call with a per-result token estimate, capped by the absolute ceiling.
pub fn run_with_budget(s: &mut Session, call: &ToolCall, tokens: usize, scope: Scope, permit: Option<Permit>) -> Result<String, String> {
    // Script failures are structured JSON: clean their error field before serialization.
    if call.name == "run" {
        return run_script(s, call, tokens, scope, permit);
    }
    run_with_budget_inner(s, call, tokens, scope, permit).map_err(|e| match e {
        Failure::Plain(e) => public_error(&e),
        Failure::Clean(e) => e,
    })
}

/// A call's failure, which [`run_with_budget`] cleans of login values; a command's failure is
/// cleaned already, with the codes of its losses added after cleaning ([`error_with_notices`]).
enum Failure {
    Plain(String),
    Clean(String),
}

impl From<String> for Failure {
    fn from(e: String) -> Self {
        Self::Plain(e)
    }
}

impl From<&str> for Failure {
    fn from(e: &str) -> Self {
        Self::Plain(e.into())
    }
}

/// Adapter-owned presentation state for one already-admitted archive command. Execution
/// authority stays in the engine's ReadAhead/Read, never in this result formatter.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct CommandCompletion {
    before: Vec<(String, Vec<u32>)>,
    said: Said,
}
#[cfg(not(target_arch = "wasm32"))]
impl CommandCompletion {
    pub(crate) fn capture(s: &Session) -> Self {
        Self { before: layer_snap(s), said: Said::now(s) }
    }
    pub(crate) fn finish(self, s: &mut Session, result: Result<Value, String>) -> Result<String, String> {
        result
            .map(|value| command_result(s, value, &self.before, &self.said, DEFAULT_OUTPUT_TOKENS))
            .map_err(|e| error_with_notices(e, s, &self.said, DEFAULT_OUTPUT_TOKENS))
    }
}

/// Retains the initiating scope and one-use permit while assembling ordinary tool results.
fn run_with_budget_inner(s: &mut Session, call: &ToolCall, tokens: usize, scope: Scope, permit: Option<Permit>) -> Result<String, Failure> {
    if matches!(call.name.as_str(), "run_command" | "run_tool") {
        if call.name == "run_command" && str_arg(&call.args, "command").is_some_and(|id| id.starts_with("ui.")) {
            return Err("ui.* commands only open panes and dialogs; the assistant can't use them".into());
        }
        let engine = policy_call(call)?.ok_or("missing executable call")?;
        let (before, said) = (layer_snap(s), Said::now(s));
        let result = match permit {
            Some(p) => s.run_permitted(scope, p, &engine),
            None => s.run_scoped(scope, &engine),
        }
        .map_err(|e| Failure::Clean(error_with_notices(e, s, &said, tokens)))?;
        return Ok(command_result(s, result, &before, &said, tokens));
    }
    if permit.is_some() {
        return Err("this approval is for an executable call".into());
    }
    s.with_scope(scope, |s| run_read(s, call, tokens))?.map_err(Failure::Plain)
}

fn run_read(s: &mut Session, call: &ToolCall, tokens: usize) -> Result<String, String> {
    let max_bytes = output_bytes(tokens);
    let a = &call.args;
    let v = match call.name.as_str() {
        "list_layers" => s.run(
            "info.project",
            &json!({"nameFilter": str_arg(a, "name_filter"), "includeFields": a.get("include_fields").and_then(Value::as_bool).unwrap_or(true)}),
        )?,
        "search_commands" => search_commands(s, a)?,
        "search_tools" => search_tools(a)?,
        "describe_tool" => describe_tool(a)?,
        "read_features" => read_features(s, a)?,
        "search_docs" => crate::docs::search(&str_arg(a, "query").unwrap_or_default(), limit_arg(a, 8, 30))?,
        "read_doc" => {
            return crate::docs::read_with_budget(
                &str_arg(a, "page").unwrap_or_default(),
                str_arg(a, "section").as_deref(),
                match a.get("offset") {
                    None => 1,
                    Some(v) => v.as_u64().filter(|n| *n > 0).ok_or("offset must be a positive integer (1-based line)")?,
                },
                limit_arg(a, 2000, 2000),
                max_bytes,
            )
            .map(|v| v.to_string());
        }
        // Fails the way a bug in RustGIS would, for the agent's panic guard.
        #[cfg(test)]
        "test_panic" => panic!("a test tool panicked"),
        other => return Err(format!("unknown tool '{other}'")),
    };
    if call.name == "read_features" {
        Ok(public_envelope(s, v, tokens))
    } else {
        let mut withheld = Withheld::default();
        let v = public_metadata(v, 0, &mut withheld);
        Ok(finish_result(s, v, tokens, withheld))
    }
}

/// How deep a result is searched for runtime login addresses; what is deeper is withheld.
const MAX_DEPTH: usize = 16;

/// `text` with every runtime login wrapper (`rustgis-login:…`, alone or after a prefix such as
/// `pmtiles:…@`) replaced by its plain address. Commands return the bound addresses the app needs
/// to use a connection's login; the model and MCP clients get only the public ones. Active
/// raw secrets are scrubbed too, including an echo with no wrapper in the result.
fn public_text(text: String) -> String {
    rustgis_engine::sources::scrub_s3_wrappers(&rustgis_engine::render::rustgis_tiles::logins::scrub(&text))
}

/// Generated errors may quote arbitrary addresses; successful values remain data.
fn public_error(text: &str) -> String {
    rustgis_engine::sources::scrub_s3_wrappers(&rustgis_engine::render::rustgis_tiles::logins::scrub_error(text))
}

/// Publishes a completed result, reporting withheld subtrees before enforcing the response budget.
#[cfg(test)]
fn public_result(s: &mut Session, value: Value, tokens: usize) -> String {
    let mut withheld = Withheld::default();
    let value = public_addresses(value, 0, &mut withheld);
    finish_result(s, value, tokens, withheld)
}

/// These fields belong to the adapter's response schema; only their values contain data.
fn public_envelope(s: &mut Session, value: Value, tokens: usize) -> String {
    let (value, withheld) = public_projection(value);
    finish_result(s, value, tokens, withheld)
}

/// [`public_envelope`]'s cleaning, without serializing, so fixed-schema fields can be added after.
fn public_projection(value: Value) -> (Value, Withheld) {
    let mut withheld = Withheld::default();
    let value = match value {
        Value::Object(o) => Value::Object(o.into_iter().map(|(key, value)| (key, public_addresses(value, 1, &mut withheld))).collect()),
        value => public_addresses(value, 0, &mut withheld),
    };
    (value, withheld)
}

/// Generated search, tool and project metadata has a fixed schema at every level. Its
/// strings may hold private data, but its keys cannot be login values. Row/property maps
/// and arbitrary command results use `public_addresses` instead.
fn public_metadata(value: Value, depth: usize, withheld: &mut Withheld) -> Value {
    match value {
        Value::String(text) => Value::String(public_text(text)),
        Value::Array(a) if depth < MAX_DEPTH => Value::Array(a.into_iter().map(|v| public_metadata(v, depth + 1, withheld)).collect()),
        Value::Object(o) if depth < MAX_DEPTH => {
            Value::Object(o.into_iter().map(|(key, v)| (key, public_metadata(v, depth + 1, withheld))).collect())
        }
        Value::Array(_) | Value::Object(_) => {
            withheld.depth = withheld.depth.saturating_add(1);
            json!("[too deep]")
        }
        other => other,
    }
}

/// Cleans the arbitrary command payload before adding adapter-owned notices/layer metadata.
fn command_result(s: &mut Session, value: Value, before: &[(String, Vec<u32>)], said: &Said, tokens: usize) -> String {
    let mut withheld = Withheld::default();
    let value = public_addresses(value, 0, &mut withheld);
    let value = with_notices(with_new_layers(value, s, before), s, said.mark);
    // Added after cleaning: its keys and codes are fixed, and its text was cleaned on its own.
    let losses = losses_since(s, said);
    finish_with_losses(s, value, losses, tokens, withheld, false)
}

/// Publishes counted projection losses without sending the completed envelope through a second redactor.
fn finish_result(s: &mut Session, value: Value, tokens: usize, withheld: Withheld) -> String {
    bounded_json(&with_withheld(s, value, withheld), tokens)
}

/// `value` with the notices of what cleaning it withheld, also given to the app.
fn with_withheld(s: &mut Session, mut value: Value, withheld: Withheld) -> Value {
    if withheld.depth > 0 || withheld.keys > 0 {
        let mark = s.notice_mark();
        if withheld.depth > 0 {
            s.notify(format!("Tool result: {} subtrees at nesting depth {MAX_DEPTH} were withheld.", withheld.depth), false);
        }
        if withheld.keys > 0 {
            s.notify(format!("Tool result: {} object(s) were withheld because redacted field names coincide.", withheld.keys), false);
        }
        if let Some(notices) = value.get_mut("notices").and_then(Value::as_array_mut) {
            notices.extend(s.notices_since(mark).iter().map(|n| json!(n.text)));
        } else {
            value = with_notices(value, s, mark);
        }
    }
    value
}

/// Counts output withheld for depth limits or field names that collide after redaction.
#[derive(Default)]
struct Withheld {
    depth: usize,
    keys: usize,
}

/// `v` with string values and field names made public ([`public_text`]), to a depth of
/// [`MAX_DEPTH`]. An object whose public field names collide is withheld as a whole.
fn public_addresses(v: Value, depth: usize, withheld: &mut Withheld) -> Value {
    match v {
        Value::String(s) => Value::String(public_text(s)),
        Value::Array(a) if depth < MAX_DEPTH => Value::Array(a.into_iter().map(|x| public_addresses(x, depth + 1, withheld)).collect()),
        Value::Object(o) if depth < MAX_DEPTH => {
            let mut public = serde_json::Map::new();
            for (key, value) in o {
                let key = public_text(key);
                if public.contains_key(&key) {
                    withheld.keys = withheld.keys.saturating_add(1);
                    return json!("[redacted field names coincide]");
                }
                public.insert(key, value);
            }
            Value::Object(public.into_iter().map(|(key, value)| (key, public_addresses(value, depth + 1, withheld))).collect())
        }
        Value::Array(_) | Value::Object(_) => {
            withheld.depth = withheld.depth.saturating_add(1);
            json!("[too deep]")
        }
        other => other,
    }
}

/// Serialize complete JSON. Oversized results become an explicit text preview envelope;
/// the excerpt is a JSON string, so quotes, escapes and UTF-8 can never break the response.
pub fn bounded_json(value: &Value, tokens: usize) -> String {
    let text = value.to_string();
    let max = output_bytes(tokens);
    if text.len() <= max {
        return text;
    }
    // Preserve the error reason before spending a small budget on preview metadata.
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        return bounded_string(&format!("error: {error}"), max);
    }
    let chars: Vec<char> = text.chars().collect();
    let envelope = |keep: usize| {
        let head: String = chars.iter().take(keep / 2).collect();
        let tail: String = chars.iter().skip(chars.len().saturating_sub(keep - keep / 2)).collect();
        json!({"truncated": true, "original_bytes": text.len(),
            "output": format!("{head}\n… output truncated; request fewer rows or a narrower query …\n{tail}")})
        .to_string()
    };
    let (mut low, mut high) = (0, chars.len());
    while low < high {
        let mid = low + (high - low).div_ceil(2);
        if envelope(mid).len() <= max {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    let result = envelope(low);
    if result.len() <= max {
        return result;
    }
    // A tiny budget cannot hold the normal wrapper. Keep minimal truncation metadata,
    // or an abbreviated marker when even that cannot fit.
    let minimal = json!({"truncated": true}).to_string();
    if minimal.len() <= max { minimal } else { bounded_string("truncated", max) }
}

/// Fit a diagnostic prefix inside a JSON string, including quotes and escaping.
fn bounded_string(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let encoded = |keep: usize| json!(chars.iter().take(keep).collect::<String>()).to_string();
    let (mut low, mut high) = (0, chars.len());
    while low < high {
        let mid = low + (high - low).div_ceil(2);
        if encoded(mid).len() <= max {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    encoded(low)
}

/// `text`, cut to `max` characters with a note when longer.
pub fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    format!("{}… (cut: {} characters in all)", text.chars().take(max).collect::<String>(), text.chars().count())
}

fn str_arg(a: &Value, k: &str) -> Option<String> {
    match a.get(k)? {
        Value::String(s) => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// The `params` a command or tool runs with: an object, or JSON text of one (some models send
/// that). Anything else is an error: running with defaults instead could act on the wrong layer.
fn params_arg(a: &Value) -> Result<Value, String> {
    rustgis_engine::params_envelope(a.get("params"))
}

/// An object parameter, which some models send as JSON text (leniently, for display only).
fn obj_arg(a: &Value, k: &str) -> Value {
    match a.get(k) {
        Some(Value::String(t)) if !t.trim().is_empty() => serde_json::from_str(t).unwrap_or(Value::Null),
        Some(v @ Value::Object(_)) => v.clone(),
        _ => json!({}),
    }
}

fn limit_arg(a: &Value, default: usize, max: usize) -> usize {
    a.get("limit").and_then(Value::as_u64).map_or(default, |n| usize::try_from(n).unwrap_or(max)).clamp(1, max)
}

/// Every map's name and layer ids, for spotting the layers a command actually added.
fn layer_snap(s: &Session) -> Vec<(String, Vec<u32>)> {
    s.project.maps.iter().map(|m| (m.name.clone(), m.layers.iter().map(|l| l.id).collect())).collect()
}

/// A run's result, with the layers it added named (the next step's inputs).
///
/// Layers are compared with the same map's layers before the run, so switching or deleting maps
/// does not report the newly visible map's layers as `layersAdded` (#212). Map names are unique, so
/// the map is found by name, or at the same place when only its name changed (a rename).
fn with_new_layers(r: Value, s: &Session, before: &[(String, Vec<u32>)]) -> Value {
    let m = s.map();
    let active = s.project.active_map;
    // A rename changes only the active map's name; every other map keeps its own.
    let renamed = before.len() == s.project.maps.len()
        && s.project.maps.iter().zip(before).enumerate().all(|(i, (now, (was, _)))| i == active || now.name == *was);
    let same =
        before.iter().find(|(name, _)| name.eq_ignore_ascii_case(&m.name)).or_else(|| if renamed { before.get(active) } else { None });
    let had = same.map_or(&[][..], |(_, ids)| ids.as_slice());
    let added: Vec<String> = m.layers.iter().filter(|l| !had.contains(&l.id)).map(|l| public_text(l.name.clone())).collect();
    if added.is_empty() {
        return if r.is_null() { json!({"ok": true}) } else { r };
    }
    json!({"result": r, "layersAdded": added})
}

/// Runs a script (`script.run`'s format: one `command-id {json}` per line, `#` comments) in one
/// call and one undo step, none when its lines only read, moved the view, exported, left a
/// request or reported they changed nothing. A failing line stops the script and nothing it did
/// to the project is kept (files it wrote stay). The answer is
/// `{success, result, lines, layersAdded, notices, losses, error, duration_s}`, where `result` is the
/// last command's; a failure is an `Err` holding the same JSON.
fn run_script(s: &mut Session, call: &ToolCall, tokens: usize, scope: Scope, permit: Option<Permit>) -> Result<String, String> {
    let a = &call.args;
    // Untrimmed, so errors name the same lines the caller wrote.
    let script = a
        .get("script")
        .and_then(Value::as_str)
        .filter(|t| !t.trim().is_empty())
        .ok_or("script is required: one `command-id {json}` per line")?;
    let start = web_time::Instant::now();
    let (before, said) = (layer_snap(s), Said::now(s));
    let mark = said.mark;
    let engine = Call::script(script).with_request(&call.id);
    let r = match permit {
        Some(p) => s.run_permitted(scope, p, &engine),
        None => s.run_scoped(scope, &engine),
    };
    let secs = (start.elapsed().as_secs_f64() * 1000.0).round() / 1000.0;
    let notices: Vec<&str> = s.notices_since(mark).iter().map(|n| n.text.as_str()).collect();
    let losses = losses_since(s, &said);
    match r {
        Ok(v) => {
            let ran = v.get("ran").and_then(Value::as_array);
            let last = ran.and_then(|r| r.last()).and_then(|l| l.get("result")).cloned().unwrap_or(Value::Null);
            let added = with_new_layers(json!({}), s, &before).get("layersAdded").cloned().unwrap_or_else(|| json!([]));
            let out = json!({"success": true, "result": last, "lines": ran.map_or(0, Vec::len), "layersAdded": added,
                "notices": notices, "error": null, "duration_s": secs});
            let (out, withheld) = public_projection(out);
            Ok(finish_with_losses(s, out, losses, tokens, withheld, true))
        }
        Err(e) => {
            let e = public_error(&e);
            // Nothing was kept, so no line ran and no layer was added.
            // The notice of the failure is the error redacted: compared as such, it isn't repeated.
            let told = rustgis_engine::data::diagnostics::redact(&e);
            let kept: Vec<&&str> = notices.iter().filter(|t| **t != e && **t != told).collect();
            let out = json!({"success": false, "result": null, "lines": 0, "layersAdded": [], "notices": kept,
                "error": clip(&e, 8000), "duration_s": secs});
            let (out, withheld) = public_projection(out);
            Err(finish_with_losses(s, out, losses, tokens, withheld, true))
        }
    }
}

/// A run's result with what it said beside it (a notice that only the first tables of a GeoPackage
/// were added, a file's warnings), which the status bar shows in the app.
fn with_notices(r: Value, s: &Session, mark: u64) -> Value {
    let said: Vec<String> = s.notices_since(mark).iter().map(|n| public_text(n.text.clone())).collect();
    if said.is_empty() {
        return r;
    }
    match r {
        Value::Object(mut o) if !o.contains_key("notices") => {
            o.insert("notices".into(), json!(said));
            Value::Object(o)
        }
        other => json!({"result": other, "notices": said}),
    }
}

/// Where a run's notices start, and the session's loss counts then: what [`losses_since`] reads.
struct Said {
    mark: u64,
    given: Vec<(rustgis_engine::data::loss::LossCode, u64)>,
}

impl Said {
    fn now(s: &Session) -> Self {
        Self { mark: s.notice_mark(), given: s.losses_given().to_vec() }
    }
}

/// The notices since `said` that report a classified loss, as `{code, kind, text, action}`: the
/// code is never translated or renamed, so it can be matched where the text can't. Losses whose
/// notices the session no longer keeps (past its last 200) come as one entry per code with their
/// `count`.
fn losses_since(s: &Session, said: &Said) -> Vec<Value> {
    let entry = |code: rustgis_engine::data::loss::LossCode, text: String| {
        let spec = code.spec();
        json!({"code": spec.id, "kind": spec.kind.id(), "text": text, "action": spec.action})
    };
    let kept: Vec<_> = s.notices_since(said.mark).iter().filter_map(|n| Some((n.loss()?, public_text(n.text.clone())))).collect();
    let mut out: Vec<Value> = s
        .losses_given()
        .iter()
        .filter_map(|&(code, given)| {
            let was = said.given.iter().find(|(c, _)| *c == code).map_or(0, |(_, n)| *n);
            let held = kept.iter().filter(|(c, _)| *c == code).count() as u64;
            let lost = given.saturating_sub(was).saturating_sub(held);
            (lost > 0).then(|| {
                let mut e = entry(code, format!("{lost} further {} notice(s) were not kept", code.spec().id));
                e["count"] = json!(lost);
                e
            })
        })
        .collect();
    out.extend(kept.into_iter().map(|(code, text)| entry(code, text)));
    out
}

/// [`finish_result`] for `r` with `losses` (or `[]` when `always`), within the output budget.
/// The list shrinks until it fits beside `r` ([`loss_forms`]): each entry's code and kind, one
/// entry per code, and at last the codes alone. When none fits beside `r` whole, `r` is cut to
/// leave room for one entry per code, keeping its `success`, and the list shrinks again if it
/// must. `lossesTrimmed` says when the list was shortened or, with the tightest budgets, left out.
fn finish_with_losses(s: &mut Session, r: Value, losses: Vec<Value>, tokens: usize, withheld: Withheld, always: bool) -> String {
    // What cleaning withheld is said first, so the sizes below are the final ones.
    let r = with_withheld(s, r, withheld);
    // A result that isn't an object goes under `result`, so the codes have their key.
    let o = match r {
        Value::Object(o) => o,
        other if losses.is_empty() && !always => return bounded_json(&other, tokens),
        other => serde_json::Map::from_iter([("result".to_string(), other)]),
    };
    if losses.is_empty() && !always {
        return bounded_json(&Value::Object(o), tokens);
    }
    // Read before a wrap below moves them. A result's `error` that is null (`run`'s on success)
    // isn't a failure.
    let failed = o.get("error").is_some_and(|e| !e.is_null());
    let success = o.get("success").cloned();
    // A result with its own `losses` goes under `result`, as a result that isn't an object does
    // beside `notices`, so the codes still have their key; its `success` stays beside them.
    let o = if o.contains_key("losses") {
        let mut wrapped = serde_json::Map::from_iter([("result".to_string(), Value::Object(o))]);
        if let Some(success) = &success {
            wrapped.insert("success".into(), success.clone());
        }
        wrapped
    } else {
        o
    };
    let max = output_bytes(tokens);
    let forms = loss_forms(&losses);
    let with = |mut o: serde_json::Map<String, Value>, (list, trimmed): &(Value, bool)| {
        o.insert("losses".into(), list.clone());
        if *trimmed {
            o.insert("lossesTrimmed".into(), json!(true));
        }
        Value::Object(o)
    };
    // Sized from the parts, so the result is serialized once: its own bytes, then each form's
    // with its key (and `lossesTrimmed`'s).
    let base = Value::Object(o.clone()).to_string().len();
    let size = |(list, trimmed): &(Value, bool)| {
        base + list.to_string().len() + r#","losses":"#.len() + if *trimmed { r#","lossesTrimmed":true"#.len() } else { 0 }
    };
    if let Some(form) = forms.iter().find(|form| size(form) <= max) {
        return with(o, form).to_string();
    }
    // The result cut to leave room for one entry per code.
    let summary = forms.get(2).or(forms.last()).map_or(0, |(list, _)| list.to_string().len());
    let text = bounded_json(&Value::Object(o.clone()), max.saturating_sub(summary + 64).div_ceil(4).max(1));
    let mut cut = match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(cut)) => cut,
        // A failure's error alone (`error: …`), kept as JSON with as much of it as fits below.
        Ok(Value::String(preview)) if failed => {
            serde_json::Map::from_iter([("error".to_string(), json!(preview)), ("truncated".to_string(), json!(true))])
        }
        _ if failed => serde_json::Map::from_iter([("error".to_string(), json!(text)), ("truncated".to_string(), json!(true))]),
        // No room for any of a result that didn't fail: only that it was cut.
        _ => serde_json::Map::from_iter([("truncated".to_string(), json!(true))]),
    };
    if let Some(success) = &success {
        cut.insert("success".into(), success.clone());
    }
    for form in forms.iter().skip(2) {
        let v = with(cut.clone(), form);
        let over = v.to_string().len().saturating_sub(max);
        if over == 0 {
            return v.to_string();
        }
        // Only a failure's error can still give way: cut it by what's over.
        if let Some(error) = v.get("error").and_then(Value::as_str) {
            let mut v = v.clone();
            v["error"] = json!(clip_bytes(error, error.len().saturating_sub(over + 8)));
            if v.to_string().len() <= max {
                return v.to_string();
            }
        }
    }
    // The codes, saying the result (or the failure's error) was cut: then only that both were.
    let mut said_cut = serde_json::Map::from_iter(success.map(|v| ("success".to_string(), v)));
    said_cut.insert("truncated".into(), json!(true));
    for form in forms.iter().skip(2) {
        let v = with(said_cut.clone(), form);
        if v.to_string().len() <= max {
            return v.to_string();
        }
    }
    // With no losses to leave out, only that the result was cut.
    let trimmed = !losses.is_empty();
    let mut v = said_cut;
    if trimmed {
        v.insert("lossesTrimmed".into(), json!(true));
    }
    let v = Value::Object(v).to_string();
    // The tightest budgets: the markers alone, then the list's name alone; below that (15 bytes)
    // no marker fits, and nothing a client could misread is said.
    let both = if trimmed { json!({"truncated": true, "lossesTrimmed": true}) } else { json!({"truncated": true}) }.to_string();
    let marker = json!({"lossesTrimmed": true}).to_string();
    let name = json!("lossesTrimmed").to_string();
    if v.len() <= max {
        v
    } else if both.len() <= max {
        both
    } else if trimmed && marker.len() <= max {
        marker
    } else if trimmed && name.len() <= max {
        name
    } else {
        "{}".to_string()
    }
}

/// `losses` in the forms [`finish_with_losses`] tries, longest first, each with whether it is
/// shortened: whole; each entry's code and kind; one entry per code ([`loss_summary`]); one entry
/// per code with its count only; the codes alone.
fn loss_forms(losses: &[Value]) -> Vec<(Value, bool)> {
    // An entry for losses whose notices weren't kept keeps its count.
    let compact: Vec<Value> = losses
        .iter()
        .map(|l| {
            let mut e = json!({"code": l["code"], "kind": l["kind"]});
            if let Some(n) = l.get("count") {
                e["count"] = n.clone();
            }
            e
        })
        .collect();
    let summary = loss_summary(losses).unwrap_or_else(|| json!([]));
    let counts: Vec<Value> =
        summary.as_array().map(|a| a.iter().map(|e| json!({"code": e["code"], "count": e["count"]})).collect()).unwrap_or_default();
    let codes: Vec<Value> = summary.as_array().map(|a| a.iter().map(|e| e["code"].clone()).collect()).unwrap_or_default();
    // An empty list is never shortened.
    let shortened = !losses.is_empty();
    vec![(json!(losses), false), (json!(compact), shortened), (summary, shortened), (json!(counts), shortened), (json!(codes), shortened)]
}

/// The first `max` bytes of `text`, cut at a character boundary.
fn clip_bytes(text: &str, max: usize) -> String {
    let mut end = max.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

/// A failed command's error, cleaned of login values, with what the command said before failing
/// (a GDAL layer that couldn't be converted). It can still report losses its rollback can't take
/// back (a counted omission): their codes follow on a `losses:` line of JSON, added after cleaning
/// so a login can't rewrite them, and kept whole however long the rest is: the error and notices
/// are cut to leave it room within 8,000 characters and the output budget. When no form of the
/// list fits, a `lossesTrimmed: true` line says the codes were left out.
fn error_with_notices(e: String, s: &Session, said: &Said, tokens: usize) -> String {
    let told = rustgis_engine::data::diagnostics::redact(&e);
    let notices: Vec<&str> = s.notices_since(said.mark).iter().map(|n| n.text.as_str()).filter(|t| *t != e && *t != told).collect();
    let Some(losses) = loss_summary(&losses_since(s, said)) else {
        // Preserve the original 8,000-character diagnostic assembly cap for MCP errors;
        // the agent additionally applies its serialized output budget.
        return public_error(&if notices.is_empty() { e } else { clip(&format!("{e}\n{}", notices.join("\n")), 8000) });
    };
    let text = public_error(&if notices.is_empty() { e } else { format!("{e}\n{}", notices.join("\n")) });
    // Within the 8,000-character cap, and the agent's `{"error": …}` within its output budget,
    // the error giving way first and then the list ([`loss_forms`]).
    let fits = |t: &str| t.chars().count() <= 8000 && json!({"error": t}).to_string().len() <= output_bytes(tokens);
    let raw: Vec<Value> = losses.as_array().cloned().unwrap_or_default();
    let lines = loss_forms(&raw).into_iter().skip(2).map(|(list, _)| format!("\nlosses: {list}"));
    // The first line that fits alone, for a budget with no room for any of the error.
    let mut alone = None;
    for line in lines.chain(["\nlossesTrimmed: true".to_string()]) {
        if !fits(&line) {
            continue;
        }
        alone.get_or_insert_with(|| line.trim_start().to_string());
        if fits(&format!("{text}{line}")) {
            return format!("{text}{line}");
        }
        // The longest start of the error that leaves the line room.
        let chars: Vec<char> = text.chars().collect();
        let cut = |keep: usize| format!("{}…{line}", chars.iter().take(keep).collect::<String>());
        let (mut low, mut high) = (0, chars.len());
        while low < high {
            let mid = low + (high - low).div_ceil(2);
            if fits(&cut(mid)) { low = mid } else { high = mid - 1 }
        }
        // With room for the mark that the error was cut, as much of it as fits; without, a
        // shorter line may leave it room.
        if fits(&cut(low)) {
            return cut(low);
        }
    }
    alone.unwrap_or_else(|| clip(&text, 8000))
}

/// The losses as one entry per code, `{code, kind, count, action}`: short enough to keep whole
/// when the full list can't be. `None` for none.
fn loss_summary(losses: &[Value]) -> Option<Value> {
    let mut out: Vec<Value> = Vec::new();
    for l in losses {
        let n = l.get("count").and_then(Value::as_u64).unwrap_or(1);
        match out.iter_mut().find(|o| o["code"] == l["code"]) {
            Some(o) => o["count"] = json!(o["count"].as_u64().unwrap_or(0).saturating_add(n)),
            None => out.push(json!({"code": l["code"], "kind": l["kind"], "count": n, "action": l["action"]})),
        }
    }
    (!out.is_empty()).then(|| json!(out))
}

/// How well `words` match `hay` (all lower case): matched words, with whole-id hits counting more.
fn score(words: &[String], fields: &[&str]) -> usize {
    words
        .iter()
        .map(|w| {
            fields.iter().enumerate().map(|(i, f)| if f.contains(w.as_str()) { if i == 0 { 3 } else { 2 } } else { 0 }).max().unwrap_or(0)
        })
        .sum()
}

/// The query's words (plural `s` dropped), or an error when it has none.
fn words(a: &Value) -> Result<Vec<String>, String> {
    let words: Vec<String> = str_arg(a, "query")
        .unwrap_or_default()
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .map(|w| if w.len() > 3 { w.trim_end_matches('s') } else { w })
        .filter(|w| w.len() > 1)
        .map(str::to_string)
        .take(12)
        .collect();
    if words.is_empty() {
        return Err("query needs at least one word of two or more letters, e.g. \"basemap\"".into());
    }
    Ok(words)
}

/// Ranks command descriptions and reports project changes independently of undo snapshots.
fn search_commands(s: &Session, a: &Value) -> Result<Value, String> {
    let words = words(a)?;
    let limit = limit_arg(a, 12, 40);
    let mut hits: Vec<(usize, Value)> = s
        .registry
        .commands
        .iter()
        // Internal plumbing the model has no use for.
        .filter(|c| !matches!(c.id, "gp.finish" | "info.commands"))
        // Commands of an extension the user turned off (#1752).
        .filter(|c| s.extensions().blocks(c.id).is_none())
        .filter_map(|c| {
            let (id, label, loc, params) =
                (c.id.to_lowercase(), c.label.to_lowercase(), c.location.to_lowercase(), c.params.to_lowercase());
            let sc = score(&words, &[&id, &label, &loc, &params]);
            (sc > 0).then(|| {
                (sc, json!({"id": c.id, "label": c.label, "location": c.location, "params": c.params, "changesProject": c.changes_project()}))
            })
        })
        .collect();
    hits.sort_by_key(|h| std::cmp::Reverse(h.0));
    let total = hits.len();
    Ok(json!({"matches": total, "commands": hits.into_iter().take(limit).map(|h| h.1).collect::<Vec<_>>()}))
}

fn search_tools(a: &Value) -> Result<Value, String> {
    let words = words(a)?;
    let limit = limit_arg(a, 12, 40);
    let mut hits: Vec<(usize, Value)> = analysis::published_tools()
        .filter_map(|t| {
            let (id, name, tb, summary) = (t.id.to_lowercase(), t.name.to_lowercase(), t.toolbox.to_lowercase(), t.summary.to_lowercase());
            let sc = score(&words, &[&id, &name, &tb, &summary]);
            (sc > 0).then(|| (sc, json!({"id": t.id, "name": t.name, "toolbox": t.toolbox, "summary": clip(t.summary, 200)})))
        })
        .collect();
    hits.sort_by_key(|h| std::cmp::Reverse(h.0));
    let total = hits.len();
    Ok(json!({"matches": total, "tools": hits.into_iter().take(limit).map(|h| h.1).collect::<Vec<_>>()}))
}

fn describe_tool(a: &Value) -> Result<Value, String> {
    let id = str_arg(a, "tool").ok_or("tool is required")?;
    let t = analysis::resolve_published(&id)
        .map(|r| r.spec())
        .ok_or_else(|| format!("no published tool '{id}'; open the tools or load the suite first"))?;
    let params: Vec<Value> = t
        .params
        .iter()
        .map(|p| {
            let kind = match p.kind {
                analysis::ParamKind::Choice(c) => json!({"choice": c}),
                k => json!(format!("{k:?}")),
            };
            json!({"name": p.name, "label": p.label, "kind": kind, "required": p.required, "default": p.default, "help": clip(p.help, 300)})
        })
        .collect();
    Ok(json!({"id": t.id, "name": t.name, "toolbox": t.toolbox, "summary": t.summary, "params": params}))
}

/// The rows `read_features` reads: a vector layer's or a standalone table's.
struct RowSource<'a> {
    /// The reply's key for the name: `layer` or `table`.
    kind: &'static str,
    name: &'a str,
    ds: &'a rustgis_engine::data::Dataset,
    /// The layer's selected rows; a standalone table has no selection.
    selection: Option<&'a [usize]>,
}

/// Resolves `read_features`' `table` (a standalone table's name or id) or `layer` (a layer's name
/// or id). A `layer` text that names no layer but a standalone table reads the table: a tool's
/// output table comes back by name, and a model passes that name as `layer` as readily as `table`.
/// Layers come first, so a layer and a table sharing a name read the layer; that fallback matches
/// table names only, by [`project::find_table`]'s rule (exact case first), not ids. A `table` given
/// as null or empty text counts as absent, since models send unused optional arguments that way;
/// otherwise it is read instead of `layer`.
fn row_source<'a>(s: &'a Session, a: &Value) -> Result<RowSource<'a>, String> {
    let table = |i: usize| {
        s.project
            .tables
            .get(i)
            .map(|t| RowSource { kind: "table", name: &t.name, ds: &t.dataset, selection: None })
            .ok_or_else(|| "no such table".to_string())
    };
    let given = |v: &&Value| !v.is_null() && v.as_str().is_none_or(|t| !t.trim().is_empty());
    if let Some(t) = a.get("table").filter(given) {
        return table(s.table_index(&json!({"table": t}))?);
    }
    let layer = a.get("layer").cloned().ok_or("layer (or table) is required")?;
    let id = match s.layer_id(&json!({"layer": layer})) {
        Ok(id) => id,
        Err(e) => {
            // Untrimmed, as `table` and layer names are matched: `stats` and `stats ` can both exist.
            let Some(n) = layer.as_str() else { return Err(e) };
            let tables = &s.project.tables;
            // `find_table` falls back to an id; keep only a name match, so `layer: "7"` can't read table 7.
            let by_name = project::find_table(tables, n).filter(|&i| tables.get(i).is_some_and(|t| t.name.eq_ignore_ascii_case(n)));
            return match by_name {
                Some(i) => table(i),
                None => Err(format!("no layer or standalone table named '{n}'")),
            };
        }
    };
    let l = s.map().layer(id).ok_or("no such layer")?;
    if l.is_raster() {
        return Err(format!("{} is a raster layer; it has no rows (try the raster.info command)", l.name));
    }
    Ok(RowSource { kind: "layer", name: &l.name, ds: &l.dataset, selection: Some(&l.selection) })
}

/// `read_features`: the rows of a layer or standalone table ([`row_source`]) that match `where` and
/// `selected`, ordered by `order_by`, as `{layer | table, total, matched, rows}`. Only the first
/// `limit` rows are returned, each with its `_row`, the chosen `fields` and a layer feature's
/// `_lon`/`_lat`.
fn read_features(s: &Session, a: &Value) -> Result<Value, String> {
    let src = row_source(s, a)?;
    let (name, ds) = (src.name, src.ds);
    let mut rows: Vec<usize> = if a.get("selected").and_then(Value::as_bool) == Some(true) {
        let sel = src.selection.ok_or_else(|| format!("{name} is a standalone table; it has no selection"))?;
        sel.iter().copied().filter(|&r| r < ds.features.len()).collect()
    } else {
        (0..ds.features.len()).collect()
    };
    if let Some(w) = str_arg(a, "where").filter(|w| !w.is_empty()) {
        let hits: std::collections::HashSet<usize> = rustgis_engine::data::query::select(&w, ds)?.into_iter().collect();
        rows.retain(|r| hits.contains(r));
    }
    let fields: Vec<usize> = match a.get("fields").and_then(Value::as_array) {
        Some(names) if !names.is_empty() => {
            let mut v = Vec::new();
            for n in names {
                let n = n.as_str().ok_or("fields must be field names (strings)")?;
                let i =
                    ds.fields.iter().position(|f| f.name.eq_ignore_ascii_case(n)).ok_or_else(|| format!("{name} has no field '{n}'"))?;
                v.push(i);
            }
            v
        }
        _ => (0..ds.fields.len()).collect(),
    };
    if let Some(by) = str_arg(a, "order_by").filter(|f| !f.is_empty()) {
        // SQL habits: `pop_max DESC`.
        let lower = by.to_ascii_lowercase();
        let (by, desc) = if let Some(f) = lower.strip_suffix(" desc") {
            (by.get(..f.len()).unwrap_or("").trim().to_string(), true)
        } else if let Some(f) = lower.strip_suffix(" asc") {
            (by.get(..f.len()).unwrap_or("").trim().to_string(), false)
        } else {
            (by, a.get("descending").and_then(Value::as_bool) == Some(true))
        };
        let fi = ds.fields.iter().position(|f| f.name.eq_ignore_ascii_case(&by)).ok_or_else(|| format!("{name} has no field '{by}'"))?;
        // A total order (a partial one can make `sort_by` panic): numbers and dates (by instant),
        // then text, then empty values.
        let key = |r: usize| -> (u8, f64, i64, String) {
            match ds.features.get(r).and_then(|f| f.attrs.get(fi)) {
                Some(rustgis_engine::data::Value::Null) | None => (2, 0.0, 0, String::new()),
                Some(v) => match (v.as_f64(), v.temporal_key()) {
                    (Some(x), _) => (0, x, 0, String::new()),
                    (None, Some(k)) => (0, 0.0, k, String::new()),
                    (None, None) => (1, 0.0, 0, v.to_string().to_lowercase()),
                },
            }
        };
        let mut keyed: Vec<(usize, (u8, f64, i64, String))> = rows.iter().map(|&r| (r, key(r))).collect();
        // Descending reverses within numbers and within text; empty values stay last.
        keyed.sort_by(|(_, a), (_, b)| {
            let within = a.1.total_cmp(&b.1).then(a.2.cmp(&b.2)).then_with(|| a.3.cmp(&b.3));
            a.0.cmp(&b.0).then(if desc { within.reverse() } else { within })
        });
        rows = keyed.into_iter().map(|(r, _)| r).collect();
    }
    let limit = limit_arg(a, 20, 200);
    let lonlat = ds.lonlat_conversion()?;
    let out: Vec<Value> = rows
        .iter()
        .take(limit)
        .filter_map(|&r| {
            let f = ds.features.get(r)?;
            let mut o = serde_json::Map::new();
            o.insert("_row".into(), json!(r));
            // Where it is (its extent's centre, in longitude/latitude), for zooming or going to it.
            // Left out where it isn't a place (`point` gives only finite ones).
            if let Some(c) = f.geometry.as_ref().and_then(|g| lonlat.point(g.extent().center())) {
                o.insert("_lon".into(), json!((c.x * 1e5).round() / 1e5));
                o.insert("_lat".into(), json!((c.y * 1e5).round() / 1e5));
            }
            for &fi in &fields {
                let (Some(field), Some(v)) = (ds.fields.get(fi), f.attrs.get(fi)) else { continue };
                let v = match v {
                    rustgis_engine::data::Value::Null => Value::Null,
                    rustgis_engine::data::Value::Int(i) => json!(i),
                    rustgis_engine::data::Value::Float(x) => json!(x),
                    rustgis_engine::data::Value::Bool(b) => json!(b),
                    rustgis_engine::data::Value::Text(t) => json!(clip(t, 200)),
                    d @ (rustgis_engine::data::Value::Date(_) | rustgis_engine::data::Value::DateTime(_)) => json!(d.to_string()),
                };
                o.insert(field.name.clone(), v);
            }
            Some(Value::Object(o))
        })
        .collect();
    let mut reply = serde_json::Map::new();
    reply.insert(src.kind.into(), json!(name));
    reply.insert("total".into(), json!(ds.features.len()));
    reply.insert("matched".into(), json!(rows.len()));
    reply.insert("rows".into(), Value::Array(out));
    Ok(Value::Object(reply))
}

/// The map as the model sees it at the start of a request: one line per layer.
pub fn context(s: &Session) -> String {
    let m = s.map();
    let c = m.center_lonlat();
    let mut out = format!(
        "Map \"{}\": basemap {}, projection {}, centre {:.4}, {:.4} (lon, lat), scale 1:{:.0}.",
        m.name,
        m.basemap.code(),
        m.projection.code(),
        c.x,
        c.y,
        m.scale()
    );
    if let Some(l) = s.active_layer.and_then(|id| m.layer(id)) {
        out.push_str(&format!(" Selected in Layers: \"{}\".", l.name));
    }
    if m.layers.is_empty() {
        out.push_str("\nNo layers.");
    } else {
        out.push_str("\nLayers (top first):");
    }
    for l in m.layers.iter().take(60) {
        let mut line = if l.is_raster() {
            format!("\n- \"{}\" (id {}, raster", l.name, l.id)
        } else {
            let size = match (l.feature_count(), l.is_streamed()) {
                (Some(n), false) => format!("{n} features"),
                (Some(n), true) => format!("{n} features, read in place"),
                (None, _) => "an unknown number of features, read in place".to_string(),
            };
            format!("\n- \"{}\" (id {}, {}, {size}", l.name, l.id, l.geometry_type().label())
        };
        if !l.visible {
            line.push_str(", hidden");
        }
        let groups = m.groups_of(l.id);
        if !groups.is_empty() {
            // Outermost first: "Base map › Hydrography".
            let path: Vec<&str> = groups.iter().rev().map(|g| g.name.as_str()).collect();
            let hidden = if groups.iter().all(|g| g.visible) { "" } else { " (hidden)" };
            line.push_str(&format!(", in group \"{}\"{hidden}", path.join(" › ")));
        }
        if !l.selection.is_empty() {
            line.push_str(&format!(", {} selected", l.selection.len()));
        }
        line.push(')');
        if !l.dataset.fields.is_empty() {
            let fields: Vec<String> = l.dataset.fields.iter().take(40).map(|f| format!("{}:{}", f.name, f.ty.label())).collect();
            line.push_str(&format!(" fields: {}", fields.join(", ")));
        }
        out.push_str(&clip(&line, 1500));
    }
    if m.layers.len() > 60 {
        out.push_str(&format!("\n… and {} more (list_layers lists them all).", m.layers.len() - 60));
    }
    if !m.tiles.is_empty() {
        out.push_str("\nWeb tile layers (top first, index for map.setTiles), each drawn over the layer it names, or under them all:");
    }
    let slots = m.tile_slots();
    for (i, t) in m.tiles.iter().enumerate() {
        // The name is clipped, not the line: the index and flags after it are what the model needs.
        let mut line = format!("\n- \"{}\" (index {i}, {}", clip(&t.name, 200), t.kind.code());
        if t.overlay {
            line.push_str(", overlay");
        }
        match slots.get(i).copied().flatten().and_then(|id| m.layer(id)) {
            Some(l) => line.push_str(&format!(", over \"{}\"", clip(&l.name, 200))),
            None => line.push_str(", under the layers"),
        }
        if !t.visible {
            line.push_str(", hidden");
        } else if let Some(e) = &t.error {
            line.push_str(&format!(", unavailable: {e}"));
        } else if !t.draws() {
            line.push_str(", hidden by the time slider (no date in its window)");
        }
        if t.opacity < 1.0 {
            line.push_str(&format!(", opacity {:.2}", t.opacity));
        }
        line.push(')');
        out.push_str(&line);
    }
    out
}

#[cfg(test)]
mod tests {
    /// Returned tool and fetch errors are cleaned before active patterns can damage URLs;
    /// successful values and the original call remain outside the generated-error projection.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn generated_tool_and_fetch_errors_clean_addresses_before_active_patterns() {
        const TEST: &str = "tools::tests::generated_tool_and_fetch_errors_clean_addresses_before_active_patterns";
        if std::env::var("RUSTGIS_ISOLATED_TOOL_ERRORS").as_deref() != Ok(TEST) {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", TEST])
                .env("RUSTGIS_ISOLATED_TOOL_ERRORS", TEST)
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && stdout.contains("1 passed; 0 failed"),
                "{stdout}\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        use rustgis_engine::render::rustgis_tiles::{failure::Failure, logins};
        let mut s = Session::new(rustgis_engine::Project::default());
        let url = "https://returned-error.invalid/a?token=RGSYNTH_returned_error&layer=roads";
        for active in [false, true] {
            let binding = active.then(|| {
                logins::activate(
                    "https://returned-error.invalid",
                    "synthetic-returned-error",
                    logins::Method::Header("X-Key".into()),
                    "https",
                    false,
                )
                .unwrap()
            });
            for (name, args) in [
                ("read_features", json!({"layer":url})),
                ("describe_tool", json!({"tool":url})),
                (url, json!({})),
                ("run", json!({"script":url})),
            ] {
                let call = ToolCall { id: "error".into(), name: name.into(), args };
                let original = call.clone();
                let error = run_with_budget(&mut s, &call, DEFAULT_OUTPUT_TOKENS, Scope::peer(), None).unwrap_err();
                assert!(!error.contains("RGSYNTH_returned_error"), "{name}, active={active}: {error}");
                assert_eq!(call, original);
                if name == "run" {
                    let reply: Value = serde_json::from_str(&error).unwrap();
                    assert_eq!(reply["success"], false);
                    assert!(reply["error"].as_str().is_some());
                }
            }
            let address = binding.as_ref().map_or_else(|| url.to_string(), |binding| logins::bind(binding, url).unwrap());
            let failure = Failure::new(&address, format!("failed to fetch {address}"));
            assert!(!failure.message.contains("RGSYNTH_returned_error"));
            if let Some(binding) = binding {
                assert!(logins::deactivate(&binding));
            }
        }
        let clean = ToolCall { id: "clean".into(), name: "read_features".into(), args: json!({"layer":"ordinary missing layer"}) };
        assert!(
            run_with_budget(&mut s, &clean, DEFAULT_OUTPUT_TOKENS, Scope::peer(), None).unwrap_err().contains("ordinary missing layer")
        );
        assert_eq!(Failure::new("https://clean.invalid/a?layer=roads", "HTTP 403").message, "HTTP 403");
        assert_eq!(logins::scrub_error("failed https://clean.invalid/a?layer=roads"), "failed https://clean.invalid/a?layer=roads");
    }
    use super::*;
    use rustgis_engine::render::rustgis_tiles::logins;

    #[test]
    fn results_carry_plain_addresses_not_login_wrappers() {
        let mut s = Session::new(rustgis_engine::Project::default());
        let origin = "https://assistant-login757.example.org";
        let definition = json!({"kind":"wmts", "address": format!("{origin}/tiles"), "auth":"header", "keyName":"X-Key"});
        let id = s.run("connection.new", &definition).unwrap()["id"].as_str().unwrap().to_string();
        s.run("connection.login", &json!({"id": id, "apiKey": "assistant-secret-757"})).unwrap();
        let plain = format!("{origin}/tiles/{{z}}/{{x}}/{{y}}.png");
        let tile = rustgis_engine::service_logins::request(&mut s, Some(&id), &plain).unwrap();
        assert!(tile.starts_with(rustgis_engine::data::login_address::PREFIX));
        // The app's command result keeps the bound address it reads through; the model's doesn't.
        let call = |command: &str, params: Value| ToolCall {
            id: String::new(),
            name: "run_command".into(),
            args: json!({"command": command, "params": params}),
        };
        let scope = Scope::assistant(757, std::sync::Arc::new(Live::default()));
        let add = call("map.addTiles", json!({"url": tile, "name": "T"}));
        let pending = match s.review(&scope, &policy_call(&add).unwrap().unwrap()) {
            Outcome::NeedsApproval(pending) => Some(pending),
            _ => None,
        }
        .unwrap();
        let added = run_with_budget(&mut s, &add, DEFAULT_OUTPUT_TOKENS, scope.clone(), Some(pending.allow())).unwrap();
        let listed = run_with_budget(&mut s, &call("map.tiles", json!({})), DEFAULT_OUTPUT_TOKENS, scope, None).unwrap();
        assert!(s.map().tiles[0].url.starts_with("rustgis-login:"));
        for text in [&added, &listed] {
            assert!(!text.contains("rustgis-login:") && !text.contains("assistant-secret-757"), "{text}");
        }
        assert!(listed.contains(&plain), "the model still learns the layer's address: {listed}");
    }

    /// The known response schema survives a secret that happens to name one of its fields.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn run_envelope_keys_are_not_data_derived_login_values() {
        if std::env::var_os("RUSTGIS_TEST_RUN_ENVELOPE").is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "tools::tests::run_envelope_keys_are_not_data_derived_login_values"])
                .env("RUSTGIS_TEST_RUN_ENVELOPE", "1")
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && stdout.contains("1 passed; 0 failed"),
                "{stdout}\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let mut s = Session::new(rustgis_engine::Project::default());
        let id = s
            .run("connection.new", &json!({"kind":"wms", "address":"https://envelope.invalid/wms", "auth":"basic", "user":"reader"}))
            .unwrap()["id"]
            .clone();
        s.run("connection.login", &json!({"id":id, "password":"success"})).unwrap();
        let scope = Scope::assistant(152140, std::sync::Arc::new(Live::default()));
        for (script, success) in [("map.zoomIn {}", true), ("not.a.command {}", false)] {
            let call = ToolCall { id: "envelope".into(), name: "run".into(), args: json!({"script":script}) };
            let text = run(&mut s, &call, scope.clone(), None).unwrap_or_else(|e| e);
            let out: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(out.get("success"), Some(&json!(success)), "{text}");
            assert!(out.get("result").is_some() && out.get("notices").is_some() && out.get("duration_s").is_some());
        }
        let data = public_addresses(json!({"success":"private field"}), 0, &mut Withheld::default());
        assert_eq!(data["[redacted]"], "private field", "the same spelling in dynamic data is still scrubbed");
        s.run("connection.login", &json!({"id":id, "password":"id"})).unwrap();
        for (name, args, array) in
            [("search_commands", json!({"query":"zoom"}), "commands"), ("search_tools", json!({"query":"buffer"}), "tools")]
        {
            let call = ToolCall { id: "metadata".into(), name: name.into(), args };
            let text = run(&mut s, &call, scope.clone(), None).unwrap();
            let out: Value = serde_json::from_str(&text).unwrap();
            let entries = out.get(array).and_then(Value::as_array).unwrap();
            assert!(!entries.is_empty(), "{text}");
            assert!(entries.iter().all(|entry| entry.get("id").is_some()), "fixed nested schema keys survive: {text}");
        }
        let data = public_addresses(json!({"id":"private field"}), 0, &mut Withheld::default());
        assert_eq!(data["[redacted]"], "private field");
    }

    /// Command discovery distinguishes publishing's project change from a read-only query.
    #[test]
    fn publishing_reports_a_project_change_in_command_search() {
        let s = Session::new(rustgis_engine::Project::default());
        for (id, changes) in [("source.publish", true), ("source.syncState", false)] {
            let out = search_commands(&s, &json!({"query":id, "limit":40})).unwrap();
            let command = out["commands"].as_array().unwrap().iter().find(|c| c["id"] == id).unwrap();
            assert_eq!(command["changesProject"], changes, "{id}");
        }
    }

    #[test]
    fn wrappers_after_a_prefix_are_made_plain_too() {
        let origin = "https://assistant-login.example";
        let binding = logins::activate(origin, "assistant", logins::Method::Header("X-Key".into()), "assistant-secret", false).unwrap();
        let archive = logins::bind(&binding, &format!("{origin}/a.pmtiles")).unwrap();
        let result = json!({"layers": [{"next": {"request": {"url": format!("pmtiles:x@{archive}")}}}], "count": 2});
        let v = public_addresses(result, 0, &mut Withheld::default());
        assert_eq!(v["layers"][0]["next"]["request"]["url"], format!("pmtiles:x@{origin}/a.pmtiles"));
        assert_eq!(v["count"], 2);
        assert!(logins::deactivate(&binding));
    }

    #[test]
    fn too_deep_results_report_each_withheld_subtree_without_losing_success() {
        let mut s = Session::new(rustgis_engine::Project::default());
        let mut deep = json!({"value": "rustgis-login:leak"});
        for _ in 0..MAX_DEPTH {
            deep = json!([deep]);
        }
        let result = json!({"success": true, "left": deep.clone(), "right": deep, "notices": ["existing warning"]});
        let out: Value = serde_json::from_str(&public_result(&mut s, result, DEFAULT_OUTPUT_TOKENS)).unwrap();
        assert_eq!(out["success"], true, "withholding output must not report an executed command as failed");
        assert!(!out.to_string().contains("rustgis-login:"));
        assert_eq!(out["notices"][0], "existing warning");
        let notice = out["notices"][1].as_str().unwrap();
        let counts: Vec<usize> = notice.split_whitespace().filter_map(|word| word.parse().ok()).collect();
        assert_eq!(counts, [2, MAX_DEPTH], "the notice reports truncated subtrees, not an unbounded count of their contents");
        assert_eq!(s.notices_since(0).len(), 1, "the app also receives one aggregate loss notice");
    }

    /// A notice that reports lost data comes with its code in `losses`, beside the notice text;
    /// a run that loses nothing has no `losses`, and a script's answer an empty list.
    #[test]
    fn results_give_the_codes_of_the_losses_their_notices_report() {
        let mut s = Session::new(rustgis_engine::Project::default());
        // An MCP agent the server lets act without asking (`--yes`).
        let scope = Scope::agent(rustgis_engine::policy::AgentAccess::Full, true, "loss-codes", None, std::sync::Arc::new(Live::default()));
        const WMS: &str = r#"<WMS_Capabilities version="1.3.0" xmlns="http://www.opengis.net/wms" xmlns:xlink="http://www.w3.org/1999/xlink">
<Service><Title>Test WMS</Title></Service>
<Capability><Request><GetMap><Format>image/png</Format><DCPType><HTTP><Get><OnlineResource xlink:href="https://maps.example.com/wms?"/></Get></HTTP></DCPType></GetMap></Request>
<Layer><CRS>EPSG:3857</CRS><Layer><Name>roads</Name><Title>Roads</Title>
<Style><Name>default</Name><LegendURL><Format>image/png</Format><OnlineResource xlink:href="LEGEND"/></LegendURL></Style></Layer></Layer></Capability></WMS_Capabilities>"#;
        // A legend picture whose address is too long to keep is left out, and said with its code.
        let long = format!("https://maps.example.com/legends/{}.png", "a".repeat(5000));
        let params = |legend: &str| json!({"url": "https://maps.example.com/wms", "layer": "roads", "text": WMS.replace("LEGEND", legend)});
        let lossy = params(&long);
        let clean = params("https://maps.example.com/legends/roads.png");
        let add = |params: &Value| ToolCall {
            id: "add".into(),
            name: "run_command".into(),
            args: json!({"command": "service.add", "params": params}),
        };
        let run = |s: &mut Session, call: &ToolCall| -> Value {
            serde_json::from_str(&run_with_budget(s, call, DEFAULT_OUTPUT_TOKENS, scope.clone(), None).unwrap()).unwrap()
        };
        let out = run(&mut s, &add(&lossy));
        let [loss] = out["losses"].as_array().map(Vec::as_slice).unwrap_or_default() else { panic!("one loss: {out}") };
        assert_eq!((&loss["code"], &loss["kind"]), (&json!("dropped.legend"), &json!("dropped")), "{out}");
        assert_eq!(loss["action"], "Use a service with short HTTP or HTTPS addresses for its legend pictures.");
        assert!(out["notices"].as_array().unwrap().iter().any(|n| n == &loss["text"]), "the loss is a notice too: {out}");
        assert!(run(&mut s, &add(&clean)).get("losses").is_none());
        let line = |params: &Value| format!("service.add {params}");
        let script = |text: String| ToolCall { id: "script".into(), name: "run".into(), args: json!({"script": text}) };
        let out = run(&mut s, &script(line(&lossy)));
        assert_eq!(out["losses"][0]["code"], "dropped.legend", "{out}");
        assert_eq!(run(&mut s, &script(line(&clean)))["losses"], json!([]));
        // A failed command still says the loss its rollback can't take back, with its code.
        let failing = format!("{}\nlayer.addDataText {}", line(&lossy), json!({"fileName": "broken.geojson", "text": "{"}));
        let command = ToolCall {
            id: "fail".into(),
            name: "run_command".into(),
            args: json!({"command": "script.run", "params": {"script": failing}}),
        };
        let error = run_with_budget(&mut s, &command, DEFAULT_OUTPUT_TOKENS, scope.clone(), None).unwrap_err();
        assert!(error.contains(r#"losses: [{"action":"Use a service"#) && error.contains(r#""code":"dropped.legend""#), "{error}");
        let error = run_with_budget(&mut s, &script(failing), DEFAULT_OUTPUT_TOKENS, scope.clone(), None).unwrap_err();
        let failed: Value = serde_json::from_str(&error).unwrap();
        assert_eq!((&failed["success"], &failed["losses"][0]["code"]), (&json!(false), &json!("dropped.legend")), "{failed}");
        // A login value that matches a code's words cleans the notice's text, never the code.
        let id = s
            .run(
                "connection.new",
                &json!({"kind":"wms","address":"https://echo-2371.example.invalid/wms","auth":"header","keyName":"X-Key"}),
            )
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        s.run("connection.login", &json!({"id": id, "apiKey": "legend"})).unwrap();
        let out = run(&mut s, &script(line(&lossy)));
        assert_eq!(out["losses"][0]["code"], "dropped.legend", "{out}");
        assert!(out["losses"][0].get("code").is_some() && !out["losses"][0]["text"].as_str().unwrap().contains("legend"), "{out}");
        let error = run_with_budget(&mut s, &command, DEFAULT_OUTPUT_TOKENS, scope.clone(), None).unwrap_err();
        assert!(error.contains(r#""code":"dropped.legend""#), "a failure's code isn't cleaned either: {error}");
        // An error too long to keep whole is cut, never the losses after it.
        let long = format!("{}\nmap.drawingMode {}", line(&lossy), json!({"mode": "x".repeat(9000)}));
        let command =
            ToolCall { id: "long".into(), name: "run_command".into(), args: json!({"command": "script.run", "params": {"script": long}}) };
        let error = run_with_budget(&mut s, &command, DEFAULT_OUTPUT_TOKENS, scope.clone(), None).unwrap_err();
        let last = error.lines().last().unwrap_or_default();
        assert!(last.starts_with("losses: [") && last.contains(r#""code":"dropped.legend""#), "{last}");
        assert!(error.chars().count() <= 8000, "{}", error.chars().count());
        // Within a small output budget too, counted in the bytes of the agent's `{"error": …}`,
        // with text of several bytes a character.
        let wide = format!("{}\nmap.drawingMode {}", line(&lossy), json!({"mode": "界".repeat(3000)}));
        let command =
            ToolCall { id: "wide".into(), name: "run_command".into(), args: json!({"command": "script.run", "params": {"script": wide}}) };
        let error = run_with_budget(&mut s, &command, 200, scope.clone(), None).unwrap_err();
        assert!(json!({"error": error}).to_string().len() <= output_bytes(200), "{error}");
        assert!(error.lines().last().unwrap_or_default().contains("dropped.legend"), "{error}");
        // With no room for even the codes, a line says they were left out.
        let error = run_with_budget(&mut s, &command, 9, scope.clone(), None).unwrap_err();
        assert!(error.ends_with("lossesTrimmed: true"), "{error}");
    }

    /// Losses that don't fit the output budget beside the result keep only their codes, so the
    /// result stays whole; when even those don't fit, one entry per code is kept and the rest of
    /// the result is cut to leave it room.
    #[test]
    fn losses_beyond_the_output_budget_keep_their_codes() {
        let small_loss = || json!({"code": "dropped.value", "kind": "dropped", "text": "t", "action": "a"});
        let mut s = Session::new(rustgis_engine::Project::default());
        let mut finish = |r: Value, losses: Vec<Value>, always: bool| -> Value {
            serde_json::from_str(&finish_with_losses(&mut s, r, losses, 200, Withheld::default(), always)).unwrap()
        };
        let loss = json!({"code": "dropped.value", "kind": "dropped", "text": "x".repeat(600), "action": "Correct the value."});
        let out = finish(json!({"notices": ["n"]}), vec![loss.clone(), loss.clone()], false);
        assert_eq!(out["losses"], json!([{"code": "dropped.value", "kind": "dropped"}, {"code": "dropped.value", "kind": "dropped"}]));
        assert_eq!((&out["lossesTrimmed"], &out["notices"]), (&json!(true), &json!(["n"])));
        // An entry for losses whose notices weren't kept keeps its count when shortened.
        let unkept = json!({"code": "dropped.value", "kind": "dropped", "count": 250, "text": "x".repeat(900), "action": "a"});
        let out = finish(json!({"notices": ["n"]}), vec![unkept], false);
        assert_eq!(out["losses"], json!([{"code": "dropped.value", "kind": "dropped", "count": 250}]), "{out}");
        let small = json!({"code": "dropped.value", "kind": "dropped", "text": "t", "action": "a"});
        let out = finish(json!({"notices": ["n"]}), vec![small.clone()], false);
        assert_eq!((&out["losses"], out.get("lossesTrimmed")), (&json!([small]), None));
        // A result over the budget by itself is cut, keeping its `success` and one entry per code
        // with its count; a tighter budget keeps the codes alone, and the tightest says only that
        // they were left out, always as JSON within the budget.
        let big = json!({"success": true, "notices": ["n"], "result": "y".repeat(5000)});
        let text = finish_with_losses(&mut s, big.clone(), vec![loss.clone(), loss.clone()], 200, Withheld::default(), false);
        assert!(text.len() <= output_bytes(200), "{}", text.len());
        let out: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(out["losses"], json!([{"code": "dropped.value", "kind": "dropped", "count": 2, "action": "Correct the value."}]));
        assert_eq!((&out["lossesTrimmed"], &out["truncated"], &out["success"]), (&json!(true), &json!(true), &json!(true)));
        // `run`'s success, whose `error` is null, is cut the same way.
        let run_ok = json!({"success": true, "error": null, "notices": ["n"], "result": "y".repeat(5000)});
        for (tokens, big) in [(32, &big), (16, &big), (32, &run_ok), (16, &run_ok)] {
            let text = finish_with_losses(&mut s, big.clone(), vec![loss.clone()], tokens, Withheld::default(), false);
            let out: Value = serde_json::from_str(&text).unwrap();
            assert!(text.len() <= output_bytes(tokens), "{tokens}: {text}");
            assert_eq!((&out["lossesTrimmed"], &out["success"]), (&json!(true), &json!(true)), "{tokens}: {text}");
            // A success cut this far is never said as an error, and always said to be cut: the
            // codes go first.
            assert!(out.get("error").is_none(), "{tokens}: {text}");
            assert_eq!(out["truncated"], json!(true), "{tokens}: {text}");
            assert_eq!(out["losses"].to_string().contains("dropped.value"), tokens == 32, "{tokens}: {text}");
        }
        for tokens in 1..=8 {
            let text = finish_with_losses(&mut s, big.clone(), vec![loss.clone()], tokens, Withheld::default(), false);
            assert!(text.len() <= output_bytes(tokens), "{tokens}: {text}");
        }
        // A cut result with no losses says it was cut, not that a list was.
        let text = finish_with_losses(&mut s, run_ok.clone(), vec![], 32, Withheld::default(), true);
        let out: Value = serde_json::from_str(&text).unwrap();
        assert_eq!((&out["truncated"], out.get("lossesTrimmed"), &out["losses"]), (&json!(true), None, &json!([])), "{out}");
        // A failure with its own `losses`, cut, keeps its `success` and says it was cut.
        let own = json!({"success": false, "error": "e".repeat(5000), "losses": 3});
        let text = finish_with_losses(&mut s, own, vec![loss.clone()], 200, Withheld::default(), true);
        let out: Value = serde_json::from_str(&text).unwrap();
        assert_eq!((&out["success"], &out["truncated"]), (&json!(false), &json!(true)), "{out}");
        // A failure whose error gives way says so, at every budget.
        for tokens in 12..=24 {
            let failed = json!({"success": false, "error": "e".repeat(5000)});
            let text = finish_with_losses(&mut s, failed, vec![loss.clone()], tokens, Withheld::default(), true);
            let out: Value = serde_json::from_str(&text).unwrap();
            assert!(text.len() <= output_bytes(tokens), "{tokens}: {text}");
            assert_eq!(out["truncated"], json!(true), "{tokens}: {text}");
        }
        // A failure's error alone gives way to its losses, as JSON.
        let failed = json!({"success": false, "error": "e".repeat(5000)});
        let text = finish_with_losses(&mut s, failed, vec![loss.clone()], 200, Withheld::default(), true);
        assert!(text.len() <= output_bytes(200), "{}", text.len());
        let out: Value = serde_json::from_str(&text).unwrap();
        assert_eq!((&out["success"], &out["losses"][0]["code"]), (&json!(false), &json!("dropped.value")), "{out}");
        assert!(out["error"].as_str().unwrap().starts_with("error: eee"), "the error as text, not encoded twice: {out}");
        assert_eq!(out["truncated"], json!(true), "the error is said to be cut: {out}");
        let mut finish = |r: Value, always: bool| -> Value {
            serde_json::from_str(&finish_with_losses(&mut s, r, vec![], 200, Withheld::default(), always)).unwrap()
        };
        assert_eq!(finish(json!({"notices": ["n"]}), true)["losses"], json!([]));
        assert!(finish(json!({"notices": ["n"]}), false).get("losses").is_none());
        // A result that isn't an object, or has its own `losses`, goes under `result`, beside the codes.
        let out: Value =
            serde_json::from_str(&finish_with_losses(&mut s, json!([1, 2]), vec![small_loss()], 200, Withheld::default(), false)).unwrap();
        assert_eq!((&out["result"], &out["losses"][0]["code"]), (&json!([1, 2]), &json!("dropped.value")), "{out}");
        let own = json!({"losses": 3});
        let out: Value =
            serde_json::from_str(&finish_with_losses(&mut s, own.clone(), vec![small_loss()], 200, Withheld::default(), false)).unwrap();
        assert_eq!((&out["result"], &out["losses"][0]["code"]), (&own, &json!("dropped.value")), "{out}");
    }

    #[test]
    fn successful_results_scrub_active_raw_service_keys_without_a_wrapper() {
        let mut s = Session::new(rustgis_engine::Project::default());
        let id = s
            .run(
                "connection.new",
                &json!({"kind":"wms","address":"https://echo-r15.example.invalid/wms","auth":"header","keyName":"X-Key"}),
            )
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        s.run("connection.login", &json!({"id":id,"apiKey":"fake001"})).unwrap();
        let result = json!({"success":true,"echo":"server said fake001","clean":"ordinary café text"});
        let out: Value = serde_json::from_str(&public_result(&mut s, result, DEFAULT_OUTPUT_TOKENS)).unwrap();
        assert_eq!(out["success"], true);
        assert_eq!(out["echo"], "server said [redacted]");
        assert_eq!(out["clean"], "ordinary café text");
        assert!(s.notices_since(0).is_empty());
        // A generated row uses imported field names as object keys. Keep the stored data,
        // but never send a field repeating an active key to the model in plain text.
        let source = json!({"type":"FeatureCollection","features":[{
            "type":"Feature","geometry":{"type":"Point","coordinates":[1,2]},
            "properties":{"fake001":"retained value","clean":"ordinary café text"}
        }]});
        s.run("layer.addDataText", &json!({"fileName":"echo-fields.geojson","text":source.to_string()})).unwrap();
        let name = s.map().layers.last().unwrap().name.clone();
        let call = ToolCall { id: "echo-fields".into(), name: "read_features".into(), args: json!({"layer":name,"limit":1}) };
        let scope = Scope::assistant(9035, std::sync::Arc::new(Live::default()));
        let mark = s.notice_mark();
        let text = run_with_budget(&mut s, &call, DEFAULT_OUTPUT_TOKENS, scope, None).unwrap();
        assert!(!text.contains("fake001"), "generated object keys must be scrubbed too: {text}");
        let out: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(out["rows"][0]["[redacted]"], "retained value");
        assert_eq!(out["rows"][0]["clean"], "ordinary café text");
        assert!(s.map().layers.last().unwrap().dataset.fields.iter().any(|f| f.name == "fake001"), "stored data stays unchanged");
        assert!(s.notices_since(mark).is_empty());
        // Two keys that become equal must not silently overwrite one value with the other.
        let mark = s.notice_mark();
        let collision = json!({"success":true,"result":{"fake001":"first value","[redacted]":"second value"}});
        let text = public_result(&mut s, collision, DEFAULT_OUTPUT_TOKENS);
        assert!(!text.contains("fake001"));
        let out: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(out["success"], true);
        assert!(out["result"].is_string(), "ambiguous redacted objects are explicitly withheld");
        assert_eq!(s.notices_since(mark).len(), 1);
        assert!(s.notices_since(mark)[0].text.contains("field names"));
    }

    #[test]
    fn clean_nested_results_need_no_withholding_notice() {
        let mut s = Session::new(rustgis_engine::Project::default());
        let result = json!({"result": {"nested": [{"value": "complete"}]}, "success": true});
        let out: Value = serde_json::from_str(&public_result(&mut s, result.clone(), DEFAULT_OUTPUT_TOKENS)).unwrap();
        assert_eq!(out, result);
        assert!(s.notices_since(0).is_empty());
    }
}
