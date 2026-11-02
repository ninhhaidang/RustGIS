//! Exercise ordered lists through the real Tools pane, including run/history round trips.
use egui_kittest::{Harness, kittest::Queryable};
use rustgis_engine::{
    Project, Session,
    data::{Dataset, Field, FieldType, GeometryType, Value},
};
use rustgis_ui_egui::RustGisApp;
use serde_json::json;

/// A standalone table and a spatial layer with different schemas reveal incorrect field sourcing.
fn harness(tool: &str, values: serde_json::Value) -> Harness<'static, RustGisApp> {
    let mut session = Session::new(Project::default());
    let mut table = Dataset::new(
        "Records",
        GeometryType::None,
        vec![Field::new("region", FieldType::Text), Field::new("category", FieldType::Text), Field::new("amount", FieldType::Double)],
    );
    table.push(None, vec![Value::Text("North".into()), Value::Text("A".into()), Value::Float(14.3)]);
    table.push(None, vec![Value::Text("North".into()), Value::Text("A".into()), Value::Float(2.0)]);
    session.project.add_table(table);
    session.map_mut().add_dataset(Dataset::new("Target", GeometryType::Point, vec![Field::new("target_only", FieldType::Text)]));
    let mut app = RustGisApp::new(session);
    app.run("ui.gp.open", json!({"tool": tool, "values": values})).unwrap();
    let mut h = Harness::builder().with_size(egui::vec2(1500.0, 1400.0)).with_max_steps(64).build_ui_state(
        |ui, app: &mut RustGisApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        app,
    );
    h.run_steps(3);
    h
}

/// Clicks Run and waits for the tool to land: on the desktop it runs on a thread of its own.
fn click_run(h: &mut Harness<'_, RustGisApp>) {
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Run").click();
    h.run_steps(1);
    let end = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while h.state().gp.tool_run.is_some() {
        assert!(std::time::Instant::now() < end, "the run never landed");
        std::thread::sleep(std::time::Duration::from_millis(5));
        h.run_steps(1);
    }
    h.run_steps(2);
}

/// AccessKit labels distinguish the list's field and statistic selectors from other tool inputs.
fn choose(h: &mut Harness<'_, RustGisApp>, label: &str, value: &str) {
    h.get_by_role_and_label(egui::accesskit::Role::ComboBox, label).click();
    h.run_steps(2);
    h.get_by_label(value).click_accesskit();
    h.run_steps(2);
}

#[test]
fn summary_list_runs_with_ordered_case_fields_and_reopens_from_history() {
    let mut h = harness("summaryStatistics", json!({"input": "Records", "case": ["region", "category"]}));
    h.get_by_label("Add statistic").click();
    h.run_steps(2);
    // There are existing case-field selectors, so choose the first list field (Statistics).
    h.get_all_by_role_and_label(egui::accesskit::Role::ComboBox, "Field").next().unwrap().click();
    h.run_steps(2);
    h.get_by_label("amount").click();
    h.run_steps(2);
    choose(&mut h, "Statistic", "SUM");
    click_run(&mut h);
    let history = h.state().session.project.gp_history.last().unwrap();
    assert_eq!(history.params["statistics"], json!([{"field":"amount","statistic":"SUM"}]));
    assert_eq!(history.params["case"], json!(["region", "category"]));
    let output = h.state().session.project.tables.last().unwrap();
    assert_eq!(output.dataset.value(0, 3).as_f64(), Some(16.3));
    let params = history.params.clone();
    h.state_mut().run("ui.gp.open", json!({"tool":"summaryStatistics", "values":params})).unwrap();
    h.run_steps(3);
    assert_eq!(serde_json::from_str::<serde_json::Value>(&h.state().gp.values["statistics"]).unwrap(), params["statistics"]);
    let mut reversed = params;
    reversed["case"] = json!(["category", "region"]);
    h.state_mut().run("ui.gp.open", json!({"tool":"summaryStatistics", "values":reversed})).unwrap();
    h.run_steps(3);
    click_run(&mut h);
    let output = &h.state().session.project.tables.last().unwrap().dataset;
    assert_eq!(output.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["category", "region", "FREQUENCY", "SUM_amount"]);
    assert_eq!(output.value(0, 3).as_f64(), Some(16.3));
}

#[test]
fn merge_rules_offer_join_fields_and_text_rules_and_preserve_delimiter() {
    let mut h =
        harness("spatialJoin", json!({"target":"Target", "join":"Records", "merge":[{"field":"region","rule":"join","delimiter":"; "}]}));
    h.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Field").click();
    h.run_steps(2);
    assert!(h.query_by_label("amount").is_some());
    assert!(h.query_by_label("target_only").is_none());
    h.get_by_label("category").click();
    h.run_steps(2);
    h.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Rule").click();
    h.run_steps(2);
    assert!(h.query_by_label("sum").is_none());
    assert!(h.query_by_label("join").is_some());
    h.get_by_role_and_label(egui::accesskit::Role::Button, "join").click_accesskit();
    h.run_steps(2);
    let rules: serde_json::Value = serde_json::from_str(&h.state().gp.values["merge"]).unwrap();
    assert_eq!(rules, json!([{"field":"category","rule":"join","delimiter":"; "}]));
    click_run(&mut h);
    let history = h.state().session.project.gp_history.last().unwrap();
    assert_eq!(history.params["merge"], json!([{"field":"category","rule":"join","delimiter":"; "}]));
}

#[test]
fn pivot_row_fields_can_be_added_reordered_and_removed() {
    let mut h = harness("pivotTable", json!({"input":"Records", "fields":"region"}));
    h.get_by_label("Add field").click();
    h.run_steps(2);
    h.get_all_by_role_and_label(egui::accesskit::Role::ComboBox, "Field").nth(1).unwrap().click();
    h.run_steps(2);
    assert!(h.query_by_label("region").is_none(), "a field cannot be listed twice");
    h.get_by_label("category").click();
    h.run_steps(2);
    h.get_all_by_role_and_label(egui::accesskit::Role::Button, "Move up").nth(1).unwrap().click();
    h.run_steps(2);
    assert_eq!(serde_json::from_str::<serde_json::Value>(&h.state().gp.values["fields"]).unwrap(), json!(["category", "region"]));
    h.get_all_by_role_and_label(egui::accesskit::Role::Button, "Remove").nth(1).unwrap().click();
    h.run_steps(2);
    assert_eq!(serde_json::from_str::<serde_json::Value>(&h.state().gp.values["fields"]).unwrap(), json!(["category"]));
}

#[test]
fn missing_fields_and_malformed_history_are_kept_until_edited() {
    let saved = json!([{"field":"missing","statistic":"P90","extension":42}]);
    let mut h = harness("summaryStatistics", json!({"input":"Records", "statistics":saved}));
    assert!(h.query_by_label("Field is missing from this input.").is_some());
    assert_eq!(serde_json::from_str::<serde_json::Value>(&h.state().gp.values["statistics"]).unwrap(), saved);
    h.state_mut().run("ui.gp.open", json!({"tool":"summaryStatistics","values":{"statistics":"{broken"}})).unwrap();
    h.run_steps(3);
    assert!(h.query_by_label("JSON value").is_some());
    assert_eq!(serde_json::from_str::<serde_json::Value>(&h.state().gp.values["statistics"]).unwrap(), json!("{broken"));
}

#[test]
fn model_lists_accept_future_fields_and_undo_the_edit() {
    use rustgis_engine::project::{ModelNode, ModelNodeKind};
    let mut h = harness("summaryStatistics", json!({}));
    h.state_mut().run("ui.pane.geoprocessing", json!({})).unwrap();
    h.state_mut().run("ui.modelBuilder", json!({})).unwrap();
    h.state_mut().mb.graph.nodes.push(ModelNode {
        id: "summary".into(),
        kind: ModelNodeKind::Tool,
        tool: Some("summaryStatistics".into()),
        params: json!({}),
        ..Default::default()
    });
    h.state_mut().mb.selected = Some("summary".into());
    h.run_steps(3);
    h.get_by_label("Add statistic").click();
    h.run_steps(3);
    h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Field").focus();
    h.step();
    h.event(egui::Event::Text("future_".into()));
    h.run_steps(3);
    h.event(egui::Event::Text("amount".into()));
    h.run_steps(3);
    choose(&mut h, "Statistic", "P90");
    assert_eq!(h.state().mb.graph.nodes[0].params["statistics"], json!([{"field":"future_amount","statistic":"P90"}]));
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Remove").click();
    h.run_steps(3);
    assert_eq!(h.state().mb.graph.nodes[0].params["statistics"], json!([]));
    // ModelBuilder owns its undo stack; the actual shortcut must restore the list.
    h.event(egui::Event::Key { key: egui::Key::Z, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND });
    h.run_steps(3);
    assert_eq!(h.state().mb.graph.nodes[0].params["statistics"], json!([{"field":"future_amount","statistic":"P90"}]));
    // Undo the statistic selection, then the entire multi-frame typing gesture.
    for _ in 0..2 {
        h.event(egui::Event::Key {
            key: egui::Key::Z,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::COMMAND,
        });
        h.run_steps(3);
    }
    assert_eq!(h.state().mb.graph.nodes[0].params["statistics"], json!([{"field":"","statistic":""}]));
}

#[test]
fn malformed_row_field_json_can_be_repaired_across_frames() {
    let mut h = harness("pivotTable", json!({"input":"Records", "fields":{"invalid":true}}));
    h.get_by_role_and_label(egui::accesskit::Role::MultilineTextInput, "JSON value").focus();
    h.step();
    h.event(egui::Event::Key { key: egui::Key::A, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND });
    h.event(egui::Event::Text("[".into()));
    h.run_steps(3);
    assert!(h.query_by_role_and_label(egui::accesskit::Role::MultilineTextInput, "JSON value").is_some());
    assert_eq!(serde_json::from_str::<serde_json::Value>(&h.state().gp.values["fields"]).unwrap(), json!({"invalid":true}));
    h.event(egui::Event::Text("\"region\"]".into()));
    h.run_steps(3);
    assert_eq!(serde_json::from_str::<serde_json::Value>(&h.state().gp.values["fields"]).unwrap(), json!(["region"]));
}

#[test]
fn model_merge_fields_follow_the_wired_join_input() {
    use rustgis_engine::project::{ModelEdge, ModelNode, ModelNodeKind};
    let mut h = harness("spatialJoin", json!({}));
    h.state_mut().run("ui.pane.geoprocessing", json!({})).unwrap();
    h.state_mut().run("ui.modelBuilder", json!({})).unwrap();
    h.state_mut().mb.graph.nodes.extend([
        ModelNode { id: "rows".into(), kind: ModelNodeKind::Input, layer: Some("Records".into()), ..Default::default() },
        ModelNode {
            id: "join".into(),
            kind: ModelNodeKind::Tool,
            tool: Some("spatialJoin".into()),
            params: json!({"target":"Target", "merge":[{"field":"region","rule":"first"}]}),
            ..Default::default()
        },
    ]);
    h.state_mut().mb.graph.edges.push(ModelEdge {
        from: "rows".into(),
        from_port: "out".into(),
        to: "join".into(),
        to_port: "join".into(),
    });
    h.state_mut().mb.selected = Some("join".into());
    h.run_steps(3);
    h.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Field").click();
    h.run_steps(3);
    assert!(h.query_by_label("amount").is_some());
    assert!(h.query_by_label("target_only").is_none());
}

#[test]
fn invalid_statistics_text_is_not_changed_into_an_empty_list() {
    let mut h = harness("summaryStatistics", json!({"input":"Records", "field":"amount", "statistics":"null"}));
    assert!(h.query_by_label("JSON value").is_some());
    click_run(&mut h);
    let result = h.state().gp.last.as_ref().unwrap();
    assert!(!result.0);
    assert!(result.1.iter().any(|message| message.contains("Statistics must be a list")));
    assert_eq!(serde_json::from_str::<serde_json::Value>(&h.state().gp.values["statistics"]).unwrap(), json!("null"));
}

#[test]
fn saved_percentile_alias_is_available_and_cannot_be_added_twice() {
    let saved = json!([{"field":"amount","statistic":"P01"}]);
    let mut h = harness("summaryStatistics", json!({"input":"Records","statistics":saved}));
    assert!(h.query_by_label("Statistic is not available for this field.").is_none());
    assert_eq!(serde_json::from_str::<serde_json::Value>(&h.state().gp.values["statistics"]).unwrap(), saved);
    h.get_by_label("Add statistic").click();
    h.run_steps(2);
    h.get_all_by_role_and_label(egui::accesskit::Role::ComboBox, "Field").nth(1).unwrap().click();
    h.run_steps(2);
    h.get_by_label("amount").click();
    h.run_steps(2);
    h.get_all_by_role_and_label(egui::accesskit::Role::ComboBox, "Statistic").nth(1).unwrap().click();
    h.run_steps(2);
    assert!(h.query_by_role_and_label(egui::accesskit::Role::Button, "P1").is_none());
    assert!(h.query_by_role_and_label(egui::accesskit::Role::Button, "P2").is_some());
}

/// Parseable intermediate JSON must keep its formatting and cursor while the shape is repaired.
#[test]
fn json_repair_keeps_formatted_non_list_drafts() {
    let original = json!({"invalid":true});
    let mut h = harness("pivotTable", json!({"input":"Records", "fields":original}));
    h.get_by_role_and_label(egui::accesskit::Role::MultilineTextInput, "JSON value").focus();
    h.step();
    for draft in ["{\n  \"invalid\": false\n}", "[ 2020 ]", "2020"] {
        h.event(egui::Event::Key {
            key: egui::Key::A,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::COMMAND,
        });
        h.event(egui::Event::Text(draft.into()));
        h.run_steps(3);
        assert_eq!(h.get_by_role_and_label(egui::accesskit::Role::MultilineTextInput, "JSON value").value().as_deref(), Some(draft));
        assert_eq!(serde_json::from_str::<serde_json::Value>(&h.state().gp.values["fields"]).unwrap(), original);
    }
    h.event(egui::Event::Key { key: egui::Key::A, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND });
    h.event(egui::Event::Text("[\"region\"]".into()));
    h.run_steps(3);
    assert_eq!(serde_json::from_str::<serde_json::Value>(&h.state().gp.values["fields"]).unwrap(), json!(["region"]));
}

/// Exact case-variant fields are distinct to merge execution and retain their own type choices.
#[test]
fn merge_picker_resolves_case_variants_and_duplicate_headers_like_execution() {
    let mut h = harness("spatialJoin", json!({"target":"Target", "join":"Records", "merge":[{"field":"region","rule":"sum"}]}));
    h.state_mut().session.project.tables[0].dataset.fields.push(Field::new("REGION", FieldType::Double));
    h.state_mut().session.project.tables[0].dataset.fields.push(Field::new("region", FieldType::Double));
    h.run_steps(3);
    assert!(h.query_by_label("Rule is not available for this field.").is_some());
    // Exact duplicate headers resolve to the first field, which is text, not the later Double.
    choose(&mut h, "Rule", "join");
    h.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Rule").click();
    h.run_steps(2);
    assert!(h.query_by_role_and_label(egui::accesskit::Role::Button, "sum").is_none());
    h.get_by_role_and_label(egui::accesskit::Role::Button, "join").click_accesskit();
    h.run_steps(2);
    h.get_by_label("Add rule").click();
    h.run_steps(3);
    h.get_all_by_role_and_label(egui::accesskit::Role::ComboBox, "Field").nth(1).unwrap().click();
    h.run_steps(2);
    assert!(h.query_by_role_and_label(egui::accesskit::Role::Button, "region").is_none());
    h.get_by_role_and_label(egui::accesskit::Role::Button, "REGION").click();
    h.run_steps(2);
    h.get_all_by_role_and_label(egui::accesskit::Role::ComboBox, "Rule").nth(1).unwrap().click();
    h.run_steps(2);
    h.get_by_role_and_label(egui::accesskit::Role::Button, "sum").click();
    h.run_steps(2);
    let rules: serde_json::Value = serde_json::from_str(&h.state().gp.values["merge"]).unwrap();
    assert_eq!(rules, json!([{"field":"region","rule":"join"},{"field":"REGION","rule":"sum"}]));
}

/// An unrun tool's output schema is unknown while editing, but the typed field must execute.
#[test]
fn model_typed_statistic_runs_on_an_unrun_tools_new_field() {
    use rustgis_engine::project::{ModelEdge, ModelNode, ModelNodeKind};
    use rustgis_engine::{data::Geometry, geom::Coord};
    let mut h = harness("summaryStatistics", json!({}));
    h.state_mut().session.map_mut().layers[0]
        .dataset
        .push(Some(Geometry::Point(Coord::new(10.0, 20.0))), vec![Value::Text("point".into())]);
    h.state_mut().run("ui.pane.geoprocessing", json!({})).unwrap();
    h.state_mut().run("ui.modelBuilder", json!({})).unwrap();
    h.state_mut().mb.graph.nodes.extend([
        ModelNode {
            id: "producer".into(),
            kind: ModelNodeKind::Tool,
            tool: Some("featureVerticesToPoints".into()),
            params: json!({"input":"Target"}),
            ..Default::default()
        },
        ModelNode {
            id: "consumer".into(),
            kind: ModelNodeKind::Tool,
            tool: Some("summaryStatistics".into()),
            params: json!({}),
            ..Default::default()
        },
    ]);
    h.state_mut().mb.graph.edges.push(ModelEdge {
        from: "producer".into(),
        from_port: "out".into(),
        to: "consumer".into(),
        to_port: "input".into(),
    });
    h.state_mut().mb.selected = Some("consumer".into());
    h.run_steps(3);
    h.get_by_label("Add statistic").click();
    h.run_steps(3);
    h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Field").focus();
    h.step();
    h.event(egui::Event::Text("orig_fid".into()));
    h.run_steps(3);
    choose(&mut h, "Statistic", "COUNT");
    let graph = h.state().mb.graph.clone();
    h.state_mut().run("model.run", json!({"graph":graph})).unwrap();
    assert!(h.state().session.model_run.as_ref().unwrap().ok);
    let ds = &h.state().session.project.tables.last().unwrap().dataset;
    let column = ds.field_index("COUNT_orig_fid").unwrap();
    assert_eq!(ds.value(0, column).as_f64(), Some(1.0));
}

/// A numeric model input must use that table's schema even when a map layer has its ID as a name.
#[test]
fn model_numeric_table_input_filters_statistics_by_its_own_fields() {
    use rustgis_engine::project::{ModelNode, ModelNodeKind};
    let mut h = harness("summaryStatistics", json!({}));
    let id = h.state().session.project.tables[0].id;
    h.state_mut().session.map_mut().add_dataset(Dataset::new(
        id.to_string(),
        GeometryType::Point,
        vec![Field::new("wrong_layer_field", FieldType::Double)],
    ));
    h.state_mut().run("ui.pane.geoprocessing", json!({})).unwrap();
    h.state_mut().run("ui.modelBuilder", json!({})).unwrap();
    h.state_mut().mb.graph.nodes.push(ModelNode {
        id: "summary".into(),
        kind: ModelNodeKind::Tool,
        tool: Some("summaryStatistics".into()),
        params: json!({"input":id,"statistics":[{"field":"region","statistic":"P90"}]}),
        ..Default::default()
    });
    h.state_mut().mb.selected = Some("summary".into());
    h.run_steps(3);
    assert!(h.query_by_label("Statistic is not available for this field.").is_some());
    h.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Field").click();
    h.run_steps(2);
    assert!(h.query_by_label("wrong_layer_field").is_none());
    h.get_by_role_and_label(egui::accesskit::Role::Button, "region").click_accesskit();
    h.run_steps(2);
    h.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Statistic").click();
    h.run_steps(2);
    assert!(h.query_by_role_and_label(egui::accesskit::Role::Button, "P90").is_none());
    h.get_by_role_and_label(egui::accesskit::Role::Button, "COUNT").click_accesskit();
    h.run_steps(2);
    assert_eq!(h.state().mb.graph.nodes[0].params["input"], json!(id));
    assert_eq!(h.state().mb.graph.nodes[0].params["statistics"], json!([{"field":"region","statistic":"COUNT"}]));
    // The legacy Statistics Field shortcut uses the same numeric table schema.
    h.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Statistics Field").click();
    h.run_steps(2);
    assert!(h.query_by_role_and_label(egui::accesskit::Role::Button, "region").is_none());
    assert!(h.query_by_role_and_label(egui::accesskit::Role::Button, "wrong_layer_field").is_none());
    h.get_by_role_and_label(egui::accesskit::Role::Button, "amount").click();
    h.run_steps(2);
    assert_eq!(h.state().mb.graph.nodes[0].params["field"], json!("amount"));
}

/// Distinct case-variant inputs cannot generate duplicate Summary output column names.
#[test]
fn summary_picker_excludes_case_variant_output_collisions() {
    let mut h = harness("summaryStatistics", json!({"input":"Records","statistics":[{"field":"region","statistic":"COUNT"}]}));
    let table = &mut h.state_mut().session.project.tables[0].dataset;
    table.fields.push(Field::new("REGION", FieldType::Double));
    for feature in &mut table.features {
        feature.attrs.push(Value::Float(2.5));
    }
    h.run_steps(3);
    h.get_by_label("Add statistic").click();
    h.run_steps(2);
    h.get_all_by_role_and_label(egui::accesskit::Role::ComboBox, "Field").nth(1).unwrap().click();
    h.run_steps(2);
    h.get_by_role_and_label(egui::accesskit::Role::Button, "REGION").click_accesskit();
    h.run_steps(2);
    h.get_all_by_role_and_label(egui::accesskit::Role::ComboBox, "Statistic").nth(1).unwrap().click();
    h.run_steps(2);
    assert!(h.query_by_role_and_label(egui::accesskit::Role::Button, "COUNT").is_none());
    h.get_by_role_and_label(egui::accesskit::Role::Button, "SUM").click_accesskit();
    h.run_steps(2);
    click_run(&mut h);
    assert!(h.state().gp.last.as_ref().unwrap().0);
    let output = &h.state().session.project.tables.last().unwrap().dataset;
    assert_eq!(output.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["FREQUENCY", "COUNT_region", "SUM_REGION"]);
    assert_eq!(output.value(0, 2).as_f64(), Some(5.0));
    h.state_mut()
        .run(
            "ui.gp.open",
            json!({"tool":"summaryStatistics","values":{"input":"Records","statistics":[{"field":"region","statistic":"COUNT"},{"field":"amount","statistic":"COUNT"}]}}),
        )
        .unwrap();
    h.run_steps(3);
    h.get_all_by_role_and_label(egui::accesskit::Role::ComboBox, "Statistic").nth(1).unwrap().click();
    h.run_steps(2);
    assert!(h.query_by_role_and_label(egui::accesskit::Role::Button, "COUNT").is_some(), "an unrelated field still offers COUNT");
}

#[test]
fn saved_duplicate_statistics_explain_how_to_repair_them() {
    let statistics = json!([{"field":"amount","statistic":"SUM"},{"field":"amount","statistic":"SUM"}]);
    let mut h = harness("summaryStatistics", json!({"input":"Records","statistics":statistics}));
    assert_eq!(h.query_all_by_label("Statistic is already listed for this field.").count(), 2);
    assert!(h.query_by_label("Statistic is not available for this field.").is_none());
    h.get_all_by_role_and_label(egui::accesskit::Role::Button, "Remove").nth(1).unwrap().click();
    h.run_steps(3);
    assert!(h.query_by_label("Statistic is already listed for this field.").is_none());
    click_run(&mut h);
    assert!(h.state().gp.last.as_ref().unwrap().0);
    let output = &h.state().session.project.tables.last().unwrap().dataset;
    assert_eq!(output.value(0, 1).as_f64(), Some(16.3));
}

/// Summarize Within's statistics list takes the fields of its `points` input, not the polygons',
/// and its separator appears only once CONCATENATE is picked.
#[test]
fn summarize_within_statistics_offer_the_point_layers_fields() {
    use rustgis_engine::{data::Geometry, geom::Coord};
    let mut h = harness("countPoints", json!({"polygons": "Zones", "points": "Target"}));
    let ring = vec![Coord::new(0.0, 0.0), Coord::new(4.0, 0.0), Coord::new(4.0, 4.0), Coord::new(0.0, 4.0), Coord::new(0.0, 0.0)];
    let mut zones = Dataset::new("Zones", GeometryType::Polygon, vec![Field::new("zone_only", FieldType::Text)]);
    zones.push(Some(Geometry::Polygon(vec![vec![ring]])), vec![Value::Text("z".into())]);
    let session = &mut h.state_mut().session;
    session.map_mut().add_dataset(zones);
    let target = session.map_mut().layers.iter_mut().find(|l| l.name == "Target").unwrap();
    for (x, name) in [(1.0, "a"), (2.0, "b"), (9.0, "c")] {
        target.dataset.push(Some(Geometry::Point(Coord::new(x, 1.0))), vec![Value::Text(name.into())]);
    }
    h.run_steps(3);
    assert_eq!(h.query_all_by_label("Separator").count(), 0, "no CONCATENATE yet");
    h.get_by_label("Add statistic").click();
    h.run_steps(2);
    h.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Field").click();
    h.run_steps(2);
    assert!(h.query_by_label("zone_only").is_none(), "the polygons' fields aren't offered");
    h.get_by_label("target_only").click();
    h.run_steps(2);
    h.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Statistic").click();
    h.run_steps(2);
    assert!(h.query_by_role_and_label(egui::accesskit::Role::Button, "SUM").is_none(), "a text field takes no SUM");
    h.get_by_role_and_label(egui::accesskit::Role::Button, "CONCATENATE").click_accesskit();
    h.run_steps(3);
    assert!(h.query_all_by_label("Separator").count() > 0);
    click_run(&mut h);
    assert!(h.state().gp.last.as_ref().unwrap().0);
    let history = h.state().session.project.gp_history.last().unwrap();
    assert_eq!(history.params["statistics"], json!([{"field":"target_only","statistic":"CONCATENATE"}]));
    let out = &h.state().session.map().layer_by_name("Zones_Summarize").unwrap().dataset;
    assert_eq!(out.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["zone_only", "point_count", "CONCATENATE_target_only"]);
    assert_eq!(out.value(0, 1), &Value::Int(2));
    assert_eq!(out.value(0, 2), &Value::Text("a, b".into()));
}
