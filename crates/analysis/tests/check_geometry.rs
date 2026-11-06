//! Check Geometry through the public tool dispatch: each problem code with its exact description,
//! several in one feature, clean shapes, selections, attributes and CRS, field-name collisions,
//! non-finite coordinates, the work bound, and input read by the real readers.
use rustgis_analysis::{Inputs, Output, ToolResult};
use rustgis_data::{Coord, Dataset, Feature, Field, FieldType, Geometry, GeometryType, Value};
use serde_json::json;

struct Layer {
    ds: Dataset,
    selected: Vec<usize>,
}
impl Inputs for Layer {
    fn dataset(&self, name: &str) -> Option<&Dataset> {
        (name == "input").then_some(&self.ds)
    }
    fn selection(&self, _: &str) -> &[usize] {
        &self.selected
    }
}

type Ring<'a> = &'a [(f64, f64)];

fn layer(ty: GeometryType) -> Layer {
    Layer { ds: Dataset::new("input", ty, vec![Field::new("name", FieldType::Text)]), selected: vec![] }
}
fn pts(r: Ring) -> Vec<Coord> {
    r.iter().map(|&(x, y)| Coord::new(x, y)).collect()
}
fn multi(parts: &[&[Ring]]) -> Geometry {
    Geometry::Polygon(parts.iter().map(|rings| rings.iter().map(|r| pts(r)).collect()).collect())
}
fn poly(rings: &[Ring]) -> Geometry {
    multi(&[rings])
}
fn line(parts: &[Ring]) -> Geometry {
    Geometry::Line(parts.iter().map(|r| pts(r)).collect())
}
fn add(l: &mut Layer, g: Option<Geometry>, name: &str) {
    l.ds.push_next(g, vec![Value::Text(name.into())]);
}
fn run(l: &Layer) -> ToolResult {
    rustgis_analysis::run("checkGeometry", l, &json!({"input": "input"})).unwrap()
}
fn dataset(r: &ToolResult) -> &Dataset {
    let Output::Layer(ds) = &r.output else { panic!("expected a layer") };
    ds
}
fn text(ds: &Dataset, row: usize, field: &str) -> String {
    match ds.value(row, ds.field_index(field).unwrap()) {
        Value::Text(t) => t.clone(),
        v => panic!("{field} is {v:?}"),
    }
}
/// The problem and detail of a single feature of type `ty` holding `g`.
fn one(ty: GeometryType, g: Option<Geometry>) -> (String, String) {
    let mut l = layer(ty);
    add(&mut l, g, "x");
    let r = run(&l);
    let ds = dataset(&r);
    assert_eq!(ds.features.len(), 1, "{:?}", r.messages);
    (text(ds, 0, "problem"), text(ds, 0, "detail"))
}
fn pair(problem: &str, detail: &str) -> (String, String) {
    (problem.into(), detail.into())
}

const SQUARE: Ring = &[(0., 0.), (10., 0.), (10., 10.), (0., 10.), (0., 0.)];
const HOLE: Ring = &[(2., 2.), (4., 2.), (4., 4.), (2., 4.), (2., 2.)];
const BOWTIE: Ring = &[(0., 0.), (2., 2.), (2., 0.), (0., 2.), (0., 0.)];
const OUTSIDE: Ring = &[(20., 20.), (21., 20.), (21., 21.), (20., 20.)];
/// A hole touching the shell at (0, 5) and (10, 5), splitting the square in two.
const ACROSS: Ring = &[(0., 5.), (5., 2.), (10., 5.), (5., 8.), (0., 5.)];

#[test]
fn each_problem_on_its_own() {
    use GeometryType::{Point, Polygon, Polyline};
    let cases: Vec<(GeometryType, Option<Geometry>, (String, String))> = vec![
        (Polygon, None, pair("null_geometry", "no geometry")),
        (Polygon, Some(Geometry::Polygon(vec![])), pair("null_geometry", "empty geometry")),
        (Polyline, Some(Geometry::Line(vec![vec![]])), pair("null_geometry", "empty geometry")),
        (Point, Some(Geometry::MultiPoint(vec![])), pair("null_geometry", "empty geometry")),
        (
            Point,
            Some(Geometry::Point(Coord::new(f64::NAN, 1.))),
            pair("non_finite_coordinate", "a non-finite coordinate in the point; written without geometry"),
        ),
        (
            Polygon,
            Some(poly(&[SQUARE, &[(2., 2.), (f64::INFINITY, 2.), (4., 4.), (2., 2.)]])),
            pair("non_finite_coordinate", "a non-finite coordinate in ring 2 of part 1; written without geometry"),
        ),
        (Polyline, Some(line(&[&[(0., 0.), (1., 1.)], &[(5., 5.)]])), pair("too_few_points", "part 2 has 1 distinct point")),
        (Polygon, Some(poly(&[&[(0., 0.), (1., 0.), (0., 0.)]])), pair("too_few_points", "exterior ring of part 1 has 2 distinct points")),
        (Polygon, Some(multi(&[&[SQUARE], &[]])), pair("too_few_points", "part 2 has no rings")),
        (
            Polygon,
            Some(poly(&[&[(0., 0.), (10., 0.), (10., 10.), (0., 10.)]])),
            pair("ring_not_closed", "exterior ring of part 1 is not closed"),
        ),
        (
            Polygon,
            Some(poly(&[SQUARE, &[(2., 2.), (4., 2.), (4., 2.), (4., 2.), (4., 4.), (2., 4.), (2., 2.)]])),
            pair("repeated_vertex", "2 repeated vertices in ring 2 of part 1"),
        ),
        (Polyline, Some(line(&[&[(0., 0.), (1., 1.), (1., 1.), (2., 0.)]])), pair("repeated_vertex", "1 repeated vertex in part 1")),
        (Polygon, Some(poly(&[BOWTIE])), pair("self_intersection", "exterior ring of part 1 crosses itself")),
        (Polygon, Some(poly(&[SQUARE, OUTSIDE])), pair("hole_outside_shell", "ring 2 of part 1 is not inside the exterior ring")),
        (
            Polygon,
            Some(poly(&[SQUARE, HOLE, &[(3., 3.), (5., 3.), (5., 5.), (3., 5.), (3., 3.)]])),
            pair("rings_intersect", "rings 2 and 3 of part 1 overlap"),
        ),
        (
            Polygon,
            Some(poly(&[SQUARE, &[(0., 2.), (3., 2.), (3., 4.), (0., 4.), (0., 2.)]])),
            pair("rings_intersect", "exterior ring and ring 2 of part 1 touch along a line"),
        ),
        (
            Polygon,
            Some(poly(&[SQUARE, ACROSS])),
            pair("disconnected_interior", "the interior of part 1 is split into pieces by rings touching at points"),
        ),
        (
            Polygon,
            Some(multi(&[&[SQUARE], &[&[(5., 5.), (15., 5.), (15., 15.), (5., 15.), (5., 5.)]]])),
            pair("parts_overlap", "parts 1 and 2 overlap"),
        ),
        (
            Polygon,
            Some(multi(&[&[SQUARE], &[&[(10., 0.), (20., 0.), (20., 10.), (10., 10.), (10., 0.)]]])),
            pair("parts_overlap", "parts 1 and 2 touch along a line"),
        ),
    ];
    for (ty, g, expected) in cases {
        assert_eq!(one(ty, g.clone()), expected, "{g:?}");
    }
}

#[test]
fn several_problems_are_listed_in_a_fixed_order() {
    // An unclosed bowtie with a repeated vertex, a hole outside it and a second, overlapping part.
    let g = multi(&[
        &[&[(0., 0.), (2., 2.), (2., 2.), (2., 0.), (0., 2.)], OUTSIDE],
        &[SQUARE],
        &[&[(5., 5.), (15., 5.), (15., 15.), (5., 15.), (5., 5.)]],
    ]);
    let (problem, detail) = one(GeometryType::Polygon, Some(g));
    assert_eq!(problem, "ring_not_closed, repeated_vertex, self_intersection, parts_overlap");
    assert_eq!(
        detail,
        "exterior ring of part 1 is not closed; 1 repeated vertex in exterior ring of part 1; \
         exterior ring of part 1 crosses itself; parts 2 and 3 overlap"
    );
}

#[test]
fn clean_shapes_give_an_empty_layer_and_say_so() {
    let mut l = layer(GeometryType::Polygon);
    add(&mut l, Some(poly(&[SQUARE])), "square");
    add(&mut l, Some(poly(&[SQUARE, HOLE])), "with hole");
    add(&mut l, Some(multi(&[&[SQUARE, HOLE], &[&[(20., 0.), (30., 0.), (30., 10.), (20., 0.)]]])), "multipart");
    // A ring of exactly three distinct points and its closing point.
    add(&mut l, Some(poly(&[&[(0., 0.), (1., 0.), (0., 1.), (0., 0.)]])), "triangle");
    let r = run(&l);
    assert_eq!(r.messages, ["Check Geometry: no problems found in 4 features"]);
    let ds = dataset(&r);
    assert!(ds.features.is_empty());
    assert_eq!(ds.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["name", "problem", "detail"]);

    let mut l = layer(GeometryType::Polyline);
    // A line crossing itself is a valid line.
    add(&mut l, Some(line(&[BOWTIE])), "crossing");
    let r = run(&l);
    assert_eq!(r.messages, ["Check Geometry: no problems found in 1 feature"]);
    assert!(dataset(&r).features.is_empty());

    let mut l = layer(GeometryType::Point);
    add(&mut l, Some(Geometry::Point(Coord::new(1., 2.))), "a");
    add(&mut l, Some(Geometry::MultiPoint(pts(&[(1., 2.), (1., 2.)]))), "b");
    assert_eq!(run(&l).messages, ["Check Geometry: no problems found in 2 features"]);
}

#[test]
fn edge_cases_of_rings() {
    // Touching itself at a vertex (two triangles meeting at (2, 2)) is invalid in OGC.
    let touch: Ring = &[(0., 0.), (4., 0.), (2., 2.), (4., 4.), (0., 4.), (2., 2.), (0., 0.)];
    assert_eq!(one(GeometryType::Polygon, Some(poly(&[touch]))), pair("self_intersection", "exterior ring of part 1 touches itself"));
    // Three collinear points: no area, the ring runs back over itself.
    assert_eq!(
        one(GeometryType::Polygon, Some(poly(&[&[(0., 0.), (1., 0.), (2., 0.), (0., 0.)]]))),
        pair("self_intersection", "exterior ring of part 1 runs back over itself")
    );
    // A duplicated closing point is a repeated vertex, nothing else.
    assert_eq!(
        one(GeometryType::Polygon, Some(poly(&[&[(0., 0.), (10., 0.), (10., 10.), (0., 0.), (0., 0.)]]))),
        pair("repeated_vertex", "1 repeated vertex in exterior ring of part 1")
    );
}

#[test]
fn a_feature_too_complex_to_check_is_reported_never_taken_for_valid() {
    // A zigzag ring whose 12,000 segments all overlap each other's x and y ranges: the sweep would
    // compare some 72 million pairs, past the bound, so it is counted and not swept.
    let n = 12_000;
    let mut ring: Vec<Coord> = (0..n).map(|i| Coord::new(f64::from(i % 2), f64::from(i) * 1e-3 + f64::from(i % 2) * 100.0)).collect();
    ring.push(Coord::new(-1., 50.));
    ring.push(Coord::new(0., 0.));
    let mut l = layer(GeometryType::Polygon);
    add(&mut l, Some(Geometry::Polygon(vec![vec![ring]])), "busy");
    add(&mut l, Some(poly(&[BOWTIE])), "bowtie");
    let started = std::time::Instant::now();
    let r = run(&l);
    assert!(started.elapsed().as_secs() < 30, "counted, not swept");
    let ds = dataset(&r);
    assert_eq!(text(ds, 0, "problem"), "too_complex");
    assert_eq!(text(ds, 0, "detail"), "not checked fully: too complex for the work limit");
    assert!(ds.features[0].geometry.is_some());
    assert_eq!(
        r.messages,
        [
            "Check Geometry: 2 of 2 features have problems (self_intersection: 1, too_complex: 1)",
            "Check Geometry: 1 feature could not be checked fully (too complex for the work limit); it is in the output as too_complex",
        ]
    );
}

#[test]
fn selection_is_honoured_once_per_feature_in_table_order() {
    let mut l = layer(GeometryType::Polygon);
    add(&mut l, Some(poly(&[BOWTIE])), "unselected bowtie");
    add(&mut l, Some(poly(&[SQUARE])), "selected square");
    add(&mut l, Some(poly(&[SQUARE, OUTSIDE])), "selected hole outside");
    add(&mut l, None, "selected null");
    l.selected = vec![3, 1, 2, 3, usize::MAX];
    let r = run(&l);
    let ds = dataset(&r);
    let names: Vec<String> = (0..ds.features.len()).map(|i| text(ds, i, "name")).collect();
    assert_eq!(names, ["selected hole outside", "selected null"]);
    assert_eq!(r.messages, ["Check Geometry: 2 of 3 features have problems (null_geometry: 1, hole_outside_shell: 1)"]);
}

#[test]
fn attributes_crs_ids_and_names_are_kept() {
    let mut l = Layer {
        ds: Dataset::new(
            "Parcels",
            GeometryType::Polygon,
            vec![Field::new("name", FieldType::Text), Field::new("area", FieldType::Double), Field::new("n", FieldType::Integer)],
        ),
        selected: vec![],
    };
    l.ds.crs = Some(rustgis_data::raster::Crs::Epsg(2180));
    l.ds.crs_incomplete = true;
    l.ds.zm_dropped = true;
    l.ds.features.push(Feature {
        id: 41,
        geometry: Some(poly(&[SQUARE])),
        attrs: vec![Value::Text("ok".into()), Value::Float(1.5), Value::Int(1)],
    });
    // A short row: its missing value is null.
    l.ds.features.push(Feature { id: 42, geometry: Some(poly(&[BOWTIE])), attrs: vec![Value::Text("bad".into()), Value::Float(2.5)] });
    let named = |output: Option<&str>| {
        let mut p = json!({"input": "input"});
        if let Some(o) = output {
            p["output"] = json!(o);
        }
        rustgis_analysis::run("checkGeometry", &l, &p).unwrap()
    };
    let r = named(None);
    let ds = dataset(&r);
    assert_eq!(ds.name, "Parcels_check");
    assert_eq!(ds.geometry_type, GeometryType::Polygon);
    assert_eq!(ds.crs, l.ds.crs);
    assert!(ds.crs_incomplete && ds.zm_dropped);
    assert_eq!(ds.fields.len(), 5);
    assert_eq!(ds.fields[..3], l.ds.fields[..]);
    assert_eq!((ds.fields[3].ty, ds.fields[4].ty), (FieldType::Text, FieldType::Text));
    let f = &ds.features[0];
    assert_eq!(f.id, 42, "the input's id leads back to its feature");
    assert_eq!(f.geometry, l.ds.features[1].geometry, "written as stored");
    assert_eq!(f.attrs[..3], [Value::Text("bad".into()), Value::Float(2.5), Value::Null]);
    assert_eq!(dataset(&named(Some(" Invalid parcels "))).name, "Invalid parcels");
}

#[test]
fn existing_problem_and_detail_fields_get_other_names() {
    let mut l = Layer {
        ds: Dataset::new(
            "input",
            GeometryType::Polygon,
            vec![
                Field::new("Problem", FieldType::Text),
                Field::new("detail", FieldType::Integer),
                Field::new("problem_2", FieldType::Text),
            ],
        ),
        selected: vec![],
    };
    l.ds.push_next(Some(poly(&[BOWTIE])), vec![Value::Text("mine".into()), Value::Int(7), Value::Text("also mine".into())]);
    let r = run(&l);
    let ds = dataset(&r);
    assert_eq!(ds.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["Problem", "detail", "problem_2", "problem_3", "detail_2"]);
    assert_eq!(ds.features[0].attrs[..3], l.ds.features[0].attrs[..]);
    assert_eq!(text(ds, 0, "problem_3"), "self_intersection");
    assert_eq!(text(ds, 0, "detail_2"), "exterior ring of part 1 crosses itself");
    assert_eq!(
        r.messages,
        [
            "Check Geometry: 1 of 1 feature has problems (self_intersection: 1)",
            "Check Geometry: 'input' already has a field named problem, so the output's problem field is problem_3",
            "Check Geometry: 'input' already has a field named detail, so the output's detail field is detail_2",
        ]
    );
}

#[test]
fn non_finite_coordinates_are_written_without_geometry() {
    let mut l = layer(GeometryType::Polygon);
    add(&mut l, Some(poly(&[&[(0., 0.), (f64::NAN, 0.), (1., 1.), (0., f64::NEG_INFINITY), (0., 0.)]])), "nan");
    let r = run(&l);
    let ds = dataset(&r);
    assert_eq!(ds.features[0].geometry, None);
    assert!(ds.extent().is_empty(), "nothing to spoil the extent");
    assert_eq!(text(ds, 0, "detail"), "2 non-finite coordinates, the first in exterior ring of part 1; written without geometry");
    assert_eq!(
        r.messages,
        [
            "Check Geometry: 1 of 1 feature has problems (non_finite_coordinate: 1)",
            "Check Geometry: 1 feature with non-finite coordinates is written without geometry",
        ]
    );
    // Several: plural. A bowtie in another part is still found; the parts aren't related, which
    // is said.
    add(&mut l, Some(Geometry::Line(vec![pts(&[(0., 0.), (f64::INFINITY, 1.)])])), "inf line");
    add(&mut l, Some(multi(&[&[BOWTIE], &[&[(5., 5.), (f64::NAN, 5.), (6., 6.), (5., 5.)]]])), "bowtie and nan");
    let r = run(&l);
    let ds = dataset(&r);
    assert!(ds.features.iter().all(|f| f.geometry.is_none()));
    assert_eq!(text(ds, 2, "problem"), "non_finite_coordinate, self_intersection");
    assert_eq!(
        r.messages,
        [
            "Check Geometry: 3 of 3 features have problems (non_finite_coordinate: 3, self_intersection: 1)",
            "Check Geometry: 3 features with non-finite coordinates are written without geometry",
            "Check Geometry: the rings or parts of 1 feature weren't all checked against each other, because some of their rings or parts already have problems of their own (see problem)",
        ]
    );
}

/// OGC requires a polygon's interior to be connected: rings may touch at points, but not so that
/// they cut a piece of the interior off.
#[test]
fn rings_touching_at_points_are_checked_for_a_split_interior() {
    let split = "the interior of part 1 is split into pieces by rings touching at points";
    let cases: Vec<(&str, Geometry, Option<&str>)> = vec![
        ("hole touching the shell twice", poly(&[SQUARE, ACROSS]), Some(split)),
        ("hole touching the shell once", poly(&[SQUARE, &[(0., 5.), (3., 4.), (3., 6.), (0., 5.)]]), None),
        ("two holes touching once", poly(&[SQUARE, HOLE, &[(4., 4.), (6., 4.), (6., 6.), (4., 4.)]]), None),
        (
            "three holes in a loop",
            poly(&[
                SQUARE,
                &[(3., 3.), (5., 6.), (1., 6.), (3., 3.)],
                &[(3., 3.), (5., 1.), (7., 3.), (3., 3.)],
                &[(7., 3.), (9., 6.), (5., 6.), (7., 3.)],
            ]),
            Some(split),
        ),
        (
            "three rings at one point",
            poly(&[SQUARE, &[(0., 5.), (3., 6.), (2., 8.), (0., 5.)], &[(0., 5.), (2., 2.), (3., 4.), (0., 5.)]]),
            None,
        ),
        (
            "shell, hole, hole, shell",
            poly(&[SQUARE, &[(0., 5.), (4., 4.), (3., 7.), (0., 5.)], &[(5., 0.), (7., 3.), (4., 4.), (5., 0.)]]),
            Some(split),
        ),
        ("valid", poly(&[SQUARE, HOLE]), None),
        (
            "in part 2, beside a valid part",
            multi(&[&[&[(20., 0.), (30., 0.), (30., 10.), (20., 0.)]], &[SQUARE, ACROSS]]),
            Some("the interior of part 2 is split into pieces by rings touching at points"),
        ),
        ("parts touching twice", multi(&[&[SQUARE], &[&[(10., 0.), (20., 5.), (10., 10.), (15., 5.), (10., 0.)]]]), None),
        ("island touching its hole four times", multi(&[&[SQUARE, HOLE], &[&[(3., 2.), (4., 3.), (3., 4.), (2., 3.), (3., 2.)]]]), None),
    ];
    let mut l = layer(GeometryType::Polygon);
    for (name, g, _) in &cases {
        add(&mut l, Some(g.clone()), name);
    }
    let r = run(&l);
    let ds = dataset(&r);
    let rows: Vec<(String, String, String)> =
        (0..ds.features.len()).map(|i| (text(ds, i, "name"), text(ds, i, "problem"), text(ds, i, "detail"))).collect();
    let expected: Vec<(String, String, String)> = cases
        .iter()
        .filter_map(|(name, _, detail)| detail.map(|d| (name.to_string(), "disconnected_interior".to_string(), d.to_string())))
        .collect();
    assert_eq!(rows, expected);
    assert_eq!(r.messages, ["Check Geometry: 4 of 10 features have problems (disconnected_interior: 4)"]);
}

#[test]
fn a_mixed_layer_counts_each_kind_of_problem_by_feature() {
    let mut l = layer(GeometryType::Polygon);
    add(&mut l, Some(poly(&[SQUARE])), "ok");
    add(&mut l, Some(poly(&[BOWTIE])), "bowtie 1");
    add(&mut l, Some(poly(&[SQUARE, OUTSIDE, BOWTIE])), "bowtie hole");
    add(&mut l, None, "null");
    add(&mut l, Some(poly(&[SQUARE, HOLE])), "ok hole");
    // A point stored in a polygon layer is checked as a point.
    add(&mut l, Some(Geometry::Point(Coord::new(1., 1.))), "point");
    add(&mut l, Some(poly(&[&[(0., 0.), (10., 0.), (10., 10.), (0., 10.)]])), "open");
    let r = run(&l);
    let ds = dataset(&r);
    let rows: Vec<(String, String)> = (0..ds.features.len()).map(|i| (text(ds, i, "name"), text(ds, i, "problem"))).collect();
    assert_eq!(
        rows,
        [
            pair("bowtie 1", "self_intersection"),
            pair("bowtie hole", "self_intersection"),
            pair("null", "null_geometry"),
            pair("open", "ring_not_closed"),
        ]
    );
    assert_eq!(
        r.messages,
        [
            "Check Geometry: 4 of 7 features have problems (null_geometry: 1, ring_not_closed: 1, self_intersection: 2)",
            "Check Geometry: the rings or parts of 1 feature weren't all checked against each other, because some of their rings or parts already have problems of their own (see problem)",
        ]
    );
}

/// Parts are only compared when their rings have no problem: a part whose hole is outside its
/// shell isn't checked against the part overlapping it, and the notice says why.
#[test]
fn parts_are_not_compared_when_a_part_has_a_problem_of_its_own() {
    let mut l = layer(GeometryType::Polygon);
    add(&mut l, Some(multi(&[&[SQUARE, OUTSIDE], &[&[(5., 5.), (15., 5.), (15., 15.), (5., 15.), (5., 5.)]]])), "hole outside, overlapped");
    let r = run(&l);
    let ds = dataset(&r);
    assert_eq!(text(ds, 0, "problem"), "hole_outside_shell");
    assert_eq!(text(ds, 0, "detail"), "ring 2 of part 1 is not inside the exterior ring");
    assert_eq!(
        r.messages,
        [
            "Check Geometry: 1 of 1 feature has problems (hole_outside_shell: 1)",
            "Check Geometry: the rings or parts of 1 feature weren't all checked against each other, because some of their rings or parts already have problems of their own (see problem)",
        ]
    );
}

#[test]
fn empty_layers_and_layers_of_null_geometries() {
    let r = run(&layer(GeometryType::Polygon));
    assert_eq!(r.messages, ["Check Geometry: no features to check"]);
    assert!(dataset(&r).features.is_empty());

    let mut l = layer(GeometryType::Polyline);
    add(&mut l, None, "a");
    add(&mut l, None, "b");
    let r = run(&l);
    let ds = dataset(&r);
    assert_eq!(ds.features.len(), 2);
    assert!(ds.features.iter().all(|f| f.geometry.is_none()));
    assert_eq!(text(ds, 1, "detail"), "no geometry");
    assert_eq!(r.messages, ["Check Geometry: 2 of 2 features have problems (null_geometry: 2)"]);
}

#[test]
fn long_lists_of_problems_are_cut_with_a_count() {
    // 25 parts with a repeated vertex each, far apart: 25 problems, 20 described.
    let parts: Vec<Vec<Vec<Coord>>> = (0..25)
        .map(|i| {
            let x = f64::from(i) * 10.0;
            vec![pts(&[(x, 0.), (x + 1., 0.), (x + 1., 0.), (x + 1., 1.), (x, 0.)])]
        })
        .collect();
    let (problem, detail) = one(GeometryType::Polygon, Some(Geometry::Polygon(parts)));
    assert_eq!(problem, "repeated_vertex");
    let items: Vec<&str> = detail.split("; ").collect();
    assert_eq!(items.len(), 21);
    assert_eq!(items[19], "1 repeated vertex in exterior ring of part 20");
    assert_eq!(items[20], "and 5 more problems");
}

#[test]
fn extra_attribute_values_are_reported() {
    let mut l = layer(GeometryType::Polygon);
    l.ds.features.push(Feature { id: 1, geometry: Some(poly(&[BOWTIE])), attrs: vec![Value::Text("a".into()), Value::Int(9)] });
    let r = run(&l);
    let ds = dataset(&r);
    assert_eq!(ds.features[0].attrs.len(), 3);
    assert_eq!(
        r.messages[1],
        "Check Geometry: 1 feature had more attribute values than the layer has fields; the extra values aren't in the output"
    );
}

#[test]
fn unusable_input_is_an_error() {
    let table = Layer { ds: Dataset::new("input", GeometryType::None, vec![Field::new("name", FieldType::Text)]), selected: vec![] };
    let e = rustgis_analysis::run("checkGeometry", &table, &json!({"input": "input"})).unwrap_err();
    assert_eq!(e, "Check Geometry needs a layer with geometry: 'input' is a table");
    let e = rustgis_analysis::run("checkGeometry", &table, &json!({"input": "nope"})).unwrap_err();
    assert_eq!(e, "no layer named 'nope'");
    assert!(rustgis_analysis::run("checkGeometry", &table, &json!({})).is_err());
}

/// GeoJSON keeps rings exactly as written: it neither closes an open ring nor rejects a bowtie or
/// a hole outside its shell, so the tool sees what the file holds.
#[test]
fn geojson_read_by_the_real_reader() {
    let geojson = r#"{"type":"FeatureCollection","features":[
        {"type":"Feature","properties":{"name":"bowtie"},"geometry":{"type":"Polygon","coordinates":[[[0,0],[2,2],[2,0],[0,2],[0,0]]]}},
        {"type":"Feature","properties":{"name":"open"},"geometry":{"type":"Polygon","coordinates":[[[0,0],[4,0],[4,4],[0,4]]]}},
        {"type":"Feature","properties":{"name":"hole outside"},"geometry":{"type":"Polygon","coordinates":[[[0,0],[4,0],[4,4],[0,4],[0,0]],[[10,10],[11,10],[11,11],[10,10]]]}},
        {"type":"Feature","properties":{"name":"empty"},"geometry":{"type":"Polygon","coordinates":[]}},
        {"type":"Feature","properties":{"name":"split"},"geometry":{"type":"Polygon","coordinates":[[[0,0],[10,0],[10,10],[0,10],[0,0]],[[0,0],[6,4],[10,10],[4,6],[0,0]]]}},
        {"type":"Feature","properties":{"name":"touching once"},"geometry":{"type":"MultiPolygon","coordinates":[[[[0,0],[10,0],[10,10],[0,10],[0,0]],[[0,0],[6,4],[4,6],[0,0]]],[[[10,0],[20,5],[10,10],[15,5],[10,0]]]]}},
        {"type":"Feature","properties":{"name":"fine"},"geometry":{"type":"Polygon","coordinates":[[[0,0],[4,0],[4,4],[0,4],[0,0]]]}}
    ]}"#;
    let (ds, report) = rustgis_data::io::read_geojson(geojson, "input").unwrap();
    assert!(report.messages.is_empty() && report.skipped == 0);
    // What the reader delivers: the open ring stays open, and an empty polygon is a part without rings.
    assert_eq!(ds.features[1].geometry, Some(poly(&[&[(0., 0.), (4., 0.), (4., 4.), (0., 4.)]])));
    assert_eq!(ds.features[3].geometry, Some(Geometry::Polygon(vec![vec![]])));
    let l = Layer { ds, selected: vec![] };
    let r = run(&l);
    let out = dataset(&r);
    let rows: Vec<(String, String, String)> =
        (0..out.features.len()).map(|i| (text(out, i, "name"), text(out, i, "problem"), text(out, i, "detail"))).collect();
    let row = |n: &str, p: &str, d: &str| (n.to_string(), p.to_string(), d.to_string());
    assert_eq!(
        rows,
        [
            row("bowtie", "self_intersection", "exterior ring of part 1 crosses itself"),
            row("open", "ring_not_closed", "exterior ring of part 1 is not closed"),
            row("hole outside", "hole_outside_shell", "ring 2 of part 1 is not inside the exterior ring"),
            row("empty", "null_geometry", "empty geometry"),
            row("split", "disconnected_interior", "the interior of part 1 is split into pieces by rings touching at points"),
        ]
    );
}

/// WKT in a CSV keeps open rings too; the Shapefile writer closes and reorients rings, and its
/// reader keeps a hole outside its shell as a ring of the same polygon.
#[test]
fn csv_wkt_and_shapefile_read_by_the_real_readers() {
    let csv = "name,wkt\n\
        bowtie,\"POLYGON((0 0, 2 2, 2 0, 0 2, 0 0))\"\n\
        open,\"POLYGON((0 0, 4 0, 4 4, 0 4))\"\n\
        hole outside,\"POLYGON((0 0, 4 0, 4 4, 0 4, 0 0),(10 10, 11 10, 11 11, 10 10))\"\n";
    let (ds, report) = rustgis_data::io::read_csv(csv, "input").unwrap();
    assert!(report.messages.is_empty() && report.skipped == 0);
    assert_eq!(ds.geometry_type, GeometryType::Polygon);
    assert_eq!(ds.features[1].geometry, Some(poly(&[&[(0., 0.), (4., 0.), (4., 4.), (0., 4.)]])), "the WKT ring stays open");
    let l = Layer { ds, selected: vec![] };
    let r = run(&l);
    let out = dataset(&r);
    let problems: Vec<String> = (0..out.features.len()).map(|i| text(out, i, "problem")).collect();
    assert_eq!(problems, ["self_intersection", "ring_not_closed", "hole_outside_shell"]);

    let (parts, left_out) = rustgis_data::io::shapefile_parts(&l.ds).unwrap();
    assert_eq!(left_out, 0);
    let dir = std::env::temp_dir().join(format!("rustgis-check-geometry-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (ext, bytes) in &parts {
        std::fs::write(dir.join(format!("input.{ext}")), bytes).unwrap();
    }
    let read = rustgis_data::io::read_shapefile_in(&dir.join("input.shp"), "input", None);
    let _ = std::fs::remove_dir_all(&dir);
    let (ds, _) = read.unwrap();
    // What the Shapefile round trip delivers: the open ring closed, the bowtie and the hole kept.
    let Some(Geometry::Polygon(open)) = &ds.features[1].geometry else { panic!("a polygon") };
    assert_eq!(open[0].first(), open[0].last());
    assert_eq!(ds.features[2].geometry.as_ref().map(|g| matches!(g, Geometry::Polygon(p) if p.len() == 1 && p[0].len() == 2)), Some(true));
    let l = Layer { ds, selected: vec![] };
    let r = run(&l);
    let out = dataset(&r);
    let rows: Vec<(String, String)> = (0..out.features.len()).map(|i| (text(out, i, "name"), text(out, i, "problem"))).collect();
    assert_eq!(rows, [pair("bowtie", "self_intersection"), pair("hole outside", "hole_outside_shell")]);
}
