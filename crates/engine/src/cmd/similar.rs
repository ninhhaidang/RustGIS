//! Similarity ranks every resident row. Desktop adapters prepare the work off the window
//! thread; only validated completion may replace selection or deliver its notices.

use rustgis_data::{Dataset, query};
use rustgis_map::effects::DisplayFilter;
use serde_json::{Value, json};

use crate::notice_text::NoticeText;
use crate::{CmdResult, Session};

#[derive(Clone, PartialEq)]
struct Target {
    id: u32,
    row: usize,
    fields: Vec<usize>,
    count: usize,
    select: bool,
}

/// The map filters affecting output selection, independently of the ranking population.
#[derive(Clone, PartialEq)]
struct Visibility {
    query: Option<String>,
    filters: Vec<DisplayFilter>,
    time: Option<(String, Option<(f64, f64)>)>,
}

/// Captures only this layer's metadata and shared row storage, never a project/session copy.
#[derive(Clone)]
struct Input {
    target: Target,
    opened: u64,
    map: u64,
    instance: u64,
    name: String,
    data: Dataset,
    visibility: Option<Visibility>,
    _retained: crate::undo_memory::ReadRows,
}

impl Input {
    /// Resolves the reference and schema without traversing features. Selection supplies only
    /// its first row when no explicit reference is given.
    fn resolve(s: &Session, p: &Value) -> Result<Self, String> {
        let id = s.layer_id(p)?;
        let l = s.map().layer(id).ok_or("no such layer")?;
        let row = match p.get("row") {
            None | Some(Value::Null) => *l.selection.first().ok_or("select a feature (or give row)")?,
            Some(value) => {
                let row = value.as_u64().ok_or("row must be a non-negative whole number")?;
                usize::try_from(row).map_err(|_| "no such row")?
            }
        };
        if row >= l.dataset.features.len() {
            return Err("no such row".into());
        }
        let fields = super::workflows::numeric_fields(&l.dataset, p)?;
        if fields.is_empty() {
            return Err("the layer has no numeric fields to compare".into());
        }
        let count = match p.get("count") {
            None | Some(Value::Null) => 10,
            Some(value) => {
                value.as_u64().filter(|count| (1..=1000).contains(count)).ok_or("count must be a whole number from 1 to 1000")?
            }
        } as usize;
        let select = super::b(p, "select").unwrap_or(true);
        let visibility = select.then(|| Visibility {
            query: l.definition_query.clone(),
            filters: l.display_filters.iter().filter(|filter| filter.applies(s.scale())).cloned().collect(),
            time: l.time.as_ref().map(|t| (t.field.clone(), t.window)),
        });
        Ok(Self {
            target: Target { id, row, fields, count, select },
            opened: s.opened,
            map: s.map().instance_id(),
            instance: l.instance_id(),
            name: l.name.clone(),
            data: l.dataset.clone(),
            visibility,
            _retained: s.undo_memory.readers().retain(vec![l.dataset.features.allocation_key()]),
        })
    }

    /// Refuses data/reference/filter changes while work ran. Comparing row allocations avoids
    /// copying or visiting features on the window thread. Only replay-created output layers
    /// may bridge worker/live instance ids; all captured rows and metadata still have to match.
    fn same(&self, now: &Self, s: &Session) -> bool {
        let instance = self.instance == now.instance;
        #[cfg(not(target_arch = "wasm32"))]
        let instance = instance || super::gp::replay_created_layer(s, self.map, self.target.id, self.instance);
        #[cfg(target_arch = "wasm32")]
        let _ = s;
        self.target == now.target
            && self.opened == now.opened
            && self.map == now.map
            && instance
            && self.name == now.name
            && self.data.features.shares(&now.data.features)
            && self.data.fields == now.data.fields
            && self.data.coord_crs() == now.data.coord_crs()
            && self.visibility == now.visibility
    }
}

#[derive(Clone)]
struct Done {
    answer: Value,
    selection: Option<Vec<usize>>,
    notices: Vec<NoticeText>,
}

#[derive(Clone)]
struct Ahead {
    input: Input,
    done: Result<Done, String>,
    request: (u64, u64),
}

/// Orders admissions, not completions. Script workers share this sequence with their session.
fn request(s: &Session) -> (u64, u64) {
    #[cfg(not(target_arch = "wasm32"))]
    if let Some((epoch, inner)) = &s.selection_admission {
        return (*epoch, inner.fetch_add(1, std::sync::atomic::Ordering::Relaxed).wrapping_add(1));
    }
    (s.selection_epoch(), 0)
}

/// Filters the bounded result/reference rows with cancellation, without a layer-wide mask.
fn keep(rows: Vec<usize>, mut matches: impl FnMut(usize) -> bool) -> Result<Vec<usize>, String> {
    try_keep(rows, |row| Ok(matches(row)))
}

/// Filters bounded result rows while preserving predicate errors, including cancellation.
fn try_keep(rows: Vec<usize>, mut matches: impl FnMut(usize) -> Result<bool, String>) -> Result<Vec<usize>, String> {
    rustgis_data::progress::begin(rows.len());
    let mut kept = Vec::with_capacity(rows.len());
    for row in rows {
        rustgis_data::progress::step(1).map_err(|e| e.to_string())?;
        if matches(row)? {
            kept.push(row);
        }
    }
    Ok(kept)
}

/// Computes rankings and output intent without mutating the session. Ranking checks Cancel in
/// its loops; expression evaluation of one candidate is indivisible, checked before/after.
/// Time detection checks Cancel and keeps no layer-wide timestamp or visibility cache. Only
/// the at-most-1,001 result/reference rows need query/filter/time evaluation.
fn gather(input: &Input) -> Result<Done, String> {
    let t = &input.target;
    let ranked = rustgis_analysis::network::similar(&input.data, t.row, &t.fields, t.count)?;
    let mut notices = Vec::new();
    if ranked.skipped > 0 {
        notices.push(NoticeText::format(
            "Candidate features skipped: {count} on {layer} (missing or non-finite compared values)",
            [("count", ranked.skipped.to_string().into()), ("layer", input.name.clone().into())],
        ));
    }
    let found = ranked.matches;
    let answer = json!({"similar": found.iter().map(|(r, d)| json!({"row": r, "distance": d})).collect::<Vec<_>>()});
    let Some(v) = &input.visibility else { return Ok(Done { answer, selection: None, notices }) };
    let mut rows: Vec<usize> = found.iter().map(|(r, _)| *r).collect();
    rows.push(t.row);
    let matched = rows.len();
    let mut failed = 0usize;
    let mut first = None;
    let mut error = |e| {
        failed += 1;
        if first.is_none() {
            first = Some(e);
        }
    };
    rustgis_data::progress::check().map_err(|e| e.to_string())?;
    match rustgis_data::rows::Query::compile(v.query.as_deref(), &input.data) {
        Ok(q) => rows = keep(rows, |r| q.matches(&input.data, r, &query::Env::default()))?,
        Err(e) => error(e),
    }
    if let Some((field, Some(window))) = &v.time {
        let time = rustgis_map::time::candidate_window(&input.data, field, *window).map_err(|e| e.to_string())?;
        rows = try_keep(rows, |row| time.keeps(row).map_err(|e| e.to_string()))?;
    }
    for filter in &v.filters {
        rustgis_data::progress::check().map_err(|e| e.to_string())?;
        match query::parse_fields(&filter.where_, &input.data) {
            Ok(q) => rows = keep(rows, |r| query::matches(&q, &input.data, r, &query::Env::default()))?,
            Err(e) => error(e),
        }
    }
    if let Some(error) = first {
        notices.push(NoticeText::format(
            "{layer}: selecting without unreadable query/filter settings ({count}). First parser error: {error}",
            [("layer", input.name.clone().into()), ("count", failed.to_string().into()), ("error", error.into())],
        ));
    }
    let excluded = matched - rows.len();
    if excluded > 0 {
        notices.push(NoticeText::format(
            "{n} features aren't drawn (the layer's definition query, time window or display filters hide them), and aren't selected",
            [("n", excluded.to_string().into())],
        ));
    }
    rustgis_data::progress::check().map_err(|e| e.to_string())?;
    Ok(Done { answer, selection: Some(rows), notices })
}

/// Prepares desktop Similar work on shared rows. Invalid requests remain cheap synchronous
/// errors; successful work is taken only by the original admitted command at landing.
pub(super) fn ahead(s: &Session, p: &Value) -> Option<super::reads::ReadWork> {
    let input = Input::resolve(s, p).ok()?;
    let request = request(s);
    Some(super::reads::ahead("Find Similar", "data.similar".into(), move || {
        let done = gather(&input);
        Ahead { input, done, request }
    }))
}

/// Desktop UI and direct MCP adapters use the prepared read queue, with progress and Cancel.
/// A borrowed synchronous Session cannot yield a deferred command reply; headless/script callers
/// run here on their caller's thread, and the browser has one thread. A registered window owner
/// refuses an unprepared run instead of repeating heavy work inline.
pub(super) fn run(s: &mut Session, p: &Value) -> CmdResult {
    let input = Input::resolve(s, p)?;
    let ahead = super::reads::take::<Ahead>(s, "data.similar");
    #[cfg(not(target_arch = "wasm32"))]
    let ahead = match ahead {
        Some(ahead) => Some(ahead),
        None => super::gp::take_read::<Ahead>(s, "data.similar", p)?,
    };
    #[cfg(not(target_arch = "wasm32"))]
    if ahead.is_none() && s.statistics_window_thread == Some(std::thread::current().id()) {
        return Err("Similarity needs the desktop background read queue; use Find Similar or a direct MCP data.similar request. In desktop scripts, run file or network setup separately before the analysis script.".into());
    }
    let (done, request) = match ahead {
        Some(a) if a.input.same(&input, s) => (a.done, a.request),
        Some(_) => return Err("the layer, reference or map filters changed while similarity was computed: ask again".into()),
        None => {
            let request = request(s);
            (gather(&input), request)
        }
    };
    #[cfg(not(target_arch = "wasm32"))]
    if matches!(s.tools_ahead, Some(super::gp::Ahead::Recording(_))) {
        super::gp::record_read(s, "data.similar", p, Ahead { input: input.clone(), done: done.clone(), request });
    }
    let done = done?;
    if let Some(rows) = done.selection {
        if request.0 >= s.selection_intent && request >= s.similar_selected {
            super::select::set_similar_result(s, input.target.id, rows);
            s.similar_selected = request;
        } else {
            s.notify_text(
                NoticeText::term(if request.0 < s.selection_intent {
                    "Selection or data changed while similarity was computed; its ranking is returned without replacing the current selection"
                } else {
                    "A newer Similar result has already changed selection; this ranking is returned without replacing it"
                }),
                false,
            );
        }
    }
    for notice in done.notices {
        s.notify_text(notice, false);
    }
    Ok(done.answer)
}
