use super::*;
use rustgis_analysis::RasterInput;
use rustgis_data::raster::{ColorHint, GeoTransform, Raster};
use rustgis_data::{Feature, Field, FieldType, Geometry};

/// A 40 × 40 DEM rising to the east (UTM 17N, 30 m pixels) and a point inside it.
struct Fixture {
    dem: Raster,
    /// The DEM as a scan: its pixels, with no coordinates.
    scan: Raster,
    points: Dataset,
    /// The DEM as an embedded GeoTIFF, as a browser upload or a tool's output is kept.
    tiff: Vec<u8>,
}

impl Fixture {
    fn new() -> Fixture {
        let gt = GeoTransform { origin_x: 500_000.0, origin_y: 4_650_000.0, pixel_width: 30.0, pixel_height: -30.0 };
        let band: Vec<f32> = (0..40 * 40).map(|i| (i % 40) as f32 * 3.0 + (i / 40) as f32).collect();
        let dem = Raster::new("dem", 40, 40, vec![band], gt, Crs::from_epsg(32617).unwrap(), None, ColorHint::Gray, "Float32").unwrap();
        // The DEM's centre, in longitude/latitude.
        let centre = Crs::from_epsg(32617).unwrap().transformer().unwrap().to_lonlat(Coord::new(500_600.0, 4_649_400.0)).unwrap();
        let mut points = Dataset::new("pts", rustgis_data::GeometryType::Point, vec![Field::new("id", FieldType::Integer)]);
        points.features.push(Feature { id: 1, geometry: Some(Geometry::Point(centre)), attrs: vec![rustgis_data::Value::Int(1)] });
        let tiff = rustgis_data::raster::geotiff::write(&dem, 0).unwrap();
        let scan = dem.clone().into_scan().unwrap();
        Fixture { dem, scan, points, tiff }
    }
}

impl Inputs for Fixture {
    fn dataset(&self, layer: &str) -> Option<&Dataset> {
        (layer == "pts").then_some(&self.points)
    }
    fn selection(&self, _: &str) -> &[usize] {
        &[]
    }
    fn raster(&self, layer: &str) -> Option<RasterInput<'_>> {
        match layer {
            "dem" => Some(RasterInput { raster: &self.dem, path: None, embedded: None }),
            "scan" => Some(RasterInput { raster: &self.scan, path: None, embedded: None }),
            "embedded" => Some(RasterInput { raster: &self.dem, path: None, embedded: Some(("dem.tif", &self.tiff)) }),
            _ => None,
        }
    }
}

/// The fixture, plus layers of these names that hold nothing a tool reads.
struct TakenNames<'a>(&'a Fixture, &'a [&'a str]);

impl Inputs for TakenNames<'_> {
    fn dataset(&self, layer: &str) -> Option<&Dataset> {
        self.0.dataset(layer).or_else(|| self.1.contains(&layer).then_some(&self.0.points))
    }
    fn selection(&self, _: &str) -> &[usize] {
        &[]
    }
    fn raster(&self, layer: &str) -> Option<RasterInput<'_>> {
        self.0.raster(layer)
    }
}

/// The fixture, or `None` (the test is skipped) when no runner is installed: set
/// `RUSTGIS_GEOLIBRE` to a `geolibre` executable, or put one on PATH, to run these.
fn setup() -> Option<Fixture> {
    register();
    if runner::find().is_none() {
        eprintln!("skipped: no GeoLibre runner (set {})", runner::ENV);
        return None;
    }
    Some(Fixture::new())
}

#[test]
fn runner_values_and_choices() {
    assert_eq!(arg_text(&json!("a b")), "a b");
    assert_eq!(arg_text(&json!(true)), "true");
    assert_eq!(arg_text(&json!(2.5)), "2.5");
    // Not a runner: refused, and the choice stays as it was.
    let before = runner::find();
    let me = std::env::current_exe().unwrap();
    assert!(runner::version_of(&me).is_none());
    assert!(set_runner(Some(me)).is_err());
    assert!(set_runner(Some(std::path::PathBuf::from("/no/such/geolibre"))).is_err());
    assert_eq!(runner::find(), before);
    assert_eq!(label_of("z_factor"), "Z factor");
    // Reloading the tools reuses the strings it made rather than leaking new copies.
    assert!(std::ptr::eq(leak("Slope"), leak(String::from("Slope"))));
    // A point that can't be converted drops the geometry instead of writing nulls.
    let line = Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(1.0, 1.0)]]);
    assert!(convert(&line, |c| Some(Coord::new(c.x * 2.0, c.y))).is_some());
    assert!(convert(&line, |c| (c.x < 0.5).then_some(c)).is_none());
    assert!(convert(&line, |_| Some(Coord::new(f64::NAN, 0.0))).is_none());
    assert_eq!(safe("a b/c.tif"), "a_b_c_tif");
    // Only the runner Install put in place can be uninstalled, not one of the user's own.
    if let Some(dir) = install::install_dir() {
        assert!(install::is_installed(&dir.join(runner::exe_name())));
        assert!(!install::is_installed(&dir.join("mine").join(runner::exe_name())));
        assert!(!install::is_installed(&dir.join("other-program")));
    }
}

#[test]
fn manifests_become_tool_forms() {
    let Some(_) = setup() else { return };
    let tools = rustgis_analysis::external_tools_of(crate::SOURCE);
    assert!(tools.len() > 1000, "{}", tools.len());
    let slope = rustgis_analysis::tool("geolibre:slope").unwrap();
    assert!(rustgis_analysis::is_external(slope));
    // Grouped by the runner's taxonomy; a runner from before it, by the coarse category.
    assert!(matches!(slope.toolbox, "GeoLibre › Terrain › Derivatives" | "GeoLibre › Raster"), "{}", slope.toolbox);
    assert_eq!(slope.params.first().map(|p| p.kind), Some(ParamKind::RasterLayer));
    // Outputs have no field of their own: one optional field names the new layer (#2224).
    let outputs: Vec<&ParamSpec> = slope.params.iter().filter(|p| p.name == OUTPUT).collect();
    assert!(matches!(outputs[..], [p] if is_output_name(p) && !p.required && p.default.is_empty()), "{outputs:?}");
    let units = slope.params.iter().find(|p| p.name == "units").unwrap();
    assert!(matches!(units.kind, ParamKind::Choice(c) if c.contains(&"percent")));
    // Defaults are JSON text (as for the built-in tools): the form and the runner parse it.
    assert_eq!(units.default, "\"degrees\"");
    assert_eq!(serde_json::from_str::<Value>(units.default).unwrap(), json!("degrees"));
    // The built-in Buffer still wins by name.
    assert_eq!(rustgis_analysis::tool("Buffer").map(|t| t.id), Some("buffer"));
    // GeoLibre's choices aren't enforced by RustGIS (#406 covers the built-in tools): some take
    // a comma-separated list of options, which its runner checks.
    let glcm = tools.iter().find(|t| t.id == "geolibre:glcm_texture").unwrap();
    let list = json!({"input": "dem", "features": "contrast,energy"});
    assert_eq!(rustgis_analysis::resolve_params(glcm, &list).unwrap()["features"], "contrast,energy");
}

#[test]
fn inputs_of_several_layers_are_layer_lists() {
    // A composite takes several rasters: the form picks layers, not a bare text box (#270).
    let manifest = json!({"id": "composite", "params": [
        {"name": "inputs", "io_role": "input", "data_kind": "raster", "schema": {"cardinality": "many"}},
        {"name": "vectors", "io_role": "input", "data_kind": "vector", "schema": {"cardinality": "many"}},
        {"name": "files", "io_role": "input", "data_kind": "lidar", "schema": {"cardinality": "many"}},
        {"name": "dem", "io_role": "input", "data_kind": "raster"},
        {"name": "output", "io_role": "output", "data_kind": "raster"}
    ]});
    let (spec, _) = spec_of(&manifest).unwrap();
    let kinds: Vec<(&str, ParamKind)> = spec.params.iter().map(|p| (p.name, p.kind)).collect();
    assert_eq!(
        kinds,
        [
            ("inputs", ParamKind::RasterLayers),
            ("vectors", ParamKind::VectorLayers),
            ("files", ParamKind::Text),
            ("dem", ParamKind::RasterLayer),
            ("output", ParamKind::Text)
        ]
    );
}

/// The layers a run adds are named by `output`, as for the built-in tools, else by the tool;
/// a name in use gets a number (#2224).
#[test]
fn outputs_are_named_by_the_output_field() {
    let f = Fixture::new();
    let one = json!({
        "id": "contours", "display_name": "Contours From Raster", "category": "Raster", "summary": "",
        "params": [
            {"name": "input", "io_role": "input", "data_kind": "raster", "required": true, "schema": {"kind": "input"}},
            {"name": "output", "io_role": "output", "data_kind": "vector", "required": false, "schema": {"kind": "output"}}
        ]
    });
    let (spec, meta) = spec_of(&one).unwrap();
    let field = spec.params.iter().find(|p| p.name == OUTPUT).unwrap();
    assert!(is_output_name(field) && field.label == "Output Name", "{field:?}");
    let job = |params: Value| {
        let params = rustgis_analysis::resolve_params(&spec, &params)?;
        prepare_with(&spec, &meta, &f, &params)
    };
    let named = |params: Value| job(params).map(|j| j.outputs.iter().map(|(_, n)| n.clone()).collect::<Vec<_>>());
    // Empty (the form's start), blank or left out: the tool's name, as before.
    for p in [json!({"input": "dem"}), json!({"input": "dem", "output": ""}), json!({"input": "dem", "output": "  "})] {
        assert_eq!(named(p.clone()).unwrap(), ["Contours From Raster"], "{p}");
    }
    let given = job(json!({"input": "dem", "output": " Contours 10 m "})).unwrap();
    assert_eq!(given.outputs, [("output.geojson".to_string(), "Contours 10 m".to_string())]);
    // The file the runner writes doesn't change: the name is only the layer's.
    assert_eq!(given.argv("/work"), ["--input=/work/in0-dem.tif", "--output=/work/output.geojson"]);
    assert!(given.notes.is_empty(), "{:?}", given.notes);
    // A layer has the name already: numbered, and said.
    let taken = job(json!({"input": "dem", "output": "pts"})).unwrap();
    assert_eq!(taken.outputs[0].1, "pts 2");
    assert!(taken.notes.iter().any(|n| n.contains("1 output name(s)") && n.contains("'pts' is now 'pts 2'")), "{:?}", taken.notes);
    // Too long: cut, and said.
    let long = job(json!({"input": "dem", "output": "x".repeat(300)})).unwrap();
    assert_eq!(long.outputs[0].1.chars().count(), MAX_OUTPUT_NAME);
    assert!(long.notes.iter().any(|n| n.contains("cut from 300 to 200")), "{:?}", long.notes);
    // Not text: an error, not a guess.
    let e = job(json!({"input": "dem", "output": 5})).unwrap_err();
    assert!(e.contains("must be text"), "{e}");

    // Several outputs: each adds its label to the name, and two can't take one name.
    let two = json!({
        "id": "flow", "display_name": "Flow", "category": "Hydrology", "summary": "",
        "params": [
            {"name": "dem", "io_role": "input", "data_kind": "raster", "required": true},
            {"name": "streams", "io_role": "output", "data_kind": "raster"},
            {"name": "basins", "io_role": "output", "data_kind": "raster"}
        ]
    });
    let (spec2, meta2) = spec_of(&two).unwrap();
    let names = |params: Value| {
        let params = rustgis_analysis::resolve_params(&spec2, &params).unwrap();
        prepare_with(&spec2, &meta2, &f, &params).unwrap().outputs.into_iter().map(|(_, n)| n).collect::<Vec<_>>()
    };
    assert_eq!(names(json!({"dem": "dem"})), ["Flow streams", "Flow basins"]);
    assert_eq!(names(json!({"dem": "dem", "output": "Rio Doce"})), ["Rio Doce streams", "Rio Doce basins"]);
    // A long name leaves room for each output's own: every name within the limit, still apart (review).
    let params = rustgis_analysis::resolve_params(&spec2, &json!({"dem": "dem", "output": "x".repeat(200)})).unwrap();
    let long = prepare_with(&spec2, &meta2, &f, &params).unwrap();
    let long_names: Vec<&str> = long.outputs.iter().map(|(_, n)| n.as_str()).collect();
    assert!(long_names.iter().all(|n| n.chars().count() == MAX_OUTPUT_NAME), "{long_names:?}");
    assert!(long_names[0].ends_with("x streams") && long_names[1].ends_with("x basins"), "{long_names:?}");
    assert_eq!(long.notes.iter().filter(|n| n.contains("cut to fit")).count(), 1, "{:?}", long.notes);
    // Both names in use: one notice with the count, not one per output (review).
    let fg = TakenNames(&f, &["Taken streams", "Taken basins"]);
    let params = rustgis_analysis::resolve_params(&spec2, &json!({"dem": "dem", "output": "Taken"})).unwrap();
    let both = prepare_with(&spec2, &meta2, &fg, &params).unwrap();
    assert_eq!(both.outputs.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>(), ["Taken streams 2", "Taken basins 2"]);
    let numbered_notes: Vec<&String> = both.notes.iter().filter(|n| n.contains("numbered")).collect();
    assert!(matches!(numbered_notes[..], [n] if n.contains("2 output name(s)")), "{:?}", both.notes);
    // A long name whose output name is taken: the number goes after the output's name, which is
    // kept whole (review).
    let long_streams = format!("{} streams", "x".repeat(192));
    let fl = TakenNames(&f, &[long_streams.as_str()]);
    let params = rustgis_analysis::resolve_params(&spec2, &json!({"dem": "dem", "output": "x".repeat(200)})).unwrap();
    let near = prepare_with(&spec2, &meta2, &fl, &params).unwrap();
    let first = &near.outputs[0].1;
    assert!(first.ends_with("x streams 2") && first.chars().count() == MAX_OUTPUT_NAME, "{first}");
    assert_eq!(numbered("a", "", |n| ["a", "a 2"].contains(&n)), "a 3");
    assert_eq!(numbered(&"y".repeat(200), "", |n| n.chars().all(|c| c == 'y')).chars().count(), MAX_OUTPUT_NAME);

    // A tool whose own option is called `output` keeps it; its outputs keep the tool's name.
    let own = json!({
        "id": "own", "display_name": "Own", "category": "Raster", "summary": "",
        "params": [
            {"name": "output", "data_kind": "string", "required": false, "schema": {"kind": "string"}},
            {"name": "result", "io_role": "output", "data_kind": "raster"}
        ]
    });
    let (spec3, meta3) = spec_of(&own).unwrap();
    assert!(!meta3.names_outputs && spec3.params.iter().filter(|p| p.name == OUTPUT).all(|p| !is_output_name(p)));
    let params = rustgis_analysis::resolve_params(&spec3, &json!({"output": "fast"})).unwrap();
    let j = prepare_with(&spec3, &meta3, &f, &params).unwrap();
    assert_eq!(j.outputs[0].1, "Own");
    assert!(j.argv("/work").contains(&"--output=fast".to_string()), "{:?}", j.argv("/work"));
}

#[test]
fn composite_bands_legacy_manifest_selects_and_stages_rasters_in_order() {
    // The input/output subset of `geolibre manifests` in 1.8.0 and 1.8.1 (#1684).
    // Unlike the generic list fixture above, the real input has no role or cardinality.
    let manifest = json!({"id": "composite_bands", "params": [
        {"name": "inputs", "data_kind": "string", "required": true, "schema": {"kind": "string"}},
        {"name": "output", "io_role": "output", "data_kind": "raster", "schema": {"kind": "output"}}
    ]});
    let (spec, meta) = spec_of(&manifest).unwrap();
    assert_eq!(spec.params[0].kind, ParamKind::RasterLayers);
    let mut f = Fixture::new();
    f.dem.bands[0].data.fill(14.3);
    // An in-memory layer and an embedded file must both be staged, in the selected order.
    for (names, values) in [
        (json!("dem;embedded"), [14.3, 0.0]),
        (json!("embedded;dem"), [0.0, 14.3]),
        (json!(["dem", "embedded"]), [14.3, 0.0]),
        (json!(["embedded", "dem"]), [0.0, 14.3]),
    ] {
        let params = rustgis_analysis::resolve_params(&spec, &json!({"inputs": names})).unwrap();
        let job = prepare_with(&spec, &meta, &f, &params).unwrap();
        assert_eq!(job.crs, f.dem.crs);
        assert_eq!(job.files.len(), 2);
        for ((name, bytes), value) in job.files.iter().zip(values) {
            let raster = rustgis_data::raster::read_bytes(name, bytes).unwrap();
            assert_eq!(raster.bands[0].data[0], value);
        }
        let staged = job.files.iter().map(|(name, _)| format!("/work/{name}")).collect::<Vec<_>>().join(";");
        assert_eq!(job.argv("/work"), [format!("--inputs={staged}"), "--output=/work/output.tif".into()]);
    }
    // Refuse malformed arrays rather than silently dropping an entry or the entire input.
    for invalid in [json!(["dem", 7]), json!([])] {
        let error = prepare_with(&spec, &meta, &f, &json!({"inputs": invalid})).unwrap_err();
        assert!(error.contains("input layer"), "{error}");
    }
    // Other tools' string options must not become raster inputs just because of their name.
    let mut unrelated = manifest.clone();
    unrelated["id"] = json!("other_tool");
    assert_eq!(spec_of(&unrelated).unwrap().0.params[0].kind, ParamKind::Text);
    // A newer runner's explicit metadata keeps its usual meaning.
    let mut explicit = manifest;
    explicit["params"][0] = json!({"name":"inputs", "io_role":"input", "data_kind":"vector", "schema":{"cardinality":"many"}});
    assert_eq!(spec_of(&explicit).unwrap().0.params[0].kind, ParamKind::VectorLayers);
}

#[test]
fn composite_bands_runner_preserves_selected_band_order() {
    let Some(mut f) = setup() else { return };
    f.dem.bands[0].data.fill(14.3);
    for (names, values) in [
        (json!("dem;embedded"), [14.3, 0.0]),
        (json!("embedded;dem"), [0.0, 14.3]),
        (json!(["dem", "embedded"]), [14.3, 0.0]),
        (json!(["embedded", "dem"]), [0.0, 14.3]),
    ] {
        let result = rustgis_analysis::run("geolibre:composite_bands", &f, &json!({"inputs": names})).unwrap();
        let Output::Layers(layers) = result.output else { panic!("layers") };
        let Some(OutputLayer::Raster { bytes, file_name, .. }) = layers.first() else { panic!("a raster: {layers:?}") };
        let raster = rustgis_data::raster::read_bytes(file_name, bytes).unwrap();
        assert_eq!(raster.bands.len(), 2);
        assert_eq!(raster.crs, f.dem.crs);
        for (band, value) in raster.bands.iter().zip(values) {
            assert!((band.data[0] - value).abs() < 1e-5, "{} != {value}", band.data[0]);
        }
    }
}

/// Two rasters on the DEM's grid, `A` holding 1 and `B` holding 2 in every cell.
struct TwoLayers {
    a: Raster,
    b: Raster,
}

impl TwoLayers {
    fn new() -> TwoLayers {
        let f = Fixture::new();
        let (mut a, mut b) = (f.dem.clone(), f.dem);
        a.bands[0].data.fill(1.0);
        b.bands[0].data.fill(2.0);
        TwoLayers { a, b }
    }
}

impl Inputs for TwoLayers {
    fn dataset(&self, _: &str) -> Option<&Dataset> {
        None
    }
    fn selection(&self, _: &str) -> &[usize] {
        &[]
    }
    fn raster(&self, layer: &str) -> Option<RasterInput<'_>> {
        match layer {
            "A" => Some(RasterInput { raster: &self.a, path: None, embedded: None }),
            "B" => Some(RasterInput { raster: &self.b, path: None, embedded: None }),
            _ => None,
        }
    }
}

/// The value in the first cell of each staged input, in the order the runner gets them.
fn staged_values(job: &Job) -> Vec<f32> {
    job.files.iter().map(|(name, bytes)| rustgis_data::raster::read_bytes(name, bytes).unwrap().bands[0].data[0]).collect()
}

#[test]
fn raster_calculator_binds_quoted_names_to_their_layers() {
    // The input/output subset of `geolibre manifests` in 1.8.1, which calls the list single.
    let manifest = json!({"id": "raster_calculator", "display_name": "Raster Calculator", "category": "Raster", "params": [
        {"name": "expression", "data_kind": "string", "required": true, "schema": {"kind": "string"}},
        {"name": "inputs", "io_role": "input", "data_kind": "raster", "required": true, "description": "Ordered input raster paths.",
         "schema": {"cardinality": "single", "kind": "input"}},
        {"name": "output", "io_role": "output", "data_kind": "raster", "required": false, "schema": {"kind": "output"}}
    ]});
    let (spec, meta) = spec_of(&manifest).unwrap();
    // The form picks several layers, and says how they meet the expression.
    let field = spec.params.iter().find(|p| p.name == "inputs").unwrap();
    assert_eq!(field.kind, ParamKind::RasterLayers);
    assert!(field.help.contains(CALCULATOR_INPUTS_HELP), "{}", field.help);
    let f = TwoLayers::new();
    let job = |params: Value| {
        let params = rustgis_analysis::resolve_params(&spec, &params)?;
        prepare_with(&spec, &meta, &f, &params)
    };
    // `"B" + 0 * "A"` is B, in whichever order the layers are listed.
    for inputs in [json!("A;B"), json!("B;A"), json!(["A", "B"])] {
        let j = job(json!({"expression": "\"B\" + 0 * \"A\"", "inputs": inputs})).unwrap();
        assert_eq!(staged_values(&j), [2.0, 1.0], "{inputs}");
        assert_eq!(
            j.argv("/work"),
            ["--expression=\"B\" + 0 * \"A\"", "--inputs=/work/in0-B.tif;/work/in1-A.tif", "--output=/work/output.tif"]
        );
    }
    // Single quotes and a name quoted twice, as the runner reads them: by first appearance.
    let j = job(json!({"expression": "if('A' > 0, 'B', 'A')", "inputs": "B;A"})).unwrap();
    assert_eq!(staged_values(&j), [1.0, 2.0]);
    // A name that isn't an input, and an input the expression doesn't use: refused, not guessed.
    let e = job(json!({"expression": "\"B\" + \"C\"", "inputs": "A;B"})).unwrap_err();
    assert!(e.starts_with("Raster Calculator: ") && e.contains("\"C\"") && e.contains("A; B"), "{e}");
    let e = job(json!({"expression": "\"B\" * 2", "inputs": "A;B"})).unwrap_err();
    assert!(e.contains("doesn't quote the input(s) A"), "{e}");
    // Layers are found by name whatever the ASCII case, so the expression may quote them so too.
    let j = job(json!({"expression": "\"b\" + 0 * \"a\"", "inputs": "A;B"})).unwrap();
    assert_eq!(staged_values(&j), [2.0, 1.0]);
    assert!(j.notes.is_empty(), "{:?}", j.notes);
    let j = job(json!({"expression": "'a' * 2", "inputs": "A"})).unwrap();
    assert_eq!(staged_values(&j), [1.0]);
    // Files only: matched by path, file name or name without extension (any case), else by
    // position (the runner's own contract), so typed paths keep working; that is said.
    let none = Fixture::new();
    let order = |expression: &str, list: &[&'static str]| calculator_order(expression, list.to_vec(), &none);
    let files = ["/d/red.tif", "/d/nir.tif"];
    assert_eq!(order("'nir' - 'red'", &files).unwrap(), (vec!["/d/nir.tif", "/d/red.tif"], None));
    assert_eq!(order("\"/d/nir.tif\" - \"red.tif\"", &files).unwrap(), (vec!["/d/nir.tif", "/d/red.tif"], None));
    assert_eq!(order("\"B04\" - \"SCL\"", &["/d/scl.tif", "/d/b04.tif"]).unwrap(), (vec!["/d/b04.tif", "/d/scl.tif"], None));
    let (kept, note) = order("'x' - 'y'", &files).unwrap();
    assert_eq!(kept, files);
    let note = note.unwrap();
    assert!(note.contains("\"x\" from /d/red.tif, \"y\" from /d/nir.tif"), "{note}");
    // By position, one file per name: a file left over, or a name without a file, is refused.
    let e = order("'x' - 'y'", &["/d/a.tif", "/d/b.tif", "/d/c.tif"]).unwrap_err();
    assert!(e.contains("2 name(s) but 3 file(s)"), "{e}");
    assert!(order("'x' - 'y' - 'z'", &files).unwrap_err().contains("3 name(s) but 2 file(s)"));
    // Every exact match before any in another case, whichever name comes first.
    assert_eq!(order("'NIR' - 'nir'", &["/d/nir.tif", "/d/Nir.tif"]).unwrap(), (vec!["/d/Nir.tif", "/d/nir.tif"], None));
    assert_eq!(order("'nIr' - 'nir'", &["/a/nir.tif", "/b/NIR.tif"]).unwrap(), (vec!["/b/NIR.tif", "/a/nir.tif"], None));
    // Every whole path before any file name, whichever name comes first.
    assert_eq!(order("'red' - '/a/red.tif'", &["/a/red.tif", "/b/red.tif"]).unwrap(), (vec!["/b/red.tif", "/a/red.tif"], None));
    // A layer's whole name before a file's stem, whichever is listed first.
    assert_eq!(order("'dem' + 0 * 'dem.tif'", &["/x/dem.tif", "dem"]).unwrap(), (vec!["dem", "/x/dem.tif"], None));
    // One name matched and one not: refused, never the matched one read from another file.
    let e = order("'nir' - 'x'", &files).unwrap_err();
    assert!(e.contains("\"x\"") && e.contains("/d/red.tif; /d/nir.tif"), "{e}");
    // A layer listed with a file it doesn't name: refused, as a layer's name is the rule then.
    assert!(order("'dem' - 'x'", &["/d/red.tif", "dem"]).is_err());
    assert_eq!(order("'dem' - 'red'", &["/d/red.tif", "dem"]).unwrap(), (vec!["dem", "/d/red.tif"], None));
    // An exact match comes before one of another case.
    assert_eq!(order("'nir' - 'NIR'", &["/d/NIR.tif", "/d/nir.tif"]).unwrap(), (vec!["/d/nir.tif", "/d/NIR.tif"], None));
    // Other tools' inputs keep the order they are listed in.
    let mut other = manifest;
    other["id"] = json!("other_calculator");
    let (spec2, meta2) = spec_of(&other).unwrap();
    assert_eq!(spec2.params.iter().find(|p| p.name == "inputs").unwrap().kind, ParamKind::RasterLayer);
    let params = rustgis_analysis::resolve_params(&spec2, &json!({"expression": "\"B\" + 0 * \"A\"", "inputs": "A;B"})).unwrap();
    assert_eq!(staged_values(&prepare_with(&spec2, &meta2, &f, &params).unwrap()), [1.0, 2.0]);
}

#[test]
fn raster_calculator_runner_reads_the_quoted_layer() {
    let Some(_) = setup() else { return };
    let f = TwoLayers::new();
    for inputs in ["A;B", "B;A"] {
        let params = json!({"expression": "\"B\" + 0 * \"A\"", "inputs": inputs});
        let result = rustgis_analysis::run("geolibre:raster_calculator", &f, &params).unwrap();
        let Output::Layers(layers) = result.output else { panic!("layers") };
        let Some(OutputLayer::Raster { bytes, file_name, .. }) = layers.first() else { panic!("a raster: {layers:?}") };
        let raster = rustgis_data::raster::read_bytes(file_name, bytes).unwrap();
        assert!((raster.bands[0].data[0] - 2.0).abs() < 1e-6, "{inputs}: {}", raster.bands[0].data[0]);
    }
}

#[test]
fn field_suggestions_keep_categorical_values_and_plural_lists() {
    let manifest = json!({"id": "field_forms", "params": [
        {"name": "input", "io_role": "input", "data_kind": "vector"},
        {"name": "value_field", "data_kind": "string"},
        {"name": "denominator_weight_field", "data_kind": "string"},
        {"name": "fields", "data_kind": "string"},
        {"name": "predictor_fields", "data_kind": "string"},
        {"name": "output_field", "data_kind": "string"},
        {"name": "field", "data_kind": "string", "schema": {"kind": "enum", "options": [{"value": "all"}]}},
        {"name": "number_field", "data_kind": "number"},
        {"name": "drop_fields", "data_kind": "bool"}
    ]});
    let (spec, _) = spec_of(&manifest).unwrap();
    let kind = |name| spec.params.iter().find(|p| p.name == name).unwrap().kind;
    assert_eq!(kind("value_field"), ParamKind::Field, "categorical values must remain selectable");
    assert_eq!(kind("denominator_weight_field"), ParamKind::NumericField);
    assert_eq!(kind("fields"), ParamKind::FieldNames);
    assert_eq!(kind("predictor_fields"), ParamKind::FieldNames);
    assert_eq!(kind("output_field"), ParamKind::Field, "the form must allow a new column name");
    assert_eq!(kind("field"), ParamKind::Choice(&["all"]), "explicit schema wins over naming");
    assert_eq!(kind("number_field"), ParamKind::Number);
    assert_eq!(kind("drop_fields"), ParamKind::Bool);
}

#[test]
fn slope_runs_on_a_raster_layer() {
    let Some(f) = setup() else { return };
    let r = rustgis_analysis::run("geolibre:slope", &f, &json!({"input": "dem", "units": "degrees"})).unwrap();
    // An embedded raster is staged as a file with its extension.
    assert!(rustgis_analysis::run("geolibre:slope", &f, &json!({"input": "embedded"})).is_ok());
    let Output::Layers(layers) = r.output else { panic!("layers") };
    let Some(OutputLayer::Raster { name, bytes, file_name }) = layers.first() else { panic!("a raster: {layers:?}") };
    assert_eq!(name, "Slope");
    let out = rustgis_data::raster::read_bytes(file_name, bytes).unwrap();
    assert_eq!((out.width, out.height), (40, 40));
    assert_eq!(out.crs, Crs::from_epsg(32617).unwrap(), "keeps the DEM's coordinate system");
    // A 3 m rise per 30 m pixel is about 5.7°.
    let mid = out.bands[0].data[20 * 40 + 20];
    assert!((mid - 5.8).abs() < 0.5, "{mid}");
}

#[test]
fn vector_tools_work_in_metres_and_return_lonlat() {
    let Some(f) = setup() else { return };
    let r = rustgis_analysis::run("geolibre:buffer_vector", &f, &json!({"input": "pts", "distance": "1000"})).unwrap();
    let Output::Layers(layers) = r.output else { panic!("layers") };
    let Some(OutputLayer::Vector(ds)) = layers.first() else { panic!("a vector layer: {layers:?}") };
    let ds = ds.in_lonlat().unwrap();
    let Some(Some(Geometry::Polygon(polys))) = ds.features.first().map(|f| f.geometry.clone()) else { panic!("a polygon") };
    let area = rustgis_geom::geodesic_ring_area_m2(&polys[0][0]).abs();
    let circle = std::f64::consts::PI * 1000.0 * 1000.0;
    assert!((area / circle - 1.0).abs() < 0.05, "a 1 km buffer, not 1000 degrees: {area}");
}

#[test]
fn rasters_and_points_meet_in_one_coordinate_system() {
    let Some(f) = setup() else { return };
    let r = rustgis_analysis::run("geolibre:extract_raster_values_at_points", &f, &json!({"rasters": "dem", "points": "pts"})).unwrap();
    let Output::Layers(layers) = r.output else { panic!("layers") };
    let Some(OutputLayer::Vector(ds)) = layers.first() else { panic!("points: {layers:?}") };
    let ds = ds.in_lonlat().unwrap();
    let p: Option<Coord> = ds.features.first().and_then(|f| f.geometry.as_ref()).map(|g| g.extent().center());
    let centre = f.points.extent().center();
    assert!(p.is_some_and(|c| (c.x - centre.x).abs() < 1e-6 && (c.y - centre.y).abs() < 1e-6), "back in lon/lat: {p:?}");
    // The sampled value is the DEM's at its centre (column 20, row 20).
    let value = ds.fields.iter().position(|fl| fl.name != "id" && fl.name != "FID").and_then(|i| ds.features[0].attrs.get(i).cloned());
    let v = value.and_then(|v| v.as_f64()).unwrap_or(f64::NAN);
    assert!((v - (20.0 * 3.0 + 20.0)).abs() < 3.5, "{v}");
}

#[test]
fn bad_inputs_are_errors_not_crashes() {
    let Some(f) = setup() else { return };
    assert!(rustgis_analysis::run("geolibre:slope", &f, &json!({})).unwrap_err().contains("required"));
    assert!(rustgis_analysis::run("geolibre:slope", &f, &json!({"input": "nope"})).unwrap_err().contains("no layer or file"));
    assert!(rustgis_analysis::run("geolibre:slope", &f, &json!({"input": "pts"})).is_err(), "a vector where a raster belongs");
    // Every tool with junk in every field fails cleanly: inputs that don't exist stop it before
    // it runs.
    for t in rustgis_analysis::external_tools_of(crate::SOURCE) {
        if !t.params.iter().any(|p| matches!(p.kind, ParamKind::RasterLayer | ParamKind::VectorLayer | ParamKind::Layer | ParamKind::Path))
        {
            continue;
        }
        let junk: serde_json::Map<String, Value> = t.params.iter().map(|p| (p.name.to_string(), json!("nope;(("))).collect();
        assert!(rustgis_analysis::run(t.id, &f, &Value::Object(junk)).is_err(), "{}", t.id);
    }
}

/// The browser's path, with no runner: prepare a run, then finish it from files a runner
/// would have written.
#[test]
fn prepare_and_finish_without_a_runner() {
    let f = Fixture::new();
    let manifest = json!({
        "id": "test_buffer", "display_name": "Test Buffer", "category": "Vector", "summary": "",
        "params": [
            {"name": "input", "io_role": "input", "data_kind": "vector", "required": true, "schema": {"kind": "input", "cardinality": "single"}},
            {"name": "others", "io_role": "input", "data_kind": "raster", "required": false, "schema": {"kind": "input", "cardinality": "multiple"}},
            {"name": "distance", "data_kind": "number", "required": true, "schema": {"kind": "scalar"}},
            {"name": "output", "io_role": "output", "data_kind": "vector", "required": false, "schema": {"kind": "output"}}
        ]
    });
    let (spec, meta) = spec_of(&manifest).unwrap();
    // No `taxonomy` (an older runner): the coarse category. With one: category › subcategory.
    assert_eq!(spec.toolbox, "GeoLibre › Vector");
    let placed = json!({"id": "slope", "taxonomy": {"category": "Terrain", "subcategory": "Derivatives"}});
    assert_eq!(toolbox_of(&placed, "Raster"), "GeoLibre › Terrain › Derivatives");
    assert_eq!(toolbox_of(&json!({"taxonomy": {"category": " ", "subcategory": 3}}), "Raster"), "GeoLibre › Raster");
    let params = rustgis_analysis::resolve_params(&spec, &json!({"input": "pts", "others": "dem;dem", "distance": "250"})).unwrap();
    let job = prepare_with(&spec, &meta, &f, &params).unwrap();
    // Inputs are staged as numbered files, even the same layer twice.
    let names: Vec<&str> = job.files.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["in0-pts.geojson", "in1-dem.tif", "in2-dem.tif"]);
    // The working CRS is the raster's (UTM 17N): the GeoJSON says so.
    assert!(String::from_utf8_lossy(&job.files[0].1).contains("EPSG::32617"));
    assert_eq!(
        job.argv("/work"),
        [
            "--input=/work/in0-pts.geojson",
            "--others=/work/in1-dem.tif;/work/in2-dem.tif",
            "--distance=250",
            "--output=/work/output.geojson"
        ]
    );
    // Whole numbers go as integers (the form sends numbers as floats).
    assert_eq!(option_value(&json!(3.0), Data::Number), json!(3));
    assert_eq!(option_value(&json!("2.5"), Data::Number), json!(2.5));
    assert!(job.argv("C:\\scratch").iter().any(|a| a == "--output=C:\\scratch\\output.geojson"), "Windows folders");
    // A scan has no coordinates to hand a tool: refused before anything is staged.
    let scan = rustgis_analysis::resolve_params(&spec, &json!({"input": "pts", "others": "scan", "distance": "250"})).unwrap();
    let e = prepare_with(&spec, &meta, &f, &scan).unwrap_err();
    assert!(e.contains("no coordinates"), "{e}");

    // Finish from what a runner returns: the output in UTM, and a JSON results line.
    let utm = Crs::from_epsg(32617).unwrap().transformer().unwrap();
    let c = utm.from_lonlat(f.points.extent().center()).unwrap();
    let out = format!(
        r#"{{"type":"FeatureCollection","features":[{{"type":"Feature","properties":{{"id":1}},"geometry":{{"type":"Point","coordinates":[{},{}]}}}}]}}"#,
        c.x, c.y
    );
    let lines = vec!["working…".to_string(), r#"{"output":"/work/output.geojson","log":"/work/my_output.geojson","count":1}"#.to_string()];
    let r = finish(&job, &[("output.geojson".into(), out.clone().into_bytes())], &lines, None);
    let Output::Layers(layers) = r.output else { panic!("layers") };
    let Some(OutputLayer::Vector(ds)) = layers.first() else { panic!("a vector: {layers:?}") };
    assert_eq!(ds.name, "Test Buffer");
    // Held in the working system, its numbers as the runner wrote them.
    assert_eq!(ds.coord_crs(), Crs::Epsg(32617));
    assert_eq!(ds.features[0].geometry, Some(Geometry::Point(c)));
    let p = ds.in_lonlat().unwrap().extent().center();
    let want = f.points.extent().center();
    assert!((p.x - want.x).abs() < 1e-6 && (p.y - want.y).abs() < 1e-6, "at the input's place: {p:?}");
    // An output that names its system (the legacy GeoJSON `crs` member) is held in it as read,
    // not converted a second time.
    let named = out.replacen(
        r#"{"type":"FeatureCollection","#,
        r#"{"type":"FeatureCollection","crs":{"type":"name","properties":{"name":"urn:ogc:def:crs:EPSG::32617"}},"#,
        1,
    );
    let again = finish(&job, &[("output.geojson".into(), named.into_bytes())], &[], None);
    let Output::Layers(layers) = again.output else { panic!("layers") };
    let Some(OutputLayer::Vector(ds)) = layers.first() else { panic!("a vector: {layers:?}") };
    assert_eq!(ds.coord_crs(), Crs::Epsg(32617));
    assert_eq!(ds.features[0].geometry, Some(Geometry::Point(c)));
    assert!(r.messages.contains(&"working…".to_string()));
    assert!(r.messages.iter().any(|m| m == "Count: 1"), "results, without the output path: {:?}", r.messages);
    assert!(!r.messages.iter().any(|m| m.contains("/work/output.geojson")), "the output came back as a layer");
    assert!(r.messages.iter().any(|m| m.contains("my_output.geojson")), "another file isn't mistaken for it");
    // No files back: a message, not a crash.
    let empty = finish(&job, &[], &[], None);
    assert!(matches!(empty.output, Output::Layers(ref l) if l.is_empty()) && !empty.messages.is_empty());
}

/// Options left at the manifest's defaults aren't passed, so the runner uses its own defaults as
/// when it runs alone (#237: an explicit `flat_increment` of 0.0001 stopped flow on flats).
#[test]
fn defaults_are_left_to_the_runner() {
    let f = Fixture::new();
    let manifest = json!({
        "id": "fill_depressions", "display_name": "Fill Depressions", "category": "Raster", "summary": "",
        "defaults": {"fix_flats": true, "flat_increment": 0.0001, "flat_resolution": "garbrecht_martz", "max_depth": null, "z": 2},
        "params": [
            {"name": "dem", "io_role": "input", "data_kind": "raster", "required": true, "schema": {"kind": "input"}},
            {"name": "fix_flats", "data_kind": "bool", "required": false, "schema": {"kind": "bool"}},
            {"name": "flat_increment", "data_kind": "number", "required": false, "schema": {"kind": "scalar"}},
            {"name": "flat_resolution", "data_kind": "string", "required": false,
             "schema": {"kind": "enum", "options": [{"value": "garbrecht_martz"}, {"value": "natural"}]}},
            {"name": "max_depth", "data_kind": "number", "required": false, "schema": {"kind": "scalar"}},
            {"name": "z", "data_kind": "number", "required": true, "schema": {"kind": "scalar"}},
            {"name": "output", "io_role": "output", "data_kind": "raster", "required": false, "schema": {"kind": "output"}}
        ]
    });
    let (spec, meta) = spec_of(&manifest).unwrap();
    // The form fills in the defaults (as text, as it sends them): none of the optional ones go.
    let params =
        rustgis_analysis::resolve_params(&spec, &json!({"dem": "embedded", "fix_flats": "true", "flat_increment": "0.0001"})).unwrap();
    let job = prepare_with(&spec, &meta, &f, &params).unwrap();
    assert_eq!(job.argv("/work"), ["--dem=/work/in0-embedded.tif", "--z=2", "--output=/work/output.tif"]);
    // Changed values go, a required one always does.
    let params = rustgis_analysis::resolve_params(
        &spec,
        &json!({"dem": "embedded", "fix_flats": false, "flat_increment": 0.001, "flat_resolution": "natural", "max_depth": 5}),
    )
    .unwrap();
    let job = prepare_with(&spec, &meta, &f, &params).unwrap();
    assert_eq!(
        job.argv("/work"),
        [
            "--dem=/work/in0-embedded.tif",
            "--fix_flats=false",
            "--flat_increment=0.001",
            "--flat_resolution=natural",
            "--max_depth=5",
            "--z=2",
            "--output=/work/output.tif"
        ]
    );
}

/// A default that isn't text, on a field the form shows as text, comes back as its JSON text and
/// is still the default (#1517: `topo_render`'s list went to the runner, which refused it).
#[test]
fn a_default_sent_back_as_text_is_left_to_the_runner() {
    let f = Fixture::new();
    let manifest = json!({
        "id": "render", "display_name": "Render", "category": "Terrain", "summary": "",
        "defaults": {"colour": [255, 255, 255, 255], "reverse": false, "ambient": 0.2, "palette": "[soft]"},
        "params": [
            {"name": "dem", "io_role": "input", "data_kind": "raster", "required": true, "schema": {"kind": "input"}},
            {"name": "colour", "data_kind": "string", "required": false, "schema": {"kind": "string"}},
            {"name": "reverse", "data_kind": "string", "required": false, "schema": {"kind": "string"}},
            {"name": "ambient", "data_kind": "string", "required": false, "schema": {"kind": "string"}},
            {"name": "palette", "data_kind": "string", "required": false, "schema": {"kind": "string"}},
            {"name": "output", "io_role": "output", "data_kind": "raster", "required": false, "schema": {"kind": "output"}}
        ]
    });
    let (spec, meta) = spec_of(&manifest).unwrap();
    let argv = |params: Value| {
        let params = rustgis_analysis::resolve_params(&spec, &params).unwrap();
        prepare_with(&spec, &meta, &f, &params).unwrap().argv("/work")
    };
    let bare = ["--dem=/work/in0-embedded.tif", "--output=/work/output.tif"];
    // The form's text, the same list spaced out, and the JSON a script or the MCP gives.
    let text = json!({"dem": "embedded", "colour": "[255,255,255,255]", "reverse": "false", "ambient": "0.2", "palette": "[soft]"});
    assert_eq!(argv(text), bare);
    assert_eq!(argv(json!({"dem": "embedded", "colour": "[255, 255, 255, 255]"})), bare);
    assert_eq!(argv(json!({"dem": "embedded", "colour": [255, 255, 255, 255], "reverse": false, "ambient": 0.2})), bare);
    // Changed values go. Text is compared as text: quoted, it isn't the text default.
    let changed = json!({"dem": "embedded", "colour": "[0,0,0,255]", "reverse": "true", "ambient": "0.5", "palette": "\"[soft]\""});
    assert_eq!(
        argv(changed),
        [
            "--dem=/work/in0-embedded.tif",
            "--colour=[0,0,0,255]",
            "--reverse=true",
            "--ambient=0.5",
            "--palette=\"[soft]\"",
            "--output=/work/output.tif"
        ]
    );
}
