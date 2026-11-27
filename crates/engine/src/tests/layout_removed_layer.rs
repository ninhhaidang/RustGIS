//! Deleting a layer says how many layout elements name it: a chart's, table's or colorbar's
//! layer, and a text's layer statistics (#3020). So for map series and presentation titles
//! (#3075). All keep the name.

use super::*;
use rustgis_map::layout::ElementKind;

/// The notices `command` gives about layout elements that lost a layer.
fn lost(s: &mut Session, command: &str, params: Value) -> Vec<String> {
    let mark = s.notice_mark();
    s.run(command, &params).unwrap();
    s.notices_since(mark).iter().map(|n| n.text.clone()).filter(|t| t.contains("layout element(s)")).collect()
}

fn add(s: &mut Session, params: Value) {
    s.run("layout.addElement", &params).unwrap();
}

fn text(s: &mut Session, text: &str) {
    add(s, json!({"kind": "text", "text": text, "x": 10, "y": 10}));
}

/// The sample world with a layout holding one map frame of it.
fn with_layout() -> Session {
    let mut s = world();
    s.run("layout.new", &json!({"page": "a4"})).unwrap();
    s
}

fn kinds(s: &Session) -> Vec<ElementKind> {
    s.project.layouts[0].elements.iter().map(|e| e.kind.clone()).collect()
}

fn said(count: usize, names: &str) -> String {
    let layers = if names.contains(", ") { "layers" } else { "layer" };
    format!("{count} layout element(s) name the removed {layers} {names}: they are drawn without it until a layer of that name comes back")
}

#[test]
fn removing_a_layer_says_how_many_layout_elements_name_it() {
    let mut s = with_layout();
    add(&mut s, json!({"kind": "table", "layer": "Countries"}));
    add(&mut s, json!({"kind": "chart", "layer": "Countries", "field": "pop_est", "label": "name"}));
    // Two statistics of the layer in one text count it once; the layer is found ignoring case.
    text(&mut s, "{count:countries} countries, {sum:Countries:pop_est} people");
    let before = kinds(&s);
    assert_eq!(lost(&mut s, "layer.remove", json!({"layer": "Countries"})), [said(3, "'Countries'")]);
    assert_eq!(kinds(&s), before, "the elements keep the name");
    assert_eq!(lost(&mut s, "edit.undo", Value::Null), Vec::<String>::new());
    assert!(s.map().layer_by_name("Countries").is_some());
    assert_eq!(kinds(&s), before);
}

#[test]
fn layers_removed_together_are_said_once() {
    let mut s = with_layout();
    s.run("layer.addDataBytes", &json!({"fileName": "dem.tif", "data": dem_b64()})).unwrap();
    let dem = s.map().layers.iter().find(|l| l.is_raster()).unwrap().name.clone();
    add(&mut s, json!({"kind": "colorbar", "layer": dem}));
    add(&mut s, json!({"kind": "table", "layer": "Countries"}));
    // One element naming both counts once.
    text(&mut s, "{count:Countries} and {count:Cities}");
    let said_now = lost(&mut s, "layer.remove", json!({"layer": ["Countries", "Cities", dem]}));
    assert_eq!(said_now.len(), 1, "{said_now:?}");
    assert!(said_now[0].starts_with("3 layout element(s) name the removed layers "), "{said_now:?}");
    for name in ["'Countries'", "'Cities'", &format!("'{dem}'")] {
        assert!(said_now[0].contains(name), "{said_now:?}");
    }
}

#[test]
fn removing_a_layer_no_element_names_says_nothing() {
    let mut s = with_layout();
    add(&mut s, json!({"kind": "table", "layer": "Countries"}));
    add(&mut s, json!({"kind": "colorbar"}));
    // A count's layer is all that follows its colon, so `Cities:pop` is another layer's name.
    text(&mut s, "Cities {count:Cities:pop} {count:Countries} {scale}");
    assert_eq!(lost(&mut s, "layer.remove", json!({"layer": "Cities"})), Vec::<String>::new());
}

#[test]
fn a_name_another_layer_still_bears_says_nothing_until_the_last_goes() {
    let mut s = with_layout();
    add(&mut s, json!({"kind": "table", "layer": "Countries"}));
    s.run("layer.rename", &json!({"layer": "Lakes", "name": "Countries"})).unwrap();
    let first = s.map().layers.iter().find(|l| l.name == "Countries").unwrap().id;
    assert_eq!(lost(&mut s, "layer.remove", json!({"layer": first})), Vec::<String>::new());
    assert_eq!(lost(&mut s, "layer.remove", json!({"layer": "Countries"})), [said(1, "'Countries'")]);
}

#[test]
fn two_layers_of_one_name_removed_together_name_it_once() {
    let mut s = with_layout();
    add(&mut s, json!({"kind": "table", "layer": "Countries"}));
    s.run("layer.rename", &json!({"layer": "Lakes", "name": "Countries"})).unwrap();
    let both: Vec<u32> = s.map().layers.iter().filter(|l| l.name == "Countries").map(|l| l.id).collect();
    assert_eq!(lost(&mut s, "layer.remove", json!({"layer": both})), [said(1, "'Countries'")]);
}

#[test]
fn elements_that_read_another_map_are_not_counted() {
    let mut s = with_layout();
    add(&mut s, json!({"kind": "table", "layer": "Countries"}));
    text(&mut s, "{count:Countries}");
    let home = s.map().name.clone();
    s.run("map.new", &json!({"name": "Empty"})).unwrap();
    s.active_layout = Some(0);
    let frame = s.project.layouts[0].first_frame().unwrap();
    s.run("layout.update", &json!({"element": frame, "map": "Empty"})).unwrap();
    s.run("map.activate", &json!({"name": home})).unwrap();
    assert_eq!(lost(&mut s, "layer.remove", json!({"layer": "Countries"})), Vec::<String>::new());
}

#[test]
fn removing_a_group_says_it_too() {
    let mut s = with_layout();
    add(&mut s, json!({"kind": "table", "layer": "Cities"}));
    s.run("group.add", &json!({"name": "Places", "layers": ["Cities"]})).unwrap();
    assert_eq!(lost(&mut s, "group.remove", json!({"group": "Places"})), [said(1, "'Cities'")]);
}

/// The notices `command` gives about map series and presentation titles that lost a layer.
fn lost_pages(s: &mut Session, command: &str, params: Value) -> Vec<String> {
    let mark = s.notice_mark();
    s.run(command, &params).unwrap();
    s.notices_since(mark).iter().map(|n| n.text.clone()).filter(|t| t.contains("map series or presentation title(s)")).collect()
}

fn said_pages(count: usize, names: &str) -> String {
    let layers = if names.contains(", ") { "layers" } else { "layer" };
    format!(
        "{count} map series or presentation title(s) name the removed {layers} {names}: a series exports only its current page, and a title shows the figure as written, until a layer of that name comes back"
    )
}

fn series(s: &mut Session, layer: &str) {
    s.run("layout.series", &json!({"layer": layer, "nameField": "name"})).unwrap();
}

fn titled(s: &mut Session, title: &str) {
    if s.project.presentations.is_empty() {
        s.run("presentation.new", &json!({"name": "Talk"})).unwrap();
    }
    s.run("presentation.mapPage", &json!({"title": title})).unwrap();
}

#[test]
fn removing_a_series_index_layer_says_it() {
    let mut s = with_layout();
    series(&mut s, "Countries");
    assert_eq!(lost_pages(&mut s, "layer.remove", json!({"layer": "Countries"})), [said_pages(1, "'Countries'")]);
    assert_eq!(s.project.layouts[0].series.as_ref().map(|r| r.layer.as_str()), Some("Countries"), "the series keeps the name");
    assert_eq!(lost_pages(&mut s, "edit.undo", Value::Null), Vec::<String>::new());
}

#[test]
fn removing_a_layer_a_presentation_title_counts_says_it() {
    let mut s = world();
    // Two statistics of the layer in one title count it once; each page counts.
    titled(&mut s, "{count:countries} countries, {sum:Countries:pop_est} people");
    titled(&mut s, "{count:Countries}");
    let pages = s.project.presentations[0].pages.len();
    assert_eq!(lost_pages(&mut s, "layer.remove", json!({"layer": "Countries"})), [said_pages(2, "'Countries'")]);
    assert_eq!(s.project.presentations[0].pages.len(), pages);
    assert_eq!(s.project.presentations[0].pages.last().map(|p| p.title.as_str()), Some("{count:Countries}"));
}

#[test]
fn a_series_and_titles_of_several_layers_are_said_once() {
    let mut s = with_layout();
    series(&mut s, "Countries");
    titled(&mut s, "{count:Countries} and {count:Cities}");
    s.run("group.add", &json!({"name": "Places", "layers": ["Countries", "Cities"]})).unwrap();
    let said_now = lost_pages(&mut s, "group.remove", json!({"group": "Places"}));
    assert_eq!(said_now.len(), 1, "{said_now:?}");
    assert!(said_now[0].starts_with("2 map series or presentation title(s) name the removed layers "), "{said_now:?}");
    for name in ["'Countries'", "'Cities'"] {
        assert_eq!(said_now[0].matches(name).count(), 1, "{said_now:?}");
    }
}

#[test]
fn a_series_or_title_no_removed_layer_is_in_says_nothing() {
    let mut s = with_layout();
    series(&mut s, "Countries");
    titled(&mut s, "{count:Countries} in {map}");
    assert_eq!(lost_pages(&mut s, "layer.remove", json!({"layer": "Cities"})), Vec::<String>::new());
    // Another layer of the name stays: nothing is lost until the last goes.
    s.run("layer.rename", &json!({"layer": "Lakes", "name": "Countries"})).unwrap();
    let first = s.map().layers.iter().find(|l| l.name == "Countries").unwrap().id;
    assert_eq!(lost_pages(&mut s, "layer.remove", json!({"layer": first})), Vec::<String>::new());
}

#[test]
fn a_series_and_titles_of_another_map_are_not_counted() {
    let mut s = with_layout();
    series(&mut s, "Countries");
    titled(&mut s, "{count:Countries}");
    s.run("map.new", &json!({"name": "Other"})).unwrap();
    s.run("layer.addSample", &json!({"name": "countries"})).unwrap();
    assert_eq!(lost_pages(&mut s, "layer.remove", json!({"layer": "Countries"})), Vec::<String>::new());
}

/// What exporting the first layout to a file named `file` warns about its map series' index layer.
fn export_warnings(s: &mut Session, file: &str, all: bool) -> Vec<String> {
    let dir = std::env::temp_dir().join(format!("rustgis-3075-{}-{:?}", std::process::id(), std::thread::current().id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(file);
    let mark = s.notice_mark();
    let run = s.run("layout.export", &json!({"path": path, "dpi": 36, "allPages": all}));
    std::fs::remove_dir_all(&dir).unwrap();
    run.unwrap();
    s.notices_since(mark).iter().map(|n| n.text.clone()).filter(|t| t.contains("index layer")).collect()
}

const ONE_PAGE: &str = "The map series' index layer 'Countries' isn't in its frame's map: the PDF holds the current page only";

#[test]
fn exporting_a_series_without_its_index_layer_says_the_pdf_holds_one_page() {
    let mut s = with_layout();
    series(&mut s, "Countries");
    s.run("layer.remove", &json!({"layer": "Countries"})).unwrap();
    assert_eq!(export_warnings(&mut s, "series.pdf", true), [ONE_PAGE]);
}

#[test]
fn an_export_that_never_wrote_the_series_says_nothing_of_its_index_layer() {
    let mut s = with_layout();
    // No series.
    assert_eq!(export_warnings(&mut s, "plain.pdf", true), Vec::<String>::new());
    series(&mut s, "Countries");
    s.run("layer.remove", &json!({"layer": "Countries"})).unwrap();
    // One page asked for, or a format that only ever writes one.
    assert_eq!(export_warnings(&mut s, "page.pdf", false), Vec::<String>::new());
    assert_eq!(export_warnings(&mut s, "page.png", true), Vec::<String>::new());
    assert_eq!(export_warnings(&mut s, "page.svg", true), Vec::<String>::new());
    // The layer back, the series is whole again.
    s.run("edit.undo", &Value::Null).unwrap();
    assert!(s.map().layer_by_name("Countries").is_some());
    assert_eq!(export_warnings(&mut s, "series.pdf", true), Vec::<String>::new());
}
