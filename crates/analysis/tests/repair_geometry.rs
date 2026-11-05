//! Repair Geometry through the public tool dispatch: each repair with its exact result and code,
//! several in one feature, valid features copied as they are, both ring windings, lines and
//! points, rows that can't be repaired, the work bound, selections, attributes and CRS, the
//! repairs field's name, how far the overlay moves coordinates, and input read by the real
//! GeoJSON and Shapefile readers. Every repaired geometry passes Check Geometry.
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
    rustgis_analysis::run("repairGeometry", l, &json!({"input": "input"})).unwrap()
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
/// The output's geometries and repairs, row by row.
fn rows(r: &ToolResult) -> Vec<(Option<Geometry>, String)> {
    let ds = dataset(r);
    (0..ds.features.len()).map(|i| (ds.features[i].geometry.clone(), text(ds, i, "repairs"))).collect()
}
/// The geometry and repairs of a single feature of type `ty` holding `g`.
fn one(ty: GeometryType, g: Option<Geometry>) -> (Option<Geometry>, String) {
    let mut l = layer(ty);
    add(&mut l, g, "x");
    let r = run(&l);
    assert_eq!(dataset(&r).features.len(), 1, "{:?}", r.messages);
    valid(dataset(&r));
    rows(&r).remove(0)
}

/// Every geometry of `ds` passes Check Geometry: its output is empty but for rows without geometry.
fn valid(ds: &Dataset) {
    let mut copy = ds.clone();
    copy.name = "input".into();
    let l = Layer { ds: copy, selected: vec![] };
    let r = rustgis_analysis::run("checkGeometry", &l, &json!({"input": "input"})).unwrap();
    let out = dataset(&r);
    let bad: Vec<(String, Option<Geometry>)> = (0..out.features.len())
        .filter(|&i| text(out, i, "problem") != "null_geometry")
        .map(|i| (text(out, i, "detail"), out.features[i].geometry.clone()))
        .collect();
    assert!(bad.is_empty(), "not valid: {bad:?}");
}

/// A polygon geometry with each ring's closing point left out and the ring started at its
/// smallest vertex (by x, then y), and the parts in order of their first vertex, so geometries
/// that differ only in where rings start compare equal. Rings keep their direction.
fn norm(g: &Geometry) -> Geometry {
    let Geometry::Polygon(parts) = g else { return g.clone() };
    let ring = |r: &Vec<Coord>| {
        let mut v: Vec<Coord> = r.clone();
        if v.len() > 1 && v.first() == v.last() {
            v.pop();
        }
        let min = (0..v.len()).min_by(|&a, &b| (v[a].x, v[a].y).partial_cmp(&(v[b].x, v[b].y)).unwrap()).unwrap_or(0);
        v.rotate_left(min);
        v
    };
    let mut parts: Vec<Vec<Vec<Coord>>> = parts
        .iter()
        .map(|rings| {
            let mut rings: Vec<Vec<Coord>> = rings.iter().map(ring).collect();
            rings[1..].sort_by(|a, b| (a[0].x, a[0].y).partial_cmp(&(b[0].x, b[0].y)).unwrap());
            rings
        })
        .collect();
    parts.sort_by(|a, b| (a[0][0].x, a[0][0].y).partial_cmp(&(b[0][0].x, b[0][0].y)).unwrap());
    Geometry::Polygon(parts)
}
/// Every coordinate of `g`, in order.
fn coords(g: &Geometry) -> Vec<Coord> {
    match g {
        Geometry::Point(c) => vec![*c],
        Geometry::MultiPoint(v) => v.clone(),
        Geometry::Line(parts) => parts.iter().flatten().copied().collect(),
        Geometry::Polygon(parts) => parts.iter().flatten().flatten().copied().collect(),
    }
}
/// Twice the signed area of a ring (positive counter-clockwise).
fn signed(r: &[Coord]) -> f64 {
    r.windows(2).map(|w| w[0].x * w[1].y - w[1].x * w[0].y).sum()
}
/// A polygon geometry's area: each part's exterior ring less its holes.
fn area(g: &Geometry) -> f64 {
    let Geometry::Polygon(parts) = g else { panic!("a polygon: {g:?}") };
    parts.iter().map(|rings| signed(&rings[0]).abs() / 2.0 - rings[1..].iter().map(|h| signed(h).abs() / 2.0).sum::<f64>()).sum()
}
/// Every ring reversed, as the other of GeoJSON's and Shapefile's windings has it.
fn reversed(g: &Geometry) -> Geometry {
    let Geometry::Polygon(parts) = g else { return g.clone() };
    Geometry::Polygon(parts.iter().map(|rings| rings.iter().map(|r| r.iter().rev().copied().collect()).collect()).collect())
}

const SQUARE: Ring = &[(0., 0.), (10., 0.), (10., 10.), (0., 10.), (0., 0.)];
const HOLE: Ring = &[(2., 2.), (4., 2.), (4., 4.), (2., 4.), (2., 2.)];
const BOWTIE: Ring = &[(0., 0.), (2., 2.), (2., 0.), (0., 2.), (0., 0.)];
const OUTSIDE: Ring = &[(20., 20.), (21., 20.), (21., 21.), (20., 20.)];
/// A hole touching the shell at (0, 5) and (10, 5), splitting the square in two.
const ACROSS: Ring = &[(0., 5.), (5., 2.), (10., 5.), (5., 8.), (0., 5.)];
const OVERLAPPING: Ring = &[(5., 5.), (15., 5.), (15., 15.), (5., 15.), (5., 5.)];
/// Three holes each touching the next at a vertex, enclosing a triangle of the interior.
const LOOP: [Ring; 3] =
    [&[(3., 3.), (5., 6.), (1., 6.), (3., 3.)], &[(3., 3.), (5., 1.), (7., 3.), (3., 3.)], &[(7., 3.), (9., 6.), (5., 6.), (7., 3.)]];

/// The fixtures the overlay repairs, by name, with the area each comes out with.
fn rebuilt() -> Vec<(&'static str, Geometry, f64)> {
    vec![
        ("bowtie", poly(&[BOWTIE]), 2.0),
        ("touching itself", poly(&[&[(0., 0.), (4., 0.), (2., 2.), (4., 4.), (0., 4.), (2., 2.), (0., 0.)]]), 8.0),
        ("hole outside", poly(&[SQUARE, OUTSIDE]), 100.0),
        ("hole half outside", poly(&[SQUARE, &[(8., 4.), (12., 4.), (12., 6.), (8., 6.), (8., 4.)]]), 96.0),
        ("holes overlapping", poly(&[SQUARE, HOLE, &[(3., 3.), (5., 3.), (5., 5.), (3., 5.), (3., 3.)]]), 93.0),
        ("hole along the shell", poly(&[SQUARE, &[(0., 2.), (3., 2.), (3., 4.), (0., 4.), (0., 2.)]]), 94.0),
        ("split interior", poly(&[SQUARE, ACROSS]), 70.0),
        ("holes in a loop", poly(&[SQUARE, LOOP[0], LOOP[1], LOOP[2]]), 84.0),
        ("parts overlapping", multi(&[&[SQUARE], &[OVERLAPPING]]), 175.0),
        ("parts along a line", multi(&[&[SQUARE], &[&[(10., 0.), (20., 0.), (20., 10.), (10., 10.), (10., 0.)]]]), 200.0),
        ("hole filled by another part", multi(&[&[SQUARE, HOLE], &[&[(1., 1.), (5., 1.), (5., 5.), (1., 5.), (1., 1.)]]]), 100.0),
        ("bowtie part over a square", multi(&[&[SQUARE], &[&[(5., 5.), (15., 15.), (15., 5.), (5., 15.), (5., 5.)]]]), 137.5),
    ]
}

#[test]
fn each_repair_on_its_own() {
    use GeometryType::{Point, Polygon, Polyline};
    let fixed = |g: Geometry, code: &str| (Some(g), code.to_string());
    let gone = |code: &str| (None, code.to_string());
    let cases: Vec<(GeometryType, Option<Geometry>, (Option<Geometry>, String))> = vec![
        // The fixes that move no coordinate.
        (Polygon, Some(poly(&[&[(0., 0.), (10., 0.), (10., 10.), (0., 10.)]])), fixed(poly(&[SQUARE]), "ring_not_closed")),
        (
            Polygon,
            Some(poly(&[SQUARE, &[(2., 2.), (4., 2.), (4., 2.), (4., 2.), (4., 4.), (2., 4.), (2., 2.)]])),
            fixed(poly(&[SQUARE, HOLE]), "repeated_vertex"),
        ),
        (
            Polygon,
            Some(poly(&[&[(0., 0.), (10., 0.), (10., 10.), (0., 10.), (0., 0.), (0., 0.)]])),
            fixed(poly(&[SQUARE]), "repeated_vertex"),
        ),
        (Polygon, Some(poly(&[SQUARE, &[(2., 2.), (3., 3.), (2., 2.)]])), fixed(poly(&[SQUARE]), "too_few_points")),
        (Polygon, Some(multi(&[&[SQUARE], &[&[(20., 0.), (21., 0.), (20., 0.)], HOLE]])), fixed(poly(&[SQUARE]), "too_few_points")),
        (Polygon, Some(multi(&[&[SQUARE], &[]])), fixed(poly(&[SQUARE]), "too_few_points")),
        (
            Polyline,
            Some(line(&[&[(0., 0.), (1., 1.), (1., 1.), (2., 0.)]])),
            fixed(line(&[&[(0., 0.), (1., 1.), (2., 0.)]]), "repeated_vertex"),
        ),
        (Polyline, Some(line(&[&[(0., 0.), (1., 1.)], &[(5., 5.)]])), fixed(line(&[&[(0., 0.), (1., 1.)]]), "too_few_points")),
        (
            Polyline,
            Some(line(&[&[(0., 0.), (1., 1.)], &[(5., 5.), (5., 5.)]])),
            fixed(line(&[&[(0., 0.), (1., 1.)]]), "too_few_points, repeated_vertex"),
        ),
        // The rebuild by area.
        (
            Polygon,
            Some(poly(&[BOWTIE])),
            fixed(multi(&[&[&[(0., 0.), (1., 1.), (0., 2.)]], &[&[(1., 1.), (2., 0.), (2., 2.)]]]), "self_intersection"),
        ),
        (
            Polygon,
            Some(poly(&[&[(0., 0.), (4., 0.), (2., 2.), (4., 4.), (0., 4.), (2., 2.), (0., 0.)]])),
            fixed(multi(&[&[&[(0., 0.), (4., 0.), (2., 2.)]], &[&[(0., 4.), (2., 2.), (4., 4.)]]]), "self_intersection"),
        ),
        (Polygon, Some(poly(&[SQUARE, OUTSIDE])), fixed(poly(&[SQUARE]), "hole_outside_shell")),
        (
            Polygon,
            Some(poly(&[SQUARE, HOLE, &[(3., 3.), (5., 3.), (5., 5.), (3., 5.), (3., 3.)]])),
            fixed(poly(&[SQUARE, &[(2., 2.), (2., 4.), (3., 4.), (3., 5.), (5., 5.), (5., 3.), (4., 3.), (4., 2.)]]), "rings_intersect"),
        ),
        (
            Polygon,
            Some(poly(&[SQUARE, &[(0., 2.), (3., 2.), (3., 4.), (0., 4.), (0., 2.)]])),
            fixed(poly(&[&[(0., 0.), (10., 0.), (10., 10.), (0., 10.), (0., 4.), (3., 4.), (3., 2.), (0., 2.)]]), "rings_intersect"),
        ),
        (
            Polygon,
            Some(poly(&[SQUARE, ACROSS])),
            fixed(
                multi(&[
                    &[&[(0., 0.), (10., 0.), (10., 5.), (5., 2.), (0., 5.)]],
                    &[&[(0., 5.), (5., 8.), (10., 5.), (10., 10.), (0., 10.)]],
                ]),
                "disconnected_interior",
            ),
        ),
        (
            Polygon,
            Some(poly(&[SQUARE, LOOP[0], LOOP[1], LOOP[2]])),
            fixed(
                multi(&[&[SQUARE, &[(1., 6.), (9., 6.), (7., 3.), (5., 1.), (3., 3.)]], &[&[(3., 3.), (7., 3.), (5., 6.)]]]),
                "disconnected_interior",
            ),
        ),
        (
            Polygon,
            Some(multi(&[&[SQUARE], &[OVERLAPPING]])),
            fixed(poly(&[&[(0., 0.), (10., 0.), (10., 5.), (15., 5.), (15., 15.), (5., 15.), (5., 10.), (0., 10.)]]), "parts_overlap"),
        ),
        (
            Polygon,
            Some(multi(&[&[SQUARE], &[&[(10., 0.), (20., 0.), (20., 10.), (10., 10.), (10., 0.)]]])),
            fixed(poly(&[&[(0., 0.), (20., 0.), (20., 10.), (0., 10.)]]), "parts_overlap"),
        ),
        // What can't be repaired is written without geometry.
        (Polygon, None, gone("null_geometry")),
        (Polygon, Some(Geometry::Polygon(vec![])), gone("null_geometry")),
        (Polyline, Some(Geometry::Line(vec![vec![]])), gone("null_geometry")),
        (Point, Some(Geometry::MultiPoint(vec![])), gone("null_geometry")),
        (Point, Some(Geometry::Point(Coord::new(f64::NAN, 1.))), gone("non_finite_coordinate")),
        (Point, Some(Geometry::MultiPoint(pts(&[(0., 0.), (f64::INFINITY, 1.)]))), gone("non_finite_coordinate")),
        (Polygon, Some(poly(&[SQUARE, &[(2., 2.), (f64::NAN, 2.), (4., 4.), (2., 2.)]])), gone("non_finite_coordinate")),
        (Polyline, Some(line(&[&[(0., 0.), (f64::NEG_INFINITY, 1.)]])), gone("non_finite_coordinate")),
        (Polygon, Some(poly(&[&[(0., 0.), (1., 0.), (0., 0.)]])), gone("too_few_points")),
        (Polyline, Some(line(&[&[(1., 1.), (1., 1.)], &[(2., 2.)]])), gone("too_few_points")),
        (Polygon, Some(poly(&[&[(0., 0.), (1., 0.), (2., 0.), (0., 0.)]])), gone("no_area")),
        (Polygon, Some(poly(&[SQUARE, SQUARE])), gone("no_area")),
    ];
    for (ty, g, (want, code)) in cases {
        let (got, repairs) = one(ty, g.clone());
        assert_eq!(repairs, code, "{g:?}");
        assert_eq!(got.as_ref().map(norm), want.as_ref().map(norm), "{g:?}");
    }
}

/// The rebuilt rings wind counter-clockwise, holes clockwise; the other fixes keep the input's
/// winding.
#[test]
fn rebuilt_rings_wind_as_ogc_says() {
    for (name, g, _) in rebuilt() {
        let (Some(Geometry::Polygon(parts)), _) = one(GeometryType::Polygon, Some(g)) else { panic!("{name}") };
        for rings in &parts {
            assert!(signed(&rings[0]) > 0.0, "{name}");
            assert!(rings[1..].iter().all(|h| signed(h) < 0.0), "{name}");
        }
    }
    let cw = reversed(&poly(&[&[(0., 0.), (10., 0.), (10., 10.), (0., 10.)]]));
    assert_eq!(one(GeometryType::Polygon, Some(cw)).0, Some(poly(&[&[(0., 10.), (10., 10.), (10., 0.), (0., 0.), (0., 10.)]])));
}

/// GeoJSON winds exterior rings counter-clockwise and Shapefiles clockwise: the rebuild gives
/// the same geometry, coordinate for coordinate, whichever way the rings are wound, and when one
/// part or ring is wound against the rest.
#[test]
fn both_windings_give_the_same_result() {
    for (name, g, want_area) in rebuilt() {
        let (want, code) = one(GeometryType::Polygon, Some(g.clone()));
        let want = want.unwrap();
        assert!((area(&want) - want_area).abs() < 1e-9, "{name}: {}", area(&want));
        assert_eq!(one(GeometryType::Polygon, Some(reversed(&g))), (Some(want.clone()), code.clone()), "{name} reversed");
        // Each ring on its own reversed.
        let Geometry::Polygon(parts) = &g else { panic!() };
        for (p, rings) in parts.iter().enumerate() {
            for r in 0..rings.len() {
                let mut mixed = parts.clone();
                mixed[p][r].reverse();
                assert_eq!(
                    one(GeometryType::Polygon, Some(Geometry::Polygon(mixed))).0.as_ref(),
                    Some(&want),
                    "{name}: ring {r} of part {p} reversed"
                );
            }
        }
    }
}

#[test]
fn several_problems_in_one_feature() {
    // An unclosed bowtie with a repeated vertex, a hole outside it, and two overlapping parts.
    let g = multi(&[&[&[(0., 0.), (2., 2.), (2., 2.), (2., 0.), (0., 2.)], OUTSIDE], &[SQUARE], &[OVERLAPPING]]);
    let (got, repairs) = one(GeometryType::Polygon, Some(g));
    assert_eq!(repairs, "ring_not_closed, repeated_vertex, self_intersection, hole_outside_shell, parts_overlap");
    assert_eq!(area(got.as_ref().unwrap()), 175.0, "the bowtie inside the square, the hole gone");
    // A ring with too few points keeps the parts from being checked against each other; once it
    // is dropped, they are, and their overlap is repaired too.
    let g = multi(&[&[SQUARE, &[(2., 2.), (3., 3.), (2., 2.)]], &[OVERLAPPING]]);
    let (got, repairs) = one(GeometryType::Polygon, Some(g));
    assert_eq!(repairs, "too_few_points, parts_overlap");
    assert_eq!(
        got.map(|g| norm(&g)),
        Some(norm(&poly(&[&[(0., 0.), (10., 0.), (10., 5.), (15., 5.), (15., 15.), (5., 15.), (5., 10.), (0., 10.)]])))
    );
    // A part crossing itself isn't checked against the other parts or its holes: what the rebuild
    // finds is said all the same.
    let g =
        multi(&[&[SQUARE], &[&[(5., 5.), (15., 15.), (15., 5.), (5., 15.), (5., 5.)], &[(30., 30.), (31., 30.), (31., 31.), (30., 30.)]]]);
    let (got, repairs) = one(GeometryType::Polygon, Some(g));
    assert_eq!(repairs, "self_intersection, hole_outside_shell, parts_overlap");
    let got = got.unwrap();
    assert_eq!(area(&got), 137.5);
    let Geometry::Polygon(parts) = &got else { panic!() };
    assert_eq!(parts.len(), 2, "the square with the bowtie's left half, and its right half touching it at a corner");
    // Without a hole outside, none is said to be.
    let g = multi(&[&[SQUARE], &[&[(5., 5.), (15., 15.), (15., 5.), (5., 15.), (5., 5.)], &[(12., 8.), (13., 8.), (13., 9.), (12., 8.)]]]);
    assert_eq!(one(GeometryType::Polygon, Some(g)).1, "self_intersection, parts_overlap");
    // Holes of a part crossing itself that overlap are merged, and said to intersect.
    let bowtie: Ring = &[(0., 0.), (20., 20.), (20., 0.), (0., 20.), (0., 0.)];
    let a: Ring = &[(1., 8.), (4., 8.), (4., 12.), (1., 12.), (1., 8.)];
    let b: Ring = &[(2., 9.), (5., 9.), (5., 11.), (2., 11.), (2., 9.)];
    let (got, repairs) = one(GeometryType::Polygon, Some(poly(&[bowtie, a, b])));
    assert_eq!(repairs, "self_intersection, rings_intersect");
    assert_eq!(area(got.as_ref().unwrap()), 200.0 - 14.0, "the holes' union taken out");
    // Holes apart, or only touching at a point, aren't.
    let c: Ring = &[(1., 9.), (3., 9.), (3., 11.), (1., 11.), (1., 9.)];
    let d: Ring = &[(4., 9.), (6., 9.), (6., 11.), (4., 11.), (4., 9.)];
    let e: Ring = &[(3., 11.), (4., 11.), (4., 12.), (3., 12.), (3., 11.)];
    assert_eq!(one(GeometryType::Polygon, Some(poly(&[bowtie, c, d]))).1, "self_intersection");
    assert_eq!(one(GeometryType::Polygon, Some(poly(&[bowtie, c, e]))).1, "self_intersection");
    // Nor parts merged that only touch at a point.
    let g = multi(&[&[BOWTIE], &[&[(2., 2.), (4., 2.), (4., 4.), (2., 2.)]]]);
    assert_eq!(one(GeometryType::Polygon, Some(g)).1, "self_intersection");
}

/// Features with only vertex-level problems keep every other coordinate exactly, even those the
/// overlay would round or leave out (a vertex in the middle of an edge).
#[test]
fn vertex_fixes_move_no_coordinate() {
    let open: Ring = &[
        (500_000.123, 5_600_000.789),
        (500_005.5, 5_600_000.789),
        (500_010.321, 5_600_000.789),
        (500_010.321, 5_600_010.1),
        (500_000.123, 5_600_010.1),
    ];
    let mut closed = open.to_vec();
    closed.push(open[0]);
    let hole: Ring =
        &[(500_002.1, 5_600_002.2), (500_002.1, 5_600_002.2), (500_002.1, 5_600_004.4), (500_004.3, 5_600_004.4), (500_002.1, 5_600_002.2)];
    let (got, repairs) = one(GeometryType::Polygon, Some(poly(&[open, hole])));
    assert_eq!(repairs, "ring_not_closed, repeated_vertex");
    assert_eq!(got, Some(poly(&[&closed, &hole[1..]])));
}

/// A feature with nothing wrong is written exactly as stored, with an empty repairs value.
#[test]
fn valid_features_are_copied_exactly() {
    let odd: Ring = &[(0.1, 0.2), (5.000_000_000_1, 0.2), (10.3, 0.2), (10.3, 10.7), (0.1, 10.7), (0.1, 0.2)];
    let cases = [
        (GeometryType::Polygon, poly(&[odd])),
        (GeometryType::Polygon, reversed(&poly(&[SQUARE, HOLE]))),
        (
            GeometryType::Polygon,
            multi(&[
                &[SQUARE, HOLE],
                &[&[(2.5, 2.5), (3.5, 2.5), (3.5, 3.5), (2.5, 2.5)]],
                &[&[(20., 0.), (30., 0.), (30., 10.), (20., 0.)]],
            ]),
        ),
        (GeometryType::Polygon, poly(&[SQUARE, &[(0., 5.), (3., 4.), (3., 6.), (0., 5.)]])),
        (GeometryType::Polyline, line(&[BOWTIE, &[(0.1, 0.1), (0.1, 0.1 + 1e-12)]])),
        (GeometryType::Point, Geometry::Point(Coord::new(-0.0, 1e-300))),
        (GeometryType::Point, Geometry::MultiPoint(pts(&[(1., 2.), (1., 2.)]))),
    ];
    for (ty, g) in cases {
        let mut l = layer(ty);
        add(&mut l, Some(g.clone()), "ok");
        let r = run(&l);
        assert_eq!(r.messages, ["Repair Geometry: no repairs needed in 1 feature"], "{g:?}");
        let (got, repairs) = rows(&r).remove(0);
        assert_eq!(repairs, "");
        let bits = |g: &Geometry| coords(g).iter().map(|c| (c.x.to_bits(), c.y.to_bits())).collect::<Vec<_>>();
        assert_eq!(got.as_ref().map(bits), Some(bits(&g)), "{g:?}");
        assert_eq!(got, Some(g));
    }
}

#[test]
fn a_mixed_layer_says_what_was_done_with_counts() {
    let mut l = layer(GeometryType::Polygon);
    add(&mut l, Some(poly(&[SQUARE])), "ok");
    add(&mut l, Some(poly(&[BOWTIE])), "bowtie");
    add(&mut l, Some(poly(&[&[(0., 0.), (10., 0.), (10., 10.), (0., 10.)]])), "open");
    add(&mut l, None, "null");
    add(&mut l, Some(poly(&[SQUARE, OUTSIDE])), "hole outside");
    add(&mut l, Some(multi(&[&[SQUARE], &[OVERLAPPING]])), "overlapping");
    add(&mut l, Some(poly(&[SQUARE, ACROSS])), "split");
    add(&mut l, Some(poly(&[SQUARE, &[(2., 2.), (3., 3.), (2., 2.)]])), "degenerate hole");
    add(&mut l, Some(poly(&[&[(0., 0.), (1., 0.), (2., 0.), (0., 0.)]])), "flat");
    add(&mut l, Some(Geometry::Point(Coord::new(f64::NAN, 0.))), "nan");
    let r = run(&l);
    let ds = dataset(&r);
    valid(ds);
    let repairs: Vec<(String, String)> = (0..ds.features.len()).map(|i| (text(ds, i, "name"), text(ds, i, "repairs"))).collect();
    let row = |n: &str, c: &str| (n.to_string(), c.to_string());
    assert_eq!(
        repairs,
        [
            row("ok", ""),
            row("bowtie", "self_intersection"),
            row("open", "ring_not_closed"),
            row("null", "null_geometry"),
            row("hole outside", "hole_outside_shell"),
            row("overlapping", "parts_overlap"),
            row("split", "disconnected_interior"),
            row("degenerate hole", "too_few_points"),
            row("flat", "no_area"),
            row("nan", "non_finite_coordinate"),
        ]
    );
    let without: Vec<bool> = ds.features.iter().map(|f| f.geometry.is_none()).collect();
    assert_eq!(without, [false, false, false, true, false, false, false, false, true, true]);
    assert_eq!(
        r.messages,
        [
            "Repair Geometry: repaired 6 of 10 features (too_few_points: 1, ring_not_closed: 1, self_intersection: 1, hole_outside_shell: 1, disconnected_interior: 1, parts_overlap: 1)",
            "Repair Geometry: 3 features couldn't be repaired and are written without geometry (null_geometry: 1, non_finite_coordinate: 1, no_area: 1)",
            "Repair Geometry: 1 ring or part with too few distinct points or no area was dropped from 1 feature",
            "Repair Geometry: holes not inside their exterior ring were taken out of 1 feature; what of a hole is inside its exterior ring stays a hole",
            "Repair Geometry: the parts that overlapped or touched along a line were merged in 1 feature",
            "Repair Geometry: 1 feature whose interior was cut into pieces by rings touching at points is written with each piece as a part of its own",
            // The hole outside makes the largest extent, 21 across: a grid step of 2^-26, five of them.
            "Repair Geometry: 4 features were rebuilt by geo's boolean overlay, which rounds coordinates to a fine grid: they may have moved by up to 7.5e-8 degrees of longitude or latitude, and vertices in the middle of straight edges were left out",
        ]
    );
}

#[test]
fn plural_counts_of_pieces_and_holes() {
    let mut l = layer(GeometryType::Polygon);
    add(
        &mut l,
        Some(multi(&[&[SQUARE, OUTSIDE, &[(30., 30.), (31., 30.), (31., 31.), (30., 30.)]], &[&[(40., 0.), (41., 0.), (40., 0.)]]])),
        "a",
    );
    add(&mut l, Some(Geometry::Line(vec![pts(&[(0., 0.), (1., 1.)]), pts(&[(2., 2.)]), vec![]])), "b");
    add(
        &mut l,
        Some(multi(&[
            &[SQUARE],
            &[&[(10., 0.), (20., 0.), (20., 10.), (10., 10.), (10., 0.)]],
            &[&[(20., 0.), (30., 0.), (30., 10.), (20., 10.), (20., 0.)]],
        ])),
        "c",
    );
    let r = run(&l);
    valid(dataset(&r));
    assert_eq!(text(dataset(&r), 0, "repairs"), "too_few_points, hole_outside_shell");
    assert_eq!(area(dataset(&r).features[2].geometry.as_ref().unwrap()), 300.0);
    assert_eq!(
        r.messages[1..4],
        [
            "Repair Geometry: 3 rings or parts with too few distinct points or no area were dropped from 2 features",
            "Repair Geometry: holes not inside their exterior ring were taken out of 1 feature; what of a hole is inside its exterior ring stays a hole",
            "Repair Geometry: the parts that overlapped or touched along a line were merged in 1 feature",
        ]
    );
}

/// A closed ring around a rectangle `w` by `h` with its lower left corner at `(x, y)`.
fn rect(x: f64, y: f64, w: f64, h: f64) -> Vec<(f64, f64)> {
    vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h), (x, y)]
}
/// A closed ring around a strip `len` long and `w` wide at `deg` degrees from the x axis, with a
/// corner at `(x, y)`.
fn strip(x: f64, y: f64, len: f64, deg: f64, w: f64) -> Vec<(f64, f64)> {
    let (s, c) = deg.to_radians().sin_cos();
    vec![(x, y), (x + len * c, y + len * s), (x + len * c - w * s, y + len * s + w * c), (x - w * s, y + w * c), (x, y)]
}
/// A strip at 45°, 5 across and up from a corner placed 0.45 of a step of the grid of the parts'
/// union (2^-20, as below) right of (1000, 1000), and `w` steps wide: one step wide, the overlay
/// rounds all its points onto one diagonal line of its grid.
fn diagonal(w: f64) -> Vec<(f64, f64)> {
    let step = (-20f64).exp2();
    let (x, y, a) = (1000. + 0.45 * step, 1000., w * step / 2f64.sqrt());
    vec![(x, y), (x + 5., y + 5.), (x + 5. - a, y + 5. + a), (x - a, y + a), (x, y)]
}
/// A bowtie 20 across, whose left lobe holds the holes at x 1 to 4 and whose right lobe holds
/// those at x 18.
const BIG_BOWTIE: Ring = &[(0., 0.), (20., 20.), (20., 0.), (0., 20.), (0., 0.)];

/// A part far from the others, or a hole far from the other holes, narrower than a step of the
/// overlay's grid would be rounded to no area by the union or the difference, which would also
/// take its vanishing for a merge: it is dropped first, counted, and said with no_area.
#[test]
fn a_part_or_hole_narrower_than_the_overlays_grid_is_dropped_and_said() {
    // Parts 1000 apart: half the extent is about 500, so the union's grid step is 2^-20; the far
    // part is three quarters of a step across.
    let tiny = rect(1000., 1000., 0.75 * (-20f64).exp2(), 0.75 * (-20f64).exp2());
    let overlapping = multi(&[&[SQUARE], &[OVERLAPPING], &[&tiny]]);
    // Parts apart, rebuilt for the bowtie: without the narrow part dropped first, its vanishing
    // would have been taken for parts that overlap.
    let apart = multi(&[&[BOWTIE], &[&rect(5., 5., 2., 2.)], &[&tiny]]);
    // A hole three quarters of a step of the difference's grid across (2^-26: the bowtie is 20
    // across), far from the other hole: their union would have taken its vanishing for holes
    // that overlap.
    let tiny_hole = rect(18., 10., 0.75 * (-26f64).exp2(), 0.75 * (-26f64).exp2());
    let holes = poly(&[BIG_BOWTIE, &rect(1., 8., 3., 4.), &tiny_hole]);

    let (got, repairs) = one(GeometryType::Polygon, Some(overlapping.clone()));
    assert_eq!(repairs, "parts_overlap, no_area");
    let got = got.unwrap();
    let Geometry::Polygon(parts) = &got else { panic!() };
    assert_eq!(parts.len(), 1, "the squares merged, the narrow part dropped");
    assert_eq!(area(&got), 175.0);

    let mut l = layer(GeometryType::Polygon);
    add(&mut l, Some(overlapping), "overlapping");
    add(&mut l, Some(apart), "apart");
    add(&mut l, Some(holes), "holes");
    let r = run(&l);
    let ds = dataset(&r);
    valid(ds);
    let repairs: Vec<String> = (0..3).map(|i| text(ds, i, "repairs")).collect();
    assert_eq!(repairs, ["parts_overlap, no_area", "self_intersection, no_area", "self_intersection, no_area"]);
    let Some(Geometry::Polygon(parts)) = &ds.features[1].geometry else { panic!() };
    assert_eq!(parts.len(), 3, "the bowtie's two lobes and the square");
    assert_eq!(area(ds.features[2].geometry.as_ref().unwrap()), 200.0 - 12.0);
    assert_eq!(
        r.messages[..3],
        [
            "Repair Geometry: repaired 3 of 3 features (self_intersection: 2, parts_overlap: 1, no_area: 3)",
            "Repair Geometry: 3 holes or parts narrower than a step of the overlay's grid, which would leave them no area, were dropped from 3 features, with no_area in repairs",
            "Repair Geometry: the parts that overlapped or touched along a line were merged in 1 feature",
        ]
    );
    let said = "Repair Geometry: 1 hole or part narrower than a step of the overlay's grid, which would leave it no area, was dropped from 1 feature, with no_area in repairs";
    let mut l = layer(GeometryType::Polygon);
    add(&mut l, Some(multi(&[&[SQUARE], &[OVERLAPPING], &[&tiny]])), "one");
    assert_eq!(run(&l).messages[1], said);

    // Long slivers at an angle, many steps across and up, but three quarters of a step wide; and a
    // strip at 45° a step wide, which rounding can put on one line, as it moves a point by up to
    // half a step on each axis: across the diagonal, by up to √2 / 2 steps.
    let step = (-20f64).exp2();
    let slivers = [strip(1000., 1000., 10., 45., 0.75 * step), strip(1000., 1000., 10., 30., 0.75 * step), diagonal(1.0)];
    for sliver in &slivers {
        for (g, want, parts_out) in [
            (multi(&[&[SQUARE], &[OVERLAPPING], &[sliver]]), "parts_overlap, no_area", 1),
            (multi(&[&[BOWTIE], &[&rect(5., 5., 2., 2.)], &[sliver]]), "self_intersection, no_area", 3),
        ] {
            let mut l = layer(GeometryType::Polygon);
            add(&mut l, Some(g.clone()), "x");
            let r = run(&l);
            let ds = dataset(&r);
            valid(ds);
            assert_eq!(text(ds, 0, "repairs"), want, "{sliver:?}");
            let Some(Geometry::Polygon(parts)) = &ds.features[0].geometry else { panic!() };
            assert_eq!(parts.len(), parts_out, "{sliver:?}");
            assert!(r.messages.iter().any(|m| m == said), "{:?}", r.messages);
        }
    }
    // A hole at 30°, a unit long and three quarters of a step of the difference's grid wide.
    let hole = strip(17., 12., 1., 30., 0.75 * (-26f64).exp2());
    let mut l = layer(GeometryType::Polygon);
    add(&mut l, Some(poly(&[BIG_BOWTIE, &rect(1., 8., 3., 4.), &hole])), "x");
    let r = run(&l);
    let ds = dataset(&r);
    valid(ds);
    assert_eq!(text(ds, 0, "repairs"), "self_intersection, no_area");
    let Some(Geometry::Polygon(parts)) = &ds.features[0].geometry else { panic!() };
    assert_eq!(parts.iter().map(Vec::len).sum::<usize>(), 3, "the lobes and the hole left of them");
    assert_eq!(r.messages[1], said);
}

/// The same features without the narrow part or hole, or with one exactly a grid step wide, or at
/// an angle as wide as rounding can move a point across it (√2 steps at 45°, cos 30° + sin 30° at
/// 30°) or a little more, which the overlay keeps: nothing is dropped, and nothing said of it.
#[test]
fn parts_and_holes_a_grid_step_wide_are_kept_unsaid() {
    let step = (-20f64).exp2();
    let sliver = rect(1000., 1000., step, 10.);
    let hole_sliver = rect(18., 9., (-26f64).exp2(), 2.);
    let (at_45, at_30) = (1.45 * step, 1.4 * step);
    let hole_at_45 = strip(17., 12., 2., 45., 1.45 * (-26f64).exp2());
    // Rounded, a side of a strip at an angle moves by up to √2 / 2 steps across it, so its area
    // changes by up to √2 steps times its length (10 at most) on the union's grid, and its ends
    // move too: twice that is allowed.
    let (exact, drift) = (1e-7, 2.0 * 10.0 * std::f64::consts::SQRT_2 * step);
    let squares = |far: &[(f64, f64)]| multi(&[&[SQUARE], &[OVERLAPPING], &[far]]);
    let apart = |far: &[(f64, f64)]| multi(&[&[BOWTIE], &[&rect(5., 5., 2., 2.)], &[far]]);
    let holes = |far: &[(f64, f64)]| poly(&[BIG_BOWTIE, &rect(1., 8., 3., 4.), far]);
    for (g, want, parts_out, rings_out, want_area, tol) in [
        (multi(&[&[SQUARE], &[OVERLAPPING]]), "parts_overlap", 1, 1, 175.0, exact),
        (squares(&sliver), "parts_overlap", 2, 2, 175.0 + 10.0 * step, exact),
        (squares(&diagonal(1.45)), "parts_overlap", 2, 2, 175.0 + 50f64.sqrt() * at_45, drift),
        (squares(&strip(1000., 1000., 10., 45., at_45)), "parts_overlap", 2, 2, 175.0 + 10.0 * at_45, drift),
        (squares(&strip(1000., 1000., 10., 30., at_30)), "parts_overlap", 2, 2, 175.0 + 10.0 * at_30, drift),
        (multi(&[&[BOWTIE], &[&rect(5., 5., 2., 2.)]]), "self_intersection", 3, 3, 6.0, exact),
        (apart(&strip(1000., 1000., 10., 30., at_30)), "self_intersection", 4, 4, 6.0 + 10.0 * at_30, drift),
        (poly(&[BIG_BOWTIE, &rect(1., 8., 3., 4.)]), "self_intersection", 2, 3, 188.0, exact),
        (holes(&hole_sliver), "self_intersection", 2, 4, 188.0 - 2.0 * (-26f64).exp2(), exact),
        (holes(&hole_at_45), "self_intersection", 2, 4, 188.0 - 2.0 * 1.45 * (-26f64).exp2(), drift),
    ] {
        let mut l = layer(GeometryType::Polygon);
        add(&mut l, Some(g.clone()), "x");
        let r = run(&l);
        let ds = dataset(&r);
        valid(ds);
        assert_eq!(text(ds, 0, "repairs"), want, "{g:?}");
        let got = ds.features[0].geometry.as_ref().unwrap();
        let Geometry::Polygon(parts) = got else { panic!() };
        assert_eq!(parts.len(), parts_out, "{g:?}");
        assert_eq!(parts.iter().map(Vec::len).sum::<usize>(), rings_out, "{g:?}");
        assert!((area(got) - want_area).abs() < tol, "{g:?}: area {}", area(got));
        assert!(!r.messages.iter().any(|m| m.contains("no_area") || m.contains("narrower")), "{:?}", r.messages);
    }
}

/// A feature too busy to check, or whose overlay would be past the work bound, is written as it
/// was, with too_complex, never dropped.
#[test]
fn a_feature_too_complex_to_repair_is_written_unchanged() {
    // A zigzag ring whose 12,000 segments all overlap each other's x and y ranges: too busy to check.
    let n = 12_000;
    let mut zigzag: Vec<Coord> = (0..n).map(|i| Coord::new(f64::from(i % 2), f64::from(i) * 1e-3 + f64::from(i % 2) * 100.0)).collect();
    zigzag.push(Coord::new(-1., 50.));
    zigzag.push(Coord::new(0., 0.));
    // A star of 2001 points, each joined to the one 1000 on: about 2 million crossings, which the
    // checks find within the bound, but which the overlay would turn into 4 million vertices.
    let (k, m) = (2001u32, 1000u32);
    let star: Vec<Coord> = (0..=k)
        .map(|i| {
            let a = std::f64::consts::TAU * f64::from((i * m) % k) / f64::from(k);
            Coord::new(a.cos(), a.sin())
        })
        .collect();
    let mut l = layer(GeometryType::Polygon);
    add(&mut l, Some(Geometry::Polygon(vec![vec![zigzag]])), "busy");
    add(&mut l, Some(Geometry::Polygon(vec![vec![star]])), "star");
    add(&mut l, Some(poly(&[BOWTIE])), "bowtie");
    let started = std::time::Instant::now();
    let r = run(&l);
    assert!(started.elapsed().as_secs() < 60, "bounded");
    let ds = dataset(&r);
    for i in 0..2 {
        assert_eq!(text(ds, i, "repairs"), "too_complex");
        assert_eq!(ds.features[i].geometry, l.ds.features[i].geometry, "written as stored");
    }
    assert_eq!(text(ds, 2, "repairs"), "self_intersection");
    assert_eq!(
        r.messages,
        [
            "Repair Geometry: repaired 1 of 3 features (self_intersection: 1)",
            "Repair Geometry: 2 features were too complex to repair within the work limit and are written unchanged, with too_complex in repairs",
            "Repair Geometry: 1 feature was rebuilt by geo's boolean overlay, which rounds coordinates to a fine grid: they may have moved by up to 9.4e-9 degrees of longitude or latitude, and vertices in the middle of straight edges were left out",
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
    add(&mut l, Some(poly(&[BOWTIE])), "another unselected bowtie");
    l.selected = vec![3, 1, 2, 3, usize::MAX];
    let r = run(&l);
    let ds = dataset(&r);
    let names: Vec<String> = (0..ds.features.len()).map(|i| text(ds, i, "name")).collect();
    assert_eq!(names, ["selected square", "selected hole outside", "selected null"]);
    assert_eq!(
        r.messages[..2],
        [
            "Repair Geometry: repaired 1 of 3 features (hole_outside_shell: 1)",
            "Repair Geometry: 1 feature couldn't be repaired and is written without geometry (null_geometry: 1)"
        ]
    );
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
    // A long row: its extra value is left out, and said so.
    l.ds.features.push(Feature {
        id: 7,
        geometry: None,
        attrs: vec![Value::Text("none".into()), Value::Null, Value::Int(3), Value::Int(9)],
    });
    let named = |output: Option<&str>| {
        let mut p = json!({"input": "input"});
        if let Some(o) = output {
            p["output"] = json!(o);
        }
        rustgis_analysis::run("repairGeometry", &l, &p).unwrap()
    };
    let r = named(None);
    let ds = dataset(&r);
    assert_eq!(ds.name, "Parcels_repaired");
    assert_eq!(ds.geometry_type, GeometryType::Polygon);
    assert_eq!(ds.crs, l.ds.crs);
    assert!(ds.crs_incomplete && ds.zm_dropped);
    assert_eq!(ds.fields.len(), 4);
    assert_eq!(ds.fields[..3], l.ds.fields[..]);
    assert_eq!((ds.fields[3].name.as_str(), ds.fields[3].ty), ("repairs", FieldType::Text));
    let ids: Vec<u64> = ds.features.iter().map(|f| f.id).collect();
    assert_eq!(ids, [41, 42, 7], "the input's ids lead back to its features");
    assert_eq!(ds.features[0].attrs, [Value::Text("ok".into()), Value::Float(1.5), Value::Int(1), Value::Text(String::new())]);
    assert_eq!(ds.features[1].attrs, [Value::Text("bad".into()), Value::Float(2.5), Value::Null, Value::Text("self_intersection".into())]);
    assert_eq!(ds.features[2].attrs, [Value::Text("none".into()), Value::Null, Value::Int(3), Value::Text("null_geometry".into())]);
    assert_eq!(
        r.messages.last().map(String::as_str),
        Some("Repair Geometry: 1 feature had more attribute values than the layer has fields; the extra values aren't in the output")
    );
    assert_eq!(dataset(&named(Some(" Fixed parcels "))).name, "Fixed parcels");
}

#[test]
fn an_existing_repairs_field_gets_another_name() {
    let mut l = Layer {
        ds: Dataset::new(
            "input",
            GeometryType::Polygon,
            vec![Field::new("Repairs", FieldType::Integer), Field::new("repairs_2", FieldType::Text)],
        ),
        selected: vec![],
    };
    l.ds.push_next(Some(poly(&[BOWTIE])), vec![Value::Int(7), Value::Text("mine".into())]);
    let r = run(&l);
    let ds = dataset(&r);
    assert_eq!(ds.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["Repairs", "repairs_2", "repairs_3"]);
    assert_eq!(ds.features[0].attrs[..2], l.ds.features[0].attrs[..]);
    assert_eq!(text(ds, 0, "repairs_3"), "self_intersection");
    assert_eq!(r.messages[0], "Repair Geometry: repaired 1 of 1 feature (self_intersection: 1)");
    assert_eq!(
        r.messages.last().map(String::as_str),
        Some("Repair Geometry: 'input' already has a field named repairs, so the output's repairs field is repairs_3")
    );
}

#[test]
fn empty_layers_tables_and_unknown_layers() {
    let r = run(&layer(GeometryType::Polygon));
    assert_eq!(r.messages, ["Repair Geometry: no features to repair"]);
    assert!(dataset(&r).features.is_empty());
    let table = Layer { ds: Dataset::new("input", GeometryType::None, vec![]), selected: vec![] };
    let e = rustgis_analysis::run("repairGeometry", &table, &json!({"input": "input"})).unwrap_err();
    assert_eq!(e, "Repair Geometry needs a layer with geometry: 'input' is a table");
    assert_eq!(rustgis_analysis::run("repairGeometry", &table, &json!({"input": "nope"})).unwrap_err(), "no layer named 'nope'");
}

/// The distance from `c` to the segment `a`–`b`.
fn to_segment(c: Coord, a: Coord, b: Coord) -> f64 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len = dx * dx + dy * dy;
    let t = if len == 0.0 { 0.0 } else { (((c.x - a.x) * dx + (c.y - a.y) * dy) / len).clamp(0.0, 1.0) };
    (c.x - (a.x + t * dx)).hypot(c.y - (a.y + t * dy))
}
/// How far the farthest vertex of `out` is from the rings of `input`.
fn farthest(out: &Geometry, input: &Geometry) -> f64 {
    let Geometry::Polygon(parts) = input else { panic!() };
    let segs: Vec<(Coord, Coord)> =
        parts.iter().flatten().flat_map(|r| r.windows(2).map(|w| (w[0], w[1])).chain(std::iter::once((r[r.len() - 1], r[0])))).collect();
    coords(out).into_iter().map(|c| segs.iter().map(|&(a, b)| to_segment(c, a, b)).fold(f64::INFINITY, f64::min)).fold(0.0, f64::max)
}

/// The overlay rounds coordinates to a grid of 2^29 steps across half the feature's extent: what
/// it writes stays within a step of the input's rings, well inside the bound the notice gives.
#[test]
fn the_overlay_moves_coordinates_by_less_than_a_grid_step() {
    let shift = |g: &Geometry, dx: f64, dy: f64, k: f64| {
        let Geometry::Polygon(parts) = g else { panic!() };
        Geometry::Polygon(
            parts
                .iter()
                .map(|rings| rings.iter().map(|r| r.iter().map(|c| Coord::new(dx + c.x * k, dy + c.y * k)).collect()).collect())
                .collect(),
        )
    };
    let mut worst_steps = 0f64;
    for (dx, dy, k) in
        [(500_000.123_456, 5_600_000.987_654, 1000.37), (-179.123_456_789, -89.987_654_321, 0.017_3), (0.1, 0.7, 1.000_000_3)]
    {
        for (name, g, want_area) in rebuilt() {
            let g = shift(&g, dx, dy, k);
            let mut l = layer(GeometryType::Polygon);
            add(&mut l, Some(g.clone()), name);
            let r = run(&l);
            valid(dataset(&r));
            let out = dataset(&r).features[0].geometry.clone().unwrap();
            let Geometry::Polygon(parts) = &g else { panic!() };
            let (mut lo, mut hi) = ((f64::INFINITY, f64::INFINITY), (f64::NEG_INFINITY, f64::NEG_INFINITY));
            for c in parts.iter().flatten().flatten() {
                (lo, hi) = ((lo.0.min(c.x), lo.1.min(c.y)), (hi.0.max(c.x), hi.1.max(c.y)));
            }
            let step = (((hi.0 - lo.0).max(hi.1 - lo.1) / 2.0).log2().round() - 29.0).exp2();
            let moved = farthest(&out, &g);
            worst_steps = worst_steps.max(moved / step);
            assert!(moved <= step, "{name} at {dx}: moved {moved}, a step is {step}");
            let bound: f64 = r.messages.last().unwrap().split("up to ").nth(1).unwrap().split(' ').next().unwrap().parse().unwrap();
            assert!(moved <= bound && bound >= 5.0 * step, "{name}: {moved} past {bound}");
            let a = area(&out);
            assert!((a - want_area * k * k).abs() <= 1e-6 * want_area * k * k, "{name}: area {a}");
        }
    }
    println!("the farthest a coordinate moved: {worst_steps:.3} grid steps");
}

/// GeoJSON keeps rings as written, wound either way, and the repairs see what the file holds.
#[test]
fn geojson_read_by_the_real_reader() {
    let geojson = r#"{"type":"FeatureCollection","features":[
        {"type":"Feature","properties":{"name":"bowtie"},"geometry":{"type":"Polygon","coordinates":[[[0,0],[2,2],[2,0],[0,2],[0,0]]]}},
        {"type":"Feature","properties":{"name":"open"},"geometry":{"type":"Polygon","coordinates":[[[0,0],[4,0],[4,4],[0,4]]]}},
        {"type":"Feature","properties":{"name":"hole outside"},"geometry":{"type":"Polygon","coordinates":[[[0,0],[4,0],[4,4],[0,4],[0,0]],[[10,10],[11,10],[11,11],[10,10]]]}},
        {"type":"Feature","properties":{"name":"empty"},"geometry":{"type":"Polygon","coordinates":[]}},
        {"type":"Feature","properties":{"name":"split"},"geometry":{"type":"Polygon","coordinates":[[[0,0],[10,0],[10,10],[0,10],[0,0]],[[0,0],[6,4],[10,10],[4,6],[0,0]]]}},
        {"type":"Feature","properties":{"name":"overlapping, wound clockwise"},"geometry":{"type":"MultiPolygon","coordinates":[[[[0,0],[0,10],[10,10],[10,0],[0,0]]],[[[5,5],[15,5],[15,15],[5,15],[5,5]]]]}},
        {"type":"Feature","properties":{"name":"null"},"geometry":null},
        {"type":"Feature","properties":{"name":"fine"},"geometry":{"type":"Polygon","coordinates":[[[0,0],[4,0],[4,4],[0,4],[0,0]]]}}
    ]}"#;
    let (ds, report) = rustgis_data::io::read_geojson(geojson, "input").unwrap();
    assert!(report.messages.is_empty() && report.skipped == 0);
    let l = Layer { ds, selected: vec![] };
    let r = run(&l);
    let out = dataset(&r);
    valid(out);
    let got: Vec<(String, String, Option<f64>)> = (0..out.features.len())
        .map(|i| (text(out, i, "name"), text(out, i, "repairs"), out.features[i].geometry.as_ref().map(area)))
        .collect();
    let row = |n: &str, c: &str, a: Option<f64>| (n.to_string(), c.to_string(), a);
    assert_eq!(
        got,
        [
            row("bowtie", "self_intersection", Some(2.0)),
            row("open", "ring_not_closed", Some(16.0)),
            row("hole outside", "hole_outside_shell", Some(16.0)),
            row("empty", "null_geometry", None),
            row("split", "disconnected_interior", Some(80.0)),
            row("overlapping, wound clockwise", "parts_overlap", Some(175.0)),
            row("null", "null_geometry", None),
            row("fine", "", Some(16.0)),
        ]
    );
    let Some(Geometry::Polygon(split)) = &out.features[4].geometry else { panic!() };
    assert_eq!(split.len(), 2, "the split interior as two parts");
    assert_eq!(out.features[7].geometry, l.ds.features[7].geometry);
}

/// The Shapefile writer winds exterior rings clockwise, against GeoJSON: read back, the rebuilt
/// features come out the same, coordinate for coordinate.
#[test]
fn a_shapefile_round_trip_gives_the_same_repairs() {
    let mut l = layer(GeometryType::Polygon);
    for (name, g, _) in rebuilt() {
        add(&mut l, Some(g), name);
    }
    let direct = run(&l);
    let (parts, left_out) = rustgis_data::io::shapefile_parts(&l.ds).unwrap();
    assert_eq!(left_out, 0);
    let dir = std::env::temp_dir().join(format!("rustgis-repair-geometry-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (ext, bytes) in &parts {
        std::fs::write(dir.join(format!("input.{ext}")), bytes).unwrap();
    }
    let read = rustgis_data::io::read_shapefile_in(&dir.join("input.shp"), "input", None);
    let _ = std::fs::remove_dir_all(&dir);
    let (ds, _) = read.unwrap();
    let shp = Layer { ds, selected: vec![] };
    let via_shapefile = run(&shp);
    let a = dataset(&direct);
    let b = dataset(&via_shapefile);
    assert_eq!(a.features.len(), b.features.len());
    for i in 0..a.features.len() {
        let name = text(a, i, "name");
        // The Shapefile reader assigns holes to the shell holding them: what it keeps of a hole
        // outside its shell may change what the checks find, never what the rebuild makes.
        assert_eq!(b.features[i].geometry.as_ref().map(norm), a.features[i].geometry.as_ref().map(norm), "{name}");
        assert_eq!(b.features[i].geometry, a.features[i].geometry, "{name}");
    }
}
