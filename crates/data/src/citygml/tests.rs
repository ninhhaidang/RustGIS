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
    let three = br#"<core:CityModel xmlns:core="http://www.opengis.net/citygml/3.0"></core:CityModel>"#;
    assert!(looks_like_citygml(three), "3.0 is known, to be refused by name");
    let err = read("new.gml", three, None).unwrap_err().to_string();
    assert!(err.contains("CityGML 3.0: RustGIS reads CityGML 1.0 and 2.0"), "{err}");
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
    let e = ds.extent();
    assert!((e.min_x - 9.0).abs() < 0.01 && (e.min_y - 49.6).abs() < 0.1, "{e:?}");
    // One naming another system is refused.
    let other = inner.replace("EPSG:25832", "EPSG:25833");
    let err = read("model.gml", &model("EPSG:25832", &building("b1", &other)), None).unwrap_err().to_string();
    assert!(err.contains("different coordinate systems (EPSG:25832 and EPSG:25833)"), "{err}");
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

/// A solid of links to its surfaces' polygons: the links aren't followed, and the footprint comes
/// from the surfaces written in place.
#[test]
fn links_are_said_and_what_is_written_out_is_drawn() {
    let links: String = ["b1-g", "b1-w", "b1-r"].iter().map(|id| format!(r##"<gml:surfaceMember xlink:href="#{id}"/>"##)).collect();
    let inner = format!(
        "<bldg:lod2Solid><gml:Solid><gml:exterior><gml:CompositeSurface>{links}</gml:CompositeSurface></gml:exterior></gml:Solid></bldg:lod2Solid>{}",
        box_surfaces("b1")
    );
    let out = read("model.gml", &lonlat(&building("b1", &inner)), None).unwrap();
    assert!((area(out[0].0.features[0].geometry.as_ref()) - 100.0).abs() < 1e-6);
    assert!(says(&out, "3 geometries or objects are given only by a link to another element (xlink:href)"), "{:?}", messages_of(&out));
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
    assert!(says(&out, "4 geometries or objects are given only by a link"), "{:?}", messages_of(&out));
    assert!(!says(&out, "can't be read"), "{:?}", messages_of(&out));
    assert_eq!(value(&out[0].0, 0, "consistsOfBuildingPart"), Value::Text("#p".into()));
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
    let e = out[0].0.extent();
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
    assert!(says(&out, "1 generic attributes in a set (genericAttributeSet) have no name"), "{:?}", messages_of(&out));
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
