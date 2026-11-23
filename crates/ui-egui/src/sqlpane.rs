//! The SQL pane: a DuckDB Spatial query editor with a database picker, completion of table,
//! column and function names, a result preview, Add to Map, and a history of the queries run.
//! Queries run off the frame (a thread); DuckDB is a desktop program, so the web app says so.

// The web app shows the pane's explanation only: what runs queries is the desktop's.
#![cfg_attr(target_arch = "wasm32", expect(dead_code, unused_imports, reason = "the browser shows the explanation only"))]

use egui::{RichText, Ui};
use serde_json::{Value, json};

use crate::theme::Tokens;
use crate::{RustGisApp, Slot, i18n};

/// The queries kept in the history (newest first).
const HISTORY: usize = 30;
/// The rows a preview asks for.
const PREVIEW_ROWS: usize = 200;
/// Completions offered at once.
const SUGGESTIONS: usize = 8;

/// Functions offered while typing (DuckDB's and DuckDB Spatial's common ones).
const FUNCTIONS: &[&str] = &[
    "ST_Area",
    "ST_AsGeoJSON",
    "ST_AsText",
    "ST_Buffer",
    "ST_Centroid",
    "ST_Contains",
    "ST_ConvexHull",
    "ST_Difference",
    "ST_Distance",
    "ST_Envelope",
    "ST_GeomFromGeoJSON",
    "ST_GeomFromText",
    "ST_GeometryType",
    "ST_Intersection",
    "ST_Intersects",
    "ST_IsValid",
    "ST_Length",
    "ST_MakeLine",
    "ST_MakePolygon",
    "ST_NPoints",
    "ST_Perimeter",
    "ST_Point",
    "ST_SetCRS",
    "ST_Simplify",
    "ST_Touches",
    "ST_Transform",
    "ST_Union",
    "ST_Union_Agg",
    "ST_Within",
    "ST_X",
    "ST_XMax",
    "ST_XMin",
    "ST_Y",
    "ST_YMax",
    "ST_YMin",
    "ST_Read",
    "read_parquet",
    "read_csv",
    "read_csv_auto",
    "read_json",
    "count",
    "sum",
    "avg",
    "min",
    "max",
    "round",
    "lower",
    "upper",
    "SELECT",
    "FROM",
    "WHERE",
    "GROUP BY",
    "ORDER BY",
    "LIMIT",
    "JOIN",
    "DISTINCT",
];

type Answer<T> = Slot<Result<T, String>>;

/// The pane's state.
#[derive(Default)]
pub struct State {
    pub text: String,
    /// The database: a DuckDB file, or a PostgreSQL connection (as in Add From Database).
    pub database: String,
    /// The connection's password, kept for the session (never in the history or the project).
    pub password: String,
    history: Vec<String>,
    history_loaded: bool,
    result: Option<Result<Value, String>>,
    /// The query and database the shown result came from.
    shown: (String, String),
    running: Option<Answer<Value>>,
    #[cfg(not(target_arch = "wasm32"))]
    adding: Option<(String, String, Answer<rustgis_engine::data::source::duckdb::Output>)>,
    /// Tables and their columns of the database named, for completion.
    names: Vec<(String, String)>,
    names_for: String,
    names_job: Option<Answer<Vec<(String, String)>>>,
    /// The completions offered: where the word starts, the cursor, the names, and the text they
    /// were offered for.
    offers: Option<(usize, usize, Vec<String>, String)>,
}

fn take<T>(slot: &Answer<T>) -> Option<Result<T, String>> {
    slot.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// Whether a query seems to hold a credential: `user:password@` in an address, a signed URL's
/// signature, or a key, token or secret.
fn holds_secret(q: &str) -> bool {
    let l = q.to_ascii_lowercase();
    let userinfo = l.split("://").skip(1).any(|rest| {
        let authority = rest.split(['/', '?', '#', '\'', '"', ' ']).next().unwrap_or("");
        authority.rsplit_once('@').is_some_and(|(user, _)| user.contains(':'))
    });
    userinfo
        || ["password", "secret", "token=", "x-amz-signature", "x-goog-signature", "sig=", "key_id", "api_key", "apikey"]
            .iter()
            .any(|k| l.contains(k))
}

/// The database a picker's text names: a bare connection string is a PostgreSQL one.
fn database_text(a: &str) -> String {
    let a = a.trim();
    if a.contains('=') && !a.contains("://") && !a.to_ascii_lowercase().starts_with("postgres:") {
        format!("postgres:{a}")
    } else {
        a.to_string()
    }
}

/// A database's table names for completion, or none with a warning saying why they couldn't be
/// read (an unreadable database, a refused thread). They are read once per database, so it is
/// said once.
fn names_or_say(session: &mut rustgis_engine::Session, read: Result<Vec<(String, String)>, String>) -> Vec<(String, String)> {
    read.unwrap_or_else(|e| {
        session.warn(crate::i18n::fmt("The database's table names for completion couldn't be read: {reason}", &[("reason", &e)]));
        Vec::new()
    })
}

/// Lands queries, Add to Map runs and name lists that finished.
pub fn poll(app: &mut RustGisApp, ctx: &egui::Context) {
    let st = &mut app.ui.sql;
    let pending = st.running.is_some() || st.names_job.is_some();
    if let Some(slot) = &st.running
        && let Some(r) = take(slot)
    {
        st.running = None;
        // A query that seems to hold a credential (a password in an address, a signed URL, a
        // secret) isn't kept in the history, which is saved with the app's preferences.
        if r.is_ok() && !holds_secret(&st.shown.0) {
            let q = st.shown.0.clone();
            st.history.retain(|h| *h != q);
            st.history.insert(0, q);
            st.history.truncate(HISTORY);
            if let Some(save) = &app.services.save_pref {
                save("sql_history", &serde_json::to_string(&st.history).unwrap_or_default());
            }
        }
        st.result = Some(r);
    }
    if let Some(slot) = &st.names_job
        && let Some(r) = take(slot)
    {
        st.names_job = None;
        st.names = names_or_say(&mut app.session, r);
    }
    #[cfg(not(target_arch = "wasm32"))]
    if let Some((sql, db, slot)) = &app.ui.sql.adding
        && let Some(r) = take(slot)
    {
        let (sql, db) = (sql.clone(), db.clone());
        app.ui.sql.adding = None;
        match r {
            Ok(out) => {
                let id = app.session.sql_ran(&sql, Some(db.as_str()).filter(|d| !d.is_empty()), out);
                let mut p = json!({"sql": sql, "name": "Query", "ran": id});
                if !db.is_empty() {
                    p["database"] = json!(db);
                }
                if app.run("layer.addSql", p).is_ok() {
                    app.map.invalidate();
                }
            }
            Err(e) => app.session.notify(e, true),
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    let pending = pending || app.ui.sql.adding.is_some();
    if pending {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}

/// Starts a query's preview off the frame.
#[cfg(not(target_arch = "wasm32"))]
fn run(app: &mut RustGisApp) {
    use rustgis_engine::data::source::duckdb;
    let st = &mut app.ui.sql;
    let db_text = database_text(&st.database);
    let db = rustgis_engine::cmd::layer::database_param(&json!({"database": db_text, "password": st.password}));
    let sql = st.text.clone();
    st.shown = (sql.clone(), db_text.clone());
    st.result = None;
    let slot: Answer<Value> = Slot::default();
    let land = slot.clone();
    let db2 = db.clone();
    crate::work_off_frame("rustgis-sql-preview", &slot, move || {
        let r = duckdb::preview(&duckdb::Query { sql: &sql, database: db2.as_ref(), crs: None }, PREVIEW_ROWS)
            .map(|p| rustgis_engine::cmd::sql::preview_json(&p))
            .map_err(|e| e.to_string());
        *land.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
    });
    st.running = Some(slot);
    // The database's names, for completion (once per database).
    if let Some(db) = db
        && st.names_for != db_text
        && st.names_job.is_none()
    {
        st.names_for = db_text;
        let slot: Answer<Vec<(String, String)>> = Slot::default();
        let land = slot.clone();
        crate::work_off_frame("rustgis-sql-names", &slot, move || {
            *land.lock().unwrap_or_else(|e| e.into_inner()) = Some(duckdb::schema_of(&db).map_err(|e| e.to_string()));
        });
        st.names_job = Some(slot);
    }
}

/// Runs the editor's query now (`ui.pane.sql` with `"run"`): off the frame on the desktop; the web
/// app has no DuckDB.
pub fn run_now(app: &mut RustGisApp) {
    #[cfg(not(target_arch = "wasm32"))]
    if app.ui.sql.running.is_none() && !app.ui.sql.text.trim().is_empty() {
        run(app);
    }
    #[cfg(target_arch = "wasm32")]
    let _ = app;
}

/// Runs the shown query again in full, off the frame, and adds its result as layers.
#[cfg(not(target_arch = "wasm32"))]
fn add_to_map(app: &mut RustGisApp) {
    use rustgis_engine::data::source::duckdb;
    let (sql, db_text) = app.ui.sql.shown.clone();
    let db = rustgis_engine::cmd::layer::database_param(&json!({"database": db_text, "password": app.ui.sql.password}));
    let slot: Answer<duckdb::Output> = Slot::default();
    let land = slot.clone();
    let sql2 = sql.clone();
    crate::work_off_frame("rustgis-sql-run", &slot, move || {
        let r = duckdb::run(&duckdb::Query { sql: &sql2, database: db.as_ref(), crs: None }).map_err(|e| e.to_string());
        *land.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
    });
    app.session.notify(tl!("Running the query for the map…").to_string(), false);
    app.ui.sql.adding = Some((sql, db_text, slot));
}

/// The names that complete `word` (case-insensitive prefix): the database's tables and columns
/// (a `table.` prefix offers that table's columns) and common functions.
fn completions(word: &str, names: &[(String, String)]) -> Vec<String> {
    let lower = word.to_ascii_lowercase();
    let mut out: Vec<String> = Vec::new();
    let mut add = |s: &str| {
        if s.to_ascii_lowercase().starts_with(&lower) && !s.eq_ignore_ascii_case(word) && !out.iter().any(|o| o == s) {
            out.push(s.to_string());
        }
    };
    if let Some((table, _)) = word.rsplit_once('.') {
        let of_table = |t: &str| t.eq_ignore_ascii_case(table) || t.rsplit('.').next().is_some_and(|n| n.eq_ignore_ascii_case(table));
        for (_, c) in names.iter().filter(|(t, _)| of_table(t)) {
            add(&format!("{table}.{c}"));
        }
    } else {
        for (t, _) in names {
            add(t);
        }
        for (_, c) in names {
            add(c);
        }
        for f in FUNCTIONS {
            add(f);
        }
    }
    out.truncate(SUGGESTIONS);
    out
}

/// Whether `events` press Ctrl+Enter (⌘+Enter) to run the query: once a press, not again for each
/// repeat of a key held down (#845), as the Script pane runs. The Layer Properties pane's Query tab
/// applies its clause with it too.
pub(crate) fn run_pressed(events: &[egui::Event]) -> bool {
    events.iter().any(|e| {
        matches!(e, egui::Event::Key { key: egui::Key::Enter, pressed: true, repeat: false, modifiers, .. }
            if modifiers.matches_logically(egui::Modifiers::COMMAND))
    })
}

/// The word being typed before character `at`: letters, digits, `_` and `.`.
fn word_before(text: &str, at: usize) -> (usize, String) {
    let chars: Vec<char> = text.chars().take(at).collect();
    let start = chars.iter().rposition(|c| !(c.is_alphanumeric() || *c == '_' || *c == '.')).map(|i| i + 1).unwrap_or(0);
    (start, chars.get(start..).map(|w| w.iter().collect()).unwrap_or_default())
}

pub fn show(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        egui::Frame::NONE.inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| body(app, ui, t));
    });
}

fn body(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    if cfg!(target_arch = "wasm32") {
        ui.label(
            RichText::new(tl!(
                "SQL queries run through DuckDB, a program the desktop app calls: open this project in the desktop app to query it."
            ))
            .weak(),
        );
        return;
    }
    // A phone's apps can't start another program (#1902).
    if !rustgis_engine::data::RUNS_PROGRAMS {
        ui.label(
            RichText::new(tl!("SQL queries run through DuckDB, a program a phone can't run: open this project on a computer to query it."))
                .weak(),
        );
        return;
    }
    if !app.ui.sql.history_loaded {
        app.ui.sql.history_loaded = true;
        app.ui.sql.history = app
            .services
            .load_pref
            .as_ref()
            .and_then(|load| load("sql_history"))
            .and_then(|h| serde_json::from_str::<Vec<String>>(&h).ok())
            .unwrap_or_default();
        app.ui.sql.history.truncate(HISTORY);
    }
    ui.label(RichText::new(tl!("One DuckDB SELECT: DuckDB Spatial's ST_ functions, files and web addresses (read_parquet('…')), and the tables of the database below. Ctrl+Enter runs it.")).small().weak());
    egui::Grid::new("sql_db").num_columns(2).spacing(egui::vec2(8.0, 6.0)).show(ui, |ui| {
        let input_label = ui.label(tl!("Database"));
        ui.add(
            egui::TextEdit::singleline(&mut app.ui.sql.database)
                .hint_text(tl!("none, a .duckdb file, or host=… dbname=… user=…"))
                .desired_width(f32::INFINITY),
        )
        .labelled_by(input_label.id);
        ui.end_row();
        if database_text(&app.ui.sql.database).starts_with("postgres") || app.ui.sql.database.contains("://") {
            let input_label = ui.label(tl!("Password"));
            ui.add(egui::TextEdit::singleline(&mut app.ui.sql.password).password(true).desired_width(f32::INFINITY))
                .labelled_by(input_label.id);
            ui.end_row();
        }
    });
    let id = egui::Id::unique("sql_editor");
    let out = egui::TextEdit::multiline(&mut app.ui.sql.text)
        .id(id)
        .code_editor()
        .desired_rows(8)
        .desired_width(f32::INFINITY)
        .hint_text("SELECT name, ST_Area(geom) AS area\nFROM read_parquet('https://…/countries.parquet')\nORDER BY area DESC")
        .show(ui);
    let focused = out.response.response.has_focus();
    let run_key = focused && ui.input(|i| run_pressed(&i.events));
    // Completion of the word before the cursor. The offers are kept in the state, so they stay
    // clickable once a button takes the focus from the editor (until the text changes).
    if focused && let Some(range) = out.cursor_range {
        let at: usize = range.primary.index.into();
        let (start, word) = word_before(&app.ui.sql.text, at);
        let offers = if word.chars().count() >= 2 { completions(&word, &app.ui.sql.names) } else { Vec::new() };
        app.ui.sql.offers = (!offers.is_empty()).then(|| (start, at, offers, app.ui.sql.text.clone()));
    }
    if let Some((start, at, offers, _)) = app.ui.sql.offers.clone().filter(|o| o.3 == app.ui.sql.text) {
        let mut picked = None;
        ui.horizontal_wrapped(|ui| {
            for o in &offers {
                if ui.small_button(RichText::new(o).monospace()).clicked() {
                    picked = Some(o.clone());
                }
            }
        });
        if let Some(o) = picked {
            let chars: Vec<char> = app.ui.sql.text.chars().collect();
            let before: String = chars.iter().take(start).collect();
            let after: String = chars.iter().skip(at).collect();
            app.ui.sql.text = format!("{before}{o}{after}");
            app.ui.sql.offers = None;
            let mut state = out.state.clone();
            let cursor = egui::text::CCursor::new(start + o.chars().count());
            state.cursor.set_char_range(Some(egui::text::CCursorRange::one(cursor)));
            state.store(ui.ctx(), id);
            ui.ctx().memory_mut(|m| m.request_focus(id));
        }
    }
    let running = app.ui.sql.running.is_some();
    #[cfg(not(target_arch = "wasm32"))]
    let adding = app.ui.sql.adding.is_some();
    #[cfg(target_arch = "wasm32")]
    let adding = false;
    let mut go = run_key && !running && !app.ui.sql.text.trim().is_empty();
    let mut add = false;
    ui.horizontal(|ui| {
        go |= ui.add_enabled(!running && !app.ui.sql.text.trim().is_empty(), egui::Button::new(tl!("Run"))).clicked();
        let can_add = !adding && matches!(&app.ui.sql.result, Some(Ok(v)) if v["geometry"].as_bool() == Some(true));
        add = ui
            .add_enabled(can_add, egui::Button::new(tl!("Add to Map")))
            .on_disabled_hover_text(tl!("Run a query whose result has a geometry column"))
            .clicked();
        if running || adding {
            ui.spinner();
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    {
        if go {
            run(app);
        }
        if add {
            add_to_map(app);
        }
    }
    #[cfg(target_arch = "wasm32")]
    let _ = (go, add);
    results(app, ui, t);
    history(app, ui);
}

fn results(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    match &app.ui.sql.result {
        None => {}
        Some(Err(e)) => {
            ui.label(RichText::new(e.chars().take(2000).collect::<String>()).color(t.red));
        }
        Some(Ok(v)) => {
            let columns: Vec<String> =
                v["columns"].as_array().into_iter().flatten().filter_map(|c| c["name"].as_str().map(str::to_string)).collect();
            let rows = v["rows"].as_array().cloned().unwrap_or_default();
            let more = v["more"].as_bool().unwrap_or(false);
            let n = rows.len().to_string();
            ui.label(
                RichText::new(if more { i18n::fmt("The first {n} rows", &[("n", &n)]) } else { i18n::fmt("{n} rows", &[("n", &n)]) })
                    .small()
                    .weak(),
            );
            egui::ScrollArea::horizontal().id_salt("sql_rows").show(ui, |ui| {
                egui::Grid::new("sql_grid").striped(true).spacing(egui::vec2(12.0, 3.0)).show(ui, |ui| {
                    for c in &columns {
                        ui.label(RichText::new(c).strong().small());
                    }
                    ui.end_row();
                    for r in &rows {
                        for v in r.as_array().into_iter().flatten() {
                            let text = match v {
                                Value::Null => RichText::new("NULL").weak(),
                                Value::String(s) => RichText::new(s.chars().take(80).collect::<String>()),
                                other => RichText::new(other.to_string()),
                            };
                            ui.label(text.small().monospace());
                        }
                        ui.end_row();
                    }
                });
            });
        }
    }
}

fn history(app: &mut RustGisApp, ui: &mut Ui) {
    if app.ui.sql.history.is_empty() {
        return;
    }
    ui.add_space(6.0);
    egui::CollapsingHeader::new(tl!("History")).default_open(false).show(ui, |ui| {
        let mut pick = None;
        for (i, q) in app.ui.sql.history.iter().enumerate() {
            let line: String = q.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(90).collect();
            if ui.add(egui::Button::new(RichText::new(line).monospace().small()).frame(false)).on_hover_text(q).clicked() {
                pick = Some(i);
            }
        }
        if let Some(q) = pick.and_then(|i| app.ui.sql.history.get(i)).cloned() {
            app.ui.sql.text = q;
        }
        if ui.small_button(tl!("Clear History")).clicked() {
            app.ui.sql.history.clear();
            if let Some(save) = &app.services.save_pref {
                save("sql_history", "[]");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Names that couldn't be read are said once, and none are kept; names read say nothing.
    #[test]
    fn names_that_cant_be_read_are_said_and_names_read_are_not() {
        let mut s = rustgis_engine::Session::new(rustgis_engine::Project::default());
        let mark = s.notice_mark();
        assert!(names_or_say(&mut s, Err("no threads left".into())).is_empty());
        let said = s.notices_since(mark);
        assert!(said.len() == 1 && said.iter().all(|n| n.text.contains("couldn't be read: no threads left")), "{said:?}");
        let mark = s.notice_mark();
        let names = names_or_say(&mut s, Ok(vec![("main".to_string(), "parcels".to_string())]));
        assert_eq!(names, vec![("main".to_string(), "parcels".to_string())]);
        assert!(s.notices_since(mark).is_empty());
    }

    #[test]
    fn secrets_stay_out_of_the_history() {
        assert!(holds_secret("SELECT * FROM read_parquet('https://me:pw@data.example/x.parquet')"));
        assert!(holds_secret("SELECT * FROM 's3://b/x.parquet?X-Amz-Signature=abc'"));
        assert!(holds_secret("CREATE SECRET (TYPE s3, KEY_ID 'k')"));
        assert!(!holds_secret("SELECT name FROM read_parquet('https://data.example/x.parquet') WHERE a = 'b@c'"));
        assert!(!holds_secret("SELECT 1"));
    }

    #[test]
    fn completes_names() {
        let names = vec![
            ("towns".to_string(), "name".to_string()),
            ("towns".to_string(), "pop".to_string()),
            ("roads".to_string(), "lanes".to_string()),
        ];
        assert_eq!(completions("st_ar", &names), vec!["ST_Area"]);
        assert_eq!(completions("to", &names), vec!["towns"]);
        assert_eq!(completions("towns.p", &names), vec!["towns.pop"]);
        assert_eq!(completions("la", &names), vec!["lanes"]);
        assert_eq!(word_before("SELECT ST_Ar", 12), (7, "ST_Ar".to_string()));
        assert_eq!(word_before("x, towns.po FROM", 11), (3, "towns.po".to_string()));
        assert_eq!(database_text(" host=h dbname=d "), "postgres:host=h dbname=d");
        assert_eq!(database_text("a.duckdb"), "a.duckdb");
    }

    /// Ctrl+Enter runs the query once a press: a key held down sends repeats (egui marks every
    /// press after the first as one), and those don't run it again (#845).
    #[test]
    fn a_held_ctrl_enter_runs_once() {
        let key = |repeat, modifiers| egui::Event::Key { key: egui::Key::Enter, pressed: true, repeat, modifiers, physical_key: None };
        assert!(run_pressed(&[key(false, egui::Modifiers::COMMAND)]));
        assert!(!run_pressed(&[key(true, egui::Modifiers::COMMAND)]), "a repeat");
        assert!(run_pressed(&[key(true, egui::Modifiers::COMMAND), key(false, egui::Modifiers::COMMAND)]), "a press among repeats");
        assert!(!run_pressed(&[key(false, egui::Modifiers::NONE)]), "Enter alone breaks the line");
        assert!(run_pressed(&[key(false, egui::Modifiers::COMMAND | egui::Modifiers::SHIFT)]), "Shift aside, as before");
    }
}
