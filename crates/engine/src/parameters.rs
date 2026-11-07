//! Authoritative adapter envelope and script parsing. Redacted log/display copies never enter
//! this module: callers retain the returned private value for planning and execution.

use serde_json::{Value, json};

/// The parameters an adapter executes: an object or JSON text of one. Missing, null and blank
/// mean `{}`; other input fails, so a malformed request cannot act with unintended defaults.
pub fn params_envelope(params: Option<&Value>) -> Result<Value, String> {
    match params {
        None | Some(Value::Null) => Ok(json!({})),
        Some(v @ Value::Object(_)) => Ok(v.clone()),
        Some(Value::String(t)) if t.trim().is_empty() => Ok(json!({})),
        Some(Value::String(t)) => match serde_json::from_str::<Value>(t) {
            Ok(v @ Value::Object(_)) => Ok(v),
            Ok(_) => Err("params must be a JSON object".into()),
            Err(e) => Err(format!("params is not valid JSON: {e}")),
        },
        Some(_) => Err("params must be a JSON object".into()),
    }
}

/// One executable script line, preserving the executor's `id[=] {json}` grammar. Unlike an
/// adapter envelope, a script's JSON value is passed as written; no parameters means null.
/// Blank and comment lines have no command. Errors are annotated with line numbers by callers.
pub fn script_line(line: &str) -> Option<Result<(String, Value), String>> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let (id, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    let id = id.trim_end_matches('=');
    let params = if rest.trim().is_empty() {
        Value::Null
    } else {
        match serde_json::from_str(rest.trim()) {
            Ok(value) => value,
            Err(e) => return Some(Err(e.to_string())),
        }
    };
    if crate::cmd::workflows::refused_in_script(id) {
        return Some(Err(format!("'{id}' can't run in a script")));
    }
    Some(Ok((id.to_string(), params)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelopes_and_script_lines_keep_the_executor_grammar() {
        let object = json!({"layer": "private source", "token": "RGSYNTH_params_20261103"});
        assert_eq!(params_envelope(Some(&object)), params_envelope(Some(&json!(object.to_string()))));
        for value in [json!(42), json!([]), json!("[1]"), json!("false"), json!("{bad")] {
            assert!(params_envelope(Some(&value)).is_err(), "{value}");
        }
        for value in [Value::Null, json!(" \t")] {
            assert_eq!(params_envelope(Some(&value)).unwrap(), json!({}));
        }
        assert_eq!(params_envelope(None).unwrap(), json!({}));
        assert!(script_line(" # comment").is_none());
        assert_eq!(script_line(" map.zoomIn= {} ").unwrap().unwrap(), ("map.zoomIn".into(), json!({})));
        assert_eq!(script_line("map.zoomIn").unwrap().unwrap().1, Value::Null);
        assert_eq!(script_line("map.zoomIn []").unwrap().unwrap().1, json!([]));
        assert!(script_line("map.zoomIn {bad").unwrap().is_err());
        assert!(script_line("script.run {}").unwrap().is_err());
    }

    /// Supplied documents and an explicitly supplied final template keep this command test
    /// network-free while exercising the real Session notice boundary.
    #[test]
    fn service_discovery_reports_withheld_destinations_and_clean_input_is_silent() {
        for (path, count) in [("tiles", 0), ("%252e/tiles", 1)] {
            let href = format!("https://h.invalid/svc/{path}?token=RGSYNTH_notice_20261103");
            let document = json!({"title": "roads", "links": [{"rel": "tilesets-map", "href": href}]}).to_string();
            let mut params = json!({"url": "https://h.invalid/svc", "type": "ogcapi", "text": document});
            let mut session = crate::Session::new(crate::Project::default());
            session.run("service.layers", &params).unwrap();
            let notices = std::mem::take(&mut session.notices);
            assert_eq!(notices.len(), count);
            assert!(notices.iter().all(|n| n.text.contains("1 address(es)") && !n.text.contains("RGSYNTH_")));
            params["layer"] = json!("map");
            params["tiles"] = json!("https://h.invalid/explicit/{z}/{y}/{x}");
            session.run("service.add", &params).unwrap();
            let notices = std::mem::take(&mut session.notices);
            assert_eq!(notices.len(), count);
            assert!(notices.iter().all(|n| n.text.contains("1 address(es)") && !n.error));
            // All fail after discovery counted its withheld destinations. The forwarding notice
            // is independent of the unchanged command error, and is emitted once on either path.
            for (changes, expected) in [
                (json!({"time": {}}), "roads (map tiles): time is a date, \"slider\" or \"default\""),
                (json!({"tiles": "not-a-template"}), "not-a-template: not a tile template"),
                (json!({"layer": "missing"}), "https://h.invalid/svc: no layer 'missing' (service.layers lists them)"),
            ] {
                let mut bad = params.clone();
                for (key, value) in changes.as_object().unwrap() {
                    bad[key] = value.clone();
                }
                assert_eq!(session.run("service.add", &bad).unwrap_err(), expected);
                let notices = std::mem::take(&mut session.notices);
                let forwarding = notices.iter().filter(|n| !n.error && n.text.contains("address(es) the service listed")).count();
                assert_eq!(forwarding, count);
                assert_eq!(notices.iter().filter(|n| n.error).count(), 1, "the original error is still reported");
                assert_eq!(notices.len(), count + 1, "one aggregated forwarding notice, clean errors silent about forwarding");
            }
        }
    }

    /// Nested success and a later caught panic must retain one counted loss, while the
    /// completion that was rolled back disappears. The next guard must publish nothing stale.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn nested_loss_notices_drain_once_after_panic_cleanup() {
        let mut session = crate::Session::new(crate::Project::default());
        let name = session.project.name.clone();
        let mark = session.notice_mark();
        const LOSS: &str = "{n} resource(s) were omitted at https://h.invalid/x?token=RGSYNTH_2";
        let result = session.guarded("Loss rollback", true, |s| {
            s.guarded("Nested loss", true, |s| {
                s.notify_loss(LOSS, 1);
                Ok(Value::Null)
            })?;
            assert!(s.notices_since(mark).is_empty(), "the outer guard has not finished");
            s.notify_loss(LOSS, 2);
            s.project.name = "temporary".into();
            s.notify("Deleted 3 features", false);
            panic!("synthetic loss rollback");
        });
        assert_eq!(result.unwrap_err(), "Loss rollback failed unexpectedly; the project was restored");
        assert_eq!(session.project.name, name);
        let notices = session.notices_since(mark);
        assert_eq!(notices.iter().filter(|n| !n.error).count(), 1);
        assert!(notices.iter().any(|n| !n.error && n.text.starts_with("3 resource(s)")));
        assert!(notices.iter().all(|n| !n.text.contains("RGSYNTH_") && n.text != "Deleted 3 features"));
        let diagnostic_losses = session.diagnostics.records().into_iter().filter(|r| r.message.starts_with("3 resource(s)")).count();
        assert_eq!(diagnostic_losses, 1);
        let mark = session.notice_mark();
        session.guarded("Next command", false, |_| Ok(Value::Null)).unwrap();
        assert!(session.notices_since(mark).is_empty());
    }
}
