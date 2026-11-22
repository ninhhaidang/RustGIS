//! A repair dialog must not replace another archive after its captured index changes.
use rustgis_engine::{
    Project, Session,
    map::{TileKind, TileLayer},
};
use rustgis_ui_egui::RustGisApp;
use serde_json::json;

fn harness() -> egui_kittest::Harness<'static, RustGisApp> {
    let mut session = Session::new(Project::default());
    for file in ["first", "second"] {
        let mut tile = TileLayer::new("Archive", TileKind::PmTiles, format!("/tmp/rustgis-repair-{file}.pmtiles"));
        tile.error = Some("The archive moved".into());
        session.map_mut().tiles.push(tile);
    }
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1000.0, 720.0)).with_max_steps(64).build_ui_state(
        |ui, app: &mut RustGisApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        RustGisApp::new(session),
    );
    h.state_mut().run("ui.dialog", json!({"name":"repairTiles", "index":0})).unwrap();
    h.run_steps(3);
    assert_eq!(h.state().dialogs.open, Some("repairSource"));
    h
}

#[test]
fn tile_repair_closes_when_removal_or_reordering_changes_its_target() {
    for remove in [false, true] {
        let mut h = harness();
        h.state_mut().run("map.setTiles", json!({"index":0,"opacity":0.5})).unwrap();
        h.run_steps(3);
        assert_eq!(h.state().dialogs.open, Some("repairSource"), "display edits keep the same target");
        if remove {
            h.state_mut().run("map.removeTiles", json!({"index":0})).unwrap();
        } else {
            h.state_mut().run("map.setTiles", json!({"index":0,"move":"bottom"})).unwrap();
        }
        let remaining = h.state().session.map().tiles.clone();
        assert!(remaining[0].url.ends_with("second.pmtiles"));
        h.run_steps(3);
        assert!(h.state().dialogs.open.is_none(), "the dialog must not retarget to the second archive (remove={remove})");
        assert_eq!(h.state().session.map().tiles, remaining);
    }
}
