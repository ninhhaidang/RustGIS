//! Publish › Report (New Report) and Data, and Publish › Save As (Report
//! File, Report Template) and Output (Export Report): reports on a layer's records.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use rustgis_map::Layer;
use rustgis_map::report::{Block, Doc, MAX_GROUPS, MAX_ITEMS, Report, ReportChart, Section, Stat, StatKind};
use rustgis_map::symbology::fmt_num;
use serde_json::{Value, json};

use super::{b, f, ps};
use crate::{CmdResult, CommandSpec as C, Session};

/// Report files and report templates: JSON with these extensions.
pub const REPORT_EXTENSION: &str = "rgisreport";
pub const TEMPLATE_EXTENSION: &str = "rgisrpttemplate";
/// Most records a report lists.
pub const MAX_RECORDS: usize = 50_000;
/// Most pictures (dynamic pictures and image attachments) a report embeds, and most base64 bytes
/// they hold together.
const MAX_PICTURES: usize = 500;
const PICTURE_BUDGET: usize = if cfg!(target_arch = "wasm32") { 64 << 20 } else { 256 << 20 };
/// Most related reports followed, one inside another.
const MAX_DEPTH: usize = 3;
/// Most related records listed under one record.
const MAX_RELATED: usize = 200;
/// Largest PDF a report export holds (less in the browser).
const PDF_BUDGET: usize = if cfg!(target_arch = "wasm32") { 256 << 20 } else { 1 << 30 };

fn need_report(s: &Session) -> Option<&'static str> {
    if s.project.reports.is_empty() { Some("add a report first (Publish › New Report)") } else { None }
}

pub fn commands() -> Vec<C> {
    vec![
        C::new("report.new", "New Report", "Publish › Report", new_report).params(
            r#"{"layer"?: name (default: the selected layer), "name"?, "title"?, "fields"?: [names] (default: all), "where"?: SQL, "selectedOnly"?: bool, "groups"?: [fields], "sort"?: field, "descending"?: bool, "page"?: "letter"|"a4"|…, "orientation"?: "portrait"|"landscape"}"#,
        ),
        C::new("report.update", "Report Properties", "Report", update).when(need_report).params(
            r#"{"report"?: name|index, "name"?, "title"?, "layer"?, "where"?, "selectedOnly"?, "sort"?: field|null, "descending"?, "page"?, "orientation"?, "width"?, "height"?: mm}"#,
        ),
        C::new("report.activate", "Open Report", "Browser", |s, p| {
            let i = index(s, p)?;
            s.active_report = Some(i);
            Ok(json!({"report": i}))
        })
        .view()
        .when(need_report)
        .params(r#"{"report": name|index}"#),
        C::new("report.remove", "Delete Report", "Browser", |s, p| {
            let i = index(s, p)?;
            let gone = s.project.reports.remove(i).name;
            // Other reports' links to it go too.
            for r in &mut s.project.reports {
                r.sections.retain(|x| !matches!(x, Section::Related { report } if report.eq_ignore_ascii_case(&gone)));
            }
            s.active_report = None;
            Ok(Value::Null)
        })
        .when(need_report)
        .params(r#"{"report"?}"#),
        // Report sections.
        C::new("report.supplementalPage", "Supplemental Page", "Publish › Report", |s, p| {
            let title: String = ps(p, "title").unwrap_or("About this report").chars().take(500).collect();
            let text: String = ps(p, "text").unwrap_or("").chars().take(100_000).collect();
            add_section(s, p, Section::Page { title, text, end: b(p, "end").unwrap_or(false) })
        })
        .when(need_report)
        .params(r#"{"report"?, "title"?, "text"?, "end"?: bool (after the records; default before)}"#),
        C::new("report.newTemplate", "New Template", "Publish › Report", |s, p| {
            let i = index(s, p)?;
            let rname = s.project.reports.get(i).map(|r| r.name.clone()).unwrap_or_default();
            let before = s.active_layout;
            let r = super::layout::new_layout(
                s,
                &json!({"name": ps(p, "name").map(str::to_string).unwrap_or(format!("{rname} page")), "page": ps(p, "page").unwrap_or("letter"), "orientation": ps(p, "orientation").unwrap_or("portrait"), "template": ps(p, "template").unwrap_or("map")}),
            )?;
            s.active_layout = before;
            let layout = r["name"].as_str().unwrap_or_default().to_string();
            add_section(s, &json!({"report": i}), Section::Layout { layout, end: b(p, "end").unwrap_or(false) })
        })
        .when(need_report)
        .params(r#"{"report"?, "name"?, "page"?, "orientation"?, "template"?: "map"|"blank", "end"?: bool}"#),
        C::new("report.existingTemplate", "Existing Template", "Publish › Report", |s, p| {
            let i = index(s, p)?;
            let layout = if let Some(n) = ps(p, "layout") {
                s.project.layouts.iter().find(|l| l.name.eq_ignore_ascii_case(n)).map(|l| l.name.clone()).ok_or_else(|| format!("no layout '{n}'"))?
            } else {
                let text = super::items::read_text(p).map_err(|e| format!("{e} (pass a layout name, or a layout file's path or text)"))?;
                let before = s.active_layout;
                let r = super::layout::import(s, &text)?;
                s.active_layout = before;
                r["name"].as_str().unwrap_or_default().to_string()
            };
            add_section(s, &json!({"report": i}), Section::Layout { layout, end: b(p, "end").unwrap_or(false) })
        })
        .when(need_report)
        .params(r#"{"report"?, "layout"?: a layout's name, "path"?: "*.rgislayout", "text"?: layout JSON, "data"?: base64 file, "end"?: bool}"#),
        C::new("report.relatedReport", "Related Report", "Publish › Report", |s, p| {
            let i = index(s, p)?;
            let other = ps(p, "related").ok_or("related is required (another report's name)")?;
            let j = s.project.reports.iter().position(|r| r.name.eq_ignore_ascii_case(other)).ok_or_else(|| format!("no report '{other}'"))?;
            if i == j {
                return Err("a report can't follow itself".into());
            }
            let report = s.project.reports.get(j).map(|r| r.name.clone()).unwrap_or_default();
            add_section(s, &json!({"report": i}), Section::Related { report })
        })
        .when(need_report)
        .params(r#"{"report"?, "related": report name}"#),
        C::new("report.subreport", "Subreport", "Publish › Report", subreport)
            .when(need_report)
            .params(r#"{"report"?, "relationship": name (Data Design › Relationships), "fields"?: [related fields]}"#),
        C::new("report.removeSection", "Remove Section", "Report", |s, p| {
            let i = index(s, p)?;
            let k = p.get("index").and_then(Value::as_u64).and_then(|k| usize::try_from(k).ok()).ok_or("index is required")?;
            let r = s.project.reports.get_mut(i).ok_or("no report")?;
            if k >= r.sections.len() {
                return Err(format!("no section {k}"));
            }
            r.sections.remove(k);
            Ok(json!({"sections": r.sections.len()}))
        })
        .when(need_report)
        .params(r#"{"report"?, "index": section number}"#),
        // Data.
        C::new("report.field", "Field", "Data › Data Design", |s, p| {
            let (i, field) = field_of(s, p, "field")?;
            let r = s.project.reports.get_mut(i).ok_or("no report")?;
            toggle(&mut r.fields, field, b(p, "remove").unwrap_or(false));
            Ok(json!({"fields": r.fields}))
        })
        .when(need_report)
        .params(r#"{"report"?, "field": name, "remove"?: bool}"#),
        C::new("report.autofill", "Autofill Fields", "Data › Data Design", |s, p| {
            let i = index(s, p)?;
            let names = layer_of(s, i)?.dataset.fields.iter().take(MAX_ITEMS).map(|f| f.name.clone()).collect::<Vec<_>>();
            let r = s.project.reports.get_mut(i).ok_or("no report")?;
            r.fields = names;
            Ok(json!({"fields": r.fields}))
        })
        .when(need_report)
        .params(r#"{"report"?}"#),
        C::new("report.statistics", "Statistics", "Data › Data Design", |s, p| {
            let kind = ps(p, "kind").unwrap_or("sum");
            let kind = StatKind::from_code(kind).ok_or_else(|| format!("unknown statistic '{kind}'"))?;
            let (i, field) = field_of(s, p, "field")?;
            let r = s.project.reports.get_mut(i).ok_or("no report")?;
            let at = r.stats.iter().position(|x| x.field.eq_ignore_ascii_case(&field) && x.kind == kind);
            match (at, b(p, "remove").unwrap_or(false)) {
                (Some(k), true) => {
                    r.stats.remove(k);
                }
                (None, false) if r.stats.len() < MAX_ITEMS => r.stats.push(Stat { field, kind }),
                (None, false) => return Err("the report has too many statistics".into()),
                _ => {}
            }
            Ok(json!({"stats": r.stats.len()}))
        })
        .when(need_report)
        .params(r#"{"report"?, "field": name, "kind"?: "count"|"sum"|"mean"|"min"|"max", "remove"?: bool}"#),
        C::new("report.group", "Group", "Data › Data Design", |s, p| {
            let (i, field) = field_of(s, p, "field")?;
            let r = s.project.reports.get_mut(i).ok_or("no report")?;
            let remove = b(p, "remove").unwrap_or(false);
            if !remove && r.groups.len() >= MAX_GROUPS && !r.groups.iter().any(|g| g.eq_ignore_ascii_case(&field)) {
                return Err(format!("reports group by at most {MAX_GROUPS} fields"));
            }
            toggle(&mut r.groups, field, remove);
            Ok(json!({"groups": r.groups}))
        })
        .when(need_report)
        .params(r#"{"report"?, "field": name, "remove"?: bool}"#),
        C::new("report.attachments", "Attachments", "Data › Data Design", |s, p| {
            let (i, field) = optional_field(s, p)?;
            let r = s.project.reports.get_mut(i).ok_or("no report")?;
            r.attachments_field = field;
            Ok(json!({"field": r.attachments_field}))
        })
        .when(need_report)
        .params(r#"{"report"?, "field": name of a field listing each record's attachments (paths separated by ';')|null}"#),
        C::new("report.dynamicPicture", "Dynamic Picture", "Data › Data Design", |s, p| {
            let (i, field) = optional_field(s, p)?;
            let r = s.project.reports.get_mut(i).ok_or("no report")?;
            r.picture_field = field;
            Ok(json!({"field": r.picture_field}))
        })
        .when(need_report)
        .params(r#"{"report"?, "field": name of a field holding a picture path or base64 PNG/JPEG|null}"#),
        C::new("report.chart", "Chart Frame", "Data › Data Design", |s, p| {
            let i = index(s, p)?;
            let chart = match p.get("category") {
                None | Some(Value::Null) => None,
                Some(_) => {
                    let (_, category) = field_of(s, p, "category")?;
                    let value = match ps(p, "value") {
                        Some(_) => Some(field_of(s, p, "value")?.1),
                        None => None,
                    };
                    Some(ReportChart { category, value })
                }
            };
            let r = s.project.reports.get_mut(i).ok_or("no report")?;
            r.chart = chart;
            Ok(json!({"chart": r.chart.is_some()}))
        })
        .when(need_report)
        .params(r#"{"report"?, "category": field|null (no chart), "value"?: field to sum (default: count records)}"#),
        // Output.
        C::new("report.export", "Export Report", "Publish › Output", |s, p| {
            let path = ps(p, "path").ok_or("path is required")?.to_string();
            let i = index(s, p)?;
            let dpi = f(p, "dpi").unwrap_or(150.0).clamp(72.0, 600.0);
            let lower = path.to_ascii_lowercase();
            let pdf = lower.ends_with(".pdf");
            if !pdf && !lower.ends_with(".png") {
                return Err("export to a .pdf or .png file".into());
            }
            let (bytes, pages) = report_bytes(s, i, dpi, pdf)?;
            rustgis_data::io::write_atomic(Path::new(&path), &bytes).map_err(|e| format!("{path}: {e}"))?;
            Ok(json!({"path": path, "pages": pages, "bytes": bytes.len()}))
        })
        .view()
        .when(need_report)
        .params(r#"{"report"?, "path": "*.pdf|*.png" (PNG: the first page), "dpi"?: 72..600}"#),
        C::new("report.save", "Report File", "Publish › Save As", |s, p| save(s, p, false))
            .view()
            .when(need_report)
            .params(r#"{"report"?, "path": "*.rgisreport"}"#),
        C::new("report.saveTemplate", "Report Template", "Publish › Save As", |s, p| save(s, p, true))
            .view()
            .when(need_report)
            .params(r#"{"report"?, "path": "*.rgisrpttemplate"}"#),
        C::new("report.open", "Import Report", "Publish › Report", open).params(
            r#"{"path"?: "*.rgisreport|*.rgisrpttemplate", "text"?: report JSON, "data"?: base64 file, "layer"?: the layer a template reports on (default: the selected layer)}"#,
        ),
    ]
}

/// The report named or numbered by `report`, else the active one, else the last one.
pub fn index(s: &Session, p: &Value) -> Result<usize, String> {
    let rs = &s.project.reports;
    match p.get("report") {
        Some(Value::Number(n)) => {
            n.as_u64().and_then(|n| usize::try_from(n).ok()).filter(|i| *i < rs.len()).ok_or_else(|| format!("no report {n}"))
        }
        Some(Value::String(n)) => rs
            .iter()
            .position(|r| r.name.eq_ignore_ascii_case(n))
            .or_else(|| n.parse::<usize>().ok().filter(|i| *i < rs.len()))
            .ok_or_else(|| format!("no report named '{n}'")),
        _ => s
            .active_report
            .filter(|i| *i < rs.len())
            .or(rs.len().checked_sub(1))
            .ok_or_else(|| "no reports yet: add one with report.new".to_string()),
    }
}

/// A layer by name: the active map's, else any map's.
fn find_layer<'a>(s: &'a Session, name: &str) -> Option<&'a Layer> {
    s.map().layer_by_name(name).or_else(|| s.project.maps.iter().find_map(|m| m.layer_by_name(name)))
}

/// The layer report `i` lists (the active map's first, else any map's).
pub fn layer_of(s: &Session, i: usize) -> Result<&Layer, String> {
    let r = s.project.reports.get(i).ok_or("no report")?;
    find_layer(s, &r.layer).ok_or_else(|| format!("the report's layer '{}' isn't in the project", r.layer))
}

/// The report and a field of its layer named by `key` (spelled as the layer spells it).
fn field_of(s: &Session, p: &Value, key: &str) -> Result<(usize, String), String> {
    let i = index(s, p)?;
    let name = ps(p, key).ok_or_else(|| format!("{key} is required"))?;
    let l = layer_of(s, i)?;
    let fi = l.dataset.field_index(name).ok_or_else(|| format!("'{}' has no field '{name}'", l.name))?;
    let field = l.dataset.fields.get(fi).map(|f| f.name.clone()).ok_or("no such field")?;
    Ok((i, field))
}

/// `field` as a field of the report's layer, or `None` for null/absent.
fn optional_field(s: &Session, p: &Value) -> Result<(usize, Option<String>), String> {
    match p.get("field") {
        None | Some(Value::Null) => Ok((index(s, p)?, None)),
        Some(_) => field_of(s, p, "field").map(|(i, f)| (i, Some(f))),
    }
}

fn toggle(list: &mut Vec<String>, field: String, remove: bool) {
    let at = list.iter().position(|x| x.eq_ignore_ascii_case(&field));
    match (at, remove) {
        (Some(k), true) => {
            list.remove(k);
        }
        (None, false) if list.len() < MAX_ITEMS => list.push(field),
        _ => {}
    }
}

fn add_section(s: &mut Session, p: &Value, sec: Section) -> CmdResult {
    let i = index(s, p)?;
    let r = s.project.reports.get_mut(i).ok_or("no report")?;
    if r.sections.len() >= MAX_ITEMS {
        return Err("the report has too many sections".into());
    }
    r.sections.push(sec);
    Ok(json!({"report": i, "sections": r.sections.len()}))
}

/// Drops what a layer lacks from a report (fields, groups, statistics, sort, picture,
/// attachments, chart, and a where clause it can't read), so the report draws on that layer.
fn fit_to_layer(r: &mut Report, ds: &rustgis_data::Dataset) {
    let has = |n: &str| ds.field_index(n).is_some();
    r.fields.retain(|f| has(f));
    r.groups.retain(|f| has(f));
    r.stats.retain(|x| has(&x.field));
    r.sort = r.sort.take().filter(|f| has(f));
    r.picture_field = r.picture_field.take().filter(|f| has(f));
    r.attachments_field = r.attachments_field.take().filter(|f| has(f));
    r.chart = r.chart.take().filter(|c| has(&c.category) && c.value.as_deref().is_none_or(has));
    if r.fields.is_empty() {
        r.fields = ds.fields.iter().take(MAX_ITEMS).map(|f| f.name.clone()).collect();
    }
    if !r.where_clause.is_empty() && rustgis_data::query::parse(&r.where_clause, ds).is_err() {
        r.where_clause.clear();
    }
}

/// A join key: numbers by value (so 1 and 1.0 match), anything else as text; empty for none.
fn join_key(ds: &rustgis_data::Dataset, row: usize, fi: Option<usize>) -> String {
    match fi.and_then(|fi| ds.value(row, fi).as_f64()) {
        Some(x) => format!("{x}"),
        None => cell(ds, row, fi),
    }
}

fn unique_name(s: &Session, base: &str, skip: Option<usize>) -> String {
    let base: String = base.trim().chars().take(200).collect();
    let base = if base.is_empty() { "Report".to_string() } else { base };
    let taken = |n: &str| s.project.reports.iter().enumerate().any(|(j, r)| Some(j) != skip && r.name.eq_ignore_ascii_case(n));
    let mut name = base.clone();
    let mut k = 1;
    while taken(&name) {
        k += 1;
        name = format!("{base} {k}");
    }
    name
}

/// Page size from `page`/`orientation`/`width`/`height`, portrait by default.
fn page(p: &Value, current: Option<(f64, f64)>) -> Result<Option<(f64, f64)>, String> {
    if ["page", "orientation", "width", "height"].iter().all(|k| p.get(k).is_none()) {
        return Ok(None);
    }
    let mut q = p.clone();
    if ps(p, "orientation").is_none() && ps(p, "page").is_some() {
        q["orientation"] = json!("portrait");
    }
    super::layout::page_size(&q, current).map(Some)
}

fn new_report(s: &mut Session, p: &Value) -> CmdResult {
    let layer = match ps(p, "layer") {
        Some(n) => find_layer(s, n).ok_or_else(|| format!("no layer named '{n}'"))?,
        None => s.active_layer.and_then(|id| s.map().layer(id)).ok_or("select a layer, or pass layer")?,
    };
    if layer.is_raster() {
        return Err("reports list a feature layer's records, not a raster's".into());
    }
    let mut r = Report {
        name: unique_name(s, ps(p, "name").unwrap_or(&format!("{} Report", layer.name)), None),
        layer: layer.name.clone(),
        ..Report::default()
    };
    r.title = ps(p, "title").unwrap_or(&layer.name).chars().take(500).collect();
    let ds = &layer.dataset;
    let fields_of = |key: &str| -> Result<Vec<String>, String> {
        let mut out = Vec::new();
        for v in p.get(key).and_then(Value::as_array).into_iter().flatten().take(MAX_ITEMS) {
            let n = v.as_str().ok_or_else(|| format!("{key} are field names"))?;
            let fi = ds.field_index(n).ok_or_else(|| format!("'{}' has no field '{n}'", layer.name))?;
            out.extend(ds.fields.get(fi).map(|f| f.name.clone()));
        }
        Ok(out)
    };
    r.fields = fields_of("fields")?;
    if r.fields.is_empty() {
        r.fields = ds.fields.iter().take(MAX_ITEMS).map(|f| f.name.clone()).collect();
    }
    r.groups = fields_of("groups")?;
    r.groups.truncate(MAX_GROUPS);
    if let Some(w) = ps(p, "where").map(str::trim).filter(|w| !w.is_empty()) {
        rustgis_data::query::parse(w, ds)?;
        r.where_clause = w.to_string();
    }
    r.selected_only = b(p, "selectedOnly").unwrap_or(false);
    if let Some(sort) = ps(p, "sort") {
        let fi = ds.field_index(sort).ok_or_else(|| format!("'{}' has no field '{sort}'", layer.name))?;
        r.sort = ds.fields.get(fi).map(|f| f.name.clone());
    }
    r.descending = b(p, "descending").unwrap_or(false);
    if let Some((w, h)) = page(p, None)? {
        (r.width, r.height) = (w, h);
    }
    r.clamp();
    let name = r.name.clone();
    if s.project.reports.len() >= crate::project::MAX_ITEMS {
        return Err(format!("a project holds at most {} reports", crate::project::MAX_ITEMS));
    }
    s.project.reports.push(r);
    let i = s.project.reports.len() - 1;
    s.active_report = Some(i);
    Ok(json!({"report": i, "name": name}))
}

fn update(s: &mut Session, p: &Value) -> CmdResult {
    let i = index(s, p)?;
    let mut r = s.project.reports.get(i).cloned().ok_or("no report")?;
    let old_name = r.name.clone();
    let mut renamed = None;
    if let Some(n) = ps(p, "layer") {
        let l = find_layer(s, n).ok_or_else(|| format!("no layer named '{n}'"))?;
        if l.is_raster() {
            return Err("reports list a feature layer's records, not a raster's".into());
        }
        r.layer = l.name.clone();
        // What the new layer lacks is dropped, so the report doesn't show blank columns, and so
        // are subreports through relationships that don't involve it. A kept subreport may now
        // list the other end of its relationship, so its fields are fitted to that layer.
        fit_to_layer(&mut r, &l.dataset);
        let name = l.name.clone();
        r.sections.retain_mut(|x| match x {
            Section::Subreport { relationship, fields } => {
                let other = relationships(s).find_map(|rel| {
                    if !rel.name.eq_ignore_ascii_case(relationship) {
                        None
                    } else if rel.origin.eq_ignore_ascii_case(&name) {
                        Some(rel.destination.clone())
                    } else if rel.destination.eq_ignore_ascii_case(&name) {
                        Some(rel.origin.clone())
                    } else {
                        None
                    }
                });
                let Some(other) = other else { return false };
                match find_layer(s, &other) {
                    Some(o) => fields.retain(|f| o.dataset.field_index(f).is_some()),
                    None => fields.clear(),
                }
                true
            }
            _ => true,
        });
    }
    // Everything is checked against the (possibly new) layer before the report changes.
    let l = find_layer(s, &r.layer);
    let field = |n: &str| -> Result<String, String> {
        let ds = &l.ok_or("the report's layer isn't in the project")?.dataset;
        ds.field_index(n).and_then(|fi| ds.fields.get(fi)).map(|f| f.name.clone()).ok_or_else(|| format!("no field '{n}'"))
    };
    if let Some(n) = ps(p, "name") {
        r.name = unique_name(s, n, Some(i));
        renamed = Some(r.name.clone());
    }
    if let Some(t) = ps(p, "title") {
        r.title = t.chars().take(500).collect();
    }
    if let Some(w) = ps(p, "where") {
        let w = w.trim();
        if !w.is_empty() {
            rustgis_data::query::parse(w, &l.ok_or("the report's layer isn't in the project")?.dataset)?;
        }
        r.where_clause = w.to_string();
    }
    if let Some(v) = b(p, "selectedOnly") {
        r.selected_only = v;
    }
    match p.get("sort") {
        Some(Value::Null) => r.sort = None,
        Some(Value::String(n)) => r.sort = Some(field(n)?),
        _ => {}
    }
    if let Some(v) = b(p, "descending") {
        r.descending = v;
    }
    if let Some((w, h)) = page(p, Some((r.width, r.height)))? {
        (r.width, r.height) = (w, h);
    }
    r.clamp();
    let name = r.name.clone();
    if let Some(slot) = s.project.reports.get_mut(i) {
        *slot = r;
    }
    // Related-report links follow a rename.
    if let Some(new) = renamed {
        for x in s.project.reports.iter_mut().flat_map(|r| r.sections.iter_mut()) {
            if let Section::Related { report } = x
                && report.eq_ignore_ascii_case(&old_name)
            {
                *report = new.clone();
            }
        }
    }
    Ok(json!({"report": i, "name": name}))
}

fn subreport(s: &mut Session, p: &Value) -> CmdResult {
    let i = index(s, p)?;
    let rel = ps(p, "relationship").ok_or("relationship is required")?;
    let layer = s.project.reports.get(i).map(|r| r.layer.clone()).unwrap_or_default();
    let r = relationships(s).find(|r| r.name.eq_ignore_ascii_case(rel)).ok_or_else(|| format!("no relationship '{rel}'"))?;
    let other = if r.origin.eq_ignore_ascii_case(&layer) {
        r.destination.clone()
    } else if r.destination.eq_ignore_ascii_case(&layer) {
        r.origin.clone()
    } else {
        return Err(format!("the relationship '{}' doesn't involve '{layer}'", r.name));
    };
    let name = r.name.clone();
    let ds = &find_layer(s, &other).ok_or_else(|| format!("no layer named '{other}'"))?.dataset;
    let mut fields = Vec::new();
    for v in p.get("fields").and_then(Value::as_array).into_iter().flatten().take(MAX_ITEMS) {
        let n = v.as_str().ok_or("fields are field names")?;
        let fi = ds.field_index(n).ok_or_else(|| format!("'{other}' has no field '{n}'"))?;
        fields.extend(ds.fields.get(fi).map(|f| f.name.clone()));
    }
    add_section(s, &json!({"report": i}), Section::Subreport { relationship: name, fields })
}

/// Every map's relationships.
fn relationships(s: &Session) -> impl Iterator<Item = &rustgis_map::design::Relationship> {
    s.project.maps.iter().flat_map(|m| m.editing.relationships.iter())
}

fn save(s: &mut Session, p: &Value, template: bool) -> CmdResult {
    let path = ps(p, "path").ok_or("path is required")?;
    let i = index(s, p)?;
    let bytes = report_file(s, i, template)?;
    rustgis_data::io::write_atomic(Path::new(path), &bytes).map_err(|e| format!("{path}: {e}"))?;
    Ok(json!({"path": path}))
}

/// A report file (the report as it is) or template (its design without the data source).
pub fn report_file(s: &Session, i: usize, template: bool) -> Result<Vec<u8>, String> {
    let r = s.project.reports.get(i).ok_or("no report")?;
    let r = if template { r.template() } else { r.clone() };
    serde_json::to_vec_pretty(&r).map_err(|e| e.to_string())
}

fn open(s: &mut Session, p: &Value) -> CmdResult {
    let text = super::items::read_text(p)?;
    let mut r: Report = serde_json::from_str(&text).map_err(|e| format!("not a RustGIS report: {e}"))?;
    r.clamp();
    if let Some(n) = ps(p, "layer") {
        r.layer = find_layer(s, n).ok_or_else(|| format!("no layer named '{n}'"))?.name.clone();
    } else if find_layer(s, &r.layer).is_none() {
        // A template (or a report on a layer this project lacks) reports on the selected layer.
        r.layer = s
            .active_layer
            .and_then(|id| s.map().layer(id))
            .map(|l| l.name.clone())
            .ok_or("select the layer to report on, or pass layer")?;
    }
    // Drop what the layer lacks, so the report still draws.
    if let Some(l) = find_layer(s, &r.layer) {
        fit_to_layer(&mut r, &l.dataset);
    }
    r.name = unique_name(s, &r.name, None);
    let name = r.name.clone();
    if s.project.reports.len() >= crate::project::MAX_ITEMS {
        return Err(format!("a project holds at most {} reports", crate::project::MAX_ITEMS));
    }
    s.project.reports.push(r);
    let i = s.project.reports.len() - 1;
    s.active_report = Some(i);
    Ok(json!({"report": i, "name": name}))
}

/// A record's value as report text (nulls blank).
fn cell(ds: &rustgis_data::Dataset, row: usize, fi: Option<usize>) -> String {
    match fi.map(|fi| ds.value(row, fi)) {
        None | Some(rustgis_data::Value::Null) => String::new(),
        Some(v) => v.to_string(),
    }
}

/// Statistic lines for some records.
fn stat_lines(r: &Report, ds: &rustgis_data::Dataset, rows: &[usize]) -> Vec<String> {
    let mut out = vec![format!("{} records", rows.len())];
    for st in &r.stats {
        let fi = ds.field_index(&st.field);
        let vals: Vec<f64> = rows.iter().filter_map(|&row| fi.and_then(|fi| ds.value(row, fi).as_f64())).collect();
        let v = st.kind.apply(&vals, rows.len()).map(fmt_num).unwrap_or_else(|| "–".into());
        out.push(format!("{} of {}: {v}", st.kind.label(), st.field));
    }
    out
}

/// A picture field's value as base64 and pixel size: base64 image data, or the path of a PNG or
/// JPEG relative to the project's folder. Attribute values come from data files, so paths may
/// not be absolute or climb out of that folder (with `..`), and an unsaved project reads none.
fn picture_of(text: &str, folder: Option<&Path>) -> Option<(String, u32, u32)> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    let lower = t.to_ascii_lowercase();
    let is_path = [".png", ".jpg", ".jpeg"].iter().any(|e| lower.ends_with(e));
    let b64 = if is_path {
        let rel = Path::new(t);
        let safe = rel.components().all(|c| matches!(c, std::path::Component::Normal(_) | std::path::Component::CurDir));
        // `folder` is canonical already; the picture is resolved through links too, so a link
        // inside the folder can't point outside it.
        let folder = folder.filter(|_| safe)?;
        let full = folder.join(rel).canonicalize().ok()?;
        if !full.starts_with(folder) {
            return None;
        }
        super::graphics::picture(&json!({"path": full.to_str()?})).ok()?
    } else {
        super::graphics::picture(&json!({"image": t})).ok()?
    };
    let (w, h) = rustgis_render::picture_size(&b64)?;
    Some((b64, w, h))
}

/// Turns a report into blocks for the renderer.
pub fn build(s: &Session, i: usize) -> Result<Doc, String> {
    let r = s.project.reports.get(i).ok_or("no report")?;
    let mut doc = Doc {
        title: if r.title.is_empty() { r.name.clone() } else { r.title.clone() },
        width: r.width,
        height: r.height,
        blocks: Vec::new(),
    };
    let mut seen = vec![i];
    blocks(s, i, &mut doc.blocks, &mut seen, 0)?;
    Ok(doc)
}

fn blocks(s: &Session, i: usize, out: &mut Vec<Block>, seen: &mut Vec<usize>, depth: usize) -> Result<(), String> {
    let r = s.project.reports.get(i).ok_or("no report")?;
    let layer = layer_of(s, i)?;
    let ds = &layer.dataset;
    // Picture paths are read relative to the project's folder (none for an unsaved project).
    // Canonical once, for checking each picture's resolved path against it.
    let folder_buf = s.path.as_deref().and_then(Path::parent).and_then(|f| f.canonicalize().ok());
    let folder = folder_buf.as_deref();
    let layout_index = |name: &str| s.project.layouts.iter().position(|l| l.name.eq_ignore_ascii_case(name));
    // Pages before the records.
    for sec in &r.sections {
        match sec {
            Section::Page { title, text, end: false } => {
                out.push(Block::Heading { level: 0, text: title.clone() });
                out.push(Block::Text(text.clone()));
                out.push(Block::PageBreak);
            }
            Section::Layout { layout, end: false } => out.extend(layout_index(layout).map(Block::Layout)),
            _ => {}
        }
    }
    // The records: matching, selected, sorted by group then sort field.
    let mut rows: Vec<usize> =
        if r.where_clause.trim().is_empty() { (0..ds.features.len()).collect() } else { rustgis_data::query::select(&r.where_clause, ds)? };
    if r.selected_only {
        let sel: std::collections::HashSet<usize> = layer.selection.iter().copied().collect();
        rows.retain(|row| sel.contains(row));
    }
    let gi: Vec<Option<usize>> = r.groups.iter().map(|g| ds.field_index(g)).collect();
    let si = r.sort.as_deref().and_then(|n| ds.field_index(n));
    // Sort keys worked out once per record: a total order even for columns mixing numbers and
    // text (numbers first, by value; then everything else as text).
    enum Key {
        Num(f64),
        Text(String),
    }
    let key = |row: usize, fi: Option<usize>| -> Key {
        let v = fi.map(|fi| ds.value(row, fi));
        match v.and_then(rustgis_data::Value::as_f64) {
            Some(x) => Key::Num(x),
            None => Key::Text(v.map(ToString::to_string).unwrap_or_default()),
        }
    };
    let cmp = |a: &Key, b: &Key| match (a, b) {
        (Key::Num(x), Key::Num(y)) => x.total_cmp(y),
        (Key::Num(_), Key::Text(_)) => std::cmp::Ordering::Less,
        (Key::Text(_), Key::Num(_)) => std::cmp::Ordering::Greater,
        (Key::Text(x), Key::Text(y)) => x.cmp(y),
    };
    let mut keyed: Vec<(Vec<Key>, Option<Key>, usize)> =
        rows.iter().map(|&row| (gi.iter().map(|g| key(row, *g)).collect(), si.map(|s| key(row, Some(s))), row)).collect();
    keyed.sort_by(|a, b| {
        for (x, y) in a.0.iter().zip(&b.0) {
            let o = cmp(x, y);
            if o != std::cmp::Ordering::Equal {
                return o;
            }
        }
        match (&a.1, &b.1) {
            (Some(x), Some(y)) => {
                let o = cmp(x, y);
                if r.descending { o.reverse() } else { o }
            }
            _ => std::cmp::Ordering::Equal,
        }
    });
    // Sorted first, so a capped report lists the first records in its order.
    let truncated = keyed.len() > MAX_RECORDS;
    let rows: Vec<usize> = keyed.into_iter().take(MAX_RECORDS).map(|k| k.2).collect();
    let title = if r.title.is_empty() { r.name.clone() } else { r.title.clone() };
    out.push(Block::Title(title));
    let mut about = format!("{} records from {}", rows.len(), layer.name);
    if !r.where_clause.trim().is_empty() {
        about.push_str(&format!(" where {}", r.where_clause.trim()));
    }
    if r.selected_only {
        about.push_str(" (selected)");
    }
    if truncated {
        about.push_str(&format!(" — the first {MAX_RECORDS} are listed"));
    }
    out.push(Block::Text(about));
    let fi: Vec<Option<usize>> = r.fields.iter().map(|n| ds.field_index(n)).collect();
    let columns = Block::Columns(r.fields.clone());
    let pic = r.picture_field.as_deref().and_then(|n| ds.field_index(n));
    let att = r.attachments_field.as_deref().and_then(|n| ds.field_index(n));
    let mut pictures = 0usize;
    let mut picture_bytes = 0usize;
    // Subreports: related records per key value.
    let subs: Vec<(String, Vec<String>, Vec<Option<usize>>, &rustgis_data::Dataset, HashMap<String, Vec<usize>>, Option<usize>)> = r
        .sections
        .iter()
        .filter_map(|sec| {
            let Section::Subreport { relationship, fields } = sec else { return None };
            let rel = relationships(s).find(|x| x.name.eq_ignore_ascii_case(relationship))?;
            // A relationship that doesn't involve this layer is skipped.
            let (here_key, other, other_key) = if rel.origin.eq_ignore_ascii_case(&layer.name) {
                (&rel.origin_key, &rel.destination, &rel.destination_key)
            } else if rel.destination.eq_ignore_ascii_case(&layer.name) {
                (&rel.destination_key, &rel.origin, &rel.origin_key)
            } else {
                return None;
            };
            let ods = &find_layer(s, other)?.dataset;
            let ok = ods.field_index(other_key)?;
            let mut by_key: HashMap<String, Vec<usize>> = HashMap::new();
            for row in 0..ods.features.len() {
                let k = join_key(ods, row, Some(ok));
                if !k.is_empty() {
                    by_key.entry(k).or_default().push(row);
                }
            }
            let names: Vec<String> =
                if fields.is_empty() { ods.fields.iter().take(6).map(|f| f.name.clone()).collect() } else { fields.clone() };
            let idx = names.iter().map(|n| ods.field_index(n)).collect();
            Some((rel.name.clone(), names, idx, ods, by_key, ds.field_index(here_key)))
        })
        .collect();
    let mut open: Vec<String> = Vec::new(); // the group values in force, outermost first
    let mut group_rows: Vec<Vec<usize>> = vec![Vec::new(); gi.len()];
    let close = |out: &mut Vec<Block>, level: usize, open: &mut Vec<String>, group_rows: &mut Vec<Vec<usize>>| {
        for l in (level..open.len()).rev() {
            if let Some(g) = group_rows.get_mut(l) {
                if !g.is_empty() {
                    out.push(Block::Stats(stat_lines(r, ds, g)));
                }
                g.clear();
            }
        }
        open.truncate(level);
    };
    if gi.is_empty() {
        out.push(columns.clone());
    }
    for &row in &rows {
        // Group values compare as the sort does: numbers by value (1 and 1.0 are one group).
        let norm = |g: Option<usize>| match g.and_then(|g| ds.value(row, g).as_f64()) {
            Some(x) => format!("{x}"),
            None => cell(ds, row, g),
        };
        let vals: Vec<String> = gi.iter().map(|g| norm(*g)).collect();
        if let Some(changed) = (0..vals.len()).find(|&l| open.get(l) != vals.get(l)) {
            close(out, changed, &mut open, &mut group_rows);
            for (l, v) in vals.iter().enumerate().skip(changed) {
                let label = r.groups.get(l).cloned().unwrap_or_default();
                out.push(Block::Heading {
                    level: l.min(255) as u8,
                    text: format!("{label}: {}", if v.is_empty() { "(blank)" } else { v }),
                });
                open.push(v.clone());
            }
            out.push(columns.clone());
        }
        for g in group_rows.iter_mut() {
            g.push(row);
        }
        out.push(Block::Row(fi.iter().map(|f| cell(ds, row, *f)).collect()));
        // Once the picture budget is spent, no more picture files are read.
        if let Some(pf) = pic
            && pictures < MAX_PICTURES
            && picture_bytes < PICTURE_BUDGET
            && let Some((image, width, height)) = picture_of(&cell(ds, row, Some(pf)), folder)
            && picture_bytes.saturating_add(image.len()) <= PICTURE_BUDGET
        {
            pictures += 1;
            picture_bytes += image.len();
            out.push(Block::Picture { image, width, height });
        }
        if let Some(af) = att {
            let items: Vec<String> =
                cell(ds, row, Some(af)).split(';').map(str::trim).filter(|x| !x.is_empty()).take(50).map(str::to_string).collect();
            if !items.is_empty() {
                let names = items
                    .iter()
                    .map(|x| Path::new(x).file_name().map_or_else(|| x.clone(), |n| n.to_string_lossy().into_owned()))
                    .collect();
                out.push(Block::Attachments(names));
                for x in &items {
                    if pictures >= MAX_PICTURES || picture_bytes >= PICTURE_BUDGET {
                        break;
                    }
                    if let Some((image, width, height)) = picture_of(x, folder)
                        && picture_bytes.saturating_add(image.len()) <= PICTURE_BUDGET
                    {
                        pictures += 1;
                        picture_bytes += image.len();
                        out.push(Block::Picture { image, width, height });
                    }
                }
            }
        }
        for (name, names, idx, ods, by_key, key) in &subs {
            let k = join_key(ds, row, *key);
            let Some(related) = by_key.get(&k).filter(|_| !k.is_empty()) else { continue };
            out.push(Block::SubColumns(
                names.iter().map(|n| format!("{name}: {n}")).take(1).chain(names.iter().skip(1).cloned()).collect(),
            ));
            for &orow in related.iter().take(MAX_RELATED) {
                out.push(Block::SubRow(idx.iter().map(|f| cell(ods, orow, *f)).collect()));
            }
        }
    }
    close(out, 0, &mut open, &mut group_rows);
    // The report's summary and chart.
    out.push(Block::Heading { level: 0, text: "Summary".into() });
    out.push(Block::Stats(stat_lines(r, ds, &rows)));
    if let Some(c) = &r.chart
        && let Some(ci) = ds.field_index(&c.category)
    {
        let vi = c.value.as_deref().and_then(|v| ds.field_index(v));
        let mut sums: BTreeMap<String, f64> = BTreeMap::new();
        for &row in &rows {
            let k = cell(ds, row, Some(ci));
            let v = match vi {
                Some(vi) => ds.value(row, vi).as_f64().unwrap_or(0.0),
                None => 1.0,
            };
            *sums.entry(if k.is_empty() { "(blank)".into() } else { k }).or_default() += v;
        }
        let mut bars: Vec<(String, f64)> = sums.into_iter().collect();
        bars.sort_by(|a, b| b.1.total_cmp(&a.1));
        bars.truncate(25);
        let title = match &c.value {
            Some(v) => format!("{v} by {}", c.category),
            None => format!("Records by {}", c.category),
        };
        out.push(Block::Chart { title, bars });
    }
    // Pages after the records, then related reports.
    for sec in &r.sections {
        match sec {
            Section::Page { title, text, end: true } => {
                out.push(Block::PageBreak);
                out.push(Block::Heading { level: 0, text: title.clone() });
                out.push(Block::Text(text.clone()));
            }
            Section::Layout { layout, end: true } => out.extend(layout_index(layout).map(Block::Layout)),
            _ => {}
        }
    }
    for sec in &r.sections {
        if let Section::Related { report } = sec
            && depth < MAX_DEPTH
            && let Some(j) = s.project.reports.iter().position(|x| x.name.eq_ignore_ascii_case(report))
            && !seen.contains(&j)
        {
            seen.push(j);
            out.push(Block::PageBreak);
            // A related report that can't be built (its layer gone, its where clause stale) is
            // noted in place rather than failing this one.
            let mut sub = Vec::new();
            match blocks(s, j, &mut sub, seen, depth + 1) {
                Ok(()) => out.extend(sub),
                Err(e) => out.push(Block::Text(format!("The related report {report} couldn't be included: {e}"))),
            }
        }
    }
    Ok(())
}

/// The report's pages, laid out.
pub fn pages(s: &Session, i: usize) -> Result<(Doc, Vec<rustgis_render::report::Page>), String> {
    let doc = build(s, i)?;
    let pages = rustgis_render::report::paginate(&doc);
    Ok((doc, pages))
}

/// The report as a PDF (every page) or a PNG (the first page), and its page count.
pub fn report_bytes(s: &mut Session, i: usize, dpi: f64, pdf: bool) -> Result<(Vec<u8>, usize), String> {
    let (doc, pages) = pages(s, i)?;
    let n = if pdf { pages.len() } else { 1 };
    let mut out = Vec::new();
    let mut held = 0usize;
    let (mut frames_pictured, mut pages_pictured) = (Vec::new(), Vec::new());
    for k in 0..n {
        if !pdf {
            let (img, ..) = rustgis_render::report::draw_page(&doc, &pages, k, dpi, &s.project.layouts, &mut s.project.maps)
                .ok_or("the page is too large to draw")?;
            return Ok((img.encode_png().map_err(|e| e.to_string())?, pages.len()));
        }
        let (page, pictured) = rustgis_render::report::pdf_page(&doc, &pages, k, dpi, &s.project.layouts, &mut s.project.maps)
            .ok_or("the page is too large to draw")?;
        if page.overflowed() {
            // Too large for vectors all the same: the page as one picture.
            pages_pictured.push(k + 1);
            let (img, w, h) = rustgis_render::report::draw_page(&doc, &pages, k, dpi, &s.project.layouts, &mut s.project.maps)
                .ok_or("the page is too large to draw")?;
            out.push(rustgis_render::pdf::Page::new(&img, w, h));
        } else {
            frames_pictured.extend(pictured);
            out.push(page);
        }
        held += out.last().map_or(0, rustgis_render::pdf::Page::len);
        if held > PDF_BUDGET {
            return Err(format!("the PDF passed {} MB at page {}; export at a lower dpi", PDF_BUDGET >> 20, k + 1));
        }
    }
    super::layout::pictured_notices(s, &frames_pictured, &pages_pictured);
    Ok((rustgis_render::pdf::write(&out, &doc.title), pages.len()))
}
