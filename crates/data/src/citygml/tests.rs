use super::*;
use crate::{Geometry, GeometryType, Value};

/// A CityGML 2.0 city model in `srs`, its namespaces declared on the root only, holding `members`.
fn model(srs: &str, members: &str) -> Vec<u8> {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<core:CityModel xmlns:core="http://www.opengis.net/citygml/2.0" xmlns:bldg="http://www.opengis.net/citygml/building/2.0"
  xmlns:gen="http://www.opengis.net/citygml/generics/2.0" xmlns:dem="http://www.opengis.net/citygml/relief/2.0"
  xmlns:frn="http://www.opengis.net/citygml/cityfurniture/2.0" xmlns:app="http://www.opengis.net/citygml/appearance/2.0"
  xmlns:grp="http://www.opengis.net/citygml/cityobjectgroup/2.0"
  xmlns:gml="http://www.opengis.net/gml" xmlns:xlink="http://www.w3.org/1999/xlink">
  <gml:boundedBy><gml:Envelope srsName="{srs}" srsDimension="3">
    <gml:lowerCorner>0 0 0</gml:lowerCorner><gml:upperCorner>10 10 5</gml:upperCorner>
  </gml:Envelope></gml:boundedBy>
  {members}
</core:CityModel>"#
    )
    .into_bytes()
}

/// A model in longitude/latitude, longitude first.
fn lonlat(members: &str) -> Vec<u8> {
    model("EPSG:4326", members)
}

/// A square ring's positions: `side` wide from (`x`, `y`), at height `z`, closed.
fn square(x: f64, y: f64, side: f64, z: f64) -> String {
    let (x1, y1) = (x + side, y + side);
    format!("{x} {y} {z} {x1} {y} {z} {x1} {y1} {z} {x} {y1} {z} {x} {y} {z}")
}

fn polygon(id: &str, positions: &str) -> String {
    format!(
        r#"<gml:Polygon gml:id="{id}"><gml:exterior><gml:LinearRing><gml:posList>{positions}</gml:posList></gml:LinearRing></gml:exterior></gml:Polygon>"#
    )
}

/// A thematic surface of `kind` holding polygons at LoD 2.
fn surface(kind: &str, polygons: &[String]) -> String {
    let members: String = polygons.iter().map(|p| format!("<gml:surfaceMember>{p}</gml:surfaceMember>")).collect();
    format!(
        "<bldg:boundedBy><bldg:{kind}><bldg:lod2MultiSurface><gml:MultiSurface>{members}</gml:MultiSurface></bldg:lod2MultiSurface></bldg:{kind}></bldg:boundedBy>"
    )
}

/// A 10-wide box's ground, wall and roof, 5 high, with ids under `id`.
fn box_surfaces(id: &str) -> String {
    let wall = "0 0 0 10 0 0 10 0 5 0 0 5 0 0 0";
    [
        surface("GroundSurface", &[polygon(&format!("{id}-g"), &square(0.0, 0.0, 10.0, 0.0))]),
        surface("WallSurface", &[polygon(&format!("{id}-w"), wall)]),
        surface("RoofSurface", &[polygon(&format!("{id}-r"), &square(0.0, 0.0, 10.0, 5.0))]),
    ]
    .concat()
}

fn building(id: &str, inner: &str) -> String {
    format!(r#"<core:cityObjectMember><bldg:Building gml:id="{id}">{inner}</bldg:Building></core:cityObjectMember>"#)
}

fn messages_of(out: &[(Dataset, ReadReport)]) -> Vec<String> {
    out.iter().flat_map(|(_, r)| r.messages.clone()).collect()
}

fn says(out: &[(Dataset, ReadReport)], text: &str) -> bool {
    messages_of(out).iter().any(|m| m.contains(text))
}

fn value(ds: &Dataset, row: usize, field: &str) -> Value {
    let at = ds.field_index(field).unwrap_or_else(|| panic!("no field {field} in {:?}", ds.fields));
    ds.features[row].attrs[at].clone()
}

fn area(g: Option<&Geometry>) -> f64 {
    let Some(Geometry::Polygon(polys)) = g else { return 0.0 };
    // A polygon's holes are taken off, so a test where holes are dropped no longer passes.
    polys
        .iter()
        .map(|p| match p.split_first() {
            Some((outer, holes)) => {
                crate::geometry::ring_area(outer).abs() - holes.iter().map(|h| crate::geometry::ring_area(h).abs()).sum::<f64>()
            }
            None => 0.0,
        })
        .sum()
}

fn layer<'a>(out: &'a [(Dataset, ReadReport)], name: &str) -> &'a Dataset {
    &out.iter().find(|(ds, _)| ds.name == name).unwrap_or_else(|| panic!("no layer {name}")).0
}

#[test]
fn it_is_known_by_its_root_and_namespace() {
    assert!(looks_like_citygml(&lonlat("")));
    assert!(!looks_like_citygml(br#"<wfs:FeatureCollection xmlns:gml="http://www.opengis.net/gml"><gml:featureMember/>"#));
    assert!(looks_like_citygml(&model3("EPSG:4326", "")), "3.0's too");
    let err = read("next.gml", br#"<core:CityModel xmlns:core="http://www.opengis.net/citygml/4.0"></core:CityModel>"#, None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("not a CityGML 1.0, 2.0 or 3.0 city model"), "{err}");
    let err = read("a.gml", br#"<Other xmlns="http://www.opengis.net/citygml/2.0"/>"#, None).unwrap_err().to_string();
    assert!(err.contains("its root isn't CityModel"), "{err}");
    let err = read("a.gml", &lonlat(""), None).unwrap_err().to_string();
    assert!(err.contains("no city objects"), "{err}");
    assert!(read("a.gml", b"<core:CityModel xmlns:core=\"http://www.opengis.net/citygml/2.0\"><a>", None).is_err(), "unclosed");
}

/// A building's thematic surfaces are its faces, and its ground its footprint: one row, with its
/// heights, and the only notice the one every 3D model gives.
#[test]
fn a_building_is_its_ground_with_its_heights() {
    let out = read("model.gml", &lonlat(&building("b1", &box_surfaces("b1"))), None).unwrap();
    assert_eq!(out.len(), 1);
    let (ds, _) = &out[0];
    assert_eq!(ds.name, "model");
    assert_eq!(ds.geometry_type, GeometryType::Polygon);
    assert_eq!(value(ds, 0, "id"), Value::Text("b1".into()));
    assert_eq!(value(ds, 0, "type"), Value::Text("Building".into()));
    assert_eq!(value(ds, 0, "lod"), Value::Text("2".into()));
    assert_eq!(value(ds, 0, "z_min"), Value::Float(0.0));
    assert_eq!(value(ds, 0, "z_max"), Value::Float(5.0));
    assert_eq!(value(ds, 0, "height"), Value::Float(5.0));
    assert!((area(ds.features[0].geometry.as_ref()) - 100.0).abs() < 1e-6, "{:?}", ds.features[0].geometry);
    let said = messages_of(&out);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].starts_with("1 3D city objects are drawn as their footprints"), "{said:?}");
}

/// The ground goes before the union of all faces: a roof wider than the ground doesn't widen it.
#[test]
fn the_ground_goes_before_a_wider_roof() {
    let inner = [
        surface("GroundSurface", &[polygon("g", &square(2.0, 2.0, 4.0, 0.0))]),
        surface("RoofSurface", &[polygon("r", &square(0.0, 0.0, 8.0, 5.0))]),
    ]
    .concat();
    let out = read("model.gml", &lonlat(&building("b1", &inner)), None).unwrap();
    assert!((area(out[0].0.features[0].geometry.as_ref()) - 16.0).abs() < 1e-6);
}

#[test]
fn attributes_are_typed_by_the_standard_and_their_element() {
    let inner = format!(
        r#"<gml:name>Town hall</gml:name>
        <bldg:function>1000</bldg:function><bldg:function>2000</bldg:function>
        <bldg:measuredHeight uom="m">12.5</bldg:measuredHeight>
        <bldg:storeysAboveGround>3</bldg:storeysAboveGround>
        <gen:intAttribute name="rooms"><gen:value>12</gen:value></gen:intAttribute>
        <gen:doubleAttribute name="area"><gen:value>10.5</gen:value></gen:doubleAttribute>
        <gen:stringAttribute name="code"><gen:value>007</gen:value></gen:stringAttribute>
        <core:externalReference><core:informationSystem>cadastre</core:informationSystem><core:externalObject><core:name>A-1</core:name></core:externalObject></core:externalReference>
        {}"#,
        box_surfaces("b1")
    );
    let out = read("model.gml", &lonlat(&building("b1", &inner)), None).unwrap();
    let ds = &out[0].0;
    assert_eq!(value(ds, 0, "name"), Value::Text("Town hall".into()));
    assert_eq!(value(ds, 0, "function"), Value::Text(r#"["1000","2000"]"#.into()), "given twice, a list");
    assert_eq!(value(ds, 0, "measuredHeight"), Value::Float(12.5));
    assert_eq!(value(ds, 0, "storeysAboveGround"), Value::Int(3));
    assert_eq!(value(ds, 0, "rooms"), Value::Int(12));
    assert_eq!(value(ds, 0, "area"), Value::Float(10.5));
    assert_eq!(value(ds, 0, "code"), Value::Text("007".into()), "text keeps its zeros");
    let Value::Text(reference) = value(ds, 0, "externalReference") else { panic!("not text") };
    assert!(reference.contains("cadastre") && reference.contains("A-1"), "{reference}");
    // A length in another unit keeps it, as text.
    let feet = format!(r#"<bldg:measuredHeight uom="ft">40</bldg:measuredHeight>{}"#, box_surfaces("b2"));
    let out = read("model.gml", &lonlat(&building("b2", &feet)), None).unwrap();
    assert_eq!(value(&out[0].0, 0, "measuredHeight"), Value::Text("40 ft".into()));
}

/// URN and URL names of a geographic system are latitude first; `EPSG:4326` isn't.
#[test]
fn latitude_comes_first_where_the_system_says_so() {
    // Latitude 10 to 12, longitude 0 to 4.
    let ground = "10 0 0 10 4 0 12 4 0 12 0 0 10 0 0";
    let inner = surface("GroundSurface", &[polygon("g", ground)]);
    for (srs, x, y) in [
        ("urn:ogc:def:crs:EPSG::4326", (0.0, 4.0), (10.0, 12.0)),
        ("http://www.opengis.net/def/crs/EPSG/0/4979", (0.0, 4.0), (10.0, 12.0)),
        ("EPSG:4326", (10.0, 12.0), (0.0, 4.0)),
        // CRS84 is longitude first, though its URN names a geographic system; a compound system
        // is as its horizontal part.
        ("urn:ogc:def:crs:OGC:1.3:CRS84", (10.0, 12.0), (0.0, 4.0)),
        ("urn:ogc:def:crs,crs:OGC:1.3:CRS84,crs:EPSG::5783", (10.0, 12.0), (0.0, 4.0)),
        ("urn:ogc:def:crs,crs:EPSG::4326,crs:EPSG::5783", (0.0, 4.0), (10.0, 12.0)),
    ] {
        let out = read("model.gml", &model(srs, &building("b1", &inner)), None).unwrap();
        let e = out[0].0.extent();
        assert_eq!((e.min_x, e.max_x, e.min_y, e.max_y), (x.0, x.1, y.0, y.1), "{srs}");
    }
}

/// A compound system is read by its horizontal part, whichever way another `srsName` writes it.
#[test]
fn a_compound_system_is_read_by_its_horizontal_part() {
    let ground = square(500_000.0, 5_500_000.0, 10.0, 0.0);
    let inner = format!(
        r#"<bldg:boundedBy><bldg:GroundSurface><bldg:lod2MultiSurface><gml:MultiSurface srsName="EPSG:25832"><gml:surfaceMember>{}</gml:surfaceMember></gml:MultiSurface></bldg:lod2MultiSurface></bldg:GroundSurface></bldg:boundedBy>"#,
        polygon("g", &ground)
    );
    let bytes = model("urn:ogc:def:crs,crs:EPSG::25832,crs:EPSG::5783", &building("b1", &inner));
    let out = read("model.gml", &bytes, None).unwrap();
    let ds = &out[0].0;
    assert_eq!(ds.crs, Some(Crs::Epsg(25832)));
    let e = ds.lonlat_extent();
    assert!((e.min_x - 9.0).abs() < 0.01 && (e.min_y - 49.6).abs() < 0.1, "{e:?}");
    // One naming another system is refused.
    let other = inner.replace("EPSG:25832", "EPSG:25833");
    let err = read("model.gml", &model("EPSG:25832", &building("b1", &other)), None).unwrap_err().to_string();
    assert!(err.contains("different coordinate systems (EPSG:25832 and EPSG:25833)"), "{err}");
}

/// A compound system's vertical part is said when its heights aren't metres upward: another unit, a
/// depth, or a system RustGIS doesn't know; each one the file names once, the system given or not.
#[test]
fn a_vertical_system_that_isnt_metres_upward_is_said() {
    let ground = square(500_000.0, 5_500_000.0, 10.0, 0.0);
    let inner = |srs: &[&str]| {
        srs.iter()
            .map(|srs| {
                format!(
                    r#"<bldg:boundedBy><bldg:GroundSurface><bldg:lod2MultiSurface><gml:MultiSurface srsName="{srs}"><gml:surfaceMember>{}</gml:surfaceMember></gml:MultiSurface></bldg:lod2MultiSurface></bldg:GroundSurface></bldg:boundedBy>"#,
                    polygon("g", &ground)
                )
            })
            .collect::<String>()
    };
    let compound = |h: &str, v: &str| format!("urn:ogc:def:crs,crs:EPSG::{h},crs:EPSG::{v}");
    let notes = |root: &str, geometries: &[&str], given: Option<Crs>| {
        messages_of(&read("model.gml", &model(root, &building("b1", &inner(geometries))), given).unwrap())
            .into_iter()
            .filter(|m| m.contains("vertical system") || m.contains("measures in"))
            .collect::<Vec<_>>()
    };
    // Metres upward, or no vertical part: nothing to say.
    assert!(notes(&compound("25832", "5783"), &["EPSG:25832"], None).is_empty());
    assert!(notes(&compound("25832", "5799"), &["EPSG:25832"], None).is_empty());
    assert!(notes("EPSG:25832", &["EPSG:25832"], None).is_empty());
    assert_eq!(
        notes(&compound("25832", "6360"), &["EPSG:25832"], None),
        [
            "a vertical system the file names (crs:EPSG::6360) measures heights in US survey feet: z_min, z_max, height and base_height are kept as the file has them, and Style › Extrusion reads metres, so objects may stand at the wrong height"
        ]
    );
    let depth = notes(&compound("25832", "5715"), &["EPSG:25832"], None);
    assert!(
        depth.len() == 1
            && depth[0].contains("(crs:EPSG::5715) measures depth, downward, in metres")
            && depth[0].contains("base_height is how far an object's top is below its topmost object's top"),
        "{depth:?}"
    );
    let unknown = notes(&compound("25832", "9999"), &["EPSG:25832"], None);
    assert!(unknown.len() == 1 && unknown[0].contains("(crs:EPSG::9999) is one RustGIS doesn't know"), "{unknown:?}");
    // A geometry whose vertical part differs from the root's is said too; one system is said once,
    // however it is spelled.
    let both = notes(&compound("25832", "5783"), &[&compound("25832", "8228")], None);
    assert!(both.len() == 1 && both[0].contains("(crs:EPSG::8228) measures heights in feet"), "{both:?}");
    let spelled = "URN:OGC:DEF:CRS,crs:EPSG:6.18:25832, crs:EPSG:6.18:6360";
    assert_eq!(notes(&compound("25832", "6360"), &[spelled], None).len(), 1);
    // The system given replaces the horizontal part only: the heights are still the file's, and a
    // geometry naming another system is read in the one given, its vertical part said.
    assert_eq!(notes(&compound("25832", "6360"), &["EPSG:25832"], Some(Crs::Epsg(25832))).len(), 1);
    let given = notes(&compound("25832", "5783"), &[&compound("25833", "8228")], Some(Crs::Epsg(25832)));
    assert!(given.len() == 1 && given[0].contains("(crs:EPSG::8228)"), "{given:?}");
    // A horizontal system in feet says so only when no vertical system says what the heights are.
    let feet = notes("EPSG:2263", &["EPSG:2263"], None);
    assert!(feet.len() == 1 && feet[0].contains("measures in us-ft"), "{feet:?}");
    assert!(notes(&compound("2263", "5703"), &["EPSG:2263"], None).is_empty());
    // A geometry naming a vertical part gives the heights' unit, whatever the first name says.
    assert!(notes("EPSG:2263", &[&compound("2263", "5703")], None).is_empty());
    let one = notes(&compound("2263", "6360"), &["EPSG:2263"], None);
    assert!(one.len() == 1 && one[0].contains("(crs:EPSG::6360)"), "{one:?}");
    // Past the systems said one by one, the rest are counted.
    let many: Vec<String> = (0..VERTICALS + 2).map(|k| compound("25832", &(9000 + k).to_string())).collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    let said = notes("EPSG:25832", &many, Some(Crs::Epsg(25832)));
    assert_eq!(said.len(), VERTICALS + 1, "{said:?}");
    assert!(said[VERTICALS].starts_with("2 geometries or envelopes name a vertical system, past the first 8 said"), "{said:?}");
}

/// A part starts at its lowest point's height above its building's lowest point, written in it or
/// linked to it; a group's member starts at 0, whatever the group's other members.
#[test]
fn a_part_above_its_building_starts_there() {
    let raised = |id: &str, z: f64| {
        [
            surface("GroundSurface", &[polygon(&format!("{id}-g"), &square(0.0, 0.0, 4.0, z))]),
            surface("RoofSurface", &[polygon(&format!("{id}-r"), &square(0.0, 0.0, 4.0, z + 1.0))]),
        ]
        .concat()
    };
    let inner = format!(
        r##"<bldg:consistsOfBuildingPart><bldg:BuildingPart gml:id="p1">{}</bldg:BuildingPart></bldg:consistsOfBuildingPart>
        <bldg:consistsOfBuildingPart xlink:href="#p2"/>"##,
        box_surfaces("p1")
    );
    let linked = format!(
        r#"<core:cityObjectMember><bldg:BuildingPart gml:id="p2">{}</bldg:BuildingPart></core:cityObjectMember>"#,
        raised("p2", 7.0)
    );
    let far = building("c1", &raised("c1", 40.0));
    let group = r##"<core:cityObjectMember><grp:CityObjectGroup gml:id="g1">
        <grp:groupMember xlink:href="#b1"/><grp:groupMember xlink:href="#c1"/></grp:CityObjectGroup></core:cityObjectMember>"##;
    let out = read("model.gml", &lonlat(&[building("b1", &inner), linked, far, group.to_string()].concat()), None).unwrap();
    let base = |id: &str| {
        out.iter()
            .find_map(|(ds, _)| {
                (0..ds.features.len()).find(|&r| value(ds, r, "id") == Value::Text(id.into())).map(|r| value(ds, r, "base_height"))
            })
            .unwrap()
    };
    assert_eq!(base("p1"), Value::Float(0.0));
    assert_eq!(base("p2"), Value::Float(7.0), "a linked part starts 7 m above its building's lowest point");
    assert_eq!(base("b1"), Value::Float(0.0));
    assert_eq!(base("c1"), Value::Float(0.0), "not 40 m above the group's lowest member");
}

/// A part is an object of its own, with its parent's id, and lends its parent its heights.
#[test]
fn a_building_part_names_its_parent_and_raises_it() {
    let inner = format!(
        r#"<gml:name>Hall</gml:name><bldg:consistsOfBuildingPart><bldg:BuildingPart gml:id="p1">{}</bldg:BuildingPart></bldg:consistsOfBuildingPart>"#,
        box_surfaces("p1")
    );
    let out = read("model.gml", &lonlat(&building("b1", &inner)), None).unwrap();
    assert_eq!(out[0].0.name, "model BuildingPart", "the layer that draws goes first");
    let part = layer(&out, "model BuildingPart");
    assert_eq!(value(part, 0, "parent"), Value::Text("b1".into()));
    let hall = layer(&out, "model Building");
    assert_eq!(hall.geometry_type, GeometryType::None);
    assert_eq!(value(hall, 0, "name"), Value::Text("Hall".into()));
    assert_eq!(value(hall, 0, "z_max"), Value::Float(5.0), "raised by its part");
    assert!(says(&out, "1 city objects have no geometry of their own"));
    assert!(!says(&out, "gml:id"), "{:?}", messages_of(&out));
    // Without an id, the part can't name it.
    let anonymous = lonlat(&format!(r#"<core:cityObjectMember><bldg:Building>{inner}</bldg:Building></core:cityObjectMember>"#));
    let out = read("model.gml", &anonymous, None).unwrap();
    assert!(says(&out, "1 city objects have no gml:id"), "{:?}", messages_of(&out));
    assert_eq!(value(layer(&out, "model BuildingPart"), 0, "parent"), Value::Null);
    // An id given twice is said.
    let twice = lonlat(&[building("b1", &box_surfaces("x")), building("b1", &box_surfaces("y"))].concat());
    assert!(says(&read("model.gml", &twice, None).unwrap(), "1 city objects have a gml:id another object has too"));
}

/// A solid of links to its surfaces' polygons, the way most CityGML files write one: the links find
/// faces the building holds already, so nothing is added and nothing is said. A link to nothing, to
/// another file, or to what isn't a geometry is said.
#[test]
fn links_are_said_and_what_is_written_out_is_drawn() {
    let links: String = ["b1-g", "b1-w", "b1-r"].iter().map(|id| format!(r##"<gml:surfaceMember xlink:href="#{id}"/>"##)).collect();
    let inner = format!(
        "<bldg:lod2Solid><gml:Solid><gml:exterior><gml:CompositeSurface>{links}</gml:CompositeSurface></gml:exterior></gml:Solid></bldg:lod2Solid>{}",
        box_surfaces("b1")
    );
    let out = read("model.gml", &lonlat(&building("b1", &inner)), None).unwrap();
    assert!((area(out[0].0.features[0].geometry.as_ref()) - 100.0).abs() < 1e-6);
    let said = messages_of(&out);
    assert_eq!(said.len(), 1, "only the footprint notice: {said:?}");
    // To nothing, to another file, and to the building itself.
    let lost = [r##"<gml:surfaceMember xlink:href="#nowhere"/>"##, r#"<gml:surfaceMember xlink:href="other.gml#b1-g"/>"#].concat();
    let inner = format!(
        r##"<bldg:lod2Solid><gml:Solid><gml:exterior><gml:CompositeSurface>{lost}<gml:surfaceMember xlink:href="#b1"/></gml:CompositeSurface></gml:exterior></gml:Solid></bldg:lod2Solid>{}"##,
        box_surfaces("b1")
    );
    let out = read("model.gml", &lonlat(&building("b1", &inner)), None).unwrap();
    assert!(says(&out, "2 geometries or objects are given only by a link to another element (xlink:href)"), "{:?}", messages_of(&out));
    assert!(
        says(&out, "1 links (xlink:href) where a geometry belongs point to an element that isn't a geometry"),
        "{:?}",
        messages_of(&out)
    );
    assert!((area(out[0].0.features[0].geometry.as_ref()) - 100.0).abs() < 1e-6);
    // A thematic surface, a geometry, a ring's curve and a building part given only by a link are
    // said as well.
    let ring = r##"<gml:Polygon><gml:exterior><gml:Ring><gml:curveMember><gml:LineString><gml:posList>0 0 0 9 0 0 9 9 0</gml:posList></gml:LineString></gml:curveMember><gml:curveMember xlink:href="#c"/></gml:Ring></gml:exterior></gml:Polygon>"##;
    let inner = format!(
        r##"<bldg:boundedBy><bldg:WallSurface xlink:href="#w"/></bldg:boundedBy>
        <bldg:lod1MultiSurface><gml:MultiSurface xlink:href="#ms"/></bldg:lod1MultiSurface>
        <bldg:lod3MultiSurface><gml:MultiSurface><gml:surfaceMember>{ring}</gml:surfaceMember></gml:MultiSurface></bldg:lod3MultiSurface>
        <bldg:consistsOfBuildingPart xlink:href="#p"/>{}"##,
        box_surfaces("b1")
    );
    let out = read("model.gml", &lonlat(&building("b1", &inner)), None).unwrap();
    assert!(says(&out, "3 geometries or objects are given only by a link"), "{:?}", messages_of(&out));
    assert!(says(&out, "1 links between city objects"), "{:?}", messages_of(&out));
    assert!(!says(&out, "can't be read"), "{:?}", messages_of(&out));
    assert_eq!(value(&out[0].0, 0, "consistsOfBuildingPart"), Value::Text("#p".into()));
}

/// Links are followed after what the object writes out: a solid listing the ground first doesn't
/// take the ground's faces as a wall's, so the ground still gives the footprint.
#[test]
fn a_linked_ground_stays_the_ground() {
    let solid = r##"<bldg:lod2Solid><gml:Solid><gml:exterior><gml:CompositeSurface><gml:surfaceMember xlink:href="#g"/><gml:surfaceMember xlink:href="#r"/></gml:CompositeSurface></gml:exterior></gml:Solid></bldg:lod2Solid>"##;
    let inner = [
        solid.to_string(),
        surface("GroundSurface", &[polygon("g", &square(2.0, 2.0, 4.0, 0.0))]),
        surface("RoofSurface", &[polygon("r", &square(0.0, 0.0, 8.0, 5.0))]),
    ]
    .concat();
    let out = read("model.gml", &lonlat(&building("b1", &inner)), None).unwrap();
    assert!((area(out[0].0.features[0].geometry.as_ref()) - 16.0).abs() < 1e-6, "{:?}", out[0].0.features[0].geometry);
    assert_eq!(messages_of(&out).len(), 1, "{:?}", messages_of(&out));
}

/// A link to a geometry its object doesn't hold, written in another object of the member (a
/// building's solid holding its installation's faces), reads it where the link stands.
#[test]
fn a_link_to_another_objects_geometry_reads_it() {
    let solid = r##"<bldg:lod1Solid><gml:Solid><gml:exterior><gml:CompositeSurface><gml:surfaceMember xlink:href="#p1-g"/><gml:surfaceMember xlink:href="#p1-r"/></gml:CompositeSurface></gml:exterior></gml:Solid></bldg:lod1Solid>"##;
    let inner = format!(
        "{solid}<bldg:consistsOfBuildingPart><bldg:BuildingPart gml:id=\"p1\">{}</bldg:BuildingPart></bldg:consistsOfBuildingPart>",
        box_surfaces("p1")
    );
    let out = read("model.gml", &lonlat(&building("b1", &inner)), None).unwrap();
    let hall = layer(&out, "model Building");
    assert_eq!(hall.geometry_type, GeometryType::Polygon, "drawn from its links");
    assert!((area(hall.features[0].geometry.as_ref()) - 100.0).abs() < 1e-6, "{:?}", hall.features[0].geometry);
    assert_eq!(value(hall, 0, "lod"), Value::Text("1".into()), "at the level of detail of the link");
    assert_eq!(value(hall, 0, "z_max"), Value::Float(5.0));
    let part = layer(&out, "model BuildingPart");
    assert!((area(part.features[0].geometry.as_ref()) - 100.0).abs() < 1e-6, "its part keeps its own");
    assert!(!says(&out, "link"), "{:?}", messages_of(&out));
}

/// Links that point at each other end, and a long chain of links is followed without a deep stack.
#[test]
fn cycles_and_chains_of_links_end() {
    // A part's surfaces link to each other; the building links to one of them.
    let cycle = format!(
        r##"<bldg:lod2MultiSurface><gml:MultiSurface gml:id="a"><gml:surfaceMember xlink:href="#c"/><gml:surfaceMember>{}</gml:surfaceMember></gml:MultiSurface></bldg:lod2MultiSurface>
        <bldg:lod3MultiSurface><gml:MultiSurface><gml:surfaceMember><gml:CompositeSurface gml:id="c"><gml:surfaceMember xlink:href="#a"/><gml:surfaceMember xlink:href="#c"/></gml:CompositeSurface></gml:surfaceMember></gml:MultiSurface></bldg:lod3MultiSurface>"##,
        polygon("pa", &square(0.0, 0.0, 10.0, 0.0))
    );
    let inner = format!(
        r##"<bldg:lod1MultiSurface><gml:MultiSurface><gml:surfaceMember xlink:href="#a"/></gml:MultiSurface></bldg:lod1MultiSurface>
        <bldg:consistsOfBuildingPart><bldg:BuildingPart gml:id="p1">{cycle}</bldg:BuildingPart></bldg:consistsOfBuildingPart>"##
    );
    let out = read("model.gml", &lonlat(&building("b1", &inner)), None).unwrap();
    assert!((area(layer(&out, "model Building").features[0].geometry.as_ref()) - 100.0).abs() < 1e-6, "{:?}", messages_of(&out));
    assert!(!says(&out, "link"), "{:?}", messages_of(&out));

    // A chain of 20,000 composites, each holding a link to the next, the last a polygon.
    let n = 20_000;
    let mut chain = String::new();
    for k in 0..n {
        chain.push_str(&format!(
            r##"<gml:surfaceMember><gml:CompositeSurface gml:id="k{k}"><gml:surfaceMember xlink:href="#k{}"/></gml:CompositeSurface></gml:surfaceMember>"##,
            k + 1
        ));
    }
    chain.push_str(&format!("<gml:surfaceMember>{}</gml:surfaceMember>", polygon(&format!("k{n}"), &square(0.0, 0.0, 10.0, 0.0))));
    let inner = format!(
        r##"<bldg:lod1MultiSurface><gml:MultiSurface><gml:surfaceMember xlink:href="#k0"/></gml:MultiSurface></bldg:lod1MultiSurface>
        <bldg:consistsOfBuildingPart><bldg:BuildingPart gml:id="p1"><bldg:lod2MultiSurface><gml:MultiSurface>{chain}</gml:MultiSurface></bldg:lod2MultiSurface></bldg:BuildingPart></bldg:consistsOfBuildingPart>"##
    );
    let out = read("model.gml", &lonlat(&building("b1", &inner)), None).unwrap();
    assert!((area(layer(&out, "model Building").features[0].geometry.as_ref()) - 100.0).abs() < 1e-6, "{:?}", messages_of(&out));
    assert!(!says(&out, "link"), "{:?}", messages_of(&out));
}

/// Many objects linking to one large geometry read it only as many times over as the member's size
/// pays for, in bytes: a single polygon with a long `posList` is as costly as many elements. The
/// links past that are said, each one (a second link to the same geometry too), and every object is
/// still read.
#[test]
fn links_read_a_member_only_so_many_times_over() {
    let ring: Vec<String> = (0..20_000).map(|k| format!("{} {} 0", f64::from(k % 100), f64::from(k / 100))).collect();
    let big = format!(
        r#"<gml:Polygon gml:id="big"><gml:exterior><gml:LinearRing><gml:posList>0 0 0 100 0 0 100 200 0 {} 0 0 0</gml:posList></gml:LinearRing></gml:exterior></gml:Polygon>"#,
        ring.join(" ")
    );
    let parts: String = (0..20)
        .map(|k| {
            format!(
                r##"<bldg:consistsOfBuildingPart><bldg:BuildingPart gml:id="p{k}"><bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember xlink:href="#big"/><gml:surfaceMember xlink:href="#big"/></gml:MultiSurface></bldg:lod2MultiSurface></bldg:BuildingPart></bldg:consistsOfBuildingPart>"##
            )
        })
        .collect();
    let inner = format!(
        r#"<bldg:lod1MultiSurface><gml:MultiSurface><gml:surfaceMember>{big}</gml:surfaceMember></gml:MultiSurface></bldg:lod1MultiSurface>{parts}"#
    );
    let out = read("model.gml", &lonlat(&building("b1", &inner)), None).unwrap();
    let part = layer(&out, "model BuildingPart");
    assert_eq!(part.features.len(), 20);
    let drawn = part.features.iter().filter(|f| f.geometry.is_some()).count();
    assert!((1..20).contains(&drawn), "some read, then the rest refused: {drawn}");
    let refused = format!(
        "{} links (xlink:href) to geometries aren't followed, as they would read their city object member, or the file, over many times",
        2 * (20 - drawn)
    );
    assert!(says(&out, &refused), "{refused}: {:?}", messages_of(&out));
}

/// A linked aggregate holding a geometry its object has read already at that level doesn't read it
/// again: a point linked on its own and through its multipoint is one point.
#[test]
fn a_linked_aggregate_doesnt_read_again_what_was_read() {
    let part = r#"<bldg:lod1Geometry><gml:MultiPoint gml:id="mp"><gml:pointMember><gml:Point gml:id="pt"><gml:pos>5 5 0</gml:pos></gml:Point></gml:pointMember></gml:MultiPoint></bldg:lod1Geometry>"#;
    let inner = format!(
        r##"<bldg:lod1Geometry><gml:MultiGeometry><gml:geometryMember xlink:href="#mp"/><gml:geometryMember xlink:href="#pt"/></gml:MultiGeometry></bldg:lod1Geometry>
        <bldg:consistsOfBuildingPart><bldg:BuildingPart gml:id="p1">{part}</bldg:BuildingPart></bldg:consistsOfBuildingPart>"##
    );
    let out = read("model.gml", &lonlat(&building("b1", &inner)), None).unwrap();
    let hall = layer(&out, "model Building");
    let points = match &hall.features[0].geometry {
        Some(Geometry::Point(_)) => 1,
        Some(Geometry::MultiPoint(p)) => p.len(),
        other => panic!("{other:?}"),
    };
    assert_eq!(points, 1, "{:?}", hall.features[0].geometry);
}

/// A geometry left out (a polygon in it can't be read) leaves its good faces unread, so a link to
/// one of them from elsewhere in the object still reads it.
#[test]
fn a_link_into_a_geometry_left_out_still_reads_its_face() {
    let bad =
        r#"<gml:Polygon><gml:exterior><gml:LinearRing><gml:posList>1 2 abc</gml:posList></gml:LinearRing></gml:exterior></gml:Polygon>"#;
    let inner = format!(
        r##"<bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember>{}</gml:surfaceMember><gml:surfaceMember>{bad}</gml:surfaceMember></gml:MultiSurface></bldg:lod2MultiSurface>
        <bldg:lod2Solid><gml:Solid><gml:exterior><gml:CompositeSurface><gml:surfaceMember xlink:href="#p"/></gml:CompositeSurface></gml:exterior></gml:Solid></bldg:lod2Solid>"##,
        polygon("p", &square(0.0, 0.0, 10.0, 0.0))
    );
    let out = read("model.gml", &lonlat(&building("b1", &inner)), None).unwrap();
    assert!(says(&out, "1 geometries of city objects have coordinates that can't be read"), "{:?}", messages_of(&out));
    assert!((area(out[0].0.features[0].geometry.as_ref()) - 100.0).abs() < 1e-6, "{:?}", messages_of(&out));

    // A solid whose linked exterior is read but whose cavity can't be: the solid is left out, and
    // the exterior it linked to is still read by the next link to it at that level.
    let shell = format!(
        r#"<bldg:lod1MultiSurface><gml:MultiSurface><gml:surfaceMember><gml:CompositeSurface gml:id="s"><gml:surfaceMember>{}</gml:surfaceMember></gml:CompositeSurface></gml:surfaceMember></gml:MultiSurface></bldg:lod1MultiSurface>"#,
        polygon("q", &square(0.0, 0.0, 10.0, 0.0))
    );
    let inner = format!(
        r##"{shell}<bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember xlink:href="#s"/></gml:MultiSurface></bldg:lod2MultiSurface>
        <bldg:lod2Solid><gml:Solid><gml:exterior xlink:href="#s"/><gml:interior><gml:CompositeSurface><gml:surfaceMember>{bad}</gml:surfaceMember></gml:CompositeSurface></gml:interior></gml:Solid></bldg:lod2Solid>"##
    );
    let out = read("model.gml", &lonlat(&building("b2", &inner)), None).unwrap();
    assert!(says(&out, "1 geometries of city objects have coordinates that can't be read"), "{:?}", messages_of(&out));
    assert_eq!(value(&out[0].0, 0, "lod"), Value::Text("2".into()), "{:?}", messages_of(&out));
    assert!((area(out[0].0.features[0].geometry.as_ref()) - 100.0).abs() < 1e-6, "{:?}", messages_of(&out));
}

/// A geometry whose coordinates can't be read is said once, however many links point to it; its
/// good parts are still read by a link to them.
#[test]
fn a_geometry_that_cant_be_read_is_said_once_for_its_links() {
    let bad = r#"<gml:Polygon gml:id="p"><gml:exterior><gml:LinearRing><gml:posList>1 2 abc</gml:posList></gml:LinearRing></gml:exterior></gml:Polygon>"#;
    let inner = format!(
        r##"<bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember>{}</gml:surfaceMember><gml:surfaceMember>{bad}</gml:surfaceMember></gml:MultiSurface></bldg:lod2MultiSurface>
        <bldg:lod2Solid><gml:Solid><gml:exterior><gml:CompositeSurface><gml:surfaceMember xlink:href="#p"/><gml:surfaceMember xlink:href="#a"/></gml:CompositeSurface></gml:exterior></gml:Solid></bldg:lod2Solid>
        <bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember xlink:href="#p"/></gml:MultiSurface></bldg:lod2MultiSurface>"##,
        polygon("a", &square(0.0, 0.0, 10.0, 0.0))
    );
    let out = read("model.gml", &lonlat(&building("b1", &inner)), None).unwrap();
    assert!(says(&out, "1 geometries of city objects have coordinates that can't be read"), "{:?}", messages_of(&out));
    assert!((area(out[0].0.features[0].geometry.as_ref()) - 100.0).abs() < 1e-6, "{:?}", messages_of(&out));
}

/// A geometry left out is said as one: what it says of its parts (a ring given by a link) isn't said
/// again by each link that reads it, and a solid whose exterior links to it is said too.
#[test]
fn a_geometry_left_out_doesnt_say_its_parts_for_each_link() {
    let bad =
        r#"<gml:Polygon><gml:exterior><gml:LinearRing><gml:posList>1 2 abc</gml:posList></gml:LinearRing></gml:exterior></gml:Polygon>"#;
    let ring = r##"<gml:Polygon><gml:exterior xlink:href="#ring"/></gml:Polygon>"##;
    let inner = format!(
        r##"<bldg:lod1MultiSurface><gml:MultiSurface><gml:surfaceMember><gml:CompositeSurface gml:id="e"><gml:surfaceMember>{ring}</gml:surfaceMember><gml:surfaceMember>{bad}</gml:surfaceMember></gml:CompositeSurface></gml:surfaceMember></gml:MultiSurface></bldg:lod1MultiSurface>
        <bldg:lod2Solid><gml:Solid><gml:exterior><gml:CompositeSurface><gml:surfaceMember xlink:href="#e"/></gml:CompositeSurface></gml:exterior></gml:Solid></bldg:lod2Solid>
        <bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember xlink:href="#e"/><gml:surfaceMember>{}</gml:surfaceMember></gml:MultiSurface></bldg:lod2MultiSurface>"##,
        polygon("a", &square(0.0, 0.0, 10.0, 0.0))
    );
    let out = read("model.gml", &lonlat(&building("b1", &inner)), None).unwrap();
    // Once, where it is written, whatever links to it.
    assert!(says(&out, "1 geometries of city objects have coordinates that can't be read"), "{:?}", messages_of(&out));
    assert!(!says(&out, "given only by a link"), "{:?}", messages_of(&out));

    // A solid whose exterior links to a geometry left out is left out with its cavities, and said.
    let cavity = format!(
        "<gml:interior><gml:CompositeSurface><gml:surfaceMember>{}</gml:surfaceMember></gml:CompositeSurface></gml:interior>",
        polygon("c", &square(2.0, 2.0, 2.0, 0.0))
    );
    let inner = format!(
        r##"<bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember><gml:CompositeSurface gml:id="s"><gml:surfaceMember>{bad}</gml:surfaceMember></gml:CompositeSurface></gml:surfaceMember></gml:MultiSurface></bldg:lod2MultiSurface>
        <bldg:lod2Solid><gml:Solid><gml:exterior xlink:href="#s"/>{cavity}</gml:Solid></bldg:lod2Solid>"##
    );
    let out = read("model.gml", &lonlat(&building("b2", &inner)), None).unwrap();
    assert!(says(&out, "2 geometries of city objects have coordinates that can't be read"), "{:?}", messages_of(&out));
}

/// A geometry that can't be read, written in one object and linked from an object nested in it, is
/// said once.
#[test]
fn a_geometry_that_cant_be_read_is_said_once_for_its_member() {
    let bad = r#"<gml:Polygon gml:id="p"><gml:exterior><gml:LinearRing><gml:posList>1 2 abc</gml:posList></gml:LinearRing></gml:exterior></gml:Polygon>"#;
    let inner = format!(
        r##"<bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember>{}</gml:surfaceMember><gml:surfaceMember>{bad}</gml:surfaceMember></gml:MultiSurface></bldg:lod2MultiSurface>
        <bldg:consistsOfBuildingPart><bldg:BuildingPart gml:id="p1"><bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember xlink:href="#p"/><gml:surfaceMember>{}</gml:surfaceMember></gml:MultiSurface></bldg:lod2MultiSurface></bldg:BuildingPart></bldg:consistsOfBuildingPart>"##,
        polygon("a", &square(0.0, 0.0, 10.0, 0.0)),
        polygon("b", &square(20.0, 0.0, 10.0, 0.0))
    );
    let out = read("model.gml", &lonlat(&building("b1", &inner)), None).unwrap();
    assert!(says(&out, "1 geometries of city objects have coordinates that can't be read"), "{:?}", messages_of(&out));
}

/// A link to a surface's patch reads its face.
#[test]
fn a_link_to_a_patch_reads_its_face() {
    let inner = format!(
        r##"<bldg:lod1MultiSurface><gml:MultiSurface><gml:surfaceMember><gml:PolyhedralSurface><gml:polygonPatches><gml:PolygonPatch gml:id="pp"><gml:exterior><gml:LinearRing><gml:posList>{}</gml:posList></gml:LinearRing></gml:exterior></gml:PolygonPatch></gml:polygonPatches></gml:PolyhedralSurface></gml:surfaceMember></gml:MultiSurface></bldg:lod1MultiSurface>
        <bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember xlink:href="#pp"/></gml:MultiSurface></bldg:lod2MultiSurface>"##,
        square(0.0, 0.0, 10.0, 0.0)
    );
    let out = read("model.gml", &lonlat(&building("b1", &inner)), None).unwrap();
    assert_eq!(value(&out[0].0, 0, "lod"), Value::Text("2".into()), "{:?}", messages_of(&out));
    assert!((area(out[0].0.features[0].geometry.as_ref()) - 100.0).abs() < 1e-6, "{:?}", messages_of(&out));
    assert!(!says(&out, "link"), "{:?}", messages_of(&out));
}

/// A geometry whose gml:id an earlier one in its member has too is said in a member with links, where
/// a link to that id reads the first; one in a member without links isn't.
#[test]
fn a_geometry_id_given_twice_is_said_where_links_read_it() {
    let twice = format!(
        "<bldg:lod1MultiSurface><gml:MultiSurface><gml:surfaceMember>{}</gml:surfaceMember><gml:surfaceMember>{}</gml:surfaceMember></gml:MultiSurface></bldg:lod1MultiSurface>",
        polygon("a", &square(0.0, 0.0, 10.0, 0.0)),
        polygon("a", &square(20.0, 0.0, 10.0, 0.0))
    );
    let linked = format!(
        r##"{twice}<bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember xlink:href="#a"/></gml:MultiSurface></bldg:lod2MultiSurface>"##
    );
    let out = read("model.gml", &lonlat(&building("b1", &linked)), None).unwrap();
    assert!(says(&out, "1 geometries have a gml:id an earlier element"), "{:?}", messages_of(&out));
    assert!((area(out[0].0.features[0].geometry.as_ref()) - 100.0).abs() < 1e-6, "{:?}", messages_of(&out));
    let out = read("model.gml", &lonlat(&building("b2", &twice)), None).unwrap();
    assert!(!says(&out, "gml:id an earlier"), "{:?}", messages_of(&out));
}

/// Geometries that are only links to themselves, or to each other, end at once: they don't spend
/// the member's budget, so the object's other links are still followed.
#[test]
fn a_geometry_that_only_links_to_itself_ends() {
    let inner = format!(
        r##"<bldg:lod1MultiSurface><gml:MultiSurface><gml:surfaceMember xlink:href="#p1-g"/></gml:MultiSurface></bldg:lod1MultiSurface>
        <bldg:lod2MultiSurface><gml:MultiSurface>
          <gml:surfaceMember><gml:Polygon gml:id="x" xlink:href="#x"/></gml:surfaceMember>
          <gml:surfaceMember><gml:Polygon gml:id="y" xlink:href="#z"/></gml:surfaceMember>
          <gml:surfaceMember><gml:Polygon gml:id="z" xlink:href="#y"/></gml:surfaceMember>
        </gml:MultiSurface></bldg:lod2MultiSurface>
        <bldg:consistsOfBuildingPart><bldg:BuildingPart gml:id="p1">{}</bldg:BuildingPart></bldg:consistsOfBuildingPart>"##,
        box_surfaces("p1")
    );
    let out = read("model.gml", &lonlat(&building("b1", &inner)), None).unwrap();
    let hall = layer(&out, "model Building");
    assert!((area(hall.features[0].geometry.as_ref()) - 100.0).abs() < 1e-6, "{:?}", messages_of(&out));
    assert!(!says(&out, "aren't followed"), "{:?}", messages_of(&out));
}

/// A face reused by a link at another level of detail is read at that level too: the LoD 3
/// footprint takes in the LoD 2 face its LoD 3 geometry links to.
#[test]
fn a_link_from_another_level_of_detail_reads_the_face_there() {
    let inner = format!(
        r##"<bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember>{}</gml:surfaceMember></gml:MultiSurface></bldg:lod2MultiSurface>
        <bldg:lod3MultiSurface><gml:MultiSurface><gml:surfaceMember xlink:href="#a"/><gml:surfaceMember>{}</gml:surfaceMember></gml:MultiSurface></bldg:lod3MultiSurface>"##,
        polygon("a", &square(0.0, 0.0, 10.0, 0.0)),
        polygon("b", &square(20.0, 0.0, 10.0, 0.0))
    );
    let out = read("model.gml", &lonlat(&building("b1", &inner)), None).unwrap();
    let ds = &out[0].0;
    assert_eq!(value(ds, 0, "lod"), Value::Text("3".into()));
    assert!((area(ds.features[0].geometry.as_ref()) - 200.0).abs() < 1e-6, "{:?}", ds.features[0].geometry);
    assert!(!says(&out, "link"), "{:?}", messages_of(&out));
}

/// A solid whose exterior is a link that is found keeps its cavities, read in the dimension the
/// solid gives, not the shell's.
#[test]
fn a_linked_exterior_keeps_its_cavities_in_the_solids_dimension() {
    let shell = format!(
        r#"<bldg:lod1MultiSurface><gml:MultiSurface><gml:surfaceMember><gml:CompositeSurface gml:id="shell"><gml:surfaceMember>{}</gml:surfaceMember></gml:CompositeSurface></gml:surfaceMember></gml:MultiSurface></bldg:lod1MultiSurface>"#,
        polygon("s", &square(0.0, 0.0, 10.0, 0.0))
    );
    let cavity = r#"<gml:Polygon><gml:exterior><gml:LinearRing><gml:posList>2 2 4 2 4 4 2 4 2 2</gml:posList></gml:LinearRing></gml:exterior></gml:Polygon>"#;
    let inner = format!(
        r##"<bldg:lod2Solid><gml:Solid srsDimension="2"><gml:exterior xlink:href="#shell"/><gml:interior><gml:CompositeSurface><gml:surfaceMember>{cavity}</gml:surfaceMember></gml:CompositeSurface></gml:interior></gml:Solid></bldg:lod2Solid>
        <bldg:consistsOfBuildingPart><bldg:BuildingPart gml:id="p1">{shell}</bldg:BuildingPart></bldg:consistsOfBuildingPart>"##
    );
    let out = read("model.gml", &lonlat(&building("b1", &inner)), None).unwrap();
    assert!(!says(&out, "can't be read"), "{:?}", messages_of(&out));
    assert!(!says(&out, "link"), "{:?}", messages_of(&out));
    let hall = layer(&out, "model Building");
    assert!((area(hall.features[0].geometry.as_ref()) - 100.0).abs() < 1e-6, "{:?}", hall.features[0].geometry);
}

/// A linked implicit geometry is drawn at its reference point, as one written out is; one linked
/// to nothing is said as a link, not as what isn't a geometry.
#[test]
fn a_linked_implicit_geometry_is_drawn_at_its_reference_point() {
    let implicit = r#"<bldg:lod1ImplicitRepresentation><core:ImplicitGeometry gml:id="ig"><core:referencePoint><gml:Point><gml:pos>5 5 0</gml:pos></gml:Point></core:referencePoint></core:ImplicitGeometry></bldg:lod1ImplicitRepresentation>"#;
    let inner = format!(
        r##"<bldg:lod2ImplicitRepresentation xlink:href="#ig"/><bldg:lod3ImplicitRepresentation xlink:href="#gone"/>
        <bldg:consistsOfBuildingPart><bldg:BuildingPart gml:id="p1">{implicit}</bldg:BuildingPart></bldg:consistsOfBuildingPart>"##
    );
    let out = read("model.gml", &lonlat(&building("b1", &inner)), None).unwrap();
    assert!(says(&out, "2 geometry instances are drawn as a point"), "{:?}", messages_of(&out));
    assert!(says(&out, "1 geometries or objects are given only by a link"), "{:?}", messages_of(&out));
    assert!(!says(&out, "isn't a geometry"), "{:?}", messages_of(&out));
}

/// A face a ground surface links to is the ground's, though a solid links to it too, whichever is
/// written first.
#[test]
fn a_ground_given_by_a_link_is_the_ground() {
    let part = [
        surface("GroundSurface", &[polygon("pg", &square(2.0, 2.0, 4.0, 0.0))]),
        surface("RoofSurface", &[polygon("pr", &square(0.0, 0.0, 8.0, 5.0))]),
    ]
    .concat();
    let inner = format!(
        r##"<bldg:boundedBy><bldg:GroundSurface><bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember xlink:href="#pg"/></gml:MultiSurface></bldg:lod2MultiSurface></bldg:GroundSurface></bldg:boundedBy>
        <bldg:lod2Solid><gml:Solid><gml:exterior><gml:CompositeSurface><gml:surfaceMember xlink:href="#pg"/><gml:surfaceMember xlink:href="#pr"/></gml:CompositeSurface></gml:exterior></gml:Solid></bldg:lod2Solid>
        <bldg:consistsOfBuildingPart><bldg:BuildingPart gml:id="p1">{part}</bldg:BuildingPart></bldg:consistsOfBuildingPart>"##
    );
    let out = read("model.gml", &lonlat(&building("b1", &inner)), None).unwrap();
    let hall = layer(&out, "model Building");
    assert!((area(hall.features[0].geometry.as_ref()) - 16.0).abs() < 1e-6, "{:?}", hall.features[0].geometry);

    // The other way round, as some exporters write it: the solid holds its faces, and the ground
    // surface links to one of them beside a face of its own. Both are the ground.
    let inner = format!(
        r##"<bldg:lod2Solid><gml:Solid><gml:exterior><gml:CompositeSurface><gml:surfaceMember>{}</gml:surfaceMember><gml:surfaceMember>{}</gml:surfaceMember></gml:CompositeSurface></gml:exterior></gml:Solid></bldg:lod2Solid>
        <bldg:boundedBy><bldg:GroundSurface><bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember xlink:href="#g1"/><gml:surfaceMember>{}</gml:surfaceMember></gml:MultiSurface></bldg:lod2MultiSurface></bldg:GroundSurface></bldg:boundedBy>"##,
        polygon("g1", &square(0.0, 0.0, 10.0, 0.0)),
        polygon("r1", &square(0.0, 0.0, 30.0, 5.0)),
        polygon("g2", &square(20.0, 20.0, 4.0, 0.0))
    );
    let out = read("model.gml", &lonlat(&building("b2", &inner)), None).unwrap();
    assert!((area(out[0].0.features[0].geometry.as_ref()) - 116.0).abs() < 1e-6, "{:?}", out[0].0.features[0].geometry);
}

/// A city object whose `gml:id` is blank is still an object, kept without an id.
#[test]
fn an_object_with_a_blank_id_is_still_one() {
    let inner = r#"<bldg:consistsOfBuildingPart><bldg:BuildingPart gml:id=" "><gml:name>Annex</gml:name></bldg:BuildingPart></bldg:consistsOfBuildingPart>"#;
    let out = read("model.gml", &lonlat(&building("b1", &format!("{inner}{}", box_surfaces("b1")))), None).unwrap();
    let part = layer(&out, "model BuildingPart");
    assert_eq!(part.features.len(), 1);
    assert_eq!(value(part, 0, "name"), Value::Text("Annex".into()));
    assert_eq!(value(part, 0, "id"), Value::Text(String::new()));
}

/// A ring given only by a link is said as a link, not as coordinates that can't be read: a linked
/// exterior leaves its face out, a linked hole only the hole.
#[test]
fn a_ring_given_by_a_link_is_said_as_one() {
    let exterior = r##"<gml:Polygon><gml:exterior><gml:LinearRing xlink:href="#r"/></gml:exterior></gml:Polygon>"##;
    let holed = format!(
        r##"<gml:Polygon><gml:exterior><gml:LinearRing><gml:posList>{}</gml:posList></gml:LinearRing></gml:exterior><gml:interior><gml:LinearRing xlink:href="#h"/></gml:interior></gml:Polygon>"##,
        square(0.0, 0.0, 10.0, 0.0)
    );
    let inner = format!(
        "<bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember>{exterior}</gml:surfaceMember></gml:MultiSurface></bldg:lod2MultiSurface>\
         <bldg:lod3MultiSurface><gml:MultiSurface><gml:surfaceMember>{holed}</gml:surfaceMember></gml:MultiSurface></bldg:lod3MultiSurface>"
    );
    let out = read("model.gml", &lonlat(&building("b1", &inner)), None).unwrap();
    assert!(says(&out, "2 geometries or objects are given only by a link"), "{:?}", messages_of(&out));
    assert!(!says(&out, "can't be read"), "{:?}", messages_of(&out));
    let drawn: f64 = out.iter().flat_map(|(ds, _)| ds.features.iter()).map(|f| area(f.geometry.as_ref())).sum();
    assert!(drawn > 0.0, "the face with a linked hole is kept: {:?}", messages_of(&out));
}

/// What isn't read is said, one notice per kind; a clean model ([`a_building_is_its_ground_with_its_heights`])
/// says none of it.
#[test]
fn what_isnt_read_is_said() {
    let furniture = r#"<core:cityObjectMember><frn:CityFurniture gml:id="f1"><frn:lod2ImplicitRepresentation><core:ImplicitGeometry>
        <core:transformationMatrix>1 0 0 0 0 1 0 0 0 0 1 0 0 0 0 1</core:transformationMatrix>
        <core:referencePoint><gml:Point><gml:pos>1 2 3</gml:pos></gml:Point></core:referencePoint>
        </core:ImplicitGeometry></frn:lod2ImplicitRepresentation></frn:CityFurniture></core:cityObjectMember>"#;
    let relief = r#"<core:cityObjectMember><dem:ReliefFeature gml:id="r1"><dem:lod>2</dem:lod><dem:reliefComponent><dem:TINRelief gml:id="t1"><dem:lod>2</dem:lod></dem:TINRelief></dem:reliefComponent></dem:ReliefFeature></core:cityObjectMember>"#;
    let appearance = r#"<app:appearanceMember><app:Appearance><app:theme>t</app:theme></app:Appearance></app:appearanceMember>"#;
    let extra = format!(
        r#"<bldg:lod2TerrainIntersection><gml:MultiCurve><gml:curveMember><gml:LineString><gml:posList>0 0 0 10 0 0</gml:posList></gml:LineString></gml:curveMember></gml:MultiCurve></bldg:lod2TerrainIntersection>
        <bldg:lod1MultiSurface><gml:MultiSurface><gml:surfaceMember>{}</gml:surfaceMember></gml:MultiSurface></bldg:lod1MultiSurface>
        <bldg:address><core:Address><core:xalAddress/></core:Address></bldg:address>{}"#,
        polygon("bad", "0 0 x 1 1 1 2 2 2 0 0 x"),
        box_surfaces("b1")
    );
    let out = read("model.gml", &lonlat(&[furniture, relief, appearance, &building("b1", &extra)].concat()), None).unwrap();
    for text in [
        "1 geometry instances are drawn as a point at their reference point, with no height",
        "1 TINRelief objects aren't drawn",
        "the file's appearances (textures and materials) aren't read: 1 are left out",
        "1 terrain intersection curves (where objects meet the ground) aren't read",
        "1 geometries of city objects have coordinates that can't be read",
        "the addresses of 1 city objects are left out",
    ] {
        assert!(says(&out, text), "{text}: {:?}", messages_of(&out));
    }
    let point = layer(&out, "model CityFurniture");
    assert_eq!(point.features[0].geometry, Some(Geometry::Point(crate::Coord::new(1.0, 2.0))));
    assert_eq!(value(point, 0, "z_max"), Value::Null, "an instance's height is its template's");
}

/// A member nested past the limit is an object that couldn't be read, not a crash, and the others
/// are read.
#[test]
fn a_member_too_deep_is_left_out_and_the_others_read() {
    let deep = format!("<core:cityObjectMember>{}{}</core:cityObjectMember>", "<gen:a>".repeat(300), "</gen:a>".repeat(300));
    let out = read("model.gml", &lonlat(&[deep, building("b1", &box_surfaces("b1"))].concat()), None).unwrap();
    assert!(says(&out, "1 city objects couldn't be read"), "{:?}", messages_of(&out));
    assert_eq!(out[0].0.features.len(), 1);
}

/// A member whose namespaces would take too long to resolve (an object declaring hundreds, around
/// thousands of children that each declare one more) is left out whole and said, as one nested too
/// deep is, and the others are read.
#[test]
fn a_member_flooded_with_namespaces_is_left_out_and_the_others_read() {
    let declarations: String = (0..300).map(|k| format!(r#" xmlns:n{k}="urn:{k}""#)).collect();
    let flooded = format!(
        r#"<core:cityObjectMember><bldg:Building gml:id="f"{declarations}>{}</bldg:Building></core:cityObjectMember>"#,
        r#"<x:note xmlns:x="urn:x"/>"#.repeat(8000)
    );
    let out = read("model.gml", &lonlat(&[flooded, building("b1", &box_surfaces("b1"))].concat()), None).unwrap();
    assert!(says(&out, "1 city objects couldn't be read"), "{:?}", messages_of(&out));
    assert_eq!(out[0].0.features.len(), 1);
    // The same children, with the namespaces they need only: read, and nothing said.
    let plain = format!(
        r#"<core:cityObjectMember><bldg:Building gml:id="f">{}{}</bldg:Building></core:cityObjectMember>"#,
        box_surfaces("f"),
        r#"<x:note xmlns:x="urn:x"/>"#.repeat(8000)
    );
    let out = read("model.gml", &lonlat(&plain), None).unwrap();
    assert!(!says(&out, "couldn't be read"), "{:?}", messages_of(&out));
    assert_eq!(out[0].0.features.len(), 1);
}

/// A root declaring many namespaces around many small members: each member is parsed with the
/// root's declarations around it, each within its own bound, but together they take what the
/// file's size allows and no more; the members past it are left out and said.
#[test]
fn members_repeating_a_crowded_root_share_the_files_namespace_work() {
    let declarations: String = (0..1000).map(|k| format!(r#" xmlns:n{k}="urn:{k}""#)).collect();
    let members: String = (0..1000).map(|k| building(&format!("b{k}"), &box_surfaces(&format!("b{k}")))).collect();
    let crowded = String::from_utf8(lonlat(&members)).unwrap().replacen(" xmlns:xlink=", &format!("{declarations} xmlns:xlink="), 1);
    let out = read("model.gml", crowded.as_bytes(), None).unwrap();
    let read_in = out[0].0.features.len();
    assert!(read_in > 0 && read_in < 1000, "{read_in} of 1000 read");
    assert!(says(&out, &format!("{} city objects couldn't be read", 1000 - read_in)), "{:?}", messages_of(&out));
    // The same members under the usual root: all read, and nothing said.
    let out = read("model.gml", &lonlat(&members), None).unwrap();
    assert_eq!(out[0].0.features.len(), 1000);
    assert!(!says(&out, "couldn't be read"), "{:?}", messages_of(&out));
}

/// `.gml` and `.xml` files holding a city model go to this reader; other GML stays with GML's.
#[test]
fn files_named_gml_or_xml_holding_a_city_model_are_read_as_one() {
    let bytes = lonlat(&building("b1", &box_surfaces("b1")));
    for name in ["city.gml", "city.xml"] {
        let layers = crate::io::read_layers(name, &bytes).unwrap();
        assert_eq!(layers[0].dataset.name, "city", "{name}");
        assert_eq!(layers[0].dataset.features.len(), 1, "{name}");
        // One layer of it, as a single-layer read gives.
        assert_eq!(crate::io::read_bytes(name, &bytes).unwrap().0.features.len(), 1, "{name}");
    }
    // Known by its root where its name gives no extension (a download).
    assert_eq!(crate::io::sniff_extension(&bytes), Some("gml"));
}

/// A file cut short: the objects before the cut are read, and the cut is said.
#[test]
fn a_file_cut_short_says_so_and_keeps_what_came_before() {
    let whole = String::from_utf8(lonlat(&[building("b1", &box_surfaces("b1")), building("b2", &box_surfaces("b2"))].concat())).unwrap();
    let start = r#"<bldg:Building gml:id="b2">"#;
    let cut = whole.find(start).map(|at| at + start.len()).unwrap();
    // Cut after a tag, inside one that opens an object, inside a member's own start tag, and
    // between members: the root never closes.
    let member = whole.find(start).and_then(|at| whole.get(..at)).and_then(|head| head.rfind("<core:cityObjectMember")).unwrap();
    for cut in [cut, cut - 5, member + 6, member] {
        let out = read("model.gml", &whole.as_bytes()[..cut], None).unwrap();
        assert_eq!(out[0].0.features.len(), 1, "cut at {cut}");
        assert!(says(&out, "the file ends before its city model closes, as if cut short"), "cut at {cut}: {:?}", messages_of(&out));
    }
    // Cut inside the root's end tag: every object is in, and nothing is said.
    let end = whole.rfind("</core:CityModel>").unwrap();
    let out = read("model.gml", &whole.as_bytes()[..end + 5], None).unwrap();
    assert!(!says(&out, "cut short"), "{:?}", messages_of(&out));
    let out = read("model.gml", whole.as_bytes(), None).unwrap();
    assert!(!says(&out, "cut short"));
}

/// A geometry in a property that names no level of detail (a group's) is drawn, with no LoD; a
/// property named `lod…` that holds text is a value.
#[test]
fn a_geometry_with_no_level_of_detail_is_drawn() {
    let group = format!(
        r##"<core:cityObjectMember><grp:CityObjectGroup gml:id="g1"><grp:groupMember xlink:href="#b1"/><bldg:lod2Note>kept</bldg:lod2Note>
        <grp:geometry><gml:MultiSurface><gml:surfaceMember>{}</gml:surfaceMember></gml:MultiSurface></grp:geometry></grp:CityObjectGroup></core:cityObjectMember>"##,
        polygon("gp", &square(0.0, 0.0, 10.0, 0.0))
    );
    let out = read("model.gml", &lonlat(&group), None).unwrap();
    let ds = &out[0].0;
    assert!((area(ds.features[0].geometry.as_ref()) - 100.0).abs() < 1e-6, "{:?}", ds.features[0].geometry);
    assert_eq!(value(ds, 0, "lod"), Value::Null);
    assert_eq!(value(ds, 0, "lod2Note"), Value::Text("kept".into()));
    assert_eq!(value(ds, 0, "groupMember"), Value::Text("#b1".into()));
}

/// The faces of the highest level of detail are united when there is no ground or LoD 0.
#[test]
fn faces_with_no_ground_are_united() {
    // Overlapping: their union is 150, as two they would be 200.
    let faces = [polygon("a", &square(0.0, 0.0, 10.0, 5.0)), polygon("b", &square(5.0, 0.0, 10.0, 5.0))];
    let members: String = faces.iter().map(|p| format!("<gml:surfaceMember>{p}</gml:surfaceMember>")).collect();
    let inner = format!("<bldg:lod2MultiSurface><gml:MultiSurface>{members}</gml:MultiSurface></bldg:lod2MultiSurface>");
    let out = read("model.gml", &lonlat(&building("b1", &inner)), None).unwrap();
    assert!((area(out[0].0.features[0].geometry.as_ref()) - 150.0).abs() < 1e-6, "{:?}", out[0].0.features[0].geometry);
    // Corner to corner: their union is 175, their hull would be 200.
    let faces = [polygon("a", &square(0.0, 0.0, 10.0, 5.0)), polygon("b", &square(5.0, 5.0, 10.0, 5.0))];
    let members: String = faces.iter().map(|p| format!("<gml:surfaceMember>{p}</gml:surfaceMember>")).collect();
    let inner = format!("<bldg:lod2MultiSurface><gml:MultiSurface>{members}</gml:MultiSurface></bldg:lod2MultiSurface>");
    let out = read("model.gml", &lonlat(&building("b1", &inner)), None).unwrap();
    assert!((area(out[0].0.features[0].geometry.as_ref()) - 175.0).abs() < 1e-6, "{:?}", out[0].0.features[0].geometry);
}

/// Positions as `gml:coordinates` (with their separators), and in two dimensions where a geometry
/// says so for what it holds.
#[test]
fn coordinates_and_dimensions_as_written() {
    let ring =
        |positions: &str| format!("<gml:Polygon><gml:exterior><gml:LinearRing>{positions}</gml:LinearRing></gml:exterior></gml:Polygon>");
    let ground = |polygon: String, dim: &str| {
        format!(
            "<bldg:boundedBy><bldg:GroundSurface><bldg:lod2MultiSurface><gml:MultiSurface{dim}><gml:surfaceMember>{polygon}</gml:surfaceMember></gml:MultiSurface></bldg:lod2MultiSurface></bldg:GroundSurface></bldg:boundedBy>"
        )
    };
    for (polygon, dim) in [
        (ring("<gml:coordinates>0,0,0 10,0,0 10,10,0 0,10,0 0,0,0</gml:coordinates>"), ""),
        (ring(r#"<gml:coordinates cs=";" ts="|">0;0;0|10;0;0|10;10;0|0;10;0|0;0;0</gml:coordinates>"#), ""),
        (ring("<gml:posList>0 0 10 0 10 10 0 10 0 0</gml:posList>"), r#" srsDimension="2""#),
    ] {
        let out = read("model.gml", &lonlat(&building("b1", &ground(polygon.clone(), dim))), None).unwrap();
        assert!((area(out[0].0.features[0].geometry.as_ref()) - 100.0).abs() < 1e-6, "{polygon}");
        assert!(!says(&out, "can't be read"), "{polygon}: {:?}", messages_of(&out));
    }
}

/// A measure attribute's unit is on its value: in metres a number, in another unit text with it.
#[test]
fn a_measure_attribute_keeps_a_unit_that_isnt_the_metre() {
    let inner = format!(
        r#"<gen:measureAttribute name="h"><gen:value uom="ft">40</gen:value></gen:measureAttribute>
        <gen:measureAttribute name="m"><gen:value uom="m">12</gen:value></gen:measureAttribute>{}"#,
        box_surfaces("b1")
    );
    let out = read("model.gml", &lonlat(&building("b1", &inner)), None).unwrap();
    assert_eq!(value(&out[0].0, 0, "h"), Value::Text("40 ft".into()));
    assert_eq!(value(&out[0].0, 0, "m"), Value::Float(12.0));
    // The URN and URL forms of the metre are metres.
    for uom in [
        "urn:ogc:def:uom:OGC:1.0:metre",
        "http://www.opengis.net/def/uom/OGC/1.0/metre",
        "urn:ogc:def:uom:EPSG::9001",
        "http://www.opengis.net/def/uom/EPSG/0/9001",
    ] {
        let given = format!(r#"<bldg:measuredHeight uom="{uom}">12</bldg:measuredHeight>{}"#, box_surfaces("b3"));
        let out = read("model.gml", &lonlat(&building("b3", &given)), None).unwrap();
        assert_eq!(value(&out[0].0, 0, "measuredHeight"), Value::Float(12.0), "{uom}");
    }
    // A blank unit is no unit: a number.
    let blank = format!(r#"<bldg:measuredHeight uom="">12</bldg:measuredHeight>{}"#, box_surfaces("b2"));
    let out = read("model.gml", &lonlat(&building("b2", &blank)), None).unwrap();
    assert_eq!(value(&out[0].0, 0, "measuredHeight"), Value::Float(12.0));
    assert!(!says(&out, "codeSpace"), "{:?}", messages_of(&out));
    // Any property with a unit is a measure; a code keeps its value, and its list is said.
    let other = format!(
        r#"<bldg:span uom="ft">40</bldg:span><bldg:depth uom="m">3</bldg:depth>
        <bldg:function codeSpace="http://example.com/functions.xml">1000</bldg:function>{}"#,
        box_surfaces("b4")
    );
    let out = read("model.gml", &lonlat(&building("b4", &other)), None).unwrap();
    assert_eq!(value(&out[0].0, 0, "span"), Value::Text("40 ft".into()));
    assert_eq!(value(&out[0].0, 0, "depth"), Value::Float(3.0));
    assert_eq!(value(&out[0].0, 0, "function"), Value::Text("1000".into()));
    assert!(says(&out, "1 values name the code list they are taken from (codeSpace)"), "{:?}", messages_of(&out));
}

/// An empty `srsName` names nothing; one system written with its axes in another order is refused.
#[test]
fn systems_named_empty_or_in_another_axis_order() {
    let inner = format!(
        r#"<bldg:lod2MultiSurface><gml:MultiSurface srsName=""><gml:surfaceMember>{}</gml:surfaceMember></gml:MultiSurface></bldg:lod2MultiSurface>"#,
        polygon("a", &square(0.0, 0.0, 1.0, 0.0))
    );
    assert!(read("model.gml", &lonlat(&building("b1", &inner)), None).is_ok());
    let swapped = inner.replace(r#"srsName="""#, r#"srsName="urn:ogc:def:crs:EPSG::4326""#);
    let err = read("model.gml", &lonlat(&building("b1", &swapped)), None).unwrap_err().to_string();
    assert!(err.contains("one coordinate system with its axes in different orders"), "{err}");
    // A name written with a character reference is the name it stands for.
    let named = inner.replace(r#"srsName="""#, r#"srsName="EPSG:4326""#);
    let out = read("model.gml", &model("EPSG:432&#54;", &building("b1", &named)), None).unwrap();
    assert_eq!(out[0].0.features.len(), 1);
}

/// Arcs are read through their points, and said; an appearance held as a city object member, an
/// object given only by a link, and objects in `gml:featureMembers` are read as the others.
#[test]
fn arcs_appearances_links_and_feature_members() {
    let curve = r#"<bldg:lod1MultiCurve><gml:MultiCurve><gml:curveMember><gml:Curve><gml:segments><gml:Arc><gml:posList>0 0 0 5 5 0 10 0 0</gml:posList></gml:Arc></gml:segments></gml:Curve></gml:curveMember></gml:MultiCurve></bldg:lod1MultiCurve>"#;
    let members = [
        building("c1", curve),
        r#"<core:cityObjectMember><app:Appearance><app:theme>t</app:theme></app:Appearance></core:cityObjectMember>"#.to_string(),
        r##"<core:cityObjectMember xlink:href="#c1"/>"##.to_string(),
        format!(
            r#"<gml:featureMembers><bldg:Building gml:id="f1">{}</bldg:Building><bldg:Building gml:id="f2">{}</bldg:Building></gml:featureMembers>"#,
            box_surfaces("f1"),
            box_surfaces("f2")
        ),
    ]
    .concat();
    let out = read("model.gml", &lonlat(&members), None).unwrap();
    for text in [
        "1 curves have arcs or splines, read as straight lines through their points",
        "the file's appearances (textures and materials) aren't read: 1 are left out",
        "1 geometries or objects are given only by a link",
    ] {
        assert!(says(&out, text), "{text}: {:?}", messages_of(&out));
    }
    let drawn: usize = out.iter().map(|(ds, _)| ds.features.iter().filter(|f| f.geometry.is_some()).count()).sum();
    assert_eq!(drawn, 3, "the curve and both feature members");
}

/// GML that only mentions CityGML (a feature collection with its namespace) stays with GML's reader.
#[test]
fn other_gml_isnt_taken_for_a_city_model() {
    let gml = br#"<ogr:FeatureCollection xmlns:ogr="http://ogr.maptools.org/" xmlns:gml="http://www.opengis.net/gml" xmlns:core="http://www.opengis.net/citygml/2.0">
<!-- not a core:CityModel -->
<gml:featureMember><ogr:towns><ogr:geometryProperty><gml:Point srsName="EPSG:4326"><gml:coordinates>10,50</gml:coordinates></gml:Point></ogr:geometryProperty><ogr:name>A</ogr:name></ogr:towns></gml:featureMember>
</ogr:FeatureCollection>"#;
    assert!(!looks_like_citygml(gml));
    let layers = crate::io::read_layers("towns.gml", gml).unwrap();
    assert_eq!(layers[0].dataset.features.len(), 1);
}

/// A system given in place of the one the file names reads the coordinates as written: the file's
/// latitude-first order goes with its system. Given as the same system, the order stays.
#[test]
fn a_system_given_reads_the_coordinates_as_written() {
    let ground = square(500_000.0, 5_500_000.0, 10.0, 0.0);
    let bytes = model("urn:ogc:def:crs:EPSG::4258", &building("b1", &surface("GroundSurface", &[polygon("g", &ground)])));
    let out = read("model.gml", &bytes, Some(Crs::Epsg(25832))).unwrap();
    let e = out[0].0.lonlat_extent();
    assert!((e.min_x - 9.0).abs() < 0.01 && (e.min_y - 49.6).abs() < 0.1, "{e:?}");
    // Latitude 10 to 12, longitude 0 to 4, given as the system the file names.
    let inner = surface("GroundSurface", &[polygon("g", "10 0 0 10 4 0 12 4 0 12 0 0 10 0 0")]);
    let out = read("model.gml", &model("urn:ogc:def:crs:EPSG::4326", &building("b1", &inner)), Some(Crs::LonLat)).unwrap();
    let e = out[0].0.extent();
    assert_eq!((e.min_x, e.max_x, e.min_y, e.max_y), (0.0, 4.0, 10.0, 12.0));
}

/// The objects of a `gml:featureMembers` are read one at a time: one that can't be read is counted
/// alone, and a cut inside the element keeps the objects before it.
#[test]
fn objects_of_feature_members_are_read_one_at_a_time() {
    let many = |second: &str| {
        lonlat(&format!(
            r#"<gml:featureMembers xmlns:x="urn:x"><bldg:Building gml:id="f1"><x:note>n</x:note>{}</bldg:Building>{second}</gml:featureMembers>"#,
            box_surfaces("f1")
        ))
    };
    // The second is nested past the limit.
    let broken = format!(r#"<bldg:Building gml:id="f2">{}{}</bldg:Building>"#, "<gen:a>".repeat(300), "</gen:a>".repeat(300));
    let out = read("model.gml", &many(&broken), None).unwrap();
    assert_eq!(out[0].0.features.len(), 1);
    assert!(says(&out, "1 city objects couldn't be read"), "{:?}", messages_of(&out));
    // Cut inside the second: the first is read, and the cut said.
    let whole = String::from_utf8(many(&format!(r#"<bldg:Building gml:id="f2">{}</bldg:Building>"#, box_surfaces("f2")))).unwrap();
    let cut = whole.find(r#"gml:id="f2-w""#).unwrap();
    let out = read("model.gml", &whole.as_bytes()[..cut], None).unwrap();
    assert_eq!(out[0].0.features.len(), 1);
    assert!(says(&out, "as if cut short"), "{:?}", messages_of(&out));
    // Whole, both, and a namespace the element declares is kept for each.
    let out = read("model.gml", whole.as_bytes(), None).unwrap();
    assert_eq!(out[0].0.features.len(), 2);
    assert_eq!(value(&out[0].0, 0, "note"), Value::Text("n".into()));
    // An object given only by a link is said, not read as an empty one.
    let out = read("model.gml", &many(r##"<bldg:Building xlink:href="#f1"/>"##), None).unwrap();
    assert_eq!(out[0].0.features.len(), 1);
    assert!(says(&out, "1 geometries or objects are given only by a link"), "{:?}", messages_of(&out));
    assert!(!says(&out, "no geometry"), "{:?}", messages_of(&out));
}

/// A file broken in the middle (an end tag that doesn't match) is refused, not taken for one cut
/// short.
#[test]
fn a_file_broken_in_the_middle_is_refused() {
    let bytes = lonlat(
        &[building("b1", &box_surfaces("b1")), "<core:cityObjectMember><bldg:Building></bldg:Wrong></core:cityObjectMember>".to_string()]
            .concat(),
    );
    let err = read("model.gml", &bytes, None).unwrap_err().to_string();
    assert!(!err.contains("cut short"), "{err}");
    // Markup that `<!` starts and nothing knows, in the middle: refused too. Ending at `<!`: cut.
    let bang = lonlat(&[building("b1", &box_surfaces("b1")), "<!x>".to_string(), building("b2", &box_surfaces("b2"))].concat());
    assert!(read("model.gml", &bang, None).is_err());
    let whole = lonlat(&[building("b1", &box_surfaces("b1")), "<!-- n -->".to_string(), building("b2", &box_surfaces("b2"))].concat());
    let cut = whole.windows(4).position(|w| w == b"<!--").unwrap() + 2;
    let out = read("model.gml", &whole[..cut], None).unwrap();
    assert_eq!(out[0].0.features.len(), 1);
    assert!(says(&out, "as if cut short"), "{:?}", messages_of(&out));
}

/// Nested objects are known without an id: a part drawn only by its thematic surfaces, a TIN relief.
/// A feature beside them with neither id nor geometry is said.
#[test]
fn nested_objects_without_an_id_are_read() {
    let inner = format!(
        r#"<bldg:consistsOfBuildingPart><bldg:BuildingPart>{}</bldg:BuildingPart><bldg:BuildingPart><gml:name>nothing</gml:name></bldg:BuildingPart></bldg:consistsOfBuildingPart>"#,
        box_surfaces("p")
    );
    let out = read("model.gml", &lonlat(&building("b1", &inner)), None).unwrap();
    let part = layer(&out, "model BuildingPart");
    assert_eq!(part.features.len(), 1);
    assert_eq!(value(part, 0, "parent"), Value::Text("b1".into()));
    assert_eq!(value(layer(&out, "model Building"), 0, "z_max"), Value::Float(5.0), "raised by its part");
    assert!(says(&out, "1 nested features with no gml:id and no geometry, beside city objects, are left out"), "{:?}", messages_of(&out));
    let relief = r#"<core:cityObjectMember><dem:ReliefFeature gml:id="r1"><dem:reliefComponent><dem:TINRelief><dem:lod>2</dem:lod></dem:TINRelief></dem:reliefComponent></dem:ReliefFeature></core:cityObjectMember>"#;
    let out = read("model.gml", &lonlat(&[relief, &building("b2", &box_surfaces("b2"))].concat()), None).unwrap();
    assert!(says(&out, "1 TINRelief objects aren't drawn"), "{:?}", messages_of(&out));
}

/// Heights come from the positions that have them: a 2D ring beside 3D ones takes nothing away.
#[test]
fn heights_come_from_the_positions_that_have_them() {
    let flat = r#"<gml:Polygon><gml:exterior><gml:LinearRing><gml:posList srsDimension="2">20 0 21 0 21 1 20 1 20 0</gml:posList></gml:LinearRing></gml:exterior></gml:Polygon>"#;
    let members: String = [polygon("a", &square(0.0, 0.0, 10.0, 10.0)), polygon("b", &square(0.0, 0.0, 10.0, 20.0)), flat.to_string()]
        .iter()
        .map(|p| format!("<gml:surfaceMember>{p}</gml:surfaceMember>"))
        .collect();
    let inner = format!("<bldg:lod2MultiSurface><gml:MultiSurface>{members}</gml:MultiSurface></bldg:lod2MultiSurface>");
    let out = read("model.gml", &lonlat(&building("b1", &inner)), None).unwrap();
    assert_eq!((value(&out[0].0, 0, "z_min"), value(&out[0].0, 0, "z_max")), (Value::Float(10.0), Value::Float(20.0)));
}

/// `gml:coordinates` with a decimal comma, as its `decimal` attribute says.
#[test]
fn a_decimal_comma_is_read_where_it_is_declared() {
    let ring = r#"<gml:Polygon><gml:exterior><gml:LinearRing><gml:coordinates decimal="," cs=";" ts=" ">0;0;0,5 2,5;0;0,5 2,5;4;0,5 0;4;0,5 0;0;0,5</gml:coordinates></gml:LinearRing></gml:exterior></gml:Polygon>"#;
    let inner = format!(
        r#"<bldg:boundedBy><bldg:GroundSurface><bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember>{ring}</gml:surfaceMember></gml:MultiSurface></bldg:lod2MultiSurface></bldg:GroundSurface></bldg:boundedBy>"#
    );
    let out = read("model.gml", &lonlat(&building("b1", &inner)), None).unwrap();
    assert!((area(out[0].0.features[0].geometry.as_ref()) - 10.0).abs() < 1e-6, "{:?}", messages_of(&out));
    assert_eq!(value(&out[0].0, 0, "z_max"), Value::Float(0.5));
}

/// A CityGML 1.0 model is read as a 2.0 one, and a namespace written with a character reference is
/// the one it stands for.
#[test]
fn version_one_and_namespaces_with_references() {
    let two = String::from_utf8(lonlat(&building("b1", &box_surfaces("b1")))).unwrap();
    let one = two.replace("citygml/2.0", "citygml/1.0").replace("building/2.0", "building/1.0");
    assert!(looks_like_citygml(one.as_bytes()));
    let out = read("model.gml", one.as_bytes(), None).unwrap();
    assert!((area(out[0].0.features[0].geometry.as_ref()) - 100.0).abs() < 1e-6);
    let referenced =
        two.replace(r#"xmlns:core="http://www.opengis.net/citygml/2.0""#, r#"xmlns:core="http://www.opengis.net/citygml/2.&#48;""#);
    assert_ne!(referenced, two);
    assert!(looks_like_citygml(referenced.as_bytes()));
    assert_eq!(read("model.gml", referenced.as_bytes(), None).unwrap()[0].0.features.len(), 1);
}

/// The footprint comes from the highest level of detail, which the row names.
#[test]
fn the_highest_level_of_detail_gives_the_footprint() {
    let at = |lod: u32, side: f64| {
        format!(
            "<bldg:lod{lod}MultiSurface><gml:MultiSurface><gml:surfaceMember>{}</gml:surfaceMember></gml:MultiSurface></bldg:lod{lod}MultiSurface>",
            polygon(&format!("l{lod}"), &square(0.0, 0.0, side, 2.0))
        )
    };
    for inner in [at(1, 1.0) + &at(2, 3.0), at(2, 3.0) + &at(1, 1.0)] {
        let out = read("model.gml", &lonlat(&building("b1", &inner)), None).unwrap();
        let ds = &out[0].0;
        assert!((area(ds.features[0].geometry.as_ref()) - 9.0).abs() < 1e-6, "{:?}", ds.features[0].geometry);
        assert_eq!(value(ds, 0, "lod"), Value::Text("2".into()));
    }
}

/// Geometries as GML writes them less often: a curve given backwards, `gml:coord`, a point with more
/// than one position, a solid with no exterior, a line with a point given by a link, and a relief's
/// points with no id.
#[test]
fn less_common_geometries() {
    let lod2 = |surface: &str| {
        format!(
            "<bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember>{surface}</gml:surfaceMember></gml:MultiSurface></bldg:lod2MultiSurface>"
        )
    };
    // A concave ring of two curves, the second written backwards: 75, not a crossed ring.
    let ring = r#"<gml:Polygon><gml:exterior><gml:Ring>
        <gml:curveMember><gml:LineString><gml:posList>0 0 0 10 0 0 10 10 0</gml:posList></gml:LineString></gml:curveMember>
        <gml:curveMember><gml:OrientableCurve orientation="-"><gml:baseCurve><gml:LineString><gml:posList>0 0 0 0 10 0 5 5 0 10 10 0</gml:posList></gml:LineString></gml:baseCurve></gml:OrientableCurve></gml:curveMember>
        </gml:Ring></gml:exterior></gml:Polygon>"#;
    let out = read("model.gml", &lonlat(&building("b1", &lod2(ring))), None).unwrap();
    assert!((area(out[0].0.features[0].geometry.as_ref()) - 75.0).abs() < 1e-6, "{:?}", out[0].0.features[0].geometry);

    // GML 2's `gml:coord`.
    let coords: String = [(0, 0), (4, 0), (4, 4), (0, 4), (0, 0)]
        .iter()
        .map(|(x, y)| format!("<gml:coord><gml:X>{x}</gml:X><gml:Y>{y}</gml:Y><gml:Z>1</gml:Z></gml:coord>"))
        .collect();
    let face = format!("<gml:Polygon><gml:exterior><gml:LinearRing>{coords}</gml:LinearRing></gml:exterior></gml:Polygon>");
    let out = read("model.gml", &lonlat(&building("b2", &lod2(&face))), None).unwrap();
    assert!((area(out[0].0.features[0].geometry.as_ref()) - 16.0).abs() < 1e-6);
    assert!(!says(&out, "can't be read"), "{:?}", messages_of(&out));

    // A point is its first position, heights included, and more positions aren't kept, said.
    let point = r#"<core:cityObjectMember><frn:CityFurniture gml:id="f1"><frn:lod1Geometry><gml:Point><gml:pos>1 1 10</gml:pos><gml:pos>2 2 100</gml:pos></gml:Point></frn:lod1Geometry></frn:CityFurniture></core:cityObjectMember>"#;
    let out = read("model.gml", &lonlat(point), None).unwrap();
    assert_eq!(value(&out[0].0, 0, "z_max"), Value::Float(10.0));
    assert!(says(&out, "1 gml:Points give more than one position"), "{:?}", messages_of(&out));
    // A first position without a height and a 3D one after it: the point keeps its own none.
    let mixed = point.replace("<gml:pos>1 1 10</gml:pos>", "<gml:pos>1 1</gml:pos>");
    let out = read("model.gml", &lonlat(&mixed), None).unwrap();
    assert_eq!(value(&out[0].0, 0, "z_max"), Value::Null, "the first position's own, not a later one's");
    // The same place as the first, at height 0 in the second: still the first has none.
    let same_place = point
        .replace("<gml:pos>1 1 10</gml:pos>", "<gml:pos>1 1</gml:pos>")
        .replace("<gml:pos>2 2 100</gml:pos>", "<gml:pos>1 1 0</gml:pos>");
    let out = read("model.gml", &lonlat(&same_place), None).unwrap();
    assert_eq!(value(&out[0].0, 0, "z_max"), Value::Null, "a 2D first point isn't given the later 3D one's height");

    // A solid with only a cavity lacks its exterior, and is said.
    let cavity = format!(
        "<bldg:lod1Solid><gml:Solid><gml:interior><gml:CompositeSurface><gml:surfaceMember>{}</gml:surfaceMember></gml:CompositeSurface></gml:interior></gml:Solid></bldg:lod1Solid>{}",
        polygon("c", &square(2.0, 2.0, 2.0, 0.0)),
        box_surfaces("b3")
    );
    let out = read("model.gml", &lonlat(&building("b3", &cavity)), None).unwrap();
    assert!(says(&out, "lack a part they need"), "{:?}", messages_of(&out));
    assert!((area(out[0].0.features[0].geometry.as_ref()) - 100.0).abs() < 1e-6);

    // A line with a point given by a link isn't whole: said as a link, and left out.
    let line = format!(
        r##"<bldg:lod1MultiCurve><gml:MultiCurve><gml:curveMember><gml:LineString><gml:pos>0 0 0</gml:pos><gml:pointProperty xlink:href="#p"/><gml:pos>9 9 0</gml:pos></gml:LineString></gml:curveMember></gml:MultiCurve></bldg:lod1MultiCurve>{}"##,
        box_surfaces("b4")
    );
    let out = read("model.gml", &lonlat(&building("b4", &line)), None).unwrap();
    assert!(says(&out, "1 geometries or objects are given only by a link"), "{:?}", messages_of(&out));
    // So is a ring with a curve whose base curve is a link.
    let ring = r##"<gml:Polygon><gml:exterior><gml:Ring>
        <gml:curveMember><gml:LineString><gml:posList>0 0 0 10 0 0 10 10 0</gml:posList></gml:LineString></gml:curveMember>
        <gml:curveMember><gml:OrientableCurve orientation="-"><gml:baseCurve xlink:href="#c"/></gml:OrientableCurve></gml:curveMember>
        </gml:Ring></gml:exterior></gml:Polygon>"##;
    let out = read("model.gml", &lonlat(&building("b5", &(lod2(ring) + &box_surfaces("b5")))), None).unwrap();
    assert!(says(&out, "1 geometries or objects are given only by a link"), "{:?}", messages_of(&out));
    assert!((area(out[0].0.features[0].geometry.as_ref()) - 100.0).abs() < 1e-6);

    // A relief's mass points with no id are an object of their relief.
    let relief = r#"<core:cityObjectMember><dem:ReliefFeature gml:id="r1"><dem:reliefComponent><dem:MassPointRelief><dem:reliefPoints><gml:MultiPoint><gml:pointMember><gml:Point><gml:pos>1 1 3</gml:pos></gml:Point></gml:pointMember></gml:MultiPoint></dem:reliefPoints></dem:MassPointRelief></dem:reliefComponent></dem:ReliefFeature></core:cityObjectMember>"#;
    let out = read("model.gml", &lonlat(relief), None).unwrap();
    let points = &out
        .iter()
        .find(|(ds, _)| ds.name == "model MassPointRelief" || ds.name == "model")
        .unwrap_or_else(|| panic!("{:?}", out.iter().map(|(d, _)| &d.name).collect::<Vec<_>>()))
        .0;
    assert_eq!(value(points, 0, "parent"), Value::Text("r1".into()));
}

/// A hole in a ring is kept (the `area` helper takes holes off); an arc's centre isn't a vertex and
/// an arc by its centre alone gives nothing; an oriented curve's line is read with its orientation,
/// said with the arcs; a solid whose exterior is a link isn't read as its cavity.
#[test]
fn less_common_geometries_of_odd_rings_curves_and_solids() {
    // A ground with a hole keeps it: the footprint isn't filled in.
    let ground = format!(
        r#"<gml:Polygon gml:id="h"><gml:exterior><gml:LinearRing><gml:posList>{}</gml:posList></gml:LinearRing></gml:exterior><gml:interior><gml:LinearRing><gml:posList>{}</gml:posList></gml:LinearRing></gml:interior></gml:Polygon>"#,
        square(0.0, 0.0, 10.0, 0.0),
        square(4.0, 4.0, 2.0, 0.0)
    );
    let inner = format!(
        "<bldg:lod0FootPrint><bldg:GroundSurface><bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember>{ground}</gml:surfaceMember></gml:MultiSurface></bldg:lod2MultiSurface></bldg:GroundSurface></bldg:lod0FootPrint>"
    );
    let out = read("model.gml", &lonlat(&building("b1", &inner)), None).unwrap();
    if let Some(Geometry::Polygon(polys)) = &out[0].0.features[0].geometry {
        assert_eq!(polys[0].len(), 2, "the hole is kept: {:?}", polys[0].len());
    } else {
        panic!("{:?}", out[0].0.features[0].geometry);
    }
    assert!((area(out[0].0.features[0].geometry.as_ref()) - 96.0).abs() < 1e-6, "{:?}", out[0].0.features[0].geometry);

    // The centre an arc is given by isn't a vertex: the straight part of a curve is read as
    // written, said with the arcs, and an arc by its centre alone gives nothing.
    let arc = r#"<gml:ArcByCenterPoint numArc="1" radius="5"><gml:pos>5 -5 0</gml:pos><gml:startAngle>0</gml:startAngle><gml:endAngle>3.14</gml:endAngle></gml:ArcByCenterPoint>"#;
    let curve = |segments: &str| {
        format!(
            r#"<bldg:lod1MultiCurve><gml:MultiCurve><gml:curveMember><gml:Curve><gml:segments>{segments}</gml:segments></gml:Curve></gml:curveMember></gml:MultiCurve></bldg:lod1MultiCurve>"#
        )
    };
    let straight = "<gml:LineStringSegment><gml:posList>0 0 0 10 0 0</gml:posList></gml:LineStringSegment>";
    let out = read("model.gml", &lonlat(&building("b2", &curve(&format!("{straight}{arc}")))), None).unwrap();
    if let Some(Geometry::Line(parts)) = &out[0].0.features[0].geometry {
        assert_eq!(parts[0].len(), 2, "the straight segment's points only: {:?}", parts);
    } else {
        panic!("{:?}", out[0].0.features[0].geometry);
    }
    assert!(says(&out, "1 curves have arcs or splines"), "{:?}", messages_of(&out));
    let out = read("model.gml", &lonlat(&building("b2", &curve(arc))), None).unwrap();
    assert!(says(&out, "1 geometries of city objects have coordinates that can't be read"), "{:?}", messages_of(&out));
    assert!(out[0].0.features.iter().all(|f| f.geometry.is_none()), "no segment left to read");

    // A ring held in curves with an arc: read through its points, and said like a Curve's are.
    let ring = "<gml:Polygon><gml:exterior><gml:Ring><gml:curveMember><gml:Curve><gml:segments><gml:Arc><gml:posList>0 0 0 5 5 0 10 0 0</gml:posList></gml:Arc></gml:segments></gml:Curve></gml:curveMember><gml:curveMember><gml:LineString><gml:posList>10 0 0 10 10 0 0 10 0 0 0 0</gml:posList></gml:LineString></gml:curveMember></gml:Ring></gml:exterior></gml:Polygon>";
    let inner = format!(
        "<bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember>{ring}</gml:surfaceMember></gml:MultiSurface></bldg:lod2MultiSurface>"
    );
    let out = read("model.gml", &lonlat(&building("b5", &inner)), None).unwrap();
    assert!(says(&out, "1 curves have arcs or splines"), "{:?}", messages_of(&out));
    assert!(out[0].0.features[0].geometry.is_some(), "{:?}", messages_of(&out));

    // An oriented curve of its own is read, written the other way when its orientation is `-`.
    let oriented = r#"<bldg:lod1MultiCurve><gml:MultiCurve><gml:curveMember><gml:OrientableCurve orientation="-"><gml:baseCurve><gml:LineString><gml:posList>0 0 0 10 0 0</gml:posList></gml:LineString></gml:baseCurve></gml:OrientableCurve></gml:curveMember></gml:MultiCurve></bldg:lod1MultiCurve>"#;
    let out = read("model.gml", &lonlat(&building("b2", oriented)), None).unwrap();
    if let Some(Geometry::Line(parts)) = &out[0].0.features[0].geometry {
        let (Some(first), Some(last)) = (parts[0].first(), parts[0].last()) else { panic!("{:?}", parts) };
        assert_eq!((first.x, first.y), (10.0, 0.0), "from the end first: {first:?}");
        assert_eq!((last.x, last.y), (0.0, 0.0), "to the start last: {last:?}");
    } else {
        panic!("{:?}", out[0].0.features[0].geometry);
    }

    // A solid whose exterior is a link: its cavity isn't drawn apart, and the link is said.
    let cavity = format!(
        "<bldg:lod1Solid><gml:Solid><gml:exterior xlink:href=\"#shell\"/><gml:interior><gml:CompositeSurface><gml:surfaceMember>{}</gml:surfaceMember></gml:CompositeSurface></gml:interior></gml:Solid></bldg:lod1Solid>{}",
        polygon("c", &square(9.0, 9.0, 2.0, 0.0)),
        box_surfaces("b3")
    );
    let out = read("model.gml", &lonlat(&building("b3", &cavity)), None).unwrap();
    assert!(says(&out, "only by a link"), "{:?}", messages_of(&out));
    assert!(!says(&out, "can't be read"), "{:?}", messages_of(&out));
    assert!((area(out[0].0.features[0].geometry.as_ref()) - 100.0).abs() < 1e-6, "{:?}", out[0].0.features[0].geometry);

    // A part with no id of its own still carries the objects it holds.
    let wrapper = format!(
        r#"<bldg:consistsOfBuildingPart><bldg:BuildingPart><bldg:consistsOfBuildingPart><bldg:BuildingPart gml:id="deep">{}</bldg:BuildingPart></bldg:consistsOfBuildingPart></bldg:BuildingPart></bldg:consistsOfBuildingPart>"#,
        box_surfaces("deep")
    );
    let out = read("model.gml", &lonlat(&building("b4", &wrapper)), None).unwrap();
    let part = layer(&out, "model BuildingPart");
    assert_eq!(part.features.iter().filter(|f| f.geometry.is_some()).count(), 1, "drawn through its wrapper");
    assert_eq!(value(part, 1, "type"), Value::Text("BuildingPart".into()));
    assert!(says(&out, "city objects have no gml:id"), "{:?}", messages_of(&out));
}

/// A file cut right after a city object's end tag, before its member's, keeps that object; a
/// polygon whose exterior is a link is said as a link only.
#[test]
fn a_city_object_finished_before_a_cut_is_kept_and_a_linked_exterior_is_said_once() {
    let text = String::from_utf8(lonlat(&building("b1", &box_surfaces("b1")))).unwrap();
    let end = text.find("</bldg:Building>").unwrap() + "</bldg:Building>".len();
    let out = read("model.gml", &text.as_bytes()[..end], None).unwrap();
    assert!(out[0].0.features.iter().any(|f| f.geometry.is_some()), "{:?}", messages_of(&out));
    assert!(says(&out, "cut short"), "{:?}", messages_of(&out));

    let ring = r##"<gml:Polygon gml:id="p"><gml:exterior xlink:href="#ring"/></gml:Polygon>"##;
    let inner = format!(
        "<bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember>{ring}</gml:surfaceMember></gml:MultiSurface></bldg:lod2MultiSurface>"
    );
    let out = read("model.gml", &lonlat(&building("b2", &inner)), None).unwrap();
    assert!(says(&out, "only by a link"), "{:?}", messages_of(&out));
    assert!(!says(&out, "shape that can't be drawn"), "{:?}", messages_of(&out));
}

/// A geometry left out after some of its points were read leaves them out of the file's reach: with no
/// system named, the file's other points still make it longitude/latitude.
#[test]
fn a_geometry_left_out_leaves_no_point_in_the_reach() {
    let bad = "<core:cityObjectMember><bldg:Building gml:id=\"bad\"><bldg:lod1Geometry><gml:MultiGeometry>\
        <gml:geometryMember><gml:LineString><gml:posList>500000 600000 0 500010 600000 0</gml:posList></gml:LineString></gml:geometryMember>\
        <gml:geometryMember><gml:LineString><gml:posList>1 2 3 abc</gml:posList></gml:LineString></gml:geometryMember>\
        </gml:MultiGeometry></bldg:lod1Geometry></bldg:Building></core:cityObjectMember>";
    let members = format!("{}{bad}", building("ok", &box_surfaces("ok")));
    let text = String::from_utf8(model("EPSG:4326", &members)).unwrap().replace(" srsName=\"EPSG:4326\"", "");
    let out = read("model.gml", text.as_bytes(), None).unwrap();
    assert!(says(&out, "coordinates are read as longitude/latitude"), "{:?}", messages_of(&out));
    assert!(says(&out, "coordinates that can't be read"), "{:?}", messages_of(&out));
    assert!(out[0].0.features.iter().any(|f| f.geometry.is_some()), "{:?}", messages_of(&out));
}

/// Ids are `gml:id`s, an extension's metadata named for a level of detail is a value, a terrain
/// intersection doesn't make a feature an object, and a file cut inside a character is read up to it.
#[test]
fn ids_values_and_a_cut_inside_a_character() {
    // An extension's `id` before `gml:id`: the object's id is the GML one.
    let named = format!(
        r#"<core:cityObjectMember><bldg:Building xmlns:x="urn:x" id="plain" x:id="ext" gml:id="b1">{}</bldg:Building></core:cityObjectMember>"#,
        box_surfaces("b1")
    );
    let out = read("model.gml", &lonlat(&named), None).unwrap();
    assert_eq!(value(&out[0].0, 0, "id"), Value::Text("b1".into()));

    // Metadata named for a level of detail is kept as a value, and isn't taken for a geometry.
    let meta = format!("<bldg:lod2Metadata><bldg:source>survey</bldg:source></bldg:lod2Metadata>{}", box_surfaces("b2"));
    let out = read("model.gml", &lonlat(&building("b2", &meta)), None).unwrap();
    assert_ne!(value(&out[0].0, 0, "lod2Metadata"), Value::Null);
    assert!(!says(&out, "unknown types"), "{:?}", messages_of(&out));

    // A part with no id holding only a terrain intersection isn't an object: it is said.
    let curve = "<gml:MultiCurve><gml:curveMember><gml:LineString><gml:posList>0 0 0 1 0 0</gml:posList></gml:LineString></gml:curveMember></gml:MultiCurve>";
    let inner = format!(
        "<bldg:consistsOfBuildingPart><bldg:BuildingPart>{}</bldg:BuildingPart><bldg:BuildingPart><bldg:lod2TerrainIntersection>{curve}</bldg:lod2TerrainIntersection></bldg:BuildingPart></bldg:consistsOfBuildingPart>",
        box_surfaces("p")
    );
    let out = read("model.gml", &lonlat(&building("b3", &inner)), None).unwrap();
    assert!(says(&out, "1 nested features with no gml:id and no geometry"), "{:?}", messages_of(&out));

    // Cut inside the "é" of the second building's name: the first is read, and the cut said.
    let whole = lonlat(
        &[building("b4", &box_surfaces("b4")), building("b5", &format!("<gml:name>Café</gml:name>{}", box_surfaces("b5")))].concat(),
    );
    let at = whole.windows(2).position(|w| w == "é".as_bytes()).unwrap() + 1;
    let out = read("model.gml", &whole[..at], None).unwrap();
    assert_eq!(out[0].0.features.len(), 1);
    assert!(says(&out, "as if cut short"), "{:?}", messages_of(&out));
    // Text that isn't UTF-8 in the middle is still refused.
    let mut broken = whole.clone();
    broken[at] = b'x';
    assert!(read("model.gml", &broken, None).is_err());

    // A latitude-first EPSG:4326 model, given EPSG:4326 by its code: read latitude first.
    let inner = format!(
        r#"<bldg:lod2MultiSurface><gml:MultiSurface srsName="urn:ogc:def:crs:EPSG::4326"><gml:surfaceMember>{}</gml:surfaceMember></gml:MultiSurface></bldg:lod2MultiSurface>"#,
        polygon("q", &square(52.0, 4.0, 0.001, 0.0))
    );
    let out = read("model.gml", &model("urn:ogc:def:crs:EPSG::4326", &building("b6", &inner)), Some(Crs::Epsg(4326))).unwrap();
    let e = out[0].0.extent();
    assert!((e.min_x - 4.0).abs() < 0.01 && (e.min_y - 52.0).abs() < 0.01, "{e:?}");
}

/// A coordinate system given takes the place of every one the file names, names that disagree
/// included, and says how many it read in its place; with none given they are refused. One system
/// with its axes in different orders is refused either way.
#[test]
fn a_system_given_reads_a_file_naming_several() {
    let ground = square(500_000.0, 5_500_000.0, 10.0, 0.0);
    let inner = format!(
        r#"<bldg:boundedBy><bldg:GroundSurface><bldg:lod2MultiSurface><gml:MultiSurface srsName="EPSG:25833"><gml:surfaceMember>{}</gml:surfaceMember></gml:MultiSurface></bldg:lod2MultiSurface></bldg:GroundSurface></bldg:boundedBy>"#,
        polygon("g", &ground)
    );
    let bytes = model("EPSG:25832", &building("b1", &inner));
    let err = read("model.gml", &bytes, None).unwrap_err().to_string();
    assert!(err.contains("it names different coordinate systems") && err.contains("crs"), "{err}");
    let out = read("model.gml", &bytes, Some(Crs::Epsg(25832))).unwrap();
    assert_eq!(out[0].0.crs, Some(Crs::Epsg(25832)));
    assert!(
        says(&out, "1 geometries or envelopes name a coordinate system other than the file's first (EPSG:25833), and are read in"),
        "{:?}",
        messages_of(&out)
    );
    // In an archive the system a model names comes first, so the names still disagree, and the
    // refusal doesn't ask for the system already given.
    let err = read_member("model.gml", &bytes, Some(Crs::Epsg(25832))).unwrap_err().to_string();
    assert!(err.contains("in an archive a model keeps the systems it names") && !err.contains("give the coordinate system"), "{err}");
    // Names that agree say nothing of it.
    let same = model("EPSG:25832", &building("b1", &inner.replace("EPSG:25833", "EPSG:25832")));
    let out = read("model.gml", &same, Some(Crs::Epsg(25832))).unwrap();
    assert!(!says(&out, "name a coordinate system other than"), "{:?}", messages_of(&out));
    // Axes in different orders: the order a name gives is how its coordinates are read.
    let swapped = inner.replace("EPSG:25833", "urn:ogc:def:crs:EPSG::4326");
    let err = read("model.gml", &model("EPSG:4326", &building("b1", &swapped)), Some(Crs::Epsg(4326))).unwrap_err().to_string();
    assert!(err.contains("axes in different orders"), "{err}");
}

/// A thematic surface's or an opening's own properties aren't kept, and are counted; surfaces with
/// only their geometry and openings say nothing.
#[test]
fn surface_and_opening_properties_are_said() {
    let wall = polygon("w", "0 0 0 10 0 0 10 0 5 0 0 5 0 0 0");
    let window = polygon("o", "2 0 1 4 0 1 4 0 3 2 0 3 2 0 1");
    let inner = format!(
        r#"<bldg:boundedBy><bldg:WallSurface><gml:name>north wall</gml:name><gml:boundedBy><gml:Envelope srsName="EPSG:4326"><gml:lowerCorner>0 0 0</gml:lowerCorner><gml:upperCorner>10 0 5</gml:upperCorner></gml:Envelope></gml:boundedBy>
        <bldg:lod3MultiSurface><gml:MultiSurface><gml:surfaceMember>{wall}</gml:surfaceMember></gml:MultiSurface></bldg:lod3MultiSurface>
        <bldg:opening><bldg:Window><gen:stringAttribute name="glass"><gen:value>double</gen:value></gen:stringAttribute>
        <bldg:lod3MultiSurface><gml:MultiSurface><gml:surfaceMember>{window}</gml:surfaceMember></gml:MultiSurface></bldg:lod3MultiSurface></bldg:Window></bldg:opening>
        </bldg:WallSurface></bldg:boundedBy>{}"#,
        box_surfaces("b1")
    );
    let out = read("model.gml", &lonlat(&building("b1", &inner)), None).unwrap();
    assert!(
        says(&out, "2 thematic surfaces or openings (walls, roofs, windows, doors) have properties of their own"),
        "{:?}",
        messages_of(&out)
    );
    let out = read("model.gml", &lonlat(&building("b1", &box_surfaces("b1"))), None).unwrap();
    assert!(!says(&out, "properties of their own"), "{:?}", messages_of(&out));
    // A surface's appearance is said as an appearance, not as a property of its own.
    let painted = format!(
        "<bldg:boundedBy><bldg:RoofSurface><app:appearance><app:Appearance><app:theme>t</app:theme></app:Appearance></app:appearance>\
         <bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember>{}</gml:surfaceMember></gml:MultiSurface></bldg:lod2MultiSurface></bldg:RoofSurface></bldg:boundedBy>{}",
        polygon("p", &square(0.0, 0.0, 10.0, 5.0)),
        box_surfaces("b1")
    );
    let out = read("model.gml", &lonlat(&building("b1", &painted)), None).unwrap();
    assert!(says(&out, "the file's appearances (textures and materials) aren't read: 1 are left out"), "{:?}", messages_of(&out));
    assert!(!says(&out, "properties of their own"), "{:?}", messages_of(&out));
}

/// A value's attributes other than its unit and code list (a language) aren't kept, and are said;
/// values with only those give no notice.
#[test]
fn value_attributes_that_are_not_kept_are_said() {
    let named = format!(
        r#"<gml:name xml:lang="de">Rathaus</gml:name><gml:name xml:lang="en">Town hall</gml:name><bldg:measuredHeight uom="m">12.5</bldg:measuredHeight>{}"#,
        box_surfaces("b1")
    );
    let out = read("model.gml", &lonlat(&building("b1", &named)), None).unwrap();
    assert!(says(&out, "2 values have attributes other than their unit (uom) or code list (codeSpace)"), "{:?}", messages_of(&out));
    let plain = format!(
        r#"<gml:name>Town hall</gml:name><bldg:function codeSpace="functions.xml">1000</bldg:function><bldg:measuredHeight uom="m">12.5</bldg:measuredHeight>{}"#,
        box_surfaces("b1")
    );
    let out = read("model.gml", &lonlat(&building("b1", &plain)), None).unwrap();
    assert!(!says(&out, "values have attributes other than"), "{:?}", messages_of(&out));
}

/// A generic attribute's value says what it carries that isn't kept (a language, a code list), and
/// a set's member with no name; a clean one, or a measure's unit, gives none of it.
#[test]
fn generic_value_attributes_that_are_not_kept_are_said() {
    let inner = format!(
        r#"<gen:stringAttribute name="label"><gen:value xml:lang="de">Rathaus</gen:value></gen:stringAttribute>
        <gen:stringAttribute name="kind"><gen:value codeSpace="kinds.xml">1</gen:value></gen:stringAttribute>
        <gen:genericAttributeSet name="survey"><gen:intAttribute><gen:value>3</gen:value></gen:intAttribute></gen:genericAttributeSet>{}"#,
        box_surfaces("b1")
    );
    let out = read("model.gml", &lonlat(&building("b1", &inner)), None).unwrap();
    assert!(says(&out, "1 values have attributes other than their unit (uom) or code list (codeSpace)"), "{:?}", messages_of(&out));
    assert!(says(&out, "1 values name the code list they are taken from (codeSpace)"), "{:?}", messages_of(&out));
    assert!(
        says(&out, "1 generic attributes (genericAttribute, or a member of a genericAttributeSet) have no name"),
        "{:?}",
        messages_of(&out)
    );
    let clean = format!(
        r#"<gen:stringAttribute name="label"><gen:value>Town hall</gen:value></gen:stringAttribute>
        <gen:measureAttribute name="depth"><gen:value uom="m">2</gen:value></gen:measureAttribute>{}"#,
        box_surfaces("b1")
    );
    let out = read("model.gml", &lonlat(&building("b1", &clean)), None).unwrap();
    for text in ["values have attributes other than", "codeSpace", "have no name"] {
        assert!(!says(&out, text), "{text}: {:?}", messages_of(&out));
    }
}

/// What isn't a feature in a city object member (malformed) isn't read as a city object, and is said.
#[test]
fn a_member_holding_no_feature_is_said() {
    let members = format!("{}<core:cityObjectMember><gml:name>x</gml:name></core:cityObjectMember>", building("b1", &box_surfaces("b1")));
    let out = read("model.gml", &lonlat(&members), None).unwrap();
    assert!(says(&out, "1 elements in city object members aren't city objects"), "{:?}", messages_of(&out));
    assert!(!says(&out, "no geometry of their own"), "{:?}", messages_of(&out));
    assert!(out.iter().all(|(ds, _)| !ds.name.to_ascii_lowercase().contains("name")), "no layer for the name");
    let out = read("model.gml", &lonlat(&building("b1", &box_surfaces("b1"))), None).unwrap();
    assert!(!says(&out, "aren't city objects"), "{:?}", messages_of(&out));
}

/// A `featureMembers` given only by a link isn't followed, and is said with the other links.
#[test]
fn a_feature_members_link_is_said() {
    for link in [r##"<gml:featureMembers xlink:href="#all"/>"##, r##"<gml:featureMembers xlink:href="#all"></gml:featureMembers>"##] {
        let members = format!("{}{link}", building("b1", &box_surfaces("b1")));
        let out = read("model.gml", &lonlat(&members), None).unwrap();
        assert!(says(&out, "1 geometries or objects are given only by a link"), "{link}: {:?}", messages_of(&out));
    }
}

/// A read counts its members and stops between them once its run is cancelled; without a run to
/// answer to it reads them all.
#[test]
fn a_cancelled_read_stops_between_members() {
    use crate::progress::tests::Counted;
    use std::sync::atomic::Ordering;
    let members: String = (0..5).map(|i| building(&format!("b{i}"), &box_surfaces(&format!("b{i}")))).collect();
    let bytes = lonlat(&members);
    let probe = Counted::cancelling_after(2);
    let res = {
        let _g = crate::progress::install(Some(probe.clone()));
        read("model.gml", &bytes, None)
    };
    assert!(matches!(res, Err(DataError::Cancelled)), "{:?}", res.map(|o| o.len()));
    assert_eq!(probe.total.load(Ordering::Relaxed), 5);
    assert_eq!(read("model.gml", &bytes, None).unwrap()[0].0.features.len(), 5);
}

/// Only GML's namespaces give an object its `gml:id`: an `id` in a namespace that merely starts
/// with GML's name is an extension's, and doesn't.
#[test]
fn gml_ids_come_from_gml_namespaces_only() {
    assert!(gml_namespace("http://www.opengis.net/gml") && gml_namespace("http://www.opengis.net/gml/3.2"));
    assert!(!gml_namespace("http://www.opengis.net/gmlfoo") && !gml_namespace("http://www.opengis.net/gm"));
    let doc = roxmltree::Document::parse(
        r#"<b xmlns:x="http://www.opengis.net/gmlfoo" xmlns:gml="http://www.opengis.net/gml" x:id="wrong" gml:id="right"/>"#,
    )
    .unwrap();
    assert_eq!(gml_id(&doc.root_element()), Some("right"));
    let doc = roxmltree::Document::parse(r#"<b xmlns:x="http://www.opengis.net/gmlfoo" x:id="wrong"/>"#).unwrap();
    assert_eq!(gml_id(&doc.root_element()), None);
}

/// The one layer of a file whose objects are all of one type, named after the file alone.
fn only(out: &[(Dataset, ReadReport)]) -> &Dataset {
    assert_eq!(out.len(), 1, "{:?}", out.iter().map(|(ds, _)| &ds.name).collect::<Vec<_>>());
    &out[0].0
}

/// The row of the object with `id` in a layer.
fn row_of(ds: &Dataset, id: &str) -> usize {
    (0..ds.features.len()).find(|&r| value(ds, r, "id") == Value::Text(id.into())).unwrap_or_else(|| panic!("no row {id}"))
}

/// The first layer of a geometry type.
fn of_type(out: &[(Dataset, ReadReport)], ty: GeometryType) -> &Dataset {
    &out.iter().find(|(ds, _)| ds.geometry_type == ty).unwrap_or_else(|| panic!("no {ty:?} layer")).0
}

/// A link to a geometry written in another member reads it, whichever member comes first.
#[test]
fn a_link_to_another_members_geometry_reads_it() {
    let solid = r##"<bldg:lod2Solid><gml:Solid><gml:exterior><gml:CompositeSurface><gml:surfaceMember xlink:href="#a-g"/><gml:surfaceMember xlink:href="#a-r"/></gml:CompositeSurface></gml:exterior></gml:Solid></bldg:lod2Solid>"##;
    let (a, b) = (building("a", &box_surfaces("a")), building("b", solid));
    for members in [[a.clone(), b.clone()], [b, a]] {
        let out = read("model.gml", &lonlat(&members.concat()), None).unwrap();
        let ds = only(&out);
        let row = row_of(ds, "b");
        assert!((area(ds.features[row].geometry.as_ref()) - 100.0).abs() < 1e-6, "{:?}", messages_of(&out));
        assert_eq!(value(ds, row, "z_max"), Value::Float(5.0));
        assert!(!says(&out, "link"), "{:?}", messages_of(&out));
    }
}

/// What a geometry's text alone loses of where it stands in its member is kept when another
/// member's link reads it: a prefix its member declares, and the dimension an ancestor gives.
#[test]
fn a_geometry_read_from_another_member_keeps_its_namespaces_and_dimension() {
    let written = r#"<core:cityObjectMember xmlns:g2="http://www.opengis.net/gml"><bldg:Building gml:id="c"><bldg:lod2MultiSurface><gml:MultiSurface srsDimension="2"><gml:surfaceMember><gml:Polygon gml:id="flat"><g2:exterior><g2:LinearRing><g2:posList>0 0 10 0 10 10 0 10 0 0</g2:posList></g2:LinearRing></g2:exterior></gml:Polygon></gml:surfaceMember></gml:MultiSurface></bldg:lod2MultiSurface></bldg:Building></core:cityObjectMember>"#;
    let linking = building(
        "d",
        r##"<bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember xlink:href="#flat"/></gml:MultiSurface></bldg:lod2MultiSurface>"##,
    );
    let out = read("model.gml", &lonlat(&[linking.as_str(), written].concat()), None).unwrap();
    let ds = only(&out);
    for id in ["c", "d"] {
        assert!((area(ds.features[row_of(ds, id)].geometry.as_ref()) - 100.0).abs() < 1e-6, "{id}: {:?}", messages_of(&out));
    }
    assert!(!says(&out, "link"), "{:?}", messages_of(&out));
    assert!(!says(&out, "can't be read"), "{:?}", messages_of(&out));
}

/// A `gml:id` under a prefix its member binds to GML is found by a link from another member; one
/// bound to another namespace is no `gml:id`.
#[test]
fn a_gml_id_under_a_members_own_prefix_is_found() {
    let written = |prefix_ns: &str| {
        format!(
            r#"<core:cityObjectMember xmlns:g2="{prefix_ns}"><bldg:Building gml:id="c"><bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember><gml:Polygon g2:id="flat"><gml:exterior><gml:LinearRing><gml:posList>{}</gml:posList></gml:LinearRing></gml:exterior></gml:Polygon></gml:surfaceMember></gml:MultiSurface></bldg:lod2MultiSurface></bldg:Building></core:cityObjectMember>"#,
            square(0.0, 0.0, 10.0, 0.0)
        )
    };
    let linking = building(
        "d",
        r##"<bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember xlink:href="#flat"/></gml:MultiSurface></bldg:lod2MultiSurface>"##,
    );
    let out = read("model.gml", &lonlat(&[linking.as_str(), &written("http://www.opengis.net/gml")].concat()), None).unwrap();
    let ds = only(&out);
    assert!((area(ds.features[row_of(ds, "d")].geometry.as_ref()) - 100.0).abs() < 1e-6, "{:?}", messages_of(&out));
    assert!(!says(&out, "link"), "{:?}", messages_of(&out));
    let out = read("model.gml", &lonlat(&[linking.as_str(), &written("urn:other")].concat()), None).unwrap();
    assert!(says(&out, "1 geometries or objects are given only by a link"), "{:?}", messages_of(&out));
}

/// Namespaces declared deep inside a member can't make indexing grow as their number times their
/// size: past what the file's size allows, a link to what stands in them finds nothing, and is said.
#[test]
fn declarations_nested_past_the_files_size_arent_copied() {
    let long = "x".repeat(200);
    let opens: String = (0..30).map(|k| format!(r#"<gen:stringAttribute xmlns:n{k}="urn:{long}">"#)).collect();
    let closes = "</gen:stringAttribute>".repeat(30);
    let written = format!(
        "<core:cityObjectMember><bldg:Building gml:id=\"c\">{opens}{}{closes}</bldg:Building></core:cityObjectMember>",
        polygon("deep", &square(0.0, 0.0, 10.0, 0.0))
    );
    let linking = building(
        "d",
        r##"<bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember xlink:href="#deep"/></gml:MultiSurface></bldg:lod2MultiSurface>"##,
    );
    let out = read("model.gml", &lonlat(&[linking, written].concat()), None).unwrap();
    assert!(says(&out, "1 geometries or objects are given only by a link"), "{:?}", messages_of(&out));
}

/// A geometry that can't be read is said once, whether a link in another member or its own object
/// reads it first; a solid whose exterior is a link to one is said too.
#[test]
fn a_geometry_that_cant_be_read_is_said_once_in_the_file() {
    let bad = r#"<gml:Polygon gml:id="p"><gml:exterior><gml:LinearRing><gml:posList>1 2 abc</gml:posList></gml:LinearRing></gml:exterior></gml:Polygon>"#;
    let written = building(
        "c",
        &format!(
            "<bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember>{bad}</gml:surfaceMember></gml:MultiSurface></bldg:lod2MultiSurface>"
        ),
    );
    let linking = building(
        "d",
        r##"<bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember xlink:href="#p"/></gml:MultiSurface></bldg:lod2MultiSurface>"##,
    );
    for members in [[written.clone(), linking.clone()], [linking.clone(), written.clone()]] {
        let out = read("model.gml", &lonlat(&members.concat()), None).unwrap();
        assert!(says(&out, "1 geometries of city objects have coordinates that can't be read"), "{:?}", messages_of(&out));
    }
    // A solid with no cavities whose exterior links to it is lost with it, and said.
    let solid = building("e", r##"<bldg:lod2Solid><gml:Solid><gml:exterior xlink:href="#p"/></gml:Solid></bldg:lod2Solid>"##);
    let out = read("model.gml", &lonlat(&[written.as_str(), &solid].concat()), None).unwrap();
    assert!(says(&out, "2 geometries of city objects have coordinates that can't be read"), "{:?}", messages_of(&out));
}

/// A composite with an id holding a geometry that can't be read is said once, however many links
/// read the composite or the geometry; and a link read before the object that writes the geometry
/// doesn't make it said twice.
#[test]
fn a_composite_holding_what_cant_be_read_is_said_once() {
    let bad = r#"<gml:Polygon gml:id="p"><gml:exterior><gml:LinearRing><gml:posList>1 2 abc</gml:posList></gml:LinearRing></gml:exterior></gml:Polygon>"#;
    let inner = format!(
        r##"<bldg:lod1MultiSurface><gml:MultiSurface><gml:surfaceMember><gml:CompositeSurface gml:id="e"><gml:surfaceMember>{bad}</gml:surfaceMember></gml:CompositeSurface></gml:surfaceMember></gml:MultiSurface></bldg:lod1MultiSurface>
        <bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember xlink:href="#e"/><gml:surfaceMember xlink:href="#p"/></gml:MultiSurface></bldg:lod2MultiSurface>
        <bldg:lod3MultiSurface><gml:MultiSurface><gml:surfaceMember xlink:href="#e"/></gml:MultiSurface></bldg:lod3MultiSurface>"##
    );
    let out = read("model.gml", &lonlat(&building("b1", &inner)), None).unwrap();
    assert!(says(&out, "1 geometries of city objects have coordinates that can't be read"), "{:?}", messages_of(&out));
    // The parent's link is followed before its part, where the geometry is written, is read.
    let inner = format!(
        r##"<bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember xlink:href="#p"/></gml:MultiSurface></bldg:lod2MultiSurface>
        <bldg:consistsOfBuildingPart><bldg:BuildingPart gml:id="p1"><bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember>{bad}</gml:surfaceMember></gml:MultiSurface></bldg:lod2MultiSurface></bldg:BuildingPart></bldg:consistsOfBuildingPart>"##
    );
    let out = read("model.gml", &lonlat(&building("b2", &inner)), None).unwrap();
    assert!(says(&out, "1 geometries of city objects have coordinates that can't be read"), "{:?}", messages_of(&out));
}

/// A geometry given only by a link stands for what it points to: a link to it that isn't followed
/// is said for each link, whichever is followed first.
#[test]
fn a_link_to_a_link_is_said_as_that_link() {
    let inner = r##"<bldg:lod2MultiSurface><gml:MultiSurface>
        <gml:surfaceMember><gml:Polygon gml:id="q" xlink:href="#nowhere"/></gml:surfaceMember>
        <gml:surfaceMember xlink:href="#q"/>
        </gml:MultiSurface></bldg:lod2MultiSurface>"##;
    let out = read("model.gml", &lonlat(&building("b1", inner)), None).unwrap();
    assert!(says(&out, "2 geometries or objects are given only by a link"), "{:?}", messages_of(&out));
}

/// A link where a geometry belongs to another member's object, or to a geometry where nothing is
/// drawn (a terrain intersection), isn't followed, and is said.
#[test]
fn a_link_to_what_isnt_drawn_there_is_said() {
    let written = building(
        "c",
        r#"<bldg:lod2TerrainIntersection><gml:MultiCurve gml:id="ti"><gml:curveMember><gml:LineString><gml:posList>0 0 0 10 0 0</gml:posList></gml:LineString></gml:curveMember></gml:MultiCurve></bldg:lod2TerrainIntersection>"#,
    );
    let linking = building(
        "d",
        r##"<bldg:lod2MultiCurve xlink:href="#ti"/><bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember xlink:href="#c"/></gml:MultiSurface></bldg:lod2MultiSurface>"##,
    );
    let out = read("model.gml", &lonlat(&[written, linking].concat()), None).unwrap();
    assert!(
        says(&out, "2 links (xlink:href) where a geometry belongs point to an element that isn't a geometry"),
        "{:?}",
        messages_of(&out)
    );
}

/// A geometry id given twice in a file with links is said, and a link from a third member reads the
/// first.
#[test]
fn a_geometry_id_given_twice_in_the_file_is_said() {
    let members = [
        building("a", &format!("<bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember>{}</gml:surfaceMember></gml:MultiSurface></bldg:lod2MultiSurface>", polygon("x", &square(0.0, 0.0, 10.0, 0.0)))),
        building("b", &format!("<bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember>{}</gml:surfaceMember></gml:MultiSurface></bldg:lod2MultiSurface>", polygon("x", &square(20.0, 0.0, 5.0, 0.0)))),
        building("c", r##"<bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember xlink:href="#x"/></gml:MultiSurface></bldg:lod2MultiSurface>"##),
    ]
    .concat();
    let out = read("model.gml", &lonlat(&members), None).unwrap();
    assert!(says(&out, "1 geometries have a gml:id an earlier element of the file has too"), "{:?}", messages_of(&out));
    let ds = only(&out);
    assert!((area(ds.features[row_of(ds, "c")].geometry.as_ref()) - 100.0).abs() < 1e-6, "the first");
}

/// A part given by a link is its building's child, wherever it is written: the building takes its
/// heights. A link to nothing, or to the building itself, is said, and kept as an attribute.
#[test]
fn a_part_given_by_a_link_is_a_child() {
    let hall = r##"<core:cityObjectMember><bldg:Building gml:id="hall"><gml:name>Hall</gml:name>
        <bldg:consistsOfBuildingPart xlink:href="#p1"/><bldg:consistsOfBuildingPart xlink:href="#gone"/><bldg:consistsOfBuildingPart xlink:href="#hall"/>
        </bldg:Building></core:cityObjectMember>"##;
    let part = format!(
        r#"<core:cityObjectMember><bldg:BuildingPart gml:id="p1">{}</bldg:BuildingPart></core:cityObjectMember>"#,
        box_surfaces("p1")
    );
    let out = read("model.gml", &lonlat(&[hall, &part].concat()), None).unwrap();
    let parts = layer(&out, "model BuildingPart");
    assert_eq!(value(parts, 0, "parent"), Value::Text("hall".into()));
    let buildings = layer(&out, "model Building");
    assert_eq!(value(buildings, 0, "z_max"), Value::Float(5.0), "raised by its part");
    assert!(says(&out, "2 links between city objects"), "{:?}", messages_of(&out));
    assert!(!says(&out, "given only by a link"), "{:?}", messages_of(&out));
}

/// A group is its members' parent, as in CityJSON, and its own parent (`grp:parent`) is the
/// group's; a member's role isn't kept, and is said.
#[test]
fn a_group_is_its_members_parent() {
    let members = [
        r##"<core:cityObjectMember><grp:CityObjectGroup gml:id="g"><grp:groupMember role="main" xlink:href="#b1"/><grp:parent xlink:href="#site"/></grp:CityObjectGroup></core:cityObjectMember>"##.to_string(),
        building("b1", &box_surfaces("b1")),
        r#"<core:cityObjectMember><bldg:Building gml:id="site"><gml:name>Site</gml:name></bldg:Building></core:cityObjectMember>"#.to_string(),
    ]
    .concat();
    let out = read("model.gml", &lonlat(&members), None).unwrap();
    let buildings = layer(&out, "model Building");
    assert_eq!(value(buildings, row_of(buildings, "b1"), "parent"), Value::Text("g".into()));
    assert_eq!(value(buildings, row_of(buildings, "site"), "z_max"), Value::Float(5.0), "raised through the group");
    let groups = layer(&out, "model CityObjectGroup");
    assert_eq!(value(groups, 0, "parent"), Value::Text("site".into()));
    assert!(says(&out, "1 members of groups (groupMember) have a role"), "{:?}", messages_of(&out));
    assert!(!says(&out, "links between city objects"), "{:?}", messages_of(&out));
}

/// A city furniture whose implicit geometry is `implicit`.
fn furniture(id: &str, implicit: &str) -> String {
    format!(
        r#"<core:cityObjectMember><frn:CityFurniture gml:id="{id}"><frn:lod2ImplicitRepresentation><core:ImplicitGeometry>{implicit}</core:ImplicitGeometry></frn:lod2ImplicitRepresentation></frn:CityFurniture></core:cityObjectMember>"#
    )
}

fn reference(x: f64, y: f64, z: f64) -> String {
    format!("<core:referencePoint><gml:Point><gml:pos>{x} {y} {z}</gml:pos></gml:Point></core:referencePoint>")
}

/// An implicit geometry is its prototype placed by its matrix at its reference point, with its
/// heights: written in place (its own `srsName`, a local system, refuses nothing) or linked from
/// another member. One whose matrix can't be read is its reference point, and said.
#[test]
fn an_implicit_geometry_is_its_prototype_placed() {
    let prototype = format!(
        r#"<core:relativeGMLGeometry><gml:MultiSurface gml:id="proto" srsName="local"><gml:surfaceMember>{}</gml:surfaceMember><gml:surfaceMember>{}</gml:surfaceMember></gml:MultiSurface></core:relativeGMLGeometry>"#,
        polygon("proto-f", &square(0.0, 0.0, 1.0, 0.0)),
        polygon("proto-r", &square(0.0, 0.0, 1.0, 1.0))
    );
    let members = [
        furniture(
            "t1",
            &format!(
                "<core:transformationMatrix>2 0 0 0 0 2 0 0 0 0 2 0 0 0 0 1</core:transformationMatrix>{prototype}{}",
                reference(5.0, 5.0, 1.0)
            ),
        ),
        furniture("t2", &format!(r##"<core:relativeGMLGeometry xlink:href="#proto"/>{}"##, reference(20.0, 20.0, 0.0))),
        furniture(
            "t3",
            &format!(
                r##"<core:transformationMatrix>1 0 0</core:transformationMatrix><core:relativeGMLGeometry xlink:href="#proto"/>{}"##,
                reference(30.0, 30.0, 0.0)
            ),
        ),
    ]
    .concat();
    let out = read("model.gml", &lonlat(&members), None).unwrap();
    let placed = of_type(&out, GeometryType::Polygon);
    let t1 = row_of(placed, "t1");
    assert!((area(placed.features[t1].geometry.as_ref()) - 4.0).abs() < 1e-9, "{:?}", placed.features[t1].geometry);
    assert_eq!((value(placed, t1, "z_min"), value(placed, t1, "z_max")), (Value::Float(1.0), Value::Float(3.0)));
    let t2 = row_of(placed, "t2");
    assert!((area(placed.features[t2].geometry.as_ref()) - 1.0).abs() < 1e-9, "{:?}", placed.features[t2].geometry);
    let Some(Geometry::Polygon(polys)) = placed.features[t2].geometry.as_ref() else { panic!("a polygon") };
    assert!(polys[0][0].iter().all(|c| (20.0..=21.0).contains(&c.x) && (20.0..=21.0).contains(&c.y)), "{polys:?}");
    assert!(says(&out, "1 geometry instances are drawn as a point"), "{:?}", messages_of(&out));
    assert!(!says(&out, "link"), "{:?}", messages_of(&out));
    assert!(!says(&out, "have coordinates that can't be read"), "{:?}", messages_of(&out));
}

/// A prototype's coordinates are in its own system, placed in the file's axis order: in a file read
/// latitude first, its first axis is the latitude, as its reference point's first coordinate is.
#[test]
fn a_prototype_is_placed_in_the_files_axis_order() {
    let prototype = format!(
        "<core:relativeGMLGeometry><gml:MultiSurface><gml:surfaceMember>{}</gml:surfaceMember></gml:MultiSurface></core:relativeGMLGeometry>",
        polygon("wide", "0 0 0 2 0 0 2 1 0 0 1 0 0 0 0")
    );
    let min = |v: &[f64]| {
        let (lo, hi) = v.iter().fold((f64::MAX, f64::MIN), |(lo, hi), &x| (lo.min(x), hi.max(x)));
        (lo, hi - lo)
    };
    for (matrix, (lon_span, lat_span)) in
        [("", (1.0, 2.0)), ("<core:transformationMatrix>1 0 0 0 0 3 0 0 0 0 1 0 0 0 0 1</core:transformationMatrix>", (3.0, 2.0))]
    {
        let furniture = furniture("t", &format!("{matrix}{prototype}{}", reference(5.0, 10.0, 0.0)));
        let out = read("model.gml", &model("urn:ogc:def:crs:EPSG::4326", &furniture), None).unwrap();
        let Some(Geometry::Polygon(polys)) = of_type(&out, GeometryType::Polygon).features[0].geometry.as_ref() else {
            panic!("a polygon")
        };
        let ring = &polys[0][0];
        let (xs, ys): (Vec<f64>, Vec<f64>) = ring.iter().map(|c| (c.x, c.y)).unzip();
        let ((x0, dx), (y0, dy)) = (min(&xs), min(&ys));
        assert!((x0 - 10.0).abs() < 1e-9 && (dx - lon_span).abs() < 1e-9, "{matrix}: {ring:?}");
        assert!((y0 - 5.0).abs() < 1e-9 && (dy - lat_span).abs() < 1e-9, "{matrix}: {ring:?}");
    }
}

/// A prototype that can't be read is said once, however many instances place it: each is drawn at
/// its reference point.
#[test]
fn a_prototype_that_cant_be_read_is_said_once() {
    let bad = r#"<core:relativeGMLGeometry><gml:Polygon gml:id="proto"><gml:exterior><gml:LinearRing><gml:posList>1 2 abc</gml:posList></gml:LinearRing></gml:exterior></gml:Polygon></core:relativeGMLGeometry>"#;
    let members = [
        furniture("t1", &format!("{bad}{}", reference(5.0, 5.0, 0.0))),
        furniture("t2", &format!(r##"<core:relativeGMLGeometry xlink:href="#proto"/>{}"##, reference(6.0, 6.0, 0.0))),
    ]
    .concat();
    let out = read("model.gml", &lonlat(&members), None).unwrap();
    assert!(says(&out, "1 geometries of city objects have coordinates that can't be read"), "{:?}", messages_of(&out));
    assert!(says(&out, "2 geometry instances are drawn as a point"), "{:?}", messages_of(&out));
}

/// A prototype's geometry linked from outside an implicit geometry isn't read as the file's.
#[test]
fn a_prototype_linked_from_outside_is_said() {
    let prototype = format!(
        "<core:relativeGMLGeometry><gml:MultiSurface gml:id=\"proto\"><gml:surfaceMember>{}</gml:surfaceMember></gml:MultiSurface></core:relativeGMLGeometry>",
        polygon("proto-f", &square(0.0, 0.0, 1.0, 0.0))
    );
    let members = [
        furniture("t", &format!("{prototype}{}", reference(5.0, 5.0, 0.0))),
        building("b", r##"<bldg:lod2MultiSurface xlink:href="#proto"/>"##),
    ]
    .concat();
    let out = read("model.gml", &lonlat(&members), None).unwrap();
    assert!(
        says(&out, "1 links (xlink:href) where a geometry belongs point to an element that isn't a geometry"),
        "{:?}",
        messages_of(&out)
    );
}

/// Two geometries given one id are two: each that can't be read is said, and one that can't be read
/// doesn't keep a link from reading the other.
#[test]
fn geometries_given_one_id_are_told_apart() {
    let bad = |object: &str| {
        building(
            object,
            r#"<bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember><gml:Polygon gml:id="p"><gml:exterior><gml:LinearRing><gml:posList>1 2 abc</gml:posList></gml:LinearRing></gml:exterior></gml:Polygon></gml:surfaceMember></gml:MultiSurface></bldg:lod2MultiSurface>"#,
        )
    };
    let out = read("model.gml", &lonlat(&[bad("a"), bad("b")].concat()), None).unwrap();
    assert!(says(&out, "2 geometries of city objects have coordinates that can't be read"), "{:?}", messages_of(&out));
    // The first `p` in the file is whole; the second can't be read; a link from a third member reads
    // the first, whatever the second did.
    let good = building(
        "a",
        &format!(
            "<bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember>{}</gml:surfaceMember></gml:MultiSurface></bldg:lod2MultiSurface>",
            polygon("p", &square(0.0, 0.0, 10.0, 0.0))
        ),
    );
    let linking = building(
        "c",
        r##"<bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember xlink:href="#p"/></gml:MultiSurface></bldg:lod2MultiSurface>"##,
    );
    let out = read("model.gml", &lonlat(&[good, bad("b"), linking].concat()), None).unwrap();
    let ds = only(&out);
    assert!((area(ds.features[row_of(ds, "c")].geometry.as_ref()) - 100.0).abs() < 1e-6, "{:?}", messages_of(&out));
    assert!(says(&out, "1 geometries of city objects have coordinates that can't be read"), "{:?}", messages_of(&out));
}

/// Two prototypes given one id are two: each instance places its own.
#[test]
fn prototypes_given_one_id_are_told_apart() {
    let prototype = |side: f64| {
        format!(
            r#"<core:relativeGMLGeometry><gml:MultiSurface gml:id="proto"><gml:surfaceMember>{}</gml:surfaceMember></gml:MultiSurface></core:relativeGMLGeometry>"#,
            polygon(&format!("f{side}"), &square(0.0, 0.0, side, 0.0))
        )
    };
    let members = [
        furniture("small", &format!("{}{}", prototype(1.0), reference(0.0, 0.0, 0.0))),
        furniture("large", &format!("{}{}", prototype(10.0), reference(50.0, 50.0, 0.0))),
    ]
    .concat();
    let out = read("model.gml", &lonlat(&members), None).unwrap();
    let placed = of_type(&out, GeometryType::Polygon);
    for (id, expected) in [("small", 1.0), ("large", 100.0)] {
        let got = area(placed.features[row_of(placed, id)].geometry.as_ref());
        assert!((got - expected).abs() < 1e-9, "{id}: {got}");
    }
}

/// Another member's geometry that can't be parsed costs only the links to it: the member's other
/// links to other members still read theirs.
#[test]
fn a_copy_that_cant_be_parsed_costs_only_its_links() {
    let broken = r#"<core:cityObjectMember><bldg:Building gml:id="a"><bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember><gml:Polygon gml:id="a1"><gml:exterior><gml:LinearRing><gml:posList>0 0 0 &bogus; 1 1 0 0 0 0</gml:posList></gml:LinearRing></gml:exterior></gml:Polygon></gml:surfaceMember></gml:MultiSurface></bldg:lod2MultiSurface></bldg:Building></core:cityObjectMember>"#;
    let whole = building(
        "c",
        &format!(
            "<bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember>{}</gml:surfaceMember></gml:MultiSurface></bldg:lod2MultiSurface>",
            polygon("c1", &square(0.0, 0.0, 10.0, 0.0))
        ),
    );
    let linking = building(
        "b",
        r##"<bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember xlink:href="#a1"/><gml:surfaceMember xlink:href="#c1"/></gml:MultiSurface></bldg:lod2MultiSurface>"##,
    );
    let out = read("model.gml", &lonlat(&[broken, &whole, &linking].concat()), None).unwrap();
    let ds = only(&out);
    assert!((area(ds.features[row_of(ds, "b")].geometry.as_ref()) - 100.0).abs() < 1e-6, "{:?}", messages_of(&out));
    assert!(says(&out, "1 geometries or objects are given only by a link"), "{:?}", messages_of(&out));
}

/// A part linked twice, or linked and written in its building, has that building as its parent once.
#[test]
fn a_parent_given_twice_is_one() {
    let hall = format!(
        r##"<core:cityObjectMember><bldg:Building gml:id="hall"><bldg:consistsOfBuildingPart xlink:href="#p1"/><bldg:consistsOfBuildingPart xlink:href="#p1"/><bldg:consistsOfBuildingPart xlink:href="#p2"/><bldg:consistsOfBuildingPart><bldg:BuildingPart gml:id="p2">{}</bldg:BuildingPart></bldg:consistsOfBuildingPart></bldg:Building></core:cityObjectMember>"##,
        box_surfaces("p2")
    );
    let part = format!(
        r#"<core:cityObjectMember><bldg:BuildingPart gml:id="p1">{}</bldg:BuildingPart></core:cityObjectMember>"#,
        box_surfaces("p1")
    );
    let out = read("model.gml", &lonlat(&[hall, part].concat()), None).unwrap();
    let parts = layer(&out, "model BuildingPart");
    for id in ["p1", "p2"] {
        assert_eq!(value(parts, row_of(parts, id), "parent"), Value::Text("hall".into()), "{id}");
    }
    assert!(!says(&out, "links between city objects"), "{:?}", messages_of(&out));
}

/// A long chain of geometries each given only by a link to the next is followed once for each, so
/// it costs its length, not its square; each link it couldn't follow is said.
#[test]
fn a_long_chain_of_links_costs_its_length() {
    let chain: String = (0..20_000)
        .map(|k| format!(r##"<gml:surfaceMember><gml:Polygon gml:id="q{k}" xlink:href="#q{}"/></gml:surfaceMember>"##, k + 1))
        .collect();
    let inner = format!("<bldg:lod2MultiSurface><gml:MultiSurface>{chain}</gml:MultiSurface></bldg:lod2MultiSurface>");
    let out = read("model.gml", &lonlat(&building("b1", &inner)), None).unwrap();
    assert!(says(&out, "20000 geometries or objects are given only by a link"), "{:?}", messages_of(&out));
}

/// Only ids a link names are indexed: an id given twice that no link names isn't said.
#[test]
fn an_id_no_link_names_isnt_indexed() {
    let polygon_in = |object: &str, id: &str| {
        building(
            object,
            &format!(
                "<bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember>{}</gml:surfaceMember></gml:MultiSurface></bldg:lod2MultiSurface>",
                polygon(id, &square(0.0, 0.0, 10.0, 0.0))
            ),
        )
    };
    let linking = building(
        "c",
        r##"<bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember xlink:href="#y"/></gml:MultiSurface></bldg:lod2MultiSurface>"##,
    );
    let out =
        read("model.gml", &lonlat(&[polygon_in("a", "x"), polygon_in("b", "x"), polygon_in("d", "y"), linking].concat()), None).unwrap();
    let ds = only(&out);
    assert!((area(ds.features[row_of(ds, "c")].geometry.as_ref()) - 100.0).abs() < 1e-6, "{:?}", messages_of(&out));
    assert!(!says(&out, "earlier element of the file"), "{:?}", messages_of(&out));
}

/// A prototype in another member, read once, is placed for every instance that links to it,
/// without being copied again for each: many small instances of a large prototype aren't held back
/// by what copies may cost.
#[test]
fn a_shared_prototype_is_read_once_for_every_member() {
    // Large in bytes, few in points.
    let padding = " ".repeat(20_000);
    let prototype = format!(
        r#"<core:relativeGMLGeometry><gml:MultiSurface gml:id="proto"><gml:surfaceMember><gml:Polygon><gml:exterior><gml:LinearRing><gml:posList>0 0 0 1 0 0 1 1 0 0 1 0 0 0 0{padding}</gml:posList></gml:LinearRing></gml:exterior></gml:Polygon></gml:surfaceMember></gml:MultiSurface></core:relativeGMLGeometry>"#
    );
    let mut members = furniture("t0", &format!("{prototype}{}", reference(0.0, 0.0, 0.0)));
    for k in 1..100 {
        let at = f64::from(k) * 2.0;
        members.push_str(&furniture(
            &format!("t{k}"),
            &format!(r##"<core:relativeGMLGeometry xlink:href="#proto"/>{}"##, reference(at, 0.0, 0.0)),
        ));
    }
    let out = read("model.gml", &lonlat(&members), None).unwrap();
    assert_eq!(of_type(&out, GeometryType::Polygon).features.len(), 100, "{:?}", messages_of(&out));
    assert!(!says(&out, "geometry instances"), "{:?}", messages_of(&out));
    assert!(!says(&out, "link"), "{:?}", messages_of(&out));
}

/// A link to an id several elements have reads the first in the file (when its member has none),
/// whatever order the member's links come in, even when another is inside a geometry it also links.
#[test]
fn a_link_reads_the_first_of_an_id_whatever_the_order_of_links() {
    let first = building(
        "a",
        &format!(
            "<bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember>{}</gml:surfaceMember></gml:MultiSurface></bldg:lod2MultiSurface>",
            polygon("x", &square(0.0, 0.0, 10.0, 0.0))
        ),
    );
    let holding = building(
        "c",
        &format!(
            r#"<bldg:lod1MultiSurface><gml:MultiSurface><gml:surfaceMember><gml:CompositeSurface gml:id="g"><gml:surfaceMember>{}</gml:surfaceMember></gml:CompositeSurface></gml:surfaceMember></gml:MultiSurface></bldg:lod1MultiSurface>"#,
            polygon("x", &square(20.0, 0.0, 5.0, 0.0))
        ),
    );
    for links in [
        r##"<gml:surfaceMember xlink:href="#g"/><gml:surfaceMember xlink:href="#x"/>"##,
        r##"<gml:surfaceMember xlink:href="#x"/><gml:surfaceMember xlink:href="#g"/>"##,
    ] {
        let linking =
            building("b", &format!("<bldg:lod2MultiSurface><gml:MultiSurface>{links}</gml:MultiSurface></bldg:lod2MultiSurface>"));
        let out = read("model.gml", &lonlat(&[first.clone(), holding.clone(), linking].concat()), None).unwrap();
        let ds = only(&out);
        let got = area(ds.features[row_of(ds, "b")].geometry.as_ref());
        assert!((got - 125.0).abs() < 1e-6, "{links}: {got} {:?}", messages_of(&out));
    }
}

/// Rings a matrix flattens when it places a prototype are left out and said for each instance; a
/// matrix that flattens nothing says nothing.
#[test]
fn rings_flattened_by_placing_are_said() {
    let prototype = format!(
        r#"<core:relativeGMLGeometry><gml:MultiSurface gml:id="proto"><gml:surfaceMember>{}</gml:surfaceMember><gml:surfaceMember>{}</gml:surfaceMember></gml:MultiSurface></core:relativeGMLGeometry>"#,
        polygon("proto-f", &square(0.0, 0.0, 1.0, 0.0)),
        polygon("proto-r", &square(0.0, 0.0, 1.0, 1.0))
    );
    let placed = |matrix: &str| {
        furniture("t", &format!("<core:transformationMatrix>{matrix}</core:transformationMatrix>{prototype}{}", reference(5.0, 5.0, 1.0)))
    };
    let out = read("model.gml", &lonlat(&placed("0 0 0 0 0 0 0 0 0 0 1 0 0 0 0 1")), None).unwrap();
    assert!(says(&out, "2 rings with fewer than three different points are left out"), "{:?}", messages_of(&out));
    let out = read("model.gml", &lonlat(&placed("2 0 0 0 0 2 0 0 0 0 2 0 0 0 0 1")), None).unwrap();
    assert!(!says(&out, "fewer than three different points"), "{:?}", messages_of(&out));
}

/// An element with many namespace declarations followed by many children declaring one each: past
/// what the sets may take, a child's set is spent without looking at its parent's pairs, and what
/// stands in it is still read. (Many more of both would be past what parsing may take, and the
/// member left out: `a_member_flooded_with_namespaces_is_left_out_and_the_others_read`.)
#[test]
fn children_past_what_declarations_may_take_are_read() {
    let many: String = (0..50).map(|k| format!(r#" xmlns:n{k}="urn:n{k}""#)).collect();
    let children: String = (0..50).map(|k| format!(r#"<gen:stringAttribute xmlns:c{k}="urn:c{k}"></gen:stringAttribute>"#)).collect();
    let inner = format!(
        "<bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember>{}</gml:surfaceMember></gml:MultiSurface></bldg:lod2MultiSurface>",
        polygon("p", &square(0.0, 0.0, 10.0, 0.0))
    );
    let written =
        format!(r#"<core:cityObjectMember><bldg:Building gml:id="c"{many}>{children}{inner}</bldg:Building></core:cityObjectMember>"#);
    // A link, so the file is indexed and every declaration is looked at.
    let linking = building(
        "d",
        r##"<bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember xlink:href="#p"/></gml:MultiSurface></bldg:lod2MultiSurface>"##,
    );
    let out = read("model.gml", &lonlat(&[written, linking].concat()), None).unwrap();
    let ds = only(&out);
    assert!((area(ds.features[row_of(ds, "c")].geometry.as_ref()) - 100.0).abs() < 1e-6, "{:?}", messages_of(&out));
}

/// A solid whose exterior links to what is itself only a link keeps its cavities for the end of
/// that chain: read with the shell it ends at, left out with the solid when it ends in nothing,
/// whether that link stands at another level of detail or at the solid's own (read already there).
#[test]
fn cavities_wait_for_the_end_of_an_exterior_chain() {
    let cavity = format!(
        "<gml:interior><gml:CompositeSurface><gml:surfaceMember>{}</gml:surfaceMember></gml:CompositeSurface></gml:interior>",
        polygon("c", &square(2.0, 2.0, 2.0, 0.0))
    );
    let model = |end: &str, alias_lod: u8| {
        let alias = format!(r##"<gml:surfaceMember><gml:CompositeSurface gml:id="l" xlink:href="#{end}"/></gml:surfaceMember>"##);
        let (at_1, at_2) = if alias_lod == 1 { (alias.as_str(), "") } else { ("", alias.as_str()) };
        let inner = format!(
            r##"<bldg:lod1MultiSurface><gml:MultiSurface><gml:surfaceMember><gml:CompositeSurface gml:id="s"><gml:surfaceMember>{}</gml:surfaceMember></gml:CompositeSurface></gml:surfaceMember>{at_1}</gml:MultiSurface></bldg:lod1MultiSurface>
            <bldg:lod2Solid><gml:Solid><gml:exterior xlink:href="#l"/>{cavity}</gml:Solid></bldg:lod2Solid>"##,
            polygon("a", &square(0.0, 0.0, 10.0, 0.0))
        );
        let inner = if at_2.is_empty() {
            inner
        } else {
            format!("{inner}<bldg:lod2MultiSurface><gml:MultiSurface>{at_2}</gml:MultiSurface></bldg:lod2MultiSurface>")
        };
        read("model.gml", &lonlat(&building("b", &inner)), None).unwrap()
    };
    let lod = |out: &[(Dataset, ReadReport)]| value(only(out), 0, "lod");
    for alias_lod in [1, 2] {
        // Ends at a shell: the solid is read, at LoD 2.
        let out = model("s", alias_lod);
        assert_eq!(lod(&out), Value::Text("2".into()), "alias at LoD {alias_lod}: {:?}", messages_of(&out));
        // Ends in nothing: the solid is left out with its cavity, and the links said; LoD 1 draws.
        let out = model("nothing", alias_lod);
        assert_eq!(lod(&out), Value::Text("1".into()), "alias at LoD {alias_lod}: {:?}", messages_of(&out));
        assert!(says(&out, "are given only by a link"), "{:?}", messages_of(&out));
    }
}

/// A group's parent written in place, not linked, is the group's parent all the same: the group is
/// its child, and raises it, and the object written there isn't the group's child.
#[test]
fn a_groups_parent_written_in_place_is_its_parent() {
    let members = [
        r##"<core:cityObjectMember><grp:CityObjectGroup gml:id="g"><grp:groupMember xlink:href="#b1"/><grp:parent><bldg:Building gml:id="site"><gml:name>Site</gml:name></bldg:Building></grp:parent></grp:CityObjectGroup></core:cityObjectMember>"##.to_string(),
        building("b1", &box_surfaces("b1")),
    ]
    .concat();
    let out = read("model.gml", &lonlat(&members), None).unwrap();
    let groups = layer(&out, "model CityObjectGroup");
    assert_eq!(value(groups, 0, "parent"), Value::Text("site".into()));
    let buildings = layer(&out, "model Building");
    let site = row_of(buildings, "site");
    assert_ne!(value(buildings, site, "parent"), Value::Text("g".into()));
    assert_eq!(value(buildings, site, "z_max"), Value::Float(5.0), "raised through the group");
}

/// A prototype with nothing to place (its surfaces all links, which it doesn't follow) leaves its
/// instance at its reference point, and said.
#[test]
fn a_prototype_with_nothing_to_place_is_its_reference_point() {
    let prototype = r##"<core:relativeGMLGeometry><gml:MultiSurface gml:id="proto"><gml:surfaceMember xlink:href="#elsewhere"/></gml:MultiSurface></core:relativeGMLGeometry>"##;
    let member = furniture(
        "t",
        &format!(
            "<core:transformationMatrix>1 0 0 0 0 1 0 0 0 0 1 0 0 0 0 1</core:transformationMatrix>{prototype}{}",
            reference(5.0, 5.0, 1.0)
        ),
    );
    let out = read("model.gml", &lonlat(&member), None).unwrap();
    assert!(says(&out, "1 geometry instances are drawn as a point"), "{:?}", messages_of(&out));
}

/// Geometries given only by links to each other, in a cycle, draw nothing, and the links are said.
#[test]
fn a_cycle_of_links_is_said() {
    let inner = r##"<bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember><gml:CompositeSurface gml:id="a" xlink:href="#b"/></gml:surfaceMember><gml:surfaceMember><gml:CompositeSurface gml:id="b" xlink:href="#a"/></gml:surfaceMember></gml:MultiSurface></bldg:lod2MultiSurface>"##;
    let out = read("model.gml", &lonlat(&building("c", inner)), None).unwrap();
    assert!(says(&out, "are given only by a link"), "{:?}", messages_of(&out));
}

/// A solid's cavities wait for its exterior to have something read (#1816): an exterior written
/// in place or given by a link, whose surfaces are links, reads them with the first of those links
/// that reads something; when every one leads nowhere, the solid is left out with its cavities,
/// the links are said, and the object keeps its other level of detail.
#[test]
fn cavities_wait_for_the_exterior_to_read_something() {
    // A cavity 8 high: the object's height says whether it was read.
    let cavity = format!(
        "<gml:interior><gml:CompositeSurface><gml:surfaceMember>{}</gml:surfaceMember></gml:CompositeSurface></gml:interior>",
        polygon("c", &square(2.0, 2.0, 2.0, 8.0))
    );
    let lod1 = format!(
        "<bldg:lod1MultiSurface><gml:MultiSurface><gml:surfaceMember>{}</gml:surfaceMember></gml:MultiSurface></bldg:lod1MultiSurface>",
        polygon("a", &square(0.0, 0.0, 10.0, 0.0))
    );
    // A part holding what the solid links to, read there at LoD 2 and here through the links; the
    // composite whose surface leads nowhere only where a case links to it, so the others say nothing.
    let part = |dead: bool| {
        let nowhere = if dead {
            r##"<gml:surfaceMember><gml:CompositeSurface gml:id="nowhere"><gml:surfaceMember xlink:href="#missing"/></gml:CompositeSurface></gml:surfaceMember>"##
        } else {
            ""
        };
        format!(
            r##"<bldg:consistsOfBuildingPart><bldg:BuildingPart gml:id="part"><bldg:lod2MultiSurface><gml:MultiSurface>
            <gml:surfaceMember>{}</gml:surfaceMember><gml:surfaceMember>{}</gml:surfaceMember>
            <gml:surfaceMember><gml:CompositeSurface gml:id="found"><gml:surfaceMember xlink:href="#p"/></gml:CompositeSurface></gml:surfaceMember>{nowhere}
            <gml:surfaceMember><gml:CompositeSurface gml:id="empty"/></gml:surfaceMember>
            </gml:MultiSurface></bldg:lod2MultiSurface></bldg:BuildingPart></bldg:consistsOfBuildingPart>"##,
            polygon("p", &square(0.0, 0.0, 10.0, 0.0)),
            polygon("q", "0 0 0 10 0 0 10 0 5 0 0 5 0 0 0")
        )
    };
    let members = |hrefs: &[&str]| -> String { hrefs.iter().map(|h| format!(r##"<gml:surfaceMember xlink:href="#{h}"/>"##)).collect() };
    let in_place = |hrefs: &[&str]| format!("<gml:exterior><gml:CompositeSurface>{}</gml:CompositeSurface></gml:exterior>", members(hrefs));
    let model = |exterior: &str, extra: &str| {
        let part = part(exterior.contains("nowhere"));
        let inner = format!("{lod1}<bldg:lod2Solid><gml:Solid>{exterior}{cavity}</gml:Solid></bldg:lod2Solid>{extra}{part}");
        read("model.gml", &lonlat(&building("b", &inner)), None).unwrap()
    };
    let of = |out: &[(Dataset, ReadReport)], field: &str| {
        let ds = layer(out, "model Building");
        value(ds, row_of(ds, "b"), field)
    };
    let kept = |out: &[(Dataset, ReadReport)], case: &str| {
        assert_eq!(of(out, "lod"), Value::Text("2".into()), "{case}: {:?}", messages_of(out));
        assert_eq!(of(out, "z_max"), Value::Float(8.0), "{case}: the cavity is read: {:?}", messages_of(out));
        assert!(!says(out, "can't be read"), "{case}: {:?}", messages_of(out));
    };
    let left_out = |out: &[(Dataset, ReadReport)], case: &str| {
        assert_eq!(of(out, "lod"), Value::Text("1".into()), "{case}: {:?}", messages_of(out));
        assert_eq!(
            of(out, "z_max"),
            Value::Float(5.0),
            "{case}: the cavity isn't read (the part's wall is 5 high): {:?}",
            messages_of(out)
        );
    };
    let links_said = |out: &[(Dataset, ReadReport)], case: &str| {
        assert!(says(out, "are given only by a link"), "{case}: {:?}", messages_of(out));
        assert!(!says(out, "can't be read"), "{case}: said as its links: {:?}", messages_of(out));
    };
    let clean = |out: &[(Dataset, ReadReport)], case: &str| {
        assert!(!says(out, "link"), "{case}: {:?}", messages_of(out));
    };

    // Every surface of the exterior leads nowhere: written in place, given by a link to a shell (the
    // part's), or through a composite of links. The links say so.
    for (exterior, case) in [
        (in_place(&["missing", "gone"]), "in place, to nothing"),
        (r##"<gml:exterior xlink:href="#nowhere"/>"##.to_string(), "linked shell, to nothing"),
        (in_place(&["nowhere"]), "in place, through a composite to nothing"),
    ] {
        let out = model(&exterior, "");
        left_out(&out, case);
        links_said(&out, case);
        // The solid's own links, not only the part's: two links to nothing in each case (the part
        // and the solid both read the composite that leads nowhere).
        assert!(says(&out, "2 geometries or objects are given only by a link"), "{case}: {:?}", messages_of(&out));
    }
    // Nothing of the exterior read and nothing to follow: a face whose ring is a link is said as
    // that link; an empty shell lacks the part it needs, and is said so.
    let ringless = r##"<gml:exterior><gml:CompositeSurface><gml:surfaceMember><gml:Polygon><gml:exterior xlink:href="#ring"/></gml:Polygon></gml:surfaceMember></gml:CompositeSurface></gml:exterior>"##;
    let out = model(ringless, "");
    left_out(&out, "a face given by a ring link");
    links_said(&out, "a face given by a ring link");
    // An empty shell, written in place, given by a link, or reached through a surface's link: no
    // notice says why nothing is read, so the solid is said as lacking the part it needs.
    for (exterior, case) in [
        ("<gml:exterior/>".to_string(), "an empty exterior"),
        (r##"<gml:exterior xlink:href="#empty"/>"##.to_string(), "a linked empty shell"),
        (in_place(&["empty"]), "a surface linking an empty shell"),
    ] {
        let out = model(&exterior, "");
        left_out(&out, case);
        assert!(says(&out, "1 geometries of city objects have coordinates that can't be read"), "{case}: {:?}", messages_of(&out));
        clean(&out, case);
    }

    // Links that are found read the cavity, once their faces are: through a composite of links too,
    // and with the shell given by a link; nothing is said.
    for (exterior, case) in [
        (in_place(&["p", "q"]), "in place, found"),
        (in_place(&["found"]), "in place, through a composite found"),
        (r##"<gml:exterior xlink:href="#found"/>"##.to_string(), "linked shell, found"),
    ] {
        let out = model(&exterior, "");
        kept(&out, case);
        clean(&out, case);
    }
    // One leading nowhere beside one found: the cavity is read, the link to nothing said.
    let out = model(&in_place(&["missing", "p"]), "");
    kept(&out, "in place, one found");
    assert!(says(&out, "1 geometries or objects are given only by a link"), "{:?}", messages_of(&out));
    // Surfaces the object has read already at that level (its thematic surfaces): the exterior
    // has them, and the cavity is read.
    let out = model(&in_place(&["s-g", "s-w", "s-r"]), &box_surfaces("s"));
    kept(&out, "read already");
    clean(&out, "read already");
}

/// A solid whose exterior is all links, and whose cavity can't be read, is left out whole: none
/// of its exterior's links draws it in part, and it is said once.
#[test]
fn a_cavity_that_cant_be_read_leaves_a_linked_exterior_out() {
    let bad =
        r#"<gml:Polygon><gml:exterior><gml:LinearRing><gml:posList>1 2 abc</gml:posList></gml:LinearRing></gml:exterior></gml:Polygon>"#;
    let inner = format!(
        r##"<bldg:lod1MultiSurface><gml:MultiSurface><gml:surfaceMember>{}</gml:surfaceMember></gml:MultiSurface></bldg:lod1MultiSurface>
        <bldg:lod2Solid><gml:Solid><gml:exterior><gml:CompositeSurface><gml:surfaceMember xlink:href="#p"/><gml:surfaceMember xlink:href="#q"/></gml:CompositeSurface></gml:exterior>
        <gml:interior><gml:CompositeSurface><gml:surfaceMember>{bad}</gml:surfaceMember></gml:CompositeSurface></gml:interior></gml:Solid></bldg:lod2Solid>
        <bldg:consistsOfBuildingPart><bldg:BuildingPart gml:id="part"><bldg:lod1MultiSurface><gml:MultiSurface><gml:surfaceMember>{}</gml:surfaceMember><gml:surfaceMember>{}</gml:surfaceMember></gml:MultiSurface></bldg:lod1MultiSurface></bldg:BuildingPart></bldg:consistsOfBuildingPart>"##,
        polygon("a", &square(0.0, 0.0, 10.0, 0.0)),
        polygon("p", &square(0.0, 0.0, 10.0, 0.0)),
        polygon("q", &square(20.0, 0.0, 10.0, 0.0))
    );
    let out = read("model.gml", &lonlat(&building("b", &inner)), None).unwrap();
    let ds = layer(&out, "model Building");
    let b = row_of(ds, "b");
    assert_eq!(value(ds, b, "lod"), Value::Text("1".into()), "{:?}", messages_of(&out));
    assert!((area(ds.features[b].geometry.as_ref()) - 100.0).abs() < 1e-6, "{:?}", ds.features[b].geometry);
    assert!(says(&out, "1 geometries of city objects have coordinates that can't be read"), "{:?}", messages_of(&out));
    assert!(!says(&out, "link"), "{:?}", messages_of(&out));
}

/// A prototype's solid whose exterior is a link, which a prototype doesn't follow, is left out with
/// its cavity: said as that link, not as coordinates that can't be read; the prototype's other faces
/// are placed.
#[test]
fn a_prototype_solid_with_a_linked_exterior_is_said_as_its_link() {
    let prototype = format!(
        r##"<core:relativeGMLGeometry><gml:MultiGeometry gml:id="proto"><gml:geometryMember>{}</gml:geometryMember><gml:geometryMember><gml:Solid><gml:exterior xlink:href="#elsewhere"/><gml:interior><gml:CompositeSurface><gml:surfaceMember>{}</gml:surfaceMember></gml:CompositeSurface></gml:interior></gml:Solid></gml:geometryMember></gml:MultiGeometry></core:relativeGMLGeometry>"##,
        polygon("proto-f", &square(0.0, 0.0, 1.0, 0.0)),
        polygon("proto-c", &square(0.2, 0.2, 0.2, 3.0))
    );
    let member = furniture(
        "t",
        &format!(
            "<core:transformationMatrix>1 0 0 0 0 1 0 0 0 0 1 0 0 0 0 1</core:transformationMatrix>{prototype}{}",
            reference(5.0, 5.0, 1.0)
        ),
    );
    let out = read("model.gml", &lonlat(&member), None).unwrap();
    assert!(says(&out, "1 geometries or objects are given only by a link"), "{:?}", messages_of(&out));
    assert!(!says(&out, "can't be read"), "{:?}", messages_of(&out));
    assert!(!says(&out, "drawn as a point"), "its face is placed: {:?}", messages_of(&out));
    let placed = of_type(&out, GeometryType::Polygon);
    assert_eq!(value(placed, row_of(placed, "t"), "z_max"), Value::Float(1.0), "the cavity isn't placed: {:?}", messages_of(&out));
}

/// A curved mesh seen from above lays its faces almost on each other: 97 faces of the rocking chair
/// in OGC's CityGML 3.0 LoD 3 sample, whose union took 15 seconds, and the whole chair's 956 never
/// finished (#1891). With their corners on the union's grid, the chair is read within five seconds
/// (milliseconds, in fact), its faces merged rather than drawn as the outline of their points.
#[test]
fn a_curved_mesh_is_merged_in_a_moment() {
    let bytes = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/rocking-chair.gml"));
    let (sent, got) = std::sync::mpsc::channel();
    std::thread::spawn(move || sent.send(read("chair.gml", bytes, None)));
    let out = got.recv_timeout(std::time::Duration::from_secs(5)).expect("read within 5 seconds").unwrap();
    assert_eq!(out[0].0.features.len(), 1);
    assert!(out[0].0.features[0].geometry.is_some());
    assert!(!says(&out, "outline"), "merged, not drawn as the outline of its points: {:?}", messages_of(&out));
}

/// A CityGML 3.0 city model in `srs`, its core the default namespace as 3.0's own samples write
/// it, holding `members`.
fn model3(srs: &str, members: &str) -> Vec<u8> {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<CityModel xmlns="http://www.opengis.net/citygml/3.0" xmlns:core="http://www.opengis.net/citygml/3.0"
  xmlns:bldg="http://www.opengis.net/citygml/building/3.0" xmlns:con="http://www.opengis.net/citygml/construction/3.0"
  xmlns:gen="http://www.opengis.net/citygml/generics/3.0" xmlns:frn="http://www.opengis.net/citygml/cityfurniture/3.0"
  xmlns:grp="http://www.opengis.net/citygml/cityobjectgroup/3.0" xmlns:ver="http://www.opengis.net/citygml/versioning/3.0"
  xmlns:gml="http://www.opengis.net/gml/3.2" xmlns:xlink="http://www.w3.org/1999/xlink">
  <gml:boundedBy><gml:Envelope srsName="{srs}" srsDimension="3">
    <gml:lowerCorner>0 0 0</gml:lowerCorner><gml:upperCorner>10 10 5</gml:upperCorner>
  </gml:Envelope></gml:boundedBy>
  {members}
</CityModel>"#
    )
    .into_bytes()
}

/// 2.0 members written as 3.0 writes them: members and implicit geometries in core, which is the
/// default namespace; thematic surfaces in `boundary` from the construction module; geometry
/// properties in core; openings in `fillingSurface`; parts in `buildingPart`; a prototype in
/// `relativeGeometry`.
fn three(members: &str) -> String {
    let mut s = members
        .replace("core:cityObjectMember", "cityObjectMember")
        .replace("bldg:boundedBy", "boundary")
        .replace("bldg:consistsOfBuildingPart", "bldg:buildingPart")
        .replace("bldg:opening", "con:fillingSurface")
        .replace("bldg:Door", "con:DoorSurface")
        .replace("core:relativeGMLGeometry", "relativeGeometry")
        .replace("frn:lod2ImplicitRepresentation", "lod2ImplicitRepresentation");
    for name in ["ImplicitGeometry", "referencePoint", "transformationMatrix"] {
        s = s.replace(&format!("core:{name}"), name);
    }
    for kind in ["GroundSurface", "WallSurface", "RoofSurface"] {
        s = s.replace(&format!("bldg:{kind}"), &format!("con:{kind}"));
    }
    for lod in 0..4 {
        for g in ["MultiSurface", "Solid", "MultiCurve"] {
            s = s.replace(&format!("bldg:lod{lod}{g}"), &format!("lod{lod}{g}"));
        }
    }
    s
}

/// The same model written in 3.0 reads as in 2.0: the same layers, rows, footprints, heights and
/// notices, for thematic surfaces, an opening, a part, a solid linking its surfaces' polygons, and implicit
/// geometries placed in place and linked from another member.
#[test]
fn a_3_0_model_reads_as_the_same_model_in_2_0() {
    let solid = r##"<bldg:lod2Solid><gml:Solid><gml:exterior><gml:CompositeSurface>
        <gml:surfaceMember xlink:href="#b1-g"/><gml:surfaceMember xlink:href="#b1-w"/><gml:surfaceMember xlink:href="#b1-r"/>
        </gml:CompositeSurface></gml:exterior></gml:Solid></bldg:lod2Solid>"##;
    let part = format!(
        r#"<bldg:consistsOfBuildingPart><bldg:BuildingPart gml:id="p1">{}</bldg:BuildingPart></bldg:consistsOfBuildingPart>"#,
        box_surfaces("p1")
    );
    let door = format!(
        r#"<bldg:boundedBy><bldg:WallSurface><bldg:opening><bldg:Door gml:id="d1"><bldg:lod3MultiSurface><gml:MultiSurface><gml:surfaceMember>{}</gml:surfaceMember></gml:MultiSurface></bldg:lod3MultiSurface></bldg:Door></bldg:opening></bldg:WallSurface></bldg:boundedBy>"#,
        polygon("d1-f", "2 0 0 4 0 0 4 0 2 2 0 2 2 0 0")
    );
    let prototype = format!(
        r#"<core:relativeGMLGeometry><gml:MultiSurface gml:id="proto" srsName="local"><gml:surfaceMember>{}</gml:surfaceMember></gml:MultiSurface></core:relativeGMLGeometry>"#,
        polygon("proto-f", &square(0.0, 0.0, 1.0, 0.0))
    );
    let members = [
        building("b1", &format!("{}{door}{solid}{part}", box_surfaces("b1"))),
        furniture(
            "t1",
            &format!(
                "<core:transformationMatrix>2 0 0 0 0 2 0 0 0 0 2 0 0 0 0 1</core:transformationMatrix>{prototype}{}",
                reference(5.0, 5.0, 1.0)
            ),
        ),
        furniture("t2", &format!(r##"<core:relativeGMLGeometry xlink:href="#proto"/>{}"##, reference(20.0, 20.0, 0.0))),
    ]
    .concat();
    let two = read("model.gml", &lonlat(&members), None).unwrap();
    let three = read("model.gml", &model3("EPSG:4326", &three(&members)), None).unwrap();
    assert!(
        two.len() > 1 && layer(&two, "model BuildingPart").features.len() == 1,
        "{:?}",
        two.iter().map(|(d, _)| &d.name).collect::<Vec<_>>()
    );
    assert!(!says(&two, "thematic surfaces or openings"), "the door is an opening: {:?}", messages_of(&two));
    assert_eq!(format!("{three:?}"), format!("{two:?}"));
}

/// 3.0's own ways: a part, a group's member and its role given by links, generic attributes named
/// by `gen:name` (a set's too), and its spaces as objects; what it holds that isn't read (relations
/// between objects, versions, terrain intersection curves, generic attributes with no name, a
/// set's code list) is said, and a clean 3.0 model says none of it.
#[test]
fn a_3_0_model_reads_its_own_ways_and_says_what_it_leaves_out() {
    let building = format!(
        r##"<cityObjectMember><bldg:Building gml:id="b1">
        <genericAttribute><gen:IntAttribute><gen:name>rooms</gen:name><gen:value>12</gen:value></gen:IntAttribute></genericAttribute>
        <genericAttribute><gen:StringAttribute><gen:name>code</gen:name><gen:value>007</gen:value></gen:StringAttribute></genericAttribute>
        <genericAttribute><gen:GenericAttributeSet><gen:name>survey</gen:name>
          <gen:genericAttribute><gen:DoubleAttribute><gen:name>area</gen:name><gen:value>10.5</gen:value></gen:DoubleAttribute></gen:genericAttribute>
        </gen:GenericAttributeSet></genericAttribute>
        <bldg:buildingPart xlink:href="#p1"/>
        <bldg:buildingRoom><bldg:BuildingRoom gml:id="room1">{}</bldg:BuildingRoom></bldg:buildingRoom>
        {}</bldg:Building></cityObjectMember>"##,
        three(&surface("FloorSurface", &[polygon("room1-f", &square(1.0, 1.0, 2.0, 0.0))]))
            .replace("bldg:FloorSurface", "con:FloorSurface"),
        three(&box_surfaces("b1"))
    );
    let part = format!(
        r#"<cityObjectMember><bldg:BuildingPart gml:id="p1">{}</bldg:BuildingPart></cityObjectMember>"#,
        three(&box_surfaces("p1"))
    );
    let group = r##"<cityObjectMember><grp:CityObjectGroup gml:id="g1">
        <grp:groupMember><grp:Role><grp:role>landmark</grp:role><grp:groupMember xlink:href="#b1"/></grp:Role></grp:groupMember>
        <grp:groupMember><grp:Role><grp:role> </grp:role><grp:groupMember xlink:href="#p1"/></grp:Role></grp:groupMember>
        </grp:CityObjectGroup></cityObjectMember>"##;
    let clean = read("model.gml", &model3("EPSG:4326", &[building.as_str(), &part, group].concat()), None).unwrap();
    let ds = layer(&clean, "model Building");
    assert_eq!(value(ds, 0, "rooms"), Value::Int(12));
    assert_eq!(value(ds, 0, "code"), Value::Text("007".into()));
    assert_eq!(value(ds, 0, "survey"), Value::Text(r#"{"area":10.5}"#.into()));
    assert_eq!(value(ds, 0, "buildingPart"), Value::Text("#p1".into()));
    let part = layer(&clean, "model BuildingPart");
    assert_eq!(value(part, 0, "parent"), Value::Text("b1, g1".into()), "the link makes it the building's part, and the group's member");
    assert_eq!(value(layer(&clean, "model BuildingRoom"), 0, "parent"), Value::Text("b1".into()));
    assert_eq!(value(layer(&clean, "model CityObjectGroup"), 0, "groupMember"), Value::Text(r##"["#b1","#p1"]"##.into()));
    assert!(
        says(&clean, "1 members of groups (groupMember) have a role in their group"),
        "a blank role isn't one: {:?}",
        messages_of(&clean)
    );
    for text in ["CityObjectRelation", "(versionMember, versionTransitionMember)", "have no name", "codeSpace"] {
        assert!(!says(&clean, text), "{text}: {:?}", messages_of(&clean));
    }
    // A relation to another object, and the model's versions: left out, and said.
    let terrain = "<lod2TerrainIntersectionCurve><gml:MultiCurve><gml:curveMember><gml:LineString><gml:posList>0 0 0 10 0 0</gml:posList></gml:LineString></gml:curveMember></gml:MultiCurve></lod2TerrainIntersectionCurve>";
    let relation = r##"<cityObjectMember><bldg:Building gml:id="b2"><relatedTo><CityObjectRelation><relationType>adjacent</relationType><relatedTo xlink:href="#b1"/></CityObjectRelation></relatedTo>
        <genericAttribute><gen:StringAttribute><gen:value>lost</gen:value></gen:StringAttribute></genericAttribute>
        <genericAttribute><gen:GenericAttributeSet><gen:name>set</gen:name><gen:codeSpace>list</gen:codeSpace>
          <gen:genericAttribute><gen:StringAttribute><gen:value>lost too</gen:value></gen:StringAttribute></gen:genericAttribute>
        </gen:GenericAttributeSet></genericAttribute>"##;
    let versions = r#"<versionMember><ver:Version gml:id="v1"/></versionMember><versionTransitionMember><ver:VersionTransition gml:id="vt1"/></versionTransitionMember>"#;
    let members = format!("{building}{relation}{terrain}{}</bldg:Building></cityObjectMember>{versions}", three(&box_surfaces("b2")));
    let out = read("model.gml", &model3("EPSG:4326", &members), None).unwrap();
    assert!(says(&out, "1 relations between city objects (CityObjectRelation) aren't read"), "{:?}", messages_of(&out));
    assert!(
        says(&out, "the file's versions (versionMember, versionTransitionMember) aren't read: 2 are left out"),
        "{:?}",
        messages_of(&out)
    );
    assert!(says(&out, "1 terrain intersection curves (where objects meet the ground) aren't read"), "{:?}", messages_of(&out));
    assert!(
        says(&out, "2 generic attributes (genericAttribute, or a member of a genericAttributeSet) have no name"),
        "{:?}",
        messages_of(&out)
    );
    assert!(says(&out, "1 sets of generic attributes (genericAttributeSet) name a code list (codeSpace)"), "{:?}", messages_of(&out));
    assert_eq!(layer(&out, "model Building").features.len(), 2);
    assert!(out.iter().all(|(ds, _)| ds.geometry_type == GeometryType::Polygon), "the curve isn't drawn as a line");
}

/// The names 3.0 adds are read only in its namespaces: an extension (ADE) of a 2.0 model may name a
/// property of its own `boundary` or `buildingPart`, and it keeps the meaning it had in 2.0. Its
/// feature in `boundary` is an object of its own, not the building's thematic surface, and its
/// `buildingPart` is a link kept as an attribute, which makes no part.
#[test]
fn an_extension_of_2_0_keeps_its_own_properties_named_as_3_0s() {
    let ade = r#"xmlns:ade="http://example.com/ade""#;
    let inner = format!(
        r##"{}<ade:boundary {ade}><ade:Sensor gml:id="s1"><ade:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember>{}</gml:surfaceMember></gml:MultiSurface></ade:lod2MultiSurface></ade:Sensor></ade:boundary>
        <ade:buildingPart {ade} xlink:href="#p1"/>"##,
        box_surfaces("b1"),
        polygon("s1-g", &square(20.0, 20.0, 2.0, 0.0))
    );
    let members = format!("{}{}", building("b1", &inner), building("p1", &box_surfaces("p1")));
    let out = read("model.gml", &lonlat(&members), None).unwrap();
    let sensors = layer(&out, "model Sensor");
    assert_eq!(value(sensors, row_of(sensors, "s1"), "parent"), Value::Text("b1".into()), "{:?}", messages_of(&out));
    let buildings = layer(&out, "model Building");
    assert_eq!(value(buildings, row_of(buildings, "p1"), "parent"), Value::Null, "no part made");
    assert_eq!(value(buildings, row_of(buildings, "b1"), "buildingPart"), Value::Text("#p1".into()));
}

/// An object of type `ty` with a floor and a roof 5 above it, as a member: in 2.0's building
/// namespace, or in 3.0's core.
fn raised(ty: &str, v3: bool) -> String {
    let faces = format!(
        "<gml:MultiSurface><gml:surfaceMember>{}</gml:surfaceMember><gml:surfaceMember>{}</gml:surfaceMember></gml:MultiSurface>",
        polygon(&format!("{ty}-f"), &square(0.0, 0.0, 10.0, 0.0)),
        polygon(&format!("{ty}-r"), &square(0.0, 0.0, 10.0, 5.0)),
    );
    if v3 {
        format!(r#"<cityObjectMember><{ty} gml:id="{ty}"><lod1MultiSurface>{faces}</lod1MultiSurface></{ty}></cityObjectMember>"#)
    } else {
        format!(
            r#"<core:cityObjectMember><bldg:{ty} gml:id="{ty}"><bldg:lod1MultiSurface>{faces}</bldg:lod1MultiSurface></bldg:{ty}></core:cityObjectMember>"#
        )
    }
}

/// Whether the layer of objects of type `ty` comes in extruded by `height` on `base_height`.
fn stands_in(out: &[(Dataset, ReadReport)], ty: &str) -> bool {
    let name = format!("m {ty}");
    let (_, report) = out.iter().find(|(d, _)| d.name == name).unwrap_or_else(|| panic!("no layer {name}"));
    report.extrusion.as_ref().is_some_and(|x| x.height_field == "height" && x.base_field == "base_height")
}

/// Volumes come in extruded by their CityGML names, 2.0's and 3.0's; surfaces on or below the
/// ground, what lies inside another object and an extension's type stay flat (#2161).
#[test]
fn volumes_come_in_extruded_and_surfaces_flat() {
    let standing_2 =
        ["Building", "BuildingPart", "BuildingInstallation", "Bridge", "BridgeConstructionElement", "CityFurniture", "GenericCityObject"];
    let flat_2 = [
        "Road",
        "Railway",
        "Track",
        "Square",
        "TransportationComplex",
        "WaterBody",
        "LandUse",
        "PlantCover",
        "Tunnel",
        "Room",
        "IntBuildingInstallation",
        "BuildingFurniture",
        "NoiseBarrier",
    ];
    let members: String = standing_2.iter().chain(&flat_2).map(|ty| raised(ty, false)).collect();
    let out = read("m.gml", &lonlat(&members), None).unwrap();
    for ty in standing_2 {
        assert!(stands_in(&out, ty), "2.0 {ty} should come in extruded");
    }
    for ty in flat_2 {
        assert!(!stands_in(&out, ty), "2.0 {ty} should come in flat");
    }
    let standing_3 = [
        "BuildingConstructiveElement",
        "BridgeConstructiveElement",
        "TunnelInstallation",
        "GenericOccupiedSpace",
        "OtherConstruction",
        "SolitaryVegetationObject",
    ];
    let flat_3 = [
        "Waterway",
        "HollowSpace",
        "BuildingRoom",
        "Storey",
        "BuildingUnit",
        "GenericUnoccupiedSpace",
        "GenericLogicalSpace",
        "GenericThematicSurface",
    ];
    let members: String = standing_3.iter().chain(&flat_3).map(|ty| raised(ty, true)).collect();
    let out = read("m.gml", &model3("EPSG:4326", &members), None).unwrap();
    for ty in standing_3 {
        assert!(stands_in(&out, ty), "3.0 {ty} should come in extruded");
    }
    for ty in flat_3 {
        assert!(!stands_in(&out, ty), "3.0 {ty} should come in flat");
    }
}
