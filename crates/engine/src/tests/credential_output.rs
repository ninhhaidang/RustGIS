use super::*;
use crate::cmd::notebook::{Cell, CellKind, Notebook};
use crate::{
    GpRecord, Peer, durable,
    project::{Model, ModelGraph, ModelNode, ModelStep, Task, TaskStep},
};
use rustgis_map::presentation::{Page, PageKind, Presentation};

const SECRET: &str = "RGSYNTH_output_20261103";
const DATA: &str = "RGSYNTH_data_20261103";

/// Database labels and durable addresses share query-key classification, without changing
/// the executable connection string or dropping clean connection options.
#[test]
fn durable_database_query_aliases_match_public_labels() {
    for scheme in ["mysql", "mariadb", "mssql", "postgresql"] {
        for key in ["access_token", "subscription-key", "api_key"] {
            let public = format!("{scheme}://reader@db.invalid/gis?ssl=true");
            let private = format!("{public}&{key}={SECRET}");
            let kept = match durable::public_address(&private) {
                durable::Kept::Changed(s) => s,
                _ => private.clone(),
            };
            assert_eq!(kept, public, "durable {scheme}/{key}");
            assert_eq!(crate::sources::redact(&private), public, "public label {scheme}/{key}");
            assert!(matches!(durable::public_address(&public), durable::Kept::Unchanged));
            assert_eq!(crate::sources::redact(&public), public);
        }
    }
}

/// Graph input fields select existing names; source-address cleaning must not rename those
/// references when saved text is opened without this process's credential restore memory.
#[test]
fn durable_model_input_names_survive_a_cold_round_trip() {
    let bound = rustgis_data::login_address::bind("0123456789abcdef", "https://example.invalid", "https://example.invalid/roads").unwrap();
    for name in ["https://example.invalid/roads#north", bound.as_str()] {
        let mut s = world();
        s.map_mut().layers[0].name = name.into();
        let graph = ModelGraph {
            nodes: vec![ModelNode {
                id: "input1".into(),
                kind: crate::project::ModelNodeKind::Input,
                layer: Some(name.into()),
                ..Default::default()
            }],
            edges: Vec::new(),
        };
        s.project.models.push(Model { name: "Names".into(), steps: Vec::new(), graph: Some(graph) });
        let (text, _) = s.project.saved_text(None).unwrap();
        for cold in [Project::from_json(&text).unwrap(), Project::from_import_json(&text, None).unwrap()] {
            let model = cold.models.last().unwrap().graph.as_ref().unwrap();
            let selector = model.nodes[0].layer.as_deref().unwrap();
            assert_eq!(selector, name, "a graph input selects a name, even when it resembles an address or runtime wrapper");
            assert!(cold.map().layer_by_name(selector).is_some(), "the opened graph still resolves its real input");
        }
    }
}

/// Address parsing must precede active-pattern masking at every generated history/error
/// boundary, even when the active key is part of the URL scheme itself.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn generated_errors_and_history_clean_addresses_before_active_patterns() {
    if isolate_process_test("tests::credential_output::generated_errors_and_history_clean_addresses_before_active_patterns") {
        return;
    }
    use crate::render::rustgis_tiles::logins;
    let binding = logins::activate(
        "https://history-collision.invalid",
        "synthetic-history-collision",
        logins::Method::Header("X-Key".into()),
        "https",
        false,
    )
    .unwrap();
    let raw = format!("failed at https://h.invalid/a?token={SECRET}&layer=roads");
    let mut s = world();
    s.push_gp_record(GpRecord {
        tool: "Unknown".into(),
        id: "unknown".into(),
        params: Value::Null,
        command: None,
        at: 0,
        ok: false,
        messages: vec![raw.clone()],
        millis: 0,
        outputs: Vec::new(),
    });
    s.notify(&raw, true);
    let error = s.run("layer.rename", &json!({"layer":format!("https://h.invalid/a?token={SECRET}"),"name":"unused"})).unwrap_err();
    let history = s.project.gp_history.last().unwrap().messages[0].clone();
    let notice = s.notices.last().unwrap().text.clone();
    assert!(logins::deactivate(&binding));
    assert!(!history.contains(SECRET), "history: {history}");
    assert!(!notice.contains(SECRET), "notice: {notice}");
    assert!(!error.contains(SECRET), "command error: {error}");
    for id in [
        format!("https://h.invalid/a?token={SECRET}"),
        format!("sql.connect={{\"password\":\"{SECRET}\"}}"),
        format!("sql.connect {{\"password\":\"{SECRET}\"}}"),
    ] {
        let error = s.run(&id, &Value::Null).unwrap_err();
        assert!(!error.contains(SECRET), "unknown command error: {error}");
    }
    assert!(!s.project.saved_text(None).unwrap().0.contains(SECRET));
}

/// Invalid command/tool identifiers are executable slots too: an address there must not
/// bypass cleaning through a null parameter or a malformed script line.
#[test]
fn durable_identifier_addresses_are_cleaned_and_counted() {
    let url = format!("https://identifier.invalid/a?token={SECRET}&layer=roads");
    let source = format!(
        "# clean comment\r\n{url}\r\n{url} {{broken\nsql.connect={{\"password\":\"{SECRET}\"}}\nsql.connect={{\"password\":\"{SECRET}\"}} junk\nmap.zoomIn {{}}\n"
    );
    let mut project = Project::default();
    project.notebooks.push(Notebook {
        name: "Identifiers".into(),
        jupyter: None,
        cells: vec![Cell { kind: CellKind::Code, source, output: None, error: false, jupyter: None }],
    });
    project.tasks.push(Task {
        name: "Identifiers".into(),
        description: String::new(),
        steps: [
            url.clone(),
            format!("sql.connect {{\"password\":\"{SECRET}\"}}"),
            format!("sql.connect:{{\"password\":\"{SECRET}\"}}"),
            format!("sql.connect:password:{SECRET}"),
            format!("sql.connect:token:{SECRET}"),
        ]
        .into_iter()
        .map(|command| TaskStep { title: String::new(), instructions: String::new(), command, params: Value::Null })
        .collect(),
    });
    project.models.push(Model {
        name: "Identifiers".into(),
        steps: vec![ModelStep { tool: url.clone(), params: Value::Null }],
        graph: Some(ModelGraph {
            nodes: vec![ModelNode { id: "tool1".into(), tool: Some(url.clone()), ..Default::default() }],
            edges: Vec::new(),
        }),
    });
    let mut s = Session::new(project);
    s.project.gp_history.push(GpRecord {
        tool: "Unknown".into(),
        id: url.clone(),
        params: Value::Null,
        command: Some((url, Value::Null)),
        at: 0,
        ok: false,
        messages: Vec::new(),
        millis: 0,
        outputs: Vec::new(),
    });
    let original = serde_json::to_value(&s.project).unwrap();
    let (text, report) = s.project.saved_text(None).unwrap();
    assert!(!text.contains(SECRET), "an identifier bypassed durable projection: {text}");
    assert!(report.lines >= 2, "{report:?}");
    assert_eq!(report.steps, 9, "each changed executable is counted once");
    assert_eq!(serde_json::to_value(&s.project).unwrap(), original, "live identifiers are restored exactly");
    let (clean, report) = crate::roles::script(
        "# clean comment\r\nmap.zoomIn {}\r\nmap.zoomOut\nplugin:token-tools/read\nplugin:demo/token\ngeolibre:token\n",
        0,
    );
    assert!(clean.is_none() && report.notices().is_empty(), "ordinary ids and formatting stay exact");
}

/// Cleaning both fields of one operation still affects one executable step.
#[test]
fn durable_executable_count_is_per_step() {
    let mut project = Project::default();
    project.tasks.push(Task {
        name: "One step".into(),
        description: String::new(),
        steps: vec![TaskStep {
            title: String::new(),
            instructions: String::new(),
            command: format!("https://identifier.invalid/?token={SECRET}"),
            params: json!({"password":SECRET}),
        }],
    });
    let (text, report) = project.saved_text(None).unwrap();
    assert!(!text.contains(SECRET));
    assert_eq!(report.steps, 1, "identifier and params belong to the same step: {report:?}");
}

/// Inline synthetic state exercises owning fields without contacting any service.
fn marked() -> Session {
    let mut s = world();
    let url = format!("https://h.invalid/a?token={SECRET}&layer=a%20b");
    s.project.maps[0].layers[0].dataset.source = Some(url.clone());
    s.project.tasks.push(Task { name: "Credentials".into(), description: DATA.into(), steps: vec![
        TaskStep { title: DATA.into(), instructions: DATA.into(), command: "layer.addUrl".into(), params: json!({"url": url}) },
        TaskStep { title: "Malformed wrapper".into(), instructions: String::new(), command: "layer.addUrl".into(), params: json!({"url": format!("rustgis-s3:broken:{SECRET}")}) },
        TaskStep { title: "Data".into(), instructions: String::new(), command: "layer.addDataText".into(), params: json!({"text": format!(r#"{{"type":"Feature","geometry":null,"properties":{{"token":"{DATA}","key":"{DATA}","password":"{DATA}","url":"https://h.invalid/?token={DATA}"}}}}"#)}) },
    ] });
    let params = json!({"input": "Cities", "token": SECRET, "values": {"token": DATA, "key": DATA, "password": DATA}});
    s.project.models.push(Model {
        name: "Credentials".into(),
        steps: vec![ModelStep { tool: "buffer".into(), params: params.clone() }],
        graph: Some(ModelGraph {
            nodes: vec![ModelNode { id: "tool1".into(), tool: Some("buffer".into()), params: params.clone(), ..Default::default() }],
            edges: Vec::new(),
        }),
    });
    s.project.gp_history.push(GpRecord {
        tool: "Buffer".into(),
        id: "buffer".into(),
        params,
        command: Some(("gp.bufferPoint".into(), json!({"token": SECRET}))),
        at: 0,
        ok: true,
        messages: vec![format!("source {url}")],
        millis: 0,
        outputs: vec!["Cities".into()],
    });
    s.project.notebooks.push(Notebook {
        name: "Credentials".into(),
        jupyter: None,
        cells: vec![Cell {
            kind: CellKind::Code,
            source: format!("# {DATA}\nlayer.addUrl {{\"url\":\"{url}\"}}\nlayer.addUrl {{\"url\":\"{url}\"\n"),
            output: Some(format!("source {url}")),
            error: true,
            jupyter: None,
        }],
    });
    s.project.presentations.push(Presentation {
        name: "Credentials".into(),
        pages: vec![Page {
            title: DATA.into(),
            kind: PageKind::Video { source: url, poster: None, caption: DATA.into() },
            notes: DATA.into(),
            seconds: 5.0,
        }],
        ..Default::default()
    });
    s
}

#[test]
fn durable_writers_clean_all_owned_roles_and_preserve_the_live_project() {
    let mut s = marked();
    let before = serde_json::to_value(&s.project).unwrap();
    let (text, report) = s.project.saved_text(None).unwrap();
    assert!(!text.contains(SECRET));
    assert!(report.layer_addresses() > 0 && report.steps > 0 && report.lines > 0 && report.videos > 0 && report.withheld > 0);
    assert!(text.contains(DATA));
    assert_eq!(before, serde_json::to_value(&s.project).unwrap());
    let opened = Project::from_json(&text).unwrap();
    assert_eq!(opened.tasks[0].steps, s.project.tasks[0].steps);
    assert_eq!(opened.models[0], s.project.models[0]);
    assert_eq!(opened.notebooks[0].cells[0].source, s.project.notebooks[0].cells[0].source);
    assert!(!opened.notebooks[0].cells[0].output.as_ref().unwrap().contains(SECRET), "generated text is never restored");
    assert!(!s.project.clone().recovery_text().unwrap().contains(SECRET));
    for kind in [
        "map",
        "layer",
        "template",
        "projectPackage",
        "mapPackage",
        "mobilePackage",
        "gpPackage",
        "task",
        "notebook",
        "jupyter",
        "presentation",
        "webTool",
    ] {
        let (_, bytes) = crate::cmd::items::item_bytes(&mut s, kind, &Value::Null).unwrap();
        let text = if bytes.starts_with(b"PK") {
            rustgis_data::zip::read_matching(&bytes, |_| true)
                .unwrap()
                .into_iter()
                .map(|(_, b)| String::from_utf8_lossy(&b).into_owned())
                .collect::<String>()
        } else {
            String::from_utf8_lossy(&bytes).into_owned()
        };
        assert!(!text.contains(SECRET), "{kind}");
        assert_eq!(before, serde_json::to_value(&s.project).unwrap(), "{kind} mutated live fields");
    }
    let code = s.run("gp.historyCode", &json!({"run": 0, "as": "command"})).unwrap();
    assert!(!code.to_string().contains(SECRET));
    s.project.gp_history[0].command = None;
    let code = s.run("gp.historyScript", &Value::Null).unwrap();
    assert!(!code.to_string().contains(SECRET));
    assert!(code.to_string().contains(DATA), "replayable data properties must remain");
}

/// Empty query pairs and clean padding survive saves; an actual token gives one counted loss.
#[test]
fn durable_empty_query_pairs_preserve_clean_urls_and_report_real_loss() {
    for (query, kept, losses) in [
        ("", "", 0),
        ("&", "&", 0),
        ("layer=1&&f=json", "layer=1&&f=json", 0),
        ("layer=1&", "layer=1&", 0),
        ("layer=1 \t", "layer=1 \t", 0),
        ("layer=1&&token=RGSYNTH_empty_query&", "layer=1", 1),
        ("layer=1&appid=RGSYNTH_empty_query", "layer=1", 1),
        ("layer=1&%61ppid=RGSYNTH_empty_query", "layer=1", 1),
        ("layer=1&subscription-key=RGSYNTH_empty_query", "layer=1", 1),
        ("layer=1&client_key=RGSYNTH_empty_query", "layer=1", 1),
        ("layer=1&sas-key=RGSYNTH_empty_query", "layer=1", 1),
        ("client_id=public&application_id=public&map=1", "client_id=public&application_id=public&map=1", 0),
    ] {
        let url = format!("https://h.invalid/tiles/{{z}}/{{x}}/{{y}}.png?{query}");
        let mut project = Project::default();
        project.map_mut().tiles.push(rustgis_map::TileLayer::new("Clean", rustgis_map::TileKind::Raster, &url));
        let before = serde_json::to_value(&project).unwrap();
        let (saved, report) = project.saved_text(None).unwrap();
        assert_eq!(report.layer_addresses(), losses, "{url}");
        assert_eq!(report.notices().len(), usize::from(losses > 0), "{url}: {:?}", report.notices());
        let saved: Value = serde_json::from_str(&saved).unwrap();
        let expected = format!("https://h.invalid/tiles/{{z}}/{{x}}/{{y}}.png?{kept}");
        assert_eq!(saved["maps"][0]["tiles"][0]["url"], expected);
        assert_eq!(serde_json::to_value(&project).unwrap(), before);
    }
}

/// Fragments are deliberately omitted, but their notice must not call them credentials.
#[test]
fn durable_fragment_removal_is_reported_accurately_and_clean_addresses_stay_quiet() {
    for fragment in ["", "#section", "#token=RGSYNTH_fragment"] {
        let url = format!("https://h.invalid/tiles/{{z}}/{{x}}/{{y}}.png{fragment}");
        let mut project = Project::default();
        project.map_mut().tiles.push(rustgis_map::TileLayer::new("Fragment", rustgis_map::TileKind::Raster, &url));
        let (saved, report) = project.saved_text(None).unwrap();
        let saved: Value = serde_json::from_str(&saved).unwrap();
        assert_eq!(saved["maps"][0]["tiles"][0]["url"], "https://h.invalid/tiles/{z}/{x}/{y}.png");
        assert_eq!(project.map().tiles[0].url, url);
        assert_eq!(report.layer_addresses(), u32::from(!fragment.is_empty()));
        if fragment.is_empty() {
            assert!(report.notices().is_empty());
        } else {
            let notices = report.notices();
            assert_eq!(notices.len(), 1);
            assert!(notices[0].contains("URL fragments"), "{notices:?}");
        }
    }
}

#[test]
fn durable_clean_and_opaque_content_keep_their_meaning() {
    let mut s = marked();
    let original = s.project.tasks[0].steps[2].params.clone();
    let (clean, report) = crate::roles::command("layer.addDataText", &original, 0);
    assert!(clean.is_none() && report.notices().is_empty());
    let source = format!("print('https://h.invalid/?token={SECRET}')");
    let notebook = Notebook {
        name: "Python".into(),
        jupyter: Some(std::sync::Arc::new(json!({"metadata":{"kernelspec":{"language":"python"}}}))),
        cells: vec![Cell { kind: CellKind::Code, source: source.clone(), output: None, error: false, jupyter: None }],
    };
    let (text, report) = durable::output_owned(notebook, |n| serde_json::to_string(n).map_err(|e| e.to_string())).unwrap();
    assert!(text.contains(SECRET) && report.notices().is_empty());
    let params = json!({"nested": {"token": SECRET}});
    let (clean, report) = crate::roles::command("plugin.unknown", &params, 0);
    assert!(clean.is_none() && report.unchecked == 1);
    for command in ["layer.addUrl", "plugin.unknown"] {
        for (token, unchecked) in [
            (json!({"value": SECRET}), true),
            (json!([SECRET]), true),
            (json!({}), false),
            (json!([]), false),
            (json!(false), false),
            (Value::Null, false),
        ] {
            let params = json!({"token": token});
            let mut project = Project::default();
            project.tasks.push(Task {
                name: "Opaque credential value".into(),
                description: String::new(),
                steps: vec![TaskStep {
                    title: String::new(),
                    instructions: String::new(),
                    command: command.into(),
                    params: params.clone(),
                }],
            });
            let before = serde_json::to_value(&project).unwrap();
            let (text, report) = project.saved_text(None).unwrap();
            let written: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(written["tasks"][0]["steps"][0]["params"], params, "{command}: opaque values keep their meaning");
            assert_eq!(serde_json::to_value(&project).unwrap(), before, "{command}: output preserves the live project");
            assert_eq!(report.unchecked, u32::from(unchecked), "{command}: {params}");
            assert_eq!(report.steps, 0);
            assert_eq!(report.withheld, 0);
            let expected = if unchecked {
                vec!["1 step(s) contain opaque parameters that may hold credentials and were kept unchecked".to_string()]
            } else {
                Vec::new()
            };
            assert_eq!(report.notices(), expected, "{command}: {params}");
            if unchecked {
                assert_eq!(crate::log_params(&params).into_log_value(), json!({"token": ""}), "logs withhold the whole value");
            }
        }
    }
    let clean = Project::default();
    let expected = serde_json::to_string(&clean).unwrap();
    let (text, report) = durable::output_owned(clean, |p| serde_json::to_string(p).map_err(|e| e.to_string())).unwrap();
    assert_eq!(text, expected);
    assert!(report.notices().is_empty());
    s.project.tasks[0].steps[0].params = json!({"url": "rustgis-s3:broken:RGSYNTH_output_20261103"});
    let (text, report) = s.project.saved_text(None).unwrap();
    assert!(!text.contains(SECRET) && report.withheld > 0);
}

/// Address lists clean string leaves, while opaque values keep their data and say so.
#[test]
fn durable_address_roles_report_opaque_shapes_without_rewriting_data() {
    let public = "https://h.invalid/a?layer=1";
    let secret = format!("{public}&token={SECRET}");
    let opaque = json!({"url":{"href":secret}});
    let nested = json!({"urls":[[secret]]});
    let clean_object = json!({"url":{"href":public}});
    for (params, expected, steps, unchecked) in [
        (json!({"url":public}), json!({"url":public}), 0, 0),
        (json!({"urls":[secret]}), json!({"urls":[public]}), 1, 0),
        (clean_object.clone(), clean_object, 0, 0),
        (opaque.clone(), opaque, 0, 1),
        (nested.clone(), nested, 0, 1),
    ] {
        let mut project = Project::default();
        project.tasks.push(Task {
            name: "Address shapes".into(),
            description: String::new(),
            steps: vec![TaskStep {
                title: String::new(),
                instructions: String::new(),
                command: "layer.addUrl".into(),
                params: params.clone(),
            }],
        });
        let before = serde_json::to_value(&project).unwrap();
        let (text, report) = project.saved_text(None).unwrap();
        let written: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(written["tasks"][0]["steps"][0]["params"], expected);
        assert_eq!(serde_json::to_value(&project).unwrap(), before);
        assert_eq!((report.steps, report.unchecked, report.withheld), (steps, unchecked, 0), "{params}");
        assert_eq!(report.notices().len(), usize::from(steps > 0) + usize::from(unchecked > 0));
        if unchecked > 0 {
            assert_eq!(report.notices(), ["1 step(s) contain opaque parameters that may hold credentials and were kept unchecked"]);
        }
        assert!(!crate::log_params(&params).into_log_value().to_string().contains(SECRET), "logs have a separate stricter policy");
    }
}

/// A newer embedded project must not be rewritten with this writer's schema or version.
#[test]
fn durable_future_documents_keep_their_exact_text_unchecked() {
    let mut future = Project::default();
    future.map_mut().tiles.push(rustgis_map::TileLayer::new(
        "Future",
        rustgis_map::TileKind::Raster,
        format!("https://h.invalid/{{z}}/{{x}}/{{y}}.png?token={SECRET}"),
    ));
    let mut document = serde_json::to_value(&future).unwrap();
    let format = crate::project::FORMAT + 1;
    document["format"] = json!(format);
    document["futureRequired"] = json!({"meaning":"preserve this unknown field"});
    let source = serde_json::to_string_pretty(&document).unwrap();
    for command in ["project.openText", "map.import"] {
        let mut project = Project::default();
        project.tasks.push(Task {
            name: "Future document".into(),
            description: String::new(),
            steps: vec![TaskStep {
                title: String::new(),
                instructions: String::new(),
                command: command.into(),
                params: json!({"text":source}),
            }],
        });
        let before = serde_json::to_value(&project).unwrap();
        let (text, report) = project.saved_text(None).unwrap();
        let written: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(written["tasks"][0]["steps"][0]["params"]["text"], source, "{command}: future document bytes stay exact");
        let embedded: Value = serde_json::from_str(written["tasks"][0]["steps"][0]["params"]["text"].as_str().unwrap()).unwrap();
        assert_eq!(embedded["format"], format);
        assert_eq!(embedded["futureRequired"], document["futureRequired"]);
        assert_eq!(serde_json::to_value(&project).unwrap(), before);
        assert_eq!((report.steps, report.unchecked, report.withheld), (0, 1, 0));
        assert_eq!(report.notices(), ["1 step(s) contain opaque parameters that may hold credentials and were kept unchecked"]);
    }
    let malformed = source.strip_suffix('}').unwrap();
    let (clean, report) = crate::roles::command("project.openText", &json!({"text":malformed}), 0);
    assert_eq!(clean.unwrap()["text"], "[parameters withheld]", "malformed credential-bearing text keeps its existing handling");
    assert_eq!((report.withheld, report.unchecked), (1, 0));
}

#[test]
fn durable_formatter_errors_unwinds_and_apply_failures_restore_whole_fields() {
    use durable::{Durable, FieldMut, Leaf, Owner, Prepare};
    let mut s = marked();
    s.project.maps[0].layers[0].dataset.source = Some("/tmp/rustgis-output/data.geojson".into());
    let mut document = Project::default();
    document.tasks = s.project.tasks.clone();
    let text = serde_json::to_string_pretty(&document).unwrap();
    let mut params = json!({"text": text});
    let mut id = "project.openText";
    for _ in 0..6 {
        params = json!({"steps": [{"title":"nested", "command":id, "params":params}]});
        id = "task.new";
    }
    s.project.tasks[0].steps[0].command = id.into();
    s.project.tasks[0].steps[0].params = params;
    // A separate document is actually parsed and rewritten before the deeper envelope withholds.
    s.project.tasks[0].steps.push(TaskStep {
        title: "document".into(),
        instructions: String::new(),
        command: "project.openText".into(),
        params: json!({"text":text}),
    });
    let before = serde_json::to_value(&s.project).unwrap();
    let result = durable::output(&mut s.project, Some("/tmp/rustgis-output"), |p| {
        assert_eq!(p.maps[0].layers[0].dataset.source.as_deref(), Some("./data.geojson"));
        assert!(!serde_json::to_string(p).unwrap().contains(SECRET));
        Err::<(), _>("formatter failed".into())
    });
    assert!(result.is_err());
    assert_eq!(before, serde_json::to_value(&s.project).unwrap());
    #[cfg(not(target_arch = "wasm32"))]
    {
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = durable::output(&mut s.project, Some("/tmp/rustgis-output"), |_| -> Result<(), String> {
                std::panic::resume_unwind(Box::new("formatter failed"))
            });
        }));
        assert!(unwound.is_err());
        assert_eq!(before, serde_json::to_value(&s.project).unwrap());
    }
    struct Bad(Project);
    impl Durable for Bad {
        /// Offers classified whole fields once, before any live mutation.
        fn prepare(&self, p: &mut Prepare<'_>) {
            self.0.prepare(p);
            p.string(Leaf::Video(usize::MAX), "https://h.invalid/?token=RGSYNTH_output_20261103", true, false);
        }

        /// Resolves a prepared location through owned fields, without allocation or policy.
        fn field_mut(&mut self, owners: &[Owner], leaf: &Leaf) -> Option<FieldMut<'_>> {
            self.0.field_mut(owners, leaf)
        }
    }
    let mut bad = Bad(s.project);
    assert!(durable::output(&mut bad, Some("/tmp/rustgis-output"), |_| Ok(())).is_err());
    assert_eq!(before, serde_json::to_value(&bad.0).unwrap());
}

#[test]
fn model_and_presentation_non_item_writers_use_public_forms() {
    let mut s = marked();
    let graph = s.project.models[0].graph.clone().unwrap();
    let out = s.run("model.export", &json!({"model":"Credentials"})).unwrap();
    assert!(!out["text"].as_str().unwrap().contains(SECRET));
    let file = std::env::temp_dir().join(format!("rustgis-output-model-{}.json", std::process::id()));
    s.run("model.export", &json!({"model":"Credentials", "path":file})).unwrap();
    assert!(!std::fs::read_to_string(&file).unwrap().contains(SECRET));
    std::fs::remove_file(file).unwrap();
    assert_eq!(s.project.models[0].graph.as_ref(), Some(&graph));
    let userinfo = format!("https://u:{SECRET}@h.invalid");
    assert_eq!(crate::cmd::presentation::video_label(&userinfo), "h.invalid");
    assert_eq!(crate::cmd::presentation::video_label("/tmp/movie?cut#1.mp4"), "movie?cut#1.mp4");
    s.run("presentation.videoPage", &json!({"source":userinfo})).unwrap();
    assert_eq!(s.project.presentations[0].pages.last().unwrap().title, "h.invalid");
    let (bytes, _) = crate::cmd::presentation::export_bytes(&mut s, 0, "html", 48.0).unwrap();
    let html = String::from_utf8(bytes).unwrap();
    assert!(!html.contains(SECRET));
    assert!(html.contains(DATA));
    assert!(s.notices.iter().any(|n| n.text.contains("video address(es)")));
}

#[test]
fn generated_notebook_errors_are_cleaned_before_the_output_is_retained() {
    let mut s = marked();
    let source = format!("layer.addUrl {{\"url\":\"https://h.invalid/?token={SECRET}\",\"cellSize\":1}}");
    s.project.notebooks[0].cells[0].source = source.clone();
    s.trusted_notebooks.insert("credentials".into(), std::sync::Arc::new(s.item_content("notebook", "credentials").unwrap()));
    let _ = s.run("notebook.run", &json!({"notebook":0}));
    assert_eq!(s.project.notebooks[0].cells[0].source, source);
    assert!(!s.project.notebooks[0].cells[0].output.as_ref().unwrap().contains(SECRET));
    for kind in ["notebook", "jupyter"] {
        assert!(!String::from_utf8_lossy(&crate::cmd::items::item_bytes(&mut s, kind, &Value::Null).unwrap().1).contains(SECRET));
    }
    // Failed file tools retain their failure history through the command rollback. The missing
    // input is a layer name, so this reaches the production error path without any network I/O.
    let history = s.project.gp_history.len();
    assert!(
        s.run(
            "gp.run",
            &json!({"tool":"vectorToPmtiles", "params":{"input":format!("https://h.invalid/?token={SECRET}"), "minZoom":0, "maxZoom":0}})
        )
        .is_err()
    );
    assert_eq!(s.project.gp_history.len(), history + 1);
    let record = s.project.gp_history.last().unwrap();
    assert_eq!(record.id, "vectorToPmtiles");
    assert!(!record.ok && !record.messages.join("\n").contains(SECRET));
}

#[test]
fn peers_bypass_saved_restore_and_retention_requires_the_current_exact_slot() {
    let mut s = marked();
    let saved = s.project.saved_text(None).unwrap().0;
    s.trusted_tasks.insert("credentials".into(), std::sync::Arc::new(s.item_content("task", "credentials").unwrap()));
    let generation = s.opened;
    s.open_peer_text(Peer::Embedder, &saved).unwrap();
    assert!(s.opened != generation && s.trusted_tasks.is_empty());
    assert!(!serde_json::to_string(&s.project).unwrap().contains(SECRET));
    for scenario in ["unchanged", "rotation", "removed", "inserted", "moved", "accounts", "generation", "duplicate_table"] {
        let mut s = marked();
        s.project.tasks.clear();
        s.project.models.clear();
        s.project.gp_history.clear();
        s.project.notebooks.clear();
        s.project.presentations.clear();
        if scenario == "accounts" {
            let mut other = s.project.maps[0].layers[0].clone();
            other.id = 9000;
            other.dataset.source = Some("https://h.invalid/a?token=RGSYNTH_other_20261103&layer=a%20b".to_string());
            s.project.maps[0].layers.push(other);
        }
        if scenario == "duplicate_table" {
            let mut table = rustgis_data::Dataset::new("Table", rustgis_data::GeometryType::None, vec![]);
            table.source = Some(format!("https://h.invalid/table?token={SECRET}"));
            s.project.add_table(table);
        }
        let (public, _) = s.peer_text(Peer::Bridge).unwrap();
        assert!(!public.contains(SECRET));
        let mut incoming: Value = serde_json::from_str(&public).unwrap();
        match scenario {
            "rotation" => {
                s.project.maps[0].layers[0].dataset.source = Some("https://h.invalid/a?token=RGSYNTH_rotated_20261103&layer=a%20b".into())
            }
            "removed" => {
                s.project.maps[0].layers.remove(0);
            }
            "inserted" => incoming["maps"][0]["layers"][0]["id"] = json!(9001),
            "moved" => {
                incoming["maps"][0]["name"] = json!("Another map");
            }
            "generation" => {
                s.run("project.new", &Value::Null).unwrap();
            }
            "duplicate_table" => {
                let table = incoming["tables"][0].clone();
                incoming["tables"].as_array_mut().unwrap().push(table);
            }
            _ => {}
        }
        s.open_peer_text(Peer::Bridge, &incoming.to_string()).unwrap();
        let full = serde_json::to_string(&s.project).unwrap();
        if scenario == "unchanged" || scenario == "duplicate_table" {
            assert!(full.contains(SECRET), "{scenario}: ordinary layer stays");
        } else {
            assert!(!full.contains(SECRET), "{scenario}");
        }
        if scenario == "duplicate_table" {
            assert!(s.project.tables.iter().all(|t| !t.source.as_deref().unwrap_or("").contains(SECRET)));
        }
        assert!(s.trusted_tasks.is_empty() && s.trusted_models.is_empty() && s.trusted_notebooks.is_empty());
    }
}

#[test]
fn peer_video_credential_losses_are_counted() {
    let public = "https://h.invalid/video.mp4";
    let private = format!("{public}?token={SECRET}");
    for (peer, source, replacement, lost) in [
        (Peer::Embedder, public, public, 0),
        (Peer::Embedder, private.as_str(), public, 1),
        (Peer::Bridge, private.as_str(), public, 0),
        (Peer::Bridge, private.as_str(), "https://h.invalid/other.mp4", 1),
    ] {
        let mut project = Project::default();
        project.presentations.push(Presentation {
            name: "Video".into(),
            pages: vec![Page {
                title: "Video".into(),
                kind: PageKind::Video { source: source.into(), poster: None, caption: String::new() },
                notes: String::new(),
                seconds: 5.0,
            }],
            ..Default::default()
        });
        let mut s = Session::new(project);
        let (text, _) = s.peer_text(peer).unwrap();
        assert!(!text.contains(SECRET));
        let report = s.open_peer_text(peer, &text.replace(public, replacement)).unwrap();
        assert_eq!(report.lost(), lost, "{peer:?}: {source} -> {replacement}");
        if lost == 0 {
            assert!(report.notices().is_empty(), "clean or retained video sources lose no credentials");
        } else {
            let notices = report.notices();
            assert_eq!(notices.len(), 1);
            assert!(notices[0].starts_with("1 ") && notices[0].contains("lost their keys"));
            assert!(!serde_json::to_string(&s.project).unwrap().contains(SECRET));
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn short_basic_passwords_are_scrubbed_only_from_output_while_active() {
    if isolate_process_test("tests::credential_output::short_basic_passwords_are_scrubbed_only_from_output_while_active") {
        return;
    }
    use crate::render::rustgis_tiles::logins;

    let owner = Session::new(Project::default());
    let mut other = Session::new(Project::default());
    let value = logins::basic("synthetic-user", "a");
    let origin = "https://short-basic.invalid";
    let binding = logins::activate(
        origin,
        &format!("{}/short-basic", owner.web_logins.session_id()),
        logins::Method::Header("Authorization".into()),
        &value,
        false,
    )
    .unwrap();
    let payload = value.strip_prefix("Basic ").unwrap();
    assert_eq!(logins::scrub(&format!("echo {value} {payload} a")), "echo [redacted] [redacted] [redacted]");
    other.notify("alpha", false);
    assert_eq!(other.notices.last().unwrap().text, "[redacted]lph[redacted]", "active-secret matching covers other sessions' output");
    other.notify("loop done", false);
    assert_eq!(other.notices.last().unwrap().text, "loop done", "unmatched output stays intact");
    let url = format!("{origin}/data");
    let bound = logins::bind(&binding, &url).unwrap();
    let request = logins::apply(&bound).unwrap().unwrap();
    assert_eq!(request.url, url, "output redaction never changes the request URL");
    assert_eq!(request.header.unwrap().1, value, "the raw credential remains the request input");
    assert!(logins::deactivate(&binding));
    other.notify("alpha", false);
    assert_eq!(other.notices.last().unwrap().text, "alpha", "revocation does not retain the short-password pattern");
}

/// Imported database origins are output fields even when no database has been opened.
#[test]
fn durable_database_origins_clean_each_owned_field_without_opening_a_database() {
    for (role, scheme, suffix) in [
        ("edit", "postgresql", "/postgis/origin/database"),
        ("postgis", "postgresql", "/source/postgis/database"),
        ("mysql", "mysql", "/source/mysql/database"),
        ("sqlserver", "mssql", "/source/sqlserver/database"),
        ("duckdb", "postgresql", "/source/duckdb/database"),
        ("gdal+duckdb", "postgresql", "/source/duckdb/database"),
    ] {
        for private in [false, true] {
            let public = format!("{scheme}://reader@db-origin.invalid/gis");
            let address =
                if private { format!("{scheme}://reader:RGSYNTH_db_origin_20261106@db-origin.invalid/gis") } else { public.clone() };
            let dataset = rustgis_data::Dataset::new("Origin", rustgis_data::GeometryType::None, Vec::new());
            let mut layer = serde_json::to_value(rustgis_map::Layer::new(1, dataset)).unwrap();
            let mut origin = json!({"database":address, "table":"rows"});
            if matches!(role, "duckdb" | "gdal+duckdb") {
                origin["family"] = json!("Points");
            }
            if role == "edit" {
                layer["postgis"] = json!({"origin":origin, "key":"id", "srid":4326, "writable":false});
            } else {
                layer["source"] = json!({});
                layer["source"][if role == "gdal+duckdb" { "duckdb" } else { role }] = origin;
                if role == "gdal+duckdb" {
                    layer["source"]["gdal"] = json!({"path":"unused.geojson", "layer":"rows"});
                }
            }
            let mut project = Project::default();
            project.map_mut().layers.push(serde_json::from_value(layer).unwrap());
            // This is the real import decoder before normalization opens any source. Session::new
            // preserves the decoded project; neither output API opens a database or runs a command.
            let imported = Project::from_peer_raw(&serde_json::to_string(&project).unwrap()).unwrap();
            let mut s = Session::new(imported);
            let before = serde_json::to_value(&s.project).unwrap();
            let slot = format!("/maps/0/layers/0{suffix}");
            assert_eq!(before.pointer(&slot), Some(&json!(address)), "{role}: Deserialize keeps the imported field");
            // Output serialization uses each field's JSON representation (not f32 -> Value widening).
            let mut expected: Value = serde_json::from_str(&serde_json::to_string(&s.project).unwrap()).unwrap();
            *expected.pointer_mut(&slot).unwrap() = json!(public);
            let (saved, report) = s.project.saved_text(None).unwrap();
            let outputs = [
                ("save", (saved, report)),
                ("embedder", s.peer_text(Peer::Embedder).unwrap()),
                ("bridge", s.peer_text(Peer::Bridge).unwrap()),
            ];
            for (kind, (text, report)) in outputs {
                assert!(!text.contains("RGSYNTH_db_origin_20261106"), "{kind}: {role}");
                assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), expected, "{kind}: {role}, private={private}");
                assert_eq!(report.layer_addresses(), u32::from(private), "{kind}: {role}");
                let notices = if private {
                    vec!["1 layer address(es) were saved without their keys, tokens, passwords or URL fragments; after RustGIS closes, add those layers again to restore missing values".to_string()]
                } else {
                    Vec::new()
                };
                assert_eq!(report.notices(), notices, "{kind}: {role}");
            }
            if role == "gdal+duckdb" {
                let mark = s.notice_mark();
                let (_, bytes) = crate::cmd::items::item_bytes(&mut s, "layer", &json!({"layer":1})).unwrap();
                let written: Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(written, expected["maps"][0]["layers"][0]);
                assert_eq!(s.notices_since(mark).len(), usize::from(private), "layer export has one aggregate notice or none");
            }
            assert_eq!(serde_json::to_value(&s.project).unwrap(), before, "{role}: all output rolls back live fields");
        }
    }
}

/// Runtime-only login omissions and executable replacement losses use different notice kinds.
#[test]
fn peer_output_counts_runtime_logins_and_changed_executable_fields() {
    let public = "https://peer-runtime.invalid/data";
    let bound = rustgis_data::login_address::bind("0123456789abcdef", "https://peer-runtime.invalid", public).unwrap();
    let mut s = Session::new(Project::default());
    let mut dataset = rustgis_data::Dataset::new("Bound", rustgis_data::GeometryType::None, Vec::new());
    dataset.source = Some(bound.clone());
    let id = s.map_mut().add_dataset(dataset);
    s.map_mut().layer_mut(id).unwrap().connection = "saved-connection".into();
    s.project.tasks.push(Task {
        name: "Login".into(),
        description: String::new(),
        steps: vec![TaskStep {
            title: String::new(),
            instructions: String::new(),
            command: "connection.login".into(),
            params: json!({"apiKey":SECRET}),
        }],
    });
    let (text, report) = s.peer_text(Peer::Embedder).unwrap();
    assert_eq!(report.layer_addresses(), 1, "a foreign receiver cannot use the recorded connection's live binding");
    assert!(!text.contains("rustgis-login:") && !text.contains(SECRET));
    assert_eq!(s.map().layer(id).unwrap().dataset.source.as_deref(), Some(bound.as_str()));
    let report = s.open_peer_text(Peer::Embedder, &text).unwrap();
    assert_eq!(report.executable_changes, 1);
    assert!(report.notices().iter().any(|n| n.contains("executable field(s) changed")));
    assert_eq!(s.project.tasks[0].steps[0].params["apiKey"], "");
}

/// A valid S3 wrapper's endpoint is also an address, not an opaque place to store a password.
#[test]
fn durable_s3_endpoints_clean_recognized_credentials_and_keep_provenance() {
    for (endpoint, expected, count) in [
        ("https://reader:RGSYNTH_endpoint@h.invalid", "https://h.invalid", 1),
        ("https://h.invalid?token=RGSYNTH_endpoint", "https://h.invalid", 1),
        ("https://h.invalid", "https://h.invalid", 0),
        ("", "", 0),
    ] {
        let address = rustgis_data::s3_address::bind("0123456789abcdef", endpoint, "s3://bucket/data").unwrap();
        let mut p = Project::default();
        let mut table = rustgis_data::Dataset::new("S3", rustgis_data::GeometryType::None, Vec::new());
        table.source = Some(address.clone());
        p.add_table(table);
        let (text, report) = p.saved_text(None).unwrap();
        assert!(!text.contains("RGSYNTH_endpoint"));
        assert_eq!(report.layer_addresses(), count);
        let value: Value = serde_json::from_str(&text).unwrap();
        let parsed = rustgis_data::s3_address::parse(value["tables"][0]["source"].as_str().unwrap()).unwrap().unwrap();
        assert_eq!((parsed.binding, parsed.endpoint, parsed.url), ("0123456789abcdef", expected, "s3://bucket/data"));
        assert_eq!(p.tables[0].source.as_deref(), Some(address.as_str()));
    }
}

#[test]
fn nested_notebook_source_stays_opaque_but_is_reported_unchecked() {
    for command in ["notebook.addCell", "notebook.editCell"] {
        for source in ["layer.addUrl {\"url\":\"https://h.invalid/?token=RGSYNTH_nested\"}", "print('RGSYNTH_nested token')", "clean prose"]
        {
            let mut p = Project::default();
            p.tasks.push(Task {
                name: "Nested".into(),
                description: String::new(),
                steps: vec![TaskStep {
                    title: String::new(),
                    instructions: String::new(),
                    command: command.into(),
                    params: json!({"source":source}),
                }],
            });
            let (text, report) = p.saved_text(None).unwrap();
            let written: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(written["tasks"][0]["steps"][0]["params"]["source"], source);
            assert_eq!(report.unchecked, u32::from(source != "clean prose"));
            assert_eq!(report.steps, 0);
        }
    }
    for url in ["https://h.invalid/a", "pg:host=h dbname=gis", "mariadb://h/gis", "pmtiles:https://h.invalid/a", "cog:https://h.invalid/a"]
    {
        assert_eq!(crate::log_params(&json!({"key":"abc", "source":{"url":url}})).into_log_value()["key"], "", "{url}");
    }
    assert_eq!(crate::log_params(&json!({"key":"abc", "data":{"url":"https://h.invalid/a"}})).into_log_value()["key"], "abc");
}

/// A retained address must actually reopen its protected reader, not merely survive as text.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn bridge_round_trip_reopens_its_protected_reader() {
    if super::isolate_process_test("tests::credential_output::bridge_round_trip_reopens_its_protected_reader") {
        return;
    }
    use rustgis_data::source::{fgb, remote};
    let (dataset, _) = rustgis_data::io::read_geojson(r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Point","coordinates":[1,2]},"properties":{"value":7}}]}"#, "Protected").unwrap();
    let bytes = fgb::write(&dataset).unwrap();
    let original = fgb::FgbSource::open_bytes(bytes.clone(), "Protected").unwrap();
    let full = "https://protected-reader.invalid/data.fgb?token=RGSYNTH_reader";
    // Session construction installs the native fetcher; replace it only after that setup.
    let mut s = Session::new(Project::default());
    remote::set_fetcher(move |url, want, done| {
        if url != full {
            done(Err(remote::Failed::Other("protected reader needs its token".into())));
        } else if matches!(want, remote::Want::Whole) {
            done(Ok(bytes.clone()));
        } else {
            done(Err(remote::Failed::NoRanges));
        }
    });
    let id = s.map_mut().add_dataset(dataset);
    s.map_mut().layer_mut(id).unwrap().source = Some(rustgis_map::source::SourceLayer::from_path(full, None, Box::new(original)));
    let (text, _) = s.peer_text(Peer::Bridge).unwrap();
    assert!(!text.contains("RGSYNTH_reader"));
    let report = s.open_peer_text(Peer::Bridge, &text).unwrap();
    assert_eq!(report.lost, 0, "the unchanged streamed source retains its address");
    let source = s.map_mut().layer_mut(id).unwrap().source.as_mut().unwrap();
    assert_eq!(source.path.as_deref(), Some(full), "the exact source keeps its protected address");
    source.wait_open(std::time::Duration::from_secs(5));
    assert!(source.is_open(), "retained reader failed to open: {:?}", source.error);
    let (read, _) = source.read_all().unwrap();
    assert_eq!(read.features.len(), 1);
    assert_eq!(read.features[0].attrs[0], rustgis_data::Value::Int(7));
}

/// Discovery must announce omitted settings even when the following supplied document is invalid.
#[test]
fn failed_collections_discovery_retains_its_counted_query_notice() {
    for (query, count) in [("", 0), ("?token=RGSYNTH_discovery", 1)] {
        let mut s = Session::new(Project::default());
        let p = json!({"url":format!("https://discovery.invalid/root{query}"), "type":"ogcapi",
            "text":json!({"links":[{"rel":"data", "type":"application/json", "href":"/catalog"}]}).to_string(),
            "documents":[{"url":"https://discovery.invalid/catalog", "text":"invalid capabilities"}]});
        assert!(s.run("service.layers", &p).is_err());
        let messages: Vec<_> = s.notices.iter().filter(|n| n.text.contains("address(es) the service listed")).collect();
        assert_eq!(messages.len(), count);
        if count > 0 {
            assert!(messages[0].text.starts_with("1 address(es)"));
        }
    }
}

/// Coverage reports discovery omissions even when the selected coverage is missing.
#[test]
fn coverage_discovery_retains_its_counted_query_notice() {
    for (query, count) in [("", 0), ("?token=RGSYNTH_coverage", 1)] {
        let mut s = Session::new(Project::default());
        let p = json!({"url":format!("https://coverage.invalid/root{query}"), "type":"ogcapi", "layer":"missing",
            "text":json!({"links":[{"rel":"data", "type":"application/json", "href":"/catalog"}]}).to_string(),
            "documents":[{"url":"https://coverage.invalid/catalog", "text":"{\"collections\":[]}"}]});
        assert!(s.run("service.coverage", &p).unwrap_err().contains("no coverage"));
        assert_eq!(s.notices.iter().filter(|n| n.text.contains("address(es) the service listed")).count(), count);
    }
}

/// Capabilities are opaque protocol documents, distinct from user feature data and expressions.
#[test]
fn supplied_service_documents_are_preserved_with_counted_unchecked_notices() {
    for command in ["service.layers", "service.add", "service.coverage", "service.import", "service.groupPlan"] {
        for private in [false, true] {
            let text = if private { "<href>https://h.invalid/a?token=RGSYNTH_document</href>" } else { "<href>https://h.invalid/a</href>" };
            let (clean, report) = crate::roles::command(command, &json!({"text":text}), 0);
            assert!(clean.is_none(), "{command}: protocol documents keep exact bytes");
            assert_eq!(report.unchecked, u32::from(private), "{command}: {report:?}");
            assert_eq!(report.notices().len(), usize::from(private));
        }
    }
}
