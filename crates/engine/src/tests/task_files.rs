//! Task files (`.rgistask`) are read with the project's header check, so an older RustGIS than the
//! file refuses what it can't keep, naming it (#2818).

use crate::{Project, Session};
use serde_json::{Value, json};

/// A task file as every release so far writes it: no `format`, no `requires`.
fn bare() -> Value {
    json!({"name": "Lesson", "description": "", "steps": [{"title": "Zoom", "instructions": "", "command": "map.zoomIn", "params": {}}]})
}

/// `bare` with `key` set to `value`.
fn with(key: &str, value: Value) -> String {
    let mut v = bare();
    v[key] = value;
    v.to_string()
}

/// A file removed when the test ends, whether or not an assertion failed first.
struct TempFile(std::path::PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn import(s: &mut Session, text: &str) -> Result<Value, String> {
    s.run("task.import", &json!({"text": text}))
}

/// What the project's own reader says for the same header, said of a task.
fn as_a_project_says(text: &str) -> String {
    crate::project::check_format(text, "project").unwrap_err().replace("this project", "this task")
}

#[test]
fn a_task_file_needing_an_unknown_capability_is_refused_by_name() {
    let mut s = Session::new(Project::default());
    let text = with("requires", json!(["label-weight", "future-step-key", "future-step-key"]));
    let err = import(&mut s, &text).unwrap_err();
    assert_eq!(err, "this task needs what this RustGIS can't read: future-step-key. Update RustGIS to open it.");
    assert_eq!(err, as_a_project_says(&text));
    assert!(s.project.tasks.is_empty(), "nothing imported");

    // Read from a path and from a base64 file, as from text.
    let file = TempFile(std::env::temp_dir().join(format!("task-requires-2818-{}.rgistask", std::process::id())));
    std::fs::write(&file.0, &text).unwrap();
    let from_path = s.run("task.import", &json!({"path": file.0.to_string_lossy()})).unwrap_err();
    assert_eq!(from_path, err);
    use base64::Engine;
    let data = base64::engine::general_purpose::STANDARD.encode(&text);
    assert_eq!(s.run("task.import", &json!({"data": data})).unwrap_err(), err);
    assert!(s.project.tasks.is_empty(), "nothing imported");

    // Told to update, not that a step is forbidden, when the file is both.
    let mut both = bare();
    both["requires"] = json!(["future-step-key"]);
    both["steps"] = json!([{"title": "Add", "command": "layer.addData", "params": {"path": "/data/roads.geojson"}}]);
    let err = import(&mut s, &both.to_string()).unwrap_err();
    assert!(err.contains("future-step-key") && !err.contains("reads or writes files"), "{err}");
}

#[test]
fn a_task_file_of_a_newer_format_is_refused() {
    let mut s = Session::new(Project::default());
    let text = with("format", json!(crate::project::READ_FORMAT + 1));
    let err = import(&mut s, &text).unwrap_err();
    assert_eq!(err, "this task was saved by a newer RustGIS (format 3). Update RustGIS to open it.");
    assert!(s.project.tasks.is_empty(), "nothing imported");
}

#[test]
fn a_task_file_with_a_hostile_requires_is_refused_as_a_project_is() {
    let mut s = Session::new(Project::default());
    let many: Vec<String> = (0..=crate::project::MAX_REQUIRES).map(|i| format!("future-{i}")).collect();
    for requires in [
        json!(5),
        json!("label-weight"),
        json!({"a": 1}),
        json!(["label-weight", 1]),
        json!([["nested"]]),
        json!(many),
        json!(["x".repeat(1 << 20)]),
        json!(["line\nbreak\u{202e}name"]),
    ] {
        let text = with("requires", requires.clone());
        let err = import(&mut s, &text).unwrap_err();
        assert_eq!(err, as_a_project_says(&text), "{}", err.chars().take(120).collect::<String>());
        assert!(err.len() < 1024, "the refusal stays short for a huge name: {} bytes", err.len());
        assert!(!err.contains('\n') && !err.contains('\u{202e}'), "{err}");
    }
    assert!(s.project.tasks.is_empty(), "nothing imported");
}

/// Not text at all, or a list, is still `not a RustGIS task`, as before.
#[test]
fn a_task_file_that_is_no_task_is_still_said_so() {
    let mut s = Session::new(Project::default());
    for text in ["[]", "5", "{}", "{\"name\": \"x\"}", "not json"] {
        let err = import(&mut s, text).unwrap_err();
        assert!(err.starts_with("not a RustGIS task"), "{text}: {err}");
    }
    // A `format` that isn't a whole number is read as the project reader reads it (it used to be
    // ignored here, as a task file never had one): not a task this RustGIS can read.
    for format in [json!("2"), json!(2.5), json!(-1), json!(null), json!([2])] {
        let text = with("format", format.clone());
        let err = import(&mut s, &text).unwrap_err();
        assert!(err.starts_with("not a RustGIS task"), "{format}: {err}");
        assert_eq!(err, crate::project::check_format(&text, "task").unwrap_err());
    }
    assert!(s.project.tasks.is_empty(), "nothing imported");
}

#[test]
fn task_files_without_a_header_or_with_known_capabilities_import() {
    let mut s = Session::new(Project::default());
    // A file as every release so far writes it.
    import(&mut s, &bare().to_string()).unwrap();
    // A header listing nothing, and one listing what this RustGIS knows.
    import(&mut s, &with("requires", json!([]))).unwrap();
    import(&mut s, &with("requires", json!(["label-weight", "dggs-system", "table-export-order"]))).unwrap();
    import(&mut s, &with("format", json!(crate::project::READ_FORMAT))).unwrap();
    assert_eq!(s.project.tasks.len(), 4);
    assert!(s.project.tasks.iter().all(|t| t.steps.len() == 1 && t.steps[0].command == "map.zoomIn"));
}

/// A recorded `task.import` keeps the document's header when its text is cleaned of credentials,
/// so replaying it refuses what the first run would have (`roles::document`).
#[test]
fn a_recorded_task_import_keeps_the_check_when_its_text_is_cleaned() {
    const SECRET: &str = "RGSYNTH_task_requires_20261124";
    let mut task = bare();
    task["requires"] = json!(["future-step-key"]);
    task["steps"] =
        json!([{"title": "Add", "command": "layer.addUrl", "params": {"url": format!("https://h.invalid/a.geojson?token={SECRET}")}}]);
    let text = task.to_string();
    let (clean, report) = crate::roles::command("task.import", &json!({"text": text}), 0);
    let clean = clean.expect("the credential is cleaned out of the text");
    let cleaned = clean["text"].as_str().unwrap();
    assert!(!cleaned.contains(SECRET), "{cleaned}");
    assert_eq!(report.unchecked, 0);
    let kept: Value = serde_json::from_str(cleaned).unwrap();
    assert_eq!(kept["requires"], json!(["future-step-key"]), "{cleaned}");
    assert_eq!((kept["name"].as_str(), kept["steps"].as_array().map(Vec::len)), (Some("Lesson"), Some(1)));
    let mut s = Session::new(Project::default());
    let err = import(&mut s, cleaned).unwrap_err();
    assert!(err.contains("future-step-key"), "{err}");

    // A text that needs no cleaning is kept as written.
    let (clean, report) = crate::roles::command("task.import", &json!({"text": bare().to_string()}), 0);
    assert!(clean.is_none() && report.unchecked == 0);
}

/// A recorded `task.import` is cleaned through the typed task as it always was: a key the task
/// doesn't have, with a credential in it, isn't kept beside the cleaned steps. Only `format` and
/// `requires` are put back from the document.
#[test]
fn a_recorded_task_import_keeps_only_its_envelope_beside_the_typed_clean() {
    const SECRET: &str = "RGSYNTH_stray_key_20261124";
    let mut task = bare();
    task["requires"] = json!(["label-weight"]);
    task["format"] = json!(crate::project::READ_FORMAT);
    task["token"] = json!(SECRET);
    task["extra"] = json!({"password": SECRET});
    // A credential in a step makes the document change, so it is written again from the typed task.
    task["steps"] =
        json!([{"title": "Add", "command": "layer.addUrl", "params": {"url": format!("https://h.invalid/a.geojson?token={SECRET}")}}]);
    let (clean, report) = crate::roles::command("task.import", &json!({"text": task.to_string()}), 0);
    let cleaned = clean.expect("the credential is cleaned out of the text");
    let cleaned = cleaned["text"].as_str().unwrap();
    assert!(!cleaned.contains(SECRET), "{cleaned}");
    assert_eq!(report.unchecked, 0);
    let kept: Value = serde_json::from_str(cleaned).unwrap();
    let mut keys: Vec<&String> = kept.as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(keys, ["description", "format", "name", "requires", "steps"], "{cleaned}");
    assert_eq!(kept["requires"], json!(["label-weight"]));
    assert_eq!(kept["format"], json!(crate::project::READ_FORMAT));
}

/// What a recorded `task.import` keeps of `requires` and `format` is only what carries no credential:
/// one that does is withheld with the whole text, which replays as no task, so a replay can't skip
/// the capability check by losing the list. A clean list is still kept, rewritten or not.
#[test]
fn a_recorded_task_import_keeps_its_requires_only_when_it_carries_no_credential() {
    const SECRET: &str = "RGSYNTH_requires_20261124";
    let secret_step =
        json!([{"title": "Add", "command": "layer.addUrl", "params": {"url": format!("https://h.invalid/a.geojson?token={SECRET}")}}]);
    let clean_step = bare()["steps"].clone();
    for (key, value) in [
        ("requires", json!(["future-step-key", format!("https://h.invalid/x?token={SECRET}")])),
        ("requires", json!({"token": SECRET})),
        ("requires", json!([{"headers": {"Authorization": format!("Bearer {SECRET}")}}])),
        ("format", json!({"token": SECRET})),
    ] {
        // Rewritten (a credential in a step) and kept as written (none).
        for steps in [&secret_step, &clean_step] {
            let mut task = bare();
            task[key] = value.clone();
            task["steps"] = steps.clone();
            let (clean, report) = crate::roles::command("task.import", &json!({"text": task.to_string()}), 0);
            let params = clean.unwrap_or_else(|| panic!("{key} {value}: kept as written, credential and all"));
            let cleaned = params["text"].as_str().unwrap();
            assert!(!cleaned.contains(SECRET), "{key} {value}: {cleaned}");
            assert_eq!(report.withheld, 1, "{key} {value}");
            // The replay is refused: nothing is imported without the check.
            let mut s = Session::new(Project::default());
            assert!(import(&mut s, cleaned).is_err(), "{key} {value}: {cleaned}");
            assert!(s.project.tasks.is_empty());
        }
    }
    // A clean list is kept, so the replay is checked.
    for steps in [&secret_step, &clean_step] {
        let mut task = bare();
        task["requires"] = json!(["future-step-key"]);
        task["steps"] = steps.clone();
        let (clean, report) = crate::roles::command("task.import", &json!({"text": task.to_string()}), 0);
        let kept = clean.map_or(task.to_string(), |p| p["text"].as_str().unwrap().to_string());
        assert_eq!(serde_json::from_str::<Value>(&kept).unwrap()["requires"], json!(["future-step-key"]), "{kept}");
        assert_eq!(report.withheld, 0);
        let mut s = Session::new(Project::default());
        assert!(import(&mut s, &kept).unwrap_err().contains("future-step-key"));
    }
}
