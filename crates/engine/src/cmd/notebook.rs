//! Analyze › Notebook (New Notebook, Add and Open Notebook): notebooks of text and command cells.
//! A command cell is a script (one command per line, `id {json}`) whose results show beneath it.
//! Notebooks save as RustGIS notebooks or Jupyter notebooks, and open from either. A Jupyter
//! notebook in another language (Python) keeps its kernel, metadata and outputs through open and
//! save; its code cells are shown but not run.

use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::ps;
use crate::{CmdResult, CommandSpec as C, Session};

/// Notebook files: JSON with this extension (`.ipynb` is read and written too).
pub const EXTENSION: &str = "rgisnb";
/// Most cells a notebook keeps, and the longest cell and output kept.
const MAX_CELLS: usize = 1000;
const MAX_SOURCE: usize = 200_000;
const MAX_OUTPUT: usize = 20_000;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Notebook {
    pub name: String,
    pub cells: Vec<Cell>,
    /// A Jupyter notebook's `metadata` (kernel, language, extensions) and `nbformat_minor`, kept
    /// to save it back as it was. Shared, so undo snapshots don't copy it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jupyter: Option<Arc<Value>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    pub kind: CellKind,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default)]
    pub error: bool,
    /// A Jupyter cell's other fields (`cell_type`, `id`, `metadata`, `outputs`,
    /// `execution_count`, attachments), kept to save it back as it was. Running the cell here
    /// drops its Jupyter outputs, which `output` then replaces. Shared, so undo snapshots don't
    /// copy the outputs (images included).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jupyter: Option<Arc<Value>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CellKind {
    /// Text (Markdown).
    Markdown,
    /// Commands, run in order.
    Code,
}

impl Cell {
    fn new(kind: CellKind, source: &str) -> Cell {
        Cell { kind, source: source.chars().take(MAX_SOURCE).collect(), output: None, error: false, jupyter: None }
    }

    /// The PNG images (base64, possibly with line breaks) among the cell's Jupyter outputs,
    /// borrowed where the file has them as one string.
    pub fn images(&self) -> Vec<std::borrow::Cow<'_, str>> {
        let outputs = self.jupyter.as_deref().and_then(|j| j.get("outputs")).and_then(Value::as_array);
        outputs
            .into_iter()
            .flatten()
            .filter_map(|o| o.get("data").and_then(|d| d.get("image/png")))
            .map(|png| match png {
                Value::String(t) => std::borrow::Cow::Borrowed(t.as_str()),
                other => std::borrow::Cow::Owned(jupyter_text(other)),
            })
            .filter(|png| !png.trim().is_empty())
            .collect()
    }
}

impl Notebook {
    /// The language of a Jupyter notebook whose code cells aren't RustGIS commands (`python`),
    /// from its kernel, or `code` when it names none (RustGIS writes `rustgis`); `None` for
    /// command notebooks.
    pub fn language(&self) -> Option<&str> {
        let Some(meta) = self.jupyter.as_deref()?.get("metadata") else { return Some("code") };
        let field = |a: &str, b: &str| meta.get(a).and_then(|v| v.get(b)).and_then(Value::as_str).map(str::trim).filter(|l| !l.is_empty());
        // The kernel's language, else the language info's, else the kernel's name (`python3`).
        let lang = field("kernelspec", "language")
            .or_else(|| field("language_info", "name"))
            .or_else(|| field("kernelspec", "name").map(|n| if n.to_ascii_lowercase().starts_with("python") { "python" } else { n }))
            .unwrap_or("code");
        (!lang.eq_ignore_ascii_case("rustgis")).then_some(lang)
    }

    /// `language` as a label (`Python`).
    pub fn language_label(&self) -> Option<String> {
        // From the file, so kept short.
        let mut chars = self.language()?.chars().take(32);
        Some(chars.next().into_iter().flat_map(char::to_uppercase).chain(chars).collect())
    }

    /// Bounds retained notebook content and reports each field or cell that lost data.
    pub(crate) fn clamp(&mut self) -> rustgis_map::content::ContentRepairs {
        let mut repairs = rustgis_map::content::ContentRepairs::default();
        repairs.cut(&mut self.name, 200);
        repairs.truncate(&mut self.cells, MAX_CELLS);
        for c in &mut self.cells {
            repairs.cut(&mut c.source, MAX_SOURCE);
            if let Some(output) = &mut c.output {
                repairs.cut(output, MAX_OUTPUT);
            }
        }
        repairs
    }
}

fn need_notebook(s: &Session) -> Option<&'static str> {
    if s.project.notebooks.is_empty() { Some("add a notebook first (Analyze › New Notebook)") } else { None }
}

pub fn commands() -> Vec<C> {
    vec![
        C::new("notebook.new", "New Notebook", "Analyze › Notebook", |s, p| {
            let name = unique_name(s, ps(p, "name").unwrap_or("Notebook"));
            let nb = Notebook {
                name: name.clone(),
                cells: vec![
                    Cell::new(CellKind::Markdown, &format!("# {name}\n\nCommand cells run one command per line: `id {{json}}`.")),
                    Cell::new(CellKind::Code, "# For example:\n# map.goToXY {\"lon\": 12.5, \"lat\": 41.9, \"scale\": 2000000}"),
                ],
                jupyter: None,
            };
            let r = push(s, nb)?;
            // Trusted only once it exists.

            Ok(r)
        })
        .roles(&[])
        .effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files))
        .params(r#"{"name"?}"#),
        C::new("notebook.open", "Add and Open Notebook", "Analyze › Notebook", |s, p| {
            let text = super::items::read_text(p)?;
            let fallback =
                ps(p, "path").map(|x| Path::new(x).file_stem().map_or_else(|| x.to_string(), |n| n.to_string_lossy().into_owned()));
            let mut nb = parse(&text, fallback.as_deref().unwrap_or("Notebook"))?;
            let repairs = nb.clamp();
            nb.name = unique_name(s, &nb.name);
            // Trust left over from a removed or undone notebook of this name doesn't carry over.
            s.trusted_notebooks.remove(&nb.name.to_ascii_lowercase());
            let name = nb.name.clone();
            let result = push(s, nb)?;
            for notice in crate::project::content_notices(repairs, &name) {
                s.notify_text(notice, false);
            }
            Ok(result)
        })
        .roles(&[crate::ParamRole { key: "text", role: crate::Role::Document(crate::Document::Notebook) }])
        .effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files))
        .params(r#"{"path"?: "*.rgisnb|*.ipynb", "text"?: notebook JSON, "data"?: base64 file}"#),
        C::new("notebook.activate", "Open Notebook", "Browser", |s, p| {
            let i = index(s, p)?;
            s.active_notebook = Some(i);
            Ok(json!({"notebook": i}))
        })
        .roles(&[])
        .effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files))
        .view()
        .when(need_notebook)
        .params(r#"{"notebook": name|index}"#),
        C::new("notebook.remove", "Delete Notebook", "Browser", |s, p| {
            let i = index(s, p)?;
            let nb = s.project.notebooks.remove(i);
            s.trusted_notebooks.remove(&nb.name.to_ascii_lowercase());
            s.active_notebook = None;
            Ok(Value::Null)
        })
        .roles(&[])
        .effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files))
        .when(need_notebook)
        .params(r#"{"notebook"?}"#),
        C::new("notebook.addCell", "Add Cell", "Notebook", |s, p| {
            let i = index(s, p)?;
            let kind = match ps(p, "kind").unwrap_or("code") {
                "code" => CellKind::Code,
                "markdown" | "text" => CellKind::Markdown,
                k => return Err(format!("unknown cell kind '{k}'")),
            };
            let nb = s.project.notebooks.get_mut(i).ok_or("no notebook")?;
            if nb.cells.len() >= MAX_CELLS {
                return Err(format!("notebooks hold at most {MAX_CELLS} cells"));
            }
            let at =
                p.get("at").and_then(Value::as_u64).and_then(|k| usize::try_from(k).ok()).unwrap_or(nb.cells.len()).min(nb.cells.len());
            nb.cells.insert(at, Cell::new(kind, ps(p, "source").unwrap_or("")));
            Ok(json!({"cell": at, "cells": nb.cells.len()}))
        })
        .roles(&[crate::ParamRole { key: "source", role: crate::Role::Opaque }])
        .effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files))
        .when(need_notebook)
        .params(r#"{"notebook"?, "kind"?: "code"|"markdown", "source"?, "at"?: index}"#),
        C::new("notebook.editCell", "Edit Cell", "Notebook", |s, p| {
            let (i, k) = cell_of(s, p)?;
            let source = ps(p, "source").ok_or("source is required")?;
            let c = s.project.notebooks.get_mut(i).and_then(|nb| nb.cells.get_mut(k)).ok_or("no cell")?;
            c.source = source.chars().take(MAX_SOURCE).collect();
            Ok(json!({"cell": k}))
        })
        .roles(&[crate::ParamRole { key: "source", role: crate::Role::Opaque }])
        .effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files))
        .when(need_notebook)
        .params(r#"{"notebook"?, "cell": index, "source": text}"#),
        C::new("notebook.removeCell", "Delete Cell", "Notebook", |s, p| {
            let (i, k) = cell_of(s, p)?;
            let nb = s.project.notebooks.get_mut(i).ok_or("no notebook")?;
            nb.cells.remove(k);
            Ok(json!({"cells": nb.cells.len()}))
        })
        .roles(&[])
        .effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files))
        .when(need_notebook)
        .params(r#"{"notebook"?, "cell": index}"#),
        C::new("notebook.run", "Run Cells", "Notebook", run)
            .roles(&[])
            .effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Automation).with(crate::policy::Effect::Files))
            .when(need_notebook)
            .params(r#"{"notebook"?, "cell"?: index (default: every command cell, stopping at an error)}"#),
        C::new("notebook.save", "Save Notebook", "Notebook", |s, p| {
            let path = ps(p, "path").ok_or("path is required")?;
            let i = index(s, p)?;
            let bytes = notebook_file(s, i, path.to_ascii_lowercase().ends_with(".ipynb"))?;
            rustgis_data::io::write_atomic(Path::new(path), &bytes).map_err(|e| format!("{path}: {e}"))?;
            Ok(json!({"path": path}))
        })
        .roles(&[])
        .effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files))
        .view()
        .when(need_notebook)
        .params(r#"{"notebook"?, "path": "*.rgisnb|*.ipynb"}"#),
    ]
}

fn push(s: &mut Session, nb: Notebook) -> CmdResult {
    let name = nb.name.clone();
    if s.project.notebooks.len() >= crate::project::MAX_ITEMS {
        return Err(format!("a project holds at most {} notebooks", crate::project::MAX_ITEMS));
    }
    s.project.notebooks.push(nb);
    let i = s.project.notebooks.len() - 1;
    s.active_notebook = Some(i);
    Ok(json!({"notebook": i, "name": name}))
}

/// The notebook named or numbered by `notebook`, else the active one, else the last.
pub fn index(s: &Session, p: &Value) -> Result<usize, String> {
    let nbs = &s.project.notebooks;
    match p.get("notebook") {
        Some(Value::Number(n)) => {
            n.as_u64().and_then(|n| usize::try_from(n).ok()).filter(|i| *i < nbs.len()).ok_or_else(|| format!("no notebook {n}"))
        }
        Some(Value::String(n)) => nbs
            .iter()
            .position(|r| r.name.eq_ignore_ascii_case(n))
            .or_else(|| n.parse::<usize>().ok().filter(|i| *i < nbs.len()))
            .ok_or_else(|| format!("no notebook named '{n}'")),
        _ => s
            .active_notebook
            .filter(|i| *i < nbs.len())
            .or(nbs.len().checked_sub(1))
            .ok_or_else(|| "no notebooks yet: add one with notebook.new".to_string()),
    }
}

fn cell_of(s: &Session, p: &Value) -> Result<(usize, usize), String> {
    let i = index(s, p)?;
    let n = s.project.notebooks.get(i).map_or(0, |nb| nb.cells.len());
    let k = p.get("cell").and_then(Value::as_u64).and_then(|k| usize::try_from(k).ok()).ok_or("cell is required")?;
    if k >= n {
        return Err(format!("no cell {k}"));
    }
    Ok((i, k))
}

fn unique_name(s: &Session, base: &str) -> String {
    let base: String = base.trim().chars().take(200).collect();
    let base = if base.is_empty() { "Notebook".to_string() } else { base };
    let taken = |n: &str| s.project.notebooks.iter().any(|r| r.name.eq_ignore_ascii_case(n));
    let mut name = base.clone();
    let mut k = 1;
    while taken(&name) {
        k += 1;
        name = format!("{base} {k}");
    }
    name
}

fn run(s: &mut Session, p: &Value) -> CmdResult {
    if s.notebook_running {
        return Err("a notebook is already running (a cell can't run notebooks, even through a task or model)".into());
    }
    s.notebook_running = true;
    // The flag is cleared on every path, a panic included.
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let name = s.item_name("notebook", p)?;
        if s.item_trusted("notebook", &name) {
            run_cells(s, p)
        } else {
            s.narrowed(crate::policy::Narrow::Item { kind: "notebook", name }, |s| run_cells(s, p))?
        }
    }))
    .unwrap_or_else(|_| Err("the notebook failed unexpectedly".into()));
    s.notebook_running = false;
    r
}

/// Runs cells through the shared command boundary and cleans retained generated output.
fn run_cells(s: &mut Session, p: &Value) -> CmdResult {
    let i = index(s, p)?;
    let cells: Vec<usize> = match p.get("cell") {
        Some(_) => vec![cell_of(s, p)?.1],
        None => s.project.notebooks.get(i).map(|nb| (0..nb.cells.len()).collect()).unwrap_or_default(),
    };
    // Commands in a cell may add or remove notebooks, so this one is found by name each time.
    let name = s.project.notebooks.get(i).map(|nb| nb.name.clone()).unwrap_or_default();
    let find = |s: &Session| s.project.notebooks.iter().position(|nb| nb.name.eq_ignore_ascii_case(&name));
    let one = p.get("cell").is_some();
    let (mut ran, mut skipped) = (0, 0);
    for k in cells {
        let Some(i) = find(s) else { break };
        let Some(c) = s.project.notebooks.get(i).and_then(|nb| nb.cells.get(k)) else { break };
        if c.kind != CellKind::Code {
            continue;
        }
        // Another language's cells (a Python notebook) are kept, not run as commands.
        if let Some(lang) = s.project.notebooks.get(i).and_then(Notebook::language_label) {
            if one {
                return Err(format!("this is a {lang} cell: RustGIS runs command cells only, so run it in Jupyter"));
            }
            skipped += 1;
            continue;
        }
        let script = c.source.clone();
        // A notebook from a file (opened, or in an opened project) is someone else's: its
        // cells may not read or write files or run other scripts.
        let trusted = s.project.notebooks.get(i).is_some_and(|nb| s.item_trusted("notebook", &nb.name));
        let r = match (trusted, untrusted_line(s, &script)) {
            (false, Some(n)) => Err(format!(
                "line {n} reads or writes files, uses the network or runs other software or reserved actions, which a notebook from a file can't do; copy it into a new notebook to run it"
            )),
            _ => super::workflows::run_script(s, &json!({"script": script})),
        };
        let failed = r.is_err();
        let text = match r {
            Ok(v) => {
                let lines: Vec<String> = v["ran"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|x| {
                        let res = &x["result"];
                        let shown = if res.is_null() { "done".to_string() } else { serde_json::to_string(res).unwrap_or_default() };
                        format!("{}: {shown}", x["command"].as_str().unwrap_or(""))
                    })
                    .collect();
                if lines.is_empty() { "(nothing to run)".to_string() } else { lines.join("\n") }
            }
            Err(e) => e,
        };
        let text = crate::durable::generated_text(&text);
        let text = if failed { rustgis_data::diagnostics::Redacted::new(&text).into_string() } else { text };
        // Into the cell that ran, which a command removing or adding cells may have moved: the
        // cell with its source nearest to where it was.
        let ran_cell = find(s).and_then(|i| s.project.notebooks.get_mut(i)).and_then(|nb| {
            let at = nb.cells.iter().enumerate().filter(|(_, c)| c.source == script).min_by_key(|(n, _)| n.abs_diff(k))?.0;
            nb.cells.get_mut(at)
        });
        if let Some(c) = ran_cell {
            // Scrubbed before the cut, so no part of a login binding a result quoted is kept.
            c.output = Some(text.chars().take(MAX_OUTPUT).collect());
            c.error = failed;
            // The Jupyter outputs are from an earlier run; this one's output replaces them.
            if let Some(j) = c.jupyter.as_mut().map(Arc::make_mut).and_then(Value::as_object_mut) {
                j.remove("outputs");
                j.insert("execution_count".into(), Value::Null);
            }
        }
        ran += 1;
        if failed {
            return Ok(json!({"ran": ran, "error": k}));
        }
    }
    if skipped > 0 { Ok(json!({"ran": ran, "skipped": skipped})) } else { Ok(json!({"ran": ran})) }
}

/// The first line (1-based) of a cell whose command could reach outside the session.
fn untrusted_line(s: &Session, script: &str) -> Option<usize> {
    script.lines().enumerate().find_map(|(n, line)| {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return None;
        }
        let (id, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        // Malformed params read as none here; `run_script` rejects that line before it runs.
        let params: Value = serde_json::from_str(rest.trim()).unwrap_or(Value::Null);
        super::items::touches_files(s, id.trim_end_matches('='), &params).then_some(n + 1)
    })
}

/// Text of a Jupyter field: a string or a list of lines.
pub(crate) fn jupyter_text(v: &Value) -> String {
    match v {
        Value::String(t) => t.clone(),
        Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
        _ => String::new(),
    }
}

/// `text` without terminal colour codes (`ESC [ … m`), as in Python tracebacks.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
        } else if chars.clone().next() == Some('[') {
            // A control sequence ends at its first letter.
            for c in chars.by_ref().skip(1) {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        }
    }
    out
}

/// The text a Jupyter cell's outputs show: streams, results and displays as plain text, errors
/// as their traceback; and whether one is an error.
fn jupyter_outputs(outputs: &[Value]) -> (String, bool) {
    let mut text = String::new();
    let mut error = false;
    let mut after_stream = false;
    for o in outputs {
        let stream = o.get("output_type").and_then(Value::as_str) == Some("stream");
        let piece = match o.get("output_type").and_then(Value::as_str) {
            Some("stream") => {
                error |= o.get("name").and_then(Value::as_str) == Some("stderr");
                jupyter_text(o.get("text").unwrap_or(&Value::Null))
            }
            Some("execute_result" | "display_data") => {
                jupyter_text(o.get("data").and_then(|d| d.get("text/plain")).unwrap_or(&Value::Null))
            }
            Some("error") => {
                error = true;
                let trace: Vec<String> =
                    o.get("traceback").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(strip_ansi).collect();
                if trace.is_empty() {
                    let field = |k: &str| o.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                    format!("{}: {}", field("ename"), field("evalue"))
                } else {
                    trace.join("\n")
                }
            }
            _ => continue,
        };
        if piece.is_empty() {
            continue;
        }
        // Streams carry on where the last stream stopped; anything else starts a line.
        if !text.is_empty() && !text.ends_with('\n') && !(stream && after_stream) {
            text.push('\n');
        }
        text.push_str(&piece);
        after_stream = stream;
    }
    (text, error)
}

/// A RustGIS or Jupyter notebook.
fn parse(text: &str, fallback_name: &str) -> Result<Notebook, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("not a notebook: {e}"))?;
    // nbformat 3 keeps its cells in worksheets: opened as 4, they'd be lost on save.
    if let Some(major) = v.get("nbformat").and_then(Value::as_u64).filter(|n| *n < 4) {
        return Err(format!("this is an nbformat {major} notebook; RustGIS opens nbformat 4 notebooks (Jupyter converts older ones)"));
    }
    if v.get("nbformat").is_some() {
        let meta = v.get("metadata").filter(|m| m.is_object());
        let name = meta.and_then(|m| m.get("title")).and_then(Value::as_str).unwrap_or(fallback_name).to_string();
        // Kept even when empty: a Jupyter notebook that doesn't say it's RustGIS isn't commands.
        let mut j = serde_json::Map::new();
        j.insert("metadata".into(), meta.cloned().unwrap_or_else(|| json!({})));
        if let Some(minor) = v.get("nbformat_minor").filter(|n| n.is_u64()) {
            j.insert("nbformat_minor".into(), minor.clone());
        }
        let jupyter = Some(Arc::new(Value::Object(j)));
        let mut nb = Notebook { name, cells: Vec::new(), jupyter };
        let cells = v.get("cells").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
        // Dropping the extra cells would lose them on the next save.
        if cells.len() > MAX_CELLS {
            return Err(format!("this notebook has {} cells; RustGIS opens notebooks of up to {MAX_CELLS}", cells.len()));
        }
        // Cutting a cell short would lose its end on the next save.
        if let Some(n) = cells.iter().position(|c| jupyter_text(c.get("source").unwrap_or(&Value::Null)).chars().nth(MAX_SOURCE).is_some())
        {
            return Err(format!("cell {} of this notebook is longer than {MAX_SOURCE} characters, the most RustGIS opens", n + 1));
        }
        for c in cells {
            // Skipping a cell that isn't an object would lose it on the next save.
            let Some(fields) = c.as_object() else { return Err("a cell of this notebook isn't a JSON object".into()) };
            let kind = if c.get("cell_type").and_then(Value::as_str) == Some("code") { CellKind::Code } else { CellKind::Markdown };
            let mut cell = Cell::new(kind, &jupyter_text(c.get("source").unwrap_or(&Value::Null)));
            let outputs = c.get("outputs").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
            let (out, error) = jupyter_outputs(outputs);
            if !out.is_empty() {
                cell.output = Some(out.chars().take(MAX_OUTPUT).collect());
                cell.error = error;
            }
            let mut rest = fields.clone();
            rest.remove("source");
            cell.jupyter = Some(Arc::new(Value::Object(rest)));
            nb.cells.push(cell);
        }
        return Ok(nb);
    }
    let cells = v.get("cells").and_then(Value::as_array).map_or(0, Vec::len);
    if cells > MAX_CELLS {
        return Err(format!("this notebook has {cells} cells; RustGIS opens notebooks of up to {MAX_CELLS}"));
    }
    let mut nb: Notebook = serde_json::from_value(v).map_err(|e| format!("not a RustGIS notebook: {e}"))?;
    if let Some(n) = nb.cells.iter().position(|c| c.source.chars().nth(MAX_SOURCE).is_some()) {
        return Err(format!("cell {} of this notebook is longer than {MAX_SOURCE} characters, the most RustGIS opens", n + 1));
    }
    if nb.name.trim().is_empty() {
        nb.name = fallback_name.to_string();
    }
    Ok(nb)
}

/// A Jupyter cell id (nbformat 4.5): 1 to 64 letters, digits, `-` and `_`.
fn valid_cell_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The notebook as an nbformat 4 Jupyter notebook, with what was kept from the file it came from.
fn jupyter_file(nb: &Notebook) -> Value {
    let lines = |t: &str| -> Value { json!(t.split_inclusive('\n').collect::<Vec<_>>()) };
    let kept = nb.jupyter.as_deref();
    let minor = kept.and_then(|j| j.get("nbformat_minor")).and_then(Value::as_u64).unwrap_or(5);
    let mut metadata = match kept.and_then(|j| j.get("metadata")).filter(|m| m.is_object()) {
        Some(m) => m.clone(),
        None => json!({"language_info": {"name": "rustgis"}}),
    };
    // A notebook from elsewhere keeps its own title, or none.
    if (kept.is_none() || metadata.get("title").is_some())
        && let Some(m) = metadata.as_object_mut()
    {
        m.insert("title".into(), json!(nb.name));
    }
    // Cells need unique ids from nbformat 4.5; ones without get a free one.
    let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();
    let ids: Vec<Option<String>> = nb
        .cells
        .iter()
        .map(|c| {
            let id = c.jupyter.as_deref().and_then(|j| j.get("id")).and_then(Value::as_str);
            id.filter(|id| valid_cell_id(id) && used.insert(id.to_string())).map(str::to_string)
        })
        .collect();
    let mut next = 0usize;
    let cells: Vec<Value> = nb
        .cells
        .iter()
        .zip(ids)
        .map(|(c, id)| {
            let mut cell = c.jupyter.as_deref().and_then(Value::as_object).cloned().unwrap_or_default();
            let raw = cell.get("cell_type").and_then(Value::as_str) == Some("raw");
            let cell_type = match c.kind {
                CellKind::Code => "code",
                CellKind::Markdown if raw => "raw",
                CellKind::Markdown => "markdown",
            };
            cell.insert("cell_type".into(), json!(cell_type));
            cell.entry("metadata").or_insert_with(|| json!({}));
            cell.insert("source".into(), lines(&c.source));
            if c.kind == CellKind::Code {
                if !cell.get("outputs").is_some_and(Value::is_array) {
                    let outputs: Vec<Value> = c
                        .output
                        .iter()
                        .map(|o| json!({"output_type": "stream", "name": if c.error { "stderr" } else { "stdout" }, "text": lines(o)}))
                        .collect();
                    cell.insert("outputs".into(), json!(outputs));
                }
                cell.entry("execution_count").or_insert(Value::Null);
            } else {
                cell.remove("outputs");
                cell.remove("execution_count");
            }
            if minor >= 5 {
                let id = id.unwrap_or_else(|| {
                    loop {
                        next += 1;
                        let id = format!("cell-{next}");
                        if used.insert(id.clone()) {
                            break id;
                        }
                    }
                });
                cell.insert("id".into(), json!(id));
            } else {
                cell.remove("id");
            }
            Value::Object(cell)
        })
        .collect();
    json!({"nbformat": 4, "nbformat_minor": minor, "metadata": metadata, "cells": cells})
}

/// The notebook as a RustGIS notebook or (nbformat 4) Jupyter notebook.
pub fn notebook_file(s: &mut Session, i: usize, jupyter: bool) -> Result<Vec<u8>, String> {
    let nb = s.project.notebooks.get_mut(i).ok_or("no notebook")?;
    let (bytes, report) = crate::durable::output(nb, None, |nb| {
        let v = if jupyter { jupyter_file(nb) } else { serde_json::to_value(nb).map_err(|e| e.to_string())? };
        serde_json::to_vec_pretty(&v).map_err(|e| e.to_string())
    })?;
    crate::durable::notify(s, &report);
    Ok(bytes)
}

impl crate::durable::Durable for Notebook {
    /// Offers classified whole fields once, before any live mutation.
    fn prepare(&self, p: &mut crate::durable::Prepare<'_>) {
        use crate::durable::Leaf;
        let Self { name: _, cells, jupyter: _ } = self;
        let commands = self.language().is_none();
        for (i, Cell { kind, source, output, error: _, jupyter: _ }) in cells.iter().enumerate() {
            if commands && *kind == CellKind::Code {
                p.script(Leaf::CellSource(i), source);
            }
            if let Some(output) = output {
                p.generated(Leaf::CellOutput(i), output);
            }
        }
    }

    /// Resolves a prepared location through owned fields, without allocation or policy.
    fn field_mut(&mut self, owners: &[crate::durable::Owner], leaf: &crate::durable::Leaf) -> Option<crate::durable::FieldMut<'_>> {
        use crate::durable::{FieldMut, Leaf};
        if !owners.is_empty() {
            return None;
        }
        match leaf {
            Leaf::CellSource(i) => Some(FieldMut::Text(&mut self.cells.get_mut(*i)?.source)),
            Leaf::CellOutput(i) => self.cells.get_mut(*i)?.output.as_mut().map(FieldMut::Text),
            _ => None,
        }
    }
}

/// Cleans a notebook document on its parsed copy, preserving foreign kernels and opaque metadata.
pub(crate) fn durable_document(text: &str, depth: usize) -> Result<(String, crate::durable::Report), String> {
    let nb = parse(text, "Notebook")?;
    let jupyter = serde_json::from_str::<Value>(text).is_ok_and(|v| v.get("nbformat").is_some());
    crate::durable::output_owned_changed(nb, depth, |nb, changed| {
        if !changed {
            return Ok(text.into());
        }
        if jupyter {
            serde_json::to_string(&jupyter_file(nb)).map_err(|e| e.to_string())
        } else {
            serde_json::to_string(nb).map_err(|e| e.to_string())
        }
    })
}
