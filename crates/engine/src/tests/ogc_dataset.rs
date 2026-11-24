//! Dataset discovery is exercised through commands, with the same documents fetched natively
//! and replayed incrementally as the browser does. The fixture invents no endpoint or layer id.

use super::{Project, Session, ogc_server};
use ogc_server::{Route, Server};
use serde_json::{Value, json};

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn ogc_dataset_supplied_listing_reads_off_frame_with_its_login() {
    use crate::analysis::control::Control;
    use crate::cmd::reads::{read_ahead, read_land};
    let mut session = Session::new(Project::default());
    let connection = session
        .run("connection.new", &json!({"kind":"ogcapi","address":"https://maps.example.org","auth":"header","keyName":"X-Key"}))
        .unwrap();
    session.run("connection.login", &json!({"id":connection["id"],"apiKey":"synthetic-dataset-read-ahead"})).unwrap();
    for (text, filter, count) in [
        (json!({"links":[{"rel":"tilesets-map","href":"/tiles"}]}).to_string(), None, 0),
        (json!({"collections":[{"id":"roads"},{"id":"water"}]}).to_string(), Some("roads"), 1),
    ] {
        let mut params = json!({"url":"https://maps.example.org/dataset","type":"ogcapi","text":text,"connection":connection["id"]});
        if let Some(filter) = filter {
            params["filter"] = json!(filter);
        }
        let expected = session.run("service.layers", &params).unwrap();
        assert_eq!(expected["count"], count, "{expected}");
        let read = read_ahead(&mut session, "service.layers", &params).unwrap();
        assert!(read.reads());
        let read = read.run(&Control::new()).unwrap();
        assert_eq!(read_land(&mut session, read).unwrap(), expected, "the worker retains the initiating login binding and filter");
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn ogc_dataset_url_only_add_prepares_discovery_on_the_worker() {
    use crate::analysis::control::Control;
    use crate::cmd::reads::{read_ahead, read_land};
    let landing = json!({"links":[{"rel":"tilesets-map","href":"/tiles?f=json"}]});
    let metadata = json!({"tileMatrixSetId":"WebMercatorQuad","links":[
        {"rel":"item","type":"image/png","href":"/map/{tileMatrix}/{tileRow}/{tileCol}.png"}]});
    let server = Server::new(vec![Route::new("/dataset", &landing.to_string()), Route::new("/tiles", &metadata.to_string())]);
    let mut session = Session::new(Project::default());
    let read =
        read_ahead(&mut session, "service.add", &json!({"url":format!("{}/dataset", server.base),"type":"ogcapi","layer":"map"})).unwrap();
    assert!(read.reads(), "a URL-only add must prepare its discovery for a worker");
    assert!(server.requests().is_empty(), "preparing the read cannot fetch on the frame");
    let done = read.run(&Control::new()).unwrap();
    assert_eq!(server.requests().len(), 2);
    assert!(session.map().tiles.is_empty());
    read_land(&mut session, done).unwrap();
    assert_eq!(session.map().tiles.len(), 1);
    assert_eq!(server.requests().len(), 2, "landing consumes the prepared discovery without fetching it again");
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn ogc_dataset_url_only_add_without_a_layer_is_refused_before_any_fetch() {
    use crate::cmd::reads::read_ahead;
    let server = Server::new(vec![Route::new("/dataset", &json!({"links":[]}).to_string())]);
    let mut session = Session::new(Project::default());
    for layer in [None, Some("  ")] {
        let mut params = json!({"url":format!("{}/dataset", server.base),"type":"ogcapi"});
        if let Some(layer) = layer {
            params["layer"] = json!(layer);
        }
        let error = read_ahead(&mut session, "service.add", &params).err().unwrap();
        assert_eq!(error, "layer is required");
    }
    let error = read_ahead(&mut session, "service.add", &json!({"url":"ftp://maps.example.org/dataset","layer":"map"})).err().unwrap();
    assert!(error.contains("http:// or https://"), "{error}");
    assert!(server.requests().is_empty(), "a refused call cannot fetch");
}

#[test]
fn ogc_dataset_stopped_discovery_counts_a_pending_tilejson_source_once() {
    let root = json!({"links":[{"rel":"tilesets-vector","href":"/tiles?f=json"},{"rel":"styles","href":"/styles?f=json"}]});
    let tiles = json!({"tilesets":[{"dataType":"vector","tileMatrixSetId":"WebMercatorQuad","links":[
        {"rel":"item","type":"application/vnd.mapbox-vector-tile","href":"/vector/{tileMatrix}/{tileRow}/{tileCol}.pbf"}]}]});
    let styles = json!({"styles":[{"id":"roads","links":[
        {"rel":"stylesheet","type":"application/vnd.mapbox.style+json","href":"/style?f=mapbox"}]}]});
    let style = json!({"version":8,"sources":{"tiles":{"type":"vector","url":"/source?f=json"}},
        "layers":[{"id":"roads","type":"line","source":"tiles","source-layer":"roads"}]});
    let params = json!({"url":"https://maps.example.org/dataset","type":"ogcapi","layer":"roads","text":root.to_string(),
    "documents":[
        {"url":"https://maps.example.org/tiles?f=json","text":tiles.to_string()},
        {"url":"https://maps.example.org/styles?f=json","text":styles.to_string()},
        {"url":"https://maps.example.org/style?f=mapbox","text":style.to_string()},
        {"url":"https://maps.example.org/source?f=json","text":" ".repeat(crate::cmd::service::MAX_DATASET_BYTES)}
    ]});
    let mut session = Session::new(Project::default());
    for command in ["service.layers", "service.add"] {
        let error = session.run(command, &params).unwrap_err();
        assert!(error.contains("metadata budget"), "{error}");
        assert!(error.contains("1 TileJSON source checks were left out: dataset discovery stopped"), "{error}");
    }
}

#[test]
fn ogc_dataset_rejected_landing_links_never_guess_a_template() {
    let mut session = Session::new(Project::default());
    for (kind, href, reason) in [
        ("text/html", "/tiles".to_string(), "no usable JSON link"),
        ("application/json", format!("https://maps.example.org/tiles?setting={}", "a".repeat(8200)), "addresses exceed 8192 bytes"),
    ] {
        for collections in [false, true] {
            let mut landing = json!({"links":[{"rel":"tilesets-map","type":kind,"href":href}]});
            if collections {
                landing["links"].as_array_mut().unwrap().push(json!({"rel":"data","type":"application/json","href":"/collections"}));
            }
            let listing = session.run("service.layers", &json!({"url":"https://maps.example.org/dataset","type":"ogcapi",
                "text":landing.to_string(),"documents":[{"url":"https://maps.example.org/collections","text":"{\"collections\":[{\"id\":\"roads\"}]}"}]})).unwrap();
            assert_eq!(listing["count"], usize::from(collections), "{listing}");
            assert!(
                listing["layers"].as_array().unwrap().iter().all(|l| l["offer"]["type"] == "features"),
                "a rejected link cannot enable a guessed template: {listing}"
            );
            assert!(listing["next"].is_null());
            let omitted = listing["notes"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_str)
                .filter(|n| n.contains("tileset documents were left out"))
                .collect::<Vec<_>>();
            assert_eq!(omitted.len(), 1, "{listing}");
            assert!(omitted[0].starts_with("1 tileset documents were left out"));
            assert!(omitted[0].contains(reason), "{listing}");
        }
    }
}

#[test]
fn ogc_dataset_empty_collections_still_discover_advertised_tiles() {
    for collections in [None, Some(json!([])), Some(json!([{"id":"roads"}]))] {
        let mut landing = json!({"links":[{"rel":"tilesets-map","href":"/tiles"}]});
        if let Some(collections) = &collections {
            landing["collections"] = collections.clone();
        }
        let tiles = json!({"tileMatrixSetId":"WebMercatorQuad","links":[
            {"rel":"item","type":"image/png","href":"/map/{tileMatrix}/{tileRow}/{tileCol}.png"}]});
        let listing = Session::new(Project::default())
            .run(
                "service.layers",
                &json!({
                    "url":"https://maps.example.org/dataset","type":"ogcapi","text":landing.to_string(),
                    "documents":[{"url":"https://maps.example.org/tiles?f=json","text":tiles.to_string()}]
                }),
            )
            .unwrap();
        assert_eq!(listing["count"], 1, "{listing}");
        let features = collections.as_ref().is_some_and(|c| !c.as_array().unwrap().is_empty());
        assert_eq!(listing["layers"][0]["offer"]["type"], if features { "features" } else { "tiles" }, "{listing}");
        assert!(listing["next"].is_null(), "{listing}");
    }
    let empty = Session::new(Project::default())
        .run(
            "service.layers",
            &json!({
                "url":"https://maps.example.org/dataset","type":"ogcapi","text":"{\"collections\":[]}"
            }),
        )
        .unwrap();
    assert_eq!(empty["count"], 0, "an ordinary empty collections document remains valid: {empty}");
}

#[test]
fn ogc_dataset_shared_tilesets_do_not_use_an_item_typed_for_the_other_kind() {
    for (media, expected) in [(Some("application/vnd.mapbox-vector-tile"), "vectorTiles"), (Some("image/png"), "tiles"), (None, "both")] {
        let landing = json!({"links":[{"rel":"tilesets-map","href":"/tiles"},{"rel":"tilesets-vector","href":"/tiles"}]});
        let mut item = json!({"rel":"item","href":"/tile/{tileMatrix}/{tileRow}/{tileCol}"});
        if let Some(media) = media {
            item["type"] = json!(media);
        }
        let list = json!({"tilesets":[{"tileMatrixSetId":"WebMercatorQuad","vector_layers":[{"id":"roads"}],"links":[item]}]});
        let listing = Session::new(Project::default())
            .run(
                "service.layers",
                &json!({
                    "url":"https://maps.example.org/dataset","type":"ogcapi","text":landing.to_string(),
                    "documents":[{"url":"https://maps.example.org/tiles?f=json","text":list.to_string()}]
                }),
            )
            .unwrap();
        let both = expected == "both";
        assert_eq!(listing["count"], if both { 2 } else { 1 }, "{listing}");
        assert!(listing["layers"].as_array().unwrap().iter().all(|l| both || l["offer"]["type"] == expected), "{listing}");
        assert_eq!(listing["notes"].as_array().unwrap().len(), usize::from(!both), "the rejected kind is counted: {listing}");
        assert!(listing["next"].is_null(), "{listing}");
    }
}

#[test]
fn ogc_dataset_mixed_landings_report_catalogues_left_out_of_collections_discovery() {
    for dataset in [false, true] {
        let mut landing = json!({"links":[{"rel":"data","type":"application/json","href":"/collections"}]});
        if dataset {
            landing["links"]
                .as_array_mut()
                .unwrap()
                .extend([json!({"rel":"tilesets-vector","href":"/tiles"}), json!({"rel":"styles","href":"/styles"})]);
        }
        let listing = Session::new(Project::default())
            .run(
                "service.layers",
                &json!({
                    "url":"https://maps.example.org/dataset","type":"ogcapi","text":landing.to_string(),
                    "documents":[{"url":"https://maps.example.org/collections","text":"{\"collections\":[{\"id\":\"roads\"}]}"}]
                }),
            )
            .unwrap();
        assert_eq!(listing["count"], 1, "{listing}");
        assert_eq!(listing["layers"][0]["offer"]["type"], "features");
        assert!(listing["next"].is_null(), "{listing}");
        let notices = listing["notes"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .filter(|note| note.contains("the landing also lists collections"))
            .collect::<Vec<_>>();
        if dataset {
            assert_eq!(notices, ["2 dataset tileset and style catalogues were left out: the landing also lists collections"]);
        } else {
            assert!(notices.is_empty(), "{listing}");
        }
    }
}

#[test]
fn ogc_dataset_unusable_style_catalogues_are_counted() {
    for styles in
        [None, Some(json!({"rel":"styles","type":"text/html","href":"/styles"})), Some(json!({"rel":"styles","type":"application/json"}))]
    {
        let mut landing = json!({"links":[{"rel":"tilesets-map","href":"/tiles"}]});
        if let Some(style) = &styles {
            landing["links"].as_array_mut().unwrap().push(style.clone());
        }
        let metadata = json!({"tileMatrixSetId":"WebMercatorQuad","links":[
            {"rel":"item","type":"image/png","href":"/map/{tileMatrix}/{tileRow}/{tileCol}.png"}]});
        let listing = Session::new(Project::default())
            .run(
                "service.layers",
                &json!({
                    "url":"https://maps.example.org/dataset","type":"ogcapi","text":landing.to_string(),
                    "documents":[{"url":"https://maps.example.org/tiles?f=json","text":metadata.to_string()}]
                }),
            )
            .unwrap();
        assert_eq!(listing["count"], 1, "{listing}");
        assert!(listing["next"].is_null(), "{listing}");
        let notes = listing["notes"].as_array().unwrap();
        if styles.is_some() {
            assert_eq!(notes, &[json!("1 style catalogues were left out: no usable JSON link")], "{listing}");
        } else {
            assert!(notes.is_empty(), "{listing}");
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn ogc_dataset_prepared_add_keeps_error_path_losses() {
    use crate::cmd::reads::{read_ahead, read_land};
    for query in [false, true] {
        let url = if query { "https://maps.example.org/landing?token=synthetic-query" } else { "https://maps.example.org/landing" };
        let text = json!({"links":[{"rel":"data","type":"application/json","href":"https://other.example.org/collections"}]}).to_string();
        let mut params = json!({"url":url,"type":"ogcapi","layer":"map","text":text});
        let listing = Session::new(Project::default()).run("service.layers", &params).unwrap();
        params["documents"] = json!([{"url":listing["next"],"text":"not JSON"}]);
        let mut direct = Session::new(Project::default());
        assert!(direct.run("service.add", &params).is_err());
        let notes = |s: &Session| s.notices.iter().filter(|n| !n.error).map(|n| n.text.clone()).collect::<Vec<_>>();
        let expected = notes(&direct);
        assert_eq!(expected.len(), usize::from(query), "{expected:?}");
        let mut prepared = Session::new(Project::default());
        let read = read_ahead(&mut prepared, "service.add", &params).unwrap().run(&crate::analysis::control::Control::new()).unwrap();
        assert!(read_land(&mut prepared, read).is_err());
        assert_eq!(notes(&prepared), expected, "prepared errors must retain counted losses after rollback");
    }
}

/// A dataset offering two projections and styles whose ids deliberately misname their grid.
fn documents() -> Vec<(&'static str, Value)> {
    vec![
        (
            "/dataset",
            json!({"title":"Dataset", "links":[
                {"rel":"tilesets-vector","type":"application/json","href":"/tiles?f=json"},
                {"rel":"styles","type":"application/json","href":"/styles?f=json"}
            ]}),
        ),
        (
            "/tiles",
            json!({"tilesets":[
                {"tileMatrixSetId":"NetherlandsRDNewQuad","dataType":"vector","links":[
                    {"rel":"item","type":"application/vnd.mapbox-vector-tile","href":"/rd/{tileMatrix}/{tileRow}/{tileCol}?f=mvt"}]},
                {"tileMatrixSetId":"WebMercatorQuad","dataType":"vector","links":[
                    {"rel":"item","type":"application/vnd.mapbox-vector-tile","href":"/wm/{tileMatrix}/{tileRow}/{tileCol}?f=mvt"}]}
            ]}),
        ),
        (
            "/styles",
            json!({"styles":[
                {"id":"rd-name","title":"Supported","links":[{"rel":"stylesheet","type":"application/vnd.mapbox.style+json","href":"/good?f=mapbox"}]},
                {"id":"mercator-name","links":[{"rel":"stylesheet","type":"application/vnd.mapbox.style+json","href":"/wrong?f=mapbox"}]}
            ]}),
        ),
        (
            "/good",
            json!({"version":8,"sources":{"basemap":{"type":"vector","tiles":["/wm/{z}/{y}/{x}?f=mvt"]}},
            "layers":[{"id":"roads","type":"line","source":"basemap","source-layer":"roads"}]}),
        ),
        (
            "/wrong",
            json!({"version":8,"sources":{"basemap":{"type":"vector","tiles":["/rd/{z}/{y}/{x}?f=mvt"]}},
            "layers":[{"id":"roads","type":"line","source":"basemap","source-layer":"roads"}]}),
        ),
    ]
}

#[test]
fn ogc_dataset_styles_use_advertised_projection_and_native_web_parity() {
    let docs = documents();
    let server = Server::new(docs.iter().map(|(path, body)| Route::new(path, &body.to_string())).collect());
    let url = format!("{}/dataset", server.base);
    let mut session = Session::new(Project::default());
    let native = session.run("service.layers", &json!({"url":url,"type":"ogcapi"})).unwrap();
    assert_eq!(native["kind"], "OGC API Tiles");
    assert_eq!(native["count"], 1, "{native}");
    assert_eq!(native["layers"][0]["name"], "rd-name");
    assert_eq!(native["layers"][0]["offer"]["type"], "style");
    assert!(native["notes"].as_array().unwrap().iter().any(|n| n.as_str().unwrap().starts_with("1 styles were left out")));
    let requests = server.requests().len();
    assert_eq!(requests, docs.len());
    #[cfg(not(target_arch = "wasm32"))]
    {
        use crate::cmd::reads::{read_ahead, read_land};
        let read = read_ahead(&mut session, "service.layers", &json!({"url":url,"type":"ogcapi"})).unwrap();
        assert!(read.reads(), "URL-only discovery must fetch on the worker too");
        let read = read.run(&crate::analysis::control::Control::new()).unwrap();
        assert_eq!(read_land(&mut session, read).unwrap(), native);
        assert_eq!(server.requests().len(), requests + docs.len());
    }
    let requests = server.requests().len();
    let mut params = json!({"url":url,"type":"ogcapi","text":docs[0].1.to_string(),"documents":[]});
    loop {
        let listing = session.run("service.layers", &params).unwrap();
        let Some(next) = listing["next"].as_str() else {
            assert_eq!(listing, native);
            break;
        };
        let address = url::Url::parse(next).unwrap();
        let body = &docs.iter().find(|(path, _)| *path == address.path()).unwrap().1;
        params["documents"].as_array_mut().unwrap().push(json!({"url":next,"text":body.to_string()}));
    }
    params["layer"] = json!("rd-name");
    params["source_url"] = native["layers"][0]["source_url"].clone();
    session.run("service.add", &params).unwrap();
    assert_eq!(session.map().tiles.len(), 1);
    assert_eq!(session.map().tiles[0].url, format!("{}/good?f=mapbox", server.base));
    #[cfg(not(target_arch = "wasm32"))]
    {
        use crate::cmd::reads::{read_ahead, read_land};
        let mut copy = Session::new(Project::default());
        let connection =
            copy.run("connection.new", &json!({"kind":"ogcapi","address":server.base,"auth":"header","keyName":"X-Key"})).unwrap();
        copy.run("connection.login", &json!({"id":connection["id"],"apiKey":"synthetic-service-add","allowInsecure":true})).unwrap();
        params["connection"] = connection["id"].clone();
        let read = read_ahead(&mut copy, "service.add", &params).unwrap();
        assert!(read.reads());
        let done = read.run(&crate::analysis::control::Control::new()).unwrap();
        assert!(copy.map().tiles.is_empty(), "discovery cannot install a layer before landing");
        read_land(&mut copy, done).unwrap();
        let tile = &copy.map().tiles[0];
        assert_eq!(tile.connection, connection["id"].as_str().unwrap());
        assert!(crate::render::rustgis_tiles::logins::apply(&tile.url).unwrap().is_some());
        assert_eq!(crate::render::rustgis_tiles::logins::inner(&tile.url), format!("{}/good?f=mapbox", server.base));
    }
    assert_eq!(server.requests().len(), requests, "supplied discovery and add never refetch documents");
}

#[test]
fn ogc_dataset_styles_require_templates_allowed_from_the_tileset_document() {
    // The first read from the internet (the app's record of its root), on its own name as the
    // record is process-wide; the last on the local network.
    crate::render::rustgis_tiles::cache::restrict_to_public("https://maps-web-templates.example.org/dataset");
    for (origin, template, allowed) in [
        ("https://maps-web-templates.example.org", "http://127.0.0.1/{z}/{y}/{x}.pbf", false),
        ("https://maps.example.org", "https://tiles.example.org/{z}/{y}/{x}.pbf", true),
        ("http://127.0.0.1", "http://127.0.0.1/{z}/{y}/{x}.pbf", true),
    ] {
        let root = json!({"links":[{"rel":"tilesets-vector","href":"/tiles?f=json"},{"rel":"styles","href":"/styles?f=json"}]});
        let item = template.replace("{z}", "{tileMatrix}").replace("{y}", "{tileRow}").replace("{x}", "{tileCol}");
        let tiles = json!({"tilesets":[{"dataType":"vector","tileMatrixSetId":"WebMercatorQuad","links":[
            {"rel":"item","type":"application/vnd.mapbox-vector-tile","href":item}]}]});
        let styles = json!({"styles":[{"id":"roads","links":[
            {"rel":"stylesheet","type":"application/vnd.mapbox.style+json","href":"/style?f=mapbox"}]}]});
        let style = json!({"version":8,"sources":{"tiles":{"type":"vector","tiles":[template]}},
            "layers":[{"id":"roads","type":"line","source":"tiles","source-layer":"roads"}]});
        let mut params = json!({"url":format!("{origin}/dataset"),"type":"ogcapi","text":root.to_string(),
            "documents":[{"url":format!("{origin}/tiles?f=json"),"text":tiles.to_string()},
                {"url":format!("{origin}/styles?f=json"),"text":styles.to_string()},
                {"url":format!("{origin}/style?f=mapbox"),"text":style.to_string()}]});
        let mut session = Session::new(Project::default());
        let listing = session.run("service.layers", &params).unwrap();
        assert!(listing["next"].is_null(), "all fixture documents must be consumed: {listing}");
        assert_eq!(listing["count"], usize::from(allowed), "{listing}");
        let notes = listing["notes"].as_array().unwrap();
        if allowed {
            assert!(notes.is_empty(), "{listing}");
        } else {
            assert!(
                notes.iter().any(|n| n.as_str().unwrap().starts_with("1 tilesets were left out: disallowed template addresses")),
                "{listing}"
            );
            params["layer"] = json!("roads");
            assert!(session.run("service.add", &params).is_err());
            assert!(session.map().tiles.is_empty());
        }
    }
}

#[test]
fn ogc_dataset_shared_map_vector_documents_are_read_once_for_both_offers() {
    for linked in [false, true] {
        let root = json!({"links":[
            {"rel":"tilesets-vector","href":"/tiles?f=json"},
            {"rel":"tilesets-map","href":"/tiles?f=json"}
        ]});
        let metadata = json!({"vector_layers":[{"id":"roads"}],"links":[
            {"rel":"item","type":"application/vnd.mapbox-vector-tile","href":"/vector/{tileMatrix}/{tileRow}/{tileCol}.pbf"},
            {"rel":"item","type":"image/png","href":"/map/{tileMatrix}/{tileRow}/{tileCol}.png"}]});
        let tiles = if linked {
            json!({"tilesets":[{"tileMatrixSetId":"WebMercatorQuad","links":[
                {"rel":"self","type":"application/json","href":"/metadata?f=json"}]}]})
        } else {
            json!({"tilesets":[
                {"dataType":"vector","tileMatrixSetId":"WebMercatorQuad","vector_layers":[{"id":"roads"}],"links":[
                    {"rel":"item","type":"application/vnd.mapbox-vector-tile","href":"/vector/{tileMatrix}/{tileRow}/{tileCol}.pbf"}]},
                {"dataType":"map","tileMatrixSetId":"WebMercatorQuad","links":[
                    {"rel":"item","type":"image/png","href":"/map/{tileMatrix}/{tileRow}/{tileCol}.png"}]}
            ]})
        };
        let server = Server::new(vec![
            Route::new("/dataset", &root.to_string()),
            Route::new("/tiles", &tiles.to_string()),
            Route::new("/metadata", &metadata.to_string()),
        ]);
        let mut session = Session::new(Project::default());
        let mut params = json!({"url":format!("{}/dataset", server.base),"type":"ogcapi"});
        let native = session.run("service.layers", &params).unwrap();
        assert_eq!(native["count"], 2, "{native}");
        let layers = native["layers"].as_array().unwrap();
        assert!(layers.iter().any(|l| l["offer"]["type"] == "vectorTiles"), "{native}");
        assert!(layers.iter().any(|l| l["offer"]["type"] == "tiles"), "{native}");
        assert!(native["notes"].as_array().unwrap().is_empty(), "{native}");
        let requests = 2 + usize::from(linked);
        assert_eq!(server.requests().len(), requests, "each shared document must be fetched only once");
        params["text"] = json!(root.to_string());
        params["documents"] = json!([{"url":format!("{}/tiles?f=json",server.base),"text":tiles.to_string()}]);
        if linked {
            params["documents"]
                .as_array_mut()
                .unwrap()
                .push(json!({"url":format!("{}/metadata?f=json",server.base),"text":metadata.to_string()}));
        }
        assert_eq!(session.run("service.layers", &params).unwrap(), native);
        assert_eq!(server.requests().len(), requests, "supplied replay must not fetch anything");
    }
}

#[test]
fn ogc_dataset_shared_documents_keep_independent_offers() {
    for metadata in [false, true] {
        let mut docs = documents();
        if metadata {
            docs[0].1["links"].as_array_mut().unwrap().truncate(1);
            docs[1].1 = json!({"tilesets":[
                {"tileMatrixSetId":"WebMercatorQuad","vector_layers":[{"id":"roads"}],"links":[{"rel":"self","type":"application/json","href":"/shared?f=json"}]},
                {"tileMatrixSetId":"WebMercatorQuad","vector_layers":[{"id":"roads"}],"links":[{"rel":"self","type":"application/json","href":"/shared?f=json"}]},
                {"tileMatrixSetId":"WebMercatorQuad","title":"Independent","vector_layers":[{"id":"roads"}],"links":[{"rel":"self","type":"application/json","href":"/later?f=json"}]}
            ]});
            let body = json!({"links":[{"rel":"item","type":"application/vnd.mapbox-vector-tile","href":"/wm/{tileMatrix}/{tileRow}/{tileCol}?f=mvt"}]});
            docs.extend([("/shared", body.clone()), ("/later", body)]);
        } else {
            docs[2].1 = json!({"styles":[
                {"id":"first","links":[{"rel":"stylesheet","type":"application/vnd.mapbox.style+json","href":"/good?f=mapbox"}]},
                {"id":"alias","links":[{"rel":"stylesheet","type":"application/vnd.mapbox.style+json","href":"/good?f=mapbox"}]},
                {"id":"later","links":[{"rel":"stylesheet","type":"application/vnd.mapbox.style+json","href":"/later?f=mapbox"}]}
            ]});
            docs.push(("/later", docs[3].1.clone()));
        }
        let server = Server::new(docs.iter().map(|(path, body)| Route::new(path, &body.to_string())).collect());
        let mut session = Session::new(Project::default());
        let mut params = json!({"url":format!("{}/dataset", server.base),"type":"ogcapi"});
        let native = session.run("service.layers", &params).unwrap();
        assert_eq!(native["count"], 2, "{native}");
        assert!(native["layers"].as_array().unwrap().iter().any(|l| l["name"] == "later" || l["title"] == "Independent"), "{native}");
        assert!(
            native["notes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n.as_str().unwrap().starts_with("1 dataset documents were left out: repeated addresses")),
            "{native}"
        );
        let requests = server.requests();
        assert_eq!(requests.iter().filter(|r| r.path.starts_with(if metadata { "/shared" } else { "/good" })).count(), 1);
        params["text"] = json!(docs[0].1.to_string());
        params["documents"] = json!(
            requests
                .iter()
                .skip(1)
                .map(|r| {
                    let body = &docs.iter().find(|(p, _)| *p == r.path).unwrap().1;
                    let mut address = url::Url::parse(&format!("{}{}", server.base, r.path)).unwrap();
                    address.query_pairs_mut().extend_pairs(r.query.iter().map(|(k, v)| (k, v)));
                    json!({"url":address.as_str(),"text":body.to_string()})
                })
                .collect::<Vec<_>>()
        );
        assert_eq!(session.run("service.layers", &params).unwrap(), native);
    }
}

#[test]
fn ogc_dataset_metadata_and_tilejson_are_read_before_listing() {
    let mut docs = documents();
    docs[1].1 = json!({"tilesets":[{"tileMatrixSetId":"WebMercatorQuad","dataType":"vector","title":"Road tiles","vector_layers":[{"id":"roads"}],"links":[
        {"rel":"self","type":"application/json","href":"/metadata?f=json"}]}]});
    docs.push((
        "/metadata",
        json!({"links":[
        {"rel":"item","type":"application/vnd.mapbox-vector-tile","href":"/wm/{tileMatrix}/{tileRow}/{tileCol}?f=mvt"}]}),
    ));
    docs[2].1["styles"].as_array_mut().unwrap().truncate(1);
    docs[3].1["sources"]["basemap"] = json!({"type":"vector","url":"/tilejson"});
    docs.push(("/tilejson", json!({"tiles":["/wm/{z}/{y}/{x}?f=mvt"]})));
    let root = "https://maps.example.org/dataset";
    let mut params = json!({"url":root,"type":"ogcapi","text":docs[0].1.to_string(),"documents":[]});
    for (path, body) in docs.iter().skip(1) {
        let url = format!(
            "https://maps.example.org{path}{}",
            if ["/tiles", "/styles", "/metadata"].contains(path) {
                "?f=json"
            } else if *path == "/good" {
                "?f=mapbox"
            } else {
                ""
            }
        );
        params["documents"].as_array_mut().unwrap().push(json!({"url":url,"type":"ogcapi","text":body.to_string()}));
    }
    let mut session = Session::new(Project::default());
    let listing = session.run("service.layers", &params).unwrap();
    assert_eq!(listing["count"], 2, "{listing}");
    assert_eq!(listing["layers"][0]["title"], "Road tiles");
    assert_eq!(listing["layers"][0]["offer"]["layers"], json!(["roads"]));
    assert!(listing.get("next").is_none());
    assert!(listing["notes"].as_array().unwrap().is_empty(), "{listing}");
    params["layer"] = listing["layers"][0]["name"].clone();
    params["source_url"] = listing["layers"][0]["source_url"].clone();
    session.run("service.add", &params).unwrap();
    assert!(session.map().tiles[0].url.contains("/wm/{z}/{y}/{x}?f=mvt"));
    params["documents"][1]["text"] =
        json!(params["documents"][1]["text"].as_str().unwrap().replace("/good?f=mapbox", "/good?f=mapbox&key=fixture-key"));
    params["documents"][2]["url"] = json!("https://maps.example.org/good?f=mapbox&key=fixture-key");
    let listing = session.run("service.layers", &params).unwrap();
    assert_eq!(listing["count"], 2, "{listing}");
    assert!(listing["notes"].as_array().unwrap().iter().any(|n| n.as_str().unwrap().contains("didn't get its keys")));
}

#[test]
fn ogc_dataset_tilejson_completion_keeps_the_stylesheets_login_provenance() {
    for foreign_catalog in [false, true] {
        let mut docs = documents();
        docs[1].1["tilesets"].as_array_mut().unwrap().remove(0);
        docs[2].1["styles"].as_array_mut().unwrap().truncate(1);
        let catalog = if foreign_catalog { "https://catalog.example.org" } else { "https://maps.example.org" };
        docs[0].1["links"][1]["href"] = json!(format!("{catalog}/styles?f=json"));
        docs[2].1["styles"][0]["links"][0]["href"] = json!("https://maps.example.org/good?f=mapbox");
        docs[3].1["sources"]["basemap"] = json!({"type":"vector","url":"https://sources.example.org/tilejson"});
        let mut session = Session::new(Project::default());
        let connection = session
            .run(
                "connection.new",
                &json!({"kind":"ogcapi","address":"https://maps.example.org",
            "auth":"header","keyName":"X-Key"}),
            )
            .unwrap();
        session.run("connection.login", &json!({"id":connection["id"],"apiKey":"RGSYNTH_dataset_provenance"})).unwrap();
        let mut params = json!({"url":"https://maps.example.org/dataset","type":"ogcapi","connection":connection["id"],
        "text":docs[0].1.to_string(),"documents":[
            {"url":"https://maps.example.org/tiles?f=json","text":docs[1].1.to_string()},
            {"url":format!("{catalog}/styles?f=json"),"text":docs[2].1.to_string()},
            {"url":"https://maps.example.org/good?f=mapbox","text":docs[3].1.to_string()},
            {"url":"https://sources.example.org/tilejson","text":json!({"tiles":["https://maps.example.org/wm/{z}/{y}/{x}?f=mvt"]}).to_string()}
        ]});
        let listing = session.run("service.layers", &params).unwrap();
        assert!(listing["next"].is_null(), "{listing}");
        assert_eq!(listing["count"], 1, "{listing}");
        assert!(listing["notes"].as_array().unwrap().is_empty(), "{listing}");
        let style = &listing["layers"][0];
        params["layer"] = style["name"].clone();
        params["source_url"] = style["source_url"].clone();
        session.run("service.add", &params).unwrap();
        assert_eq!(session.map().tiles.len(), 1);
        assert_eq!(session.map().tiles[0].connection.is_empty(), foreign_catalog);
        let source = style["source_url"].as_str().unwrap();
        assert_eq!(crate::data::login_address::inner(source), "https://maps.example.org/good?f=mapbox", "{listing}");
        assert_eq!(style["anonymous"].as_bool().unwrap_or(false), foreign_catalog, "{listing}");
        let bound = crate::data::login_address::parse(style["offer"]["url"].as_str().unwrap()).unwrap().is_some();
        assert_eq!(bound, !foreign_catalog, "the stylesheet retains only its own login history: {listing}");
    }
}

#[test]
fn ogc_dataset_reports_missing_names_and_rejects_mixed_style_sources() {
    let mut docs = documents();
    docs[3].1["sources"]["basemap"]["tiles"].as_array_mut().unwrap().push(json!("/rd/{z}/{y}/{x}?f=mvt"));
    let server = Server::new(docs.iter().map(|(path, body)| Route::new(path, &body.to_string())).collect());
    let mut session = Session::new(Project::default());
    let listing = session.run("service.layers", &json!({"url":format!("{}/dataset",server.base),"type":"ogcapi"})).unwrap();
    assert_eq!(listing["count"], 0);
    assert!(listing["notes"].as_array().unwrap().iter().any(|n| n.as_str().unwrap().contains("neither named vector-layer metadata")));
}

#[test]
fn ogc_dataset_style_discovery_is_bounded_and_checks_source_addresses() {
    let mut docs = documents();
    docs[2].1["styles"] = json!(
        (0..17)
            .map(|i| json!({"id":format!("style-{i}"),"links":[
                {"rel":"stylesheet","type":"application/vnd.mapbox.style+json","href":format!("/style-{i}")}
            ]}))
            .collect::<Vec<_>>()
    );
    let good = docs[3].1.clone();
    let mut routes: Vec<Route> = docs.iter().map(|(path, body)| Route::new(path, &body.to_string())).collect();
    for i in 0..17 {
        routes.push(Route::new(&format!("/style-{i}"), &good.to_string()));
    }
    let server = Server::new(routes);
    let mut session = Session::new(Project::default());
    let listing = session.run("service.layers", &json!({"url":format!("{}/dataset",server.base),"type":"ogcapi"})).unwrap();
    assert_eq!(listing["count"], 16, "{listing}");
    assert!(listing["notes"].as_array().unwrap().iter().any(|n| n.as_str().unwrap().contains("16-style discovery limit")));
    assert_eq!(server.requests().len(), 19);
    // The root read from the internet (the app's record of it), on its own name as the record is
    // process-wide.
    let root = "https://maps-web-styles.example.org/dataset";
    crate::render::rustgis_tiles::cache::restrict_to_public(root);
    let mut docs = documents();
    docs[2].1["styles"].as_array_mut().unwrap().truncate(1);
    docs[3].1["sources"]["basemap"] = json!({"type":"vector","url":"http://127.0.0.1/private"});
    let params = json!({"url":root,"type":"ogcapi","text":docs[0].1.to_string(),"documents":[
        {"url":"https://maps-web-styles.example.org/tiles?f=json","text":docs[1].1.to_string()},
        {"url":"https://maps-web-styles.example.org/styles?f=json","text":docs[2].1.to_string()},
        {"url":"https://maps-web-styles.example.org/good?f=mapbox","text":docs[3].1.to_string()}
    ]});
    let mut params = params;
    params["documents"][1]["text"] = json!(
        json!({"styles":[
            {"id":"private", "links":[{"rel":"stylesheet","type":"application/vnd.mapbox.style+json","href":"/good?f=mapbox"}]},
            {"id":"public", "links":[{"rel":"stylesheet","type":"application/vnd.mapbox.style+json","href":"/public"}]}
        ]})
        .to_string()
    );
    params["documents"].as_array_mut().unwrap().push(json!({"url":"https://maps-web-styles.example.org/public","text":good.to_string()}));
    let listing = session.run("service.layers", &params).unwrap();
    assert_eq!(listing["count"], 1, "{listing}");
    assert_eq!(listing["layers"][0]["name"], "public");
    assert!(listing["next"].is_null(), "no private source is fetched or requested");
    assert!(listing["notes"].as_array().unwrap().iter().any(|n| n.as_str().unwrap().contains("disallowed addresses")));
}

#[test]
fn ogc_dataset_canonical_grid_allows_an_opaque_scheme_definition_link() {
    use rustgis_data::ogc::{TilesStep, resolve_tiles};
    let root = "https://maps.example.org/dataset";
    let text = json!({"links":[{"rel":"tilesets-map","type":"application/json","href":"/tiles"}]}).to_string();
    let scheme = json!({"rel":"tiling-scheme","href":"/definitions/smerc"});
    let set = json!({"tileMatrixSetURI":"http://www.opengis.net/def/tilematrixset/OGC/1.0/WebMercatorQuad", "links":[scheme,
        {"rel":"self","type":"application/json","href":"/metadata"}]});
    let metadata = json!({"links":[scheme,{"rel":"item","type":"image/png","href":"/map/{tileMatrix}/{tileRow}/{tileCol}.png"}]});
    let mut direct = set.clone();
    direct["links"] = metadata["links"].clone();
    let direct = json!({"tilesets":[direct]}).to_string();
    assert_eq!(
        resolve_tiles("https://maps.example.org/tiles", "https://maps.example.org/tiles", direct.as_bytes(), false).unwrap().step,
        TilesStep::Template("https://maps.example.org/map/{z}/{y}/{x}.png".into()),
        "the shared collection resolver accepts the canonical grid too"
    );
    let params = json!({"url":root,"type":"ogcapi","text":text,"documents":[
        {"url":"https://maps.example.org/tiles","text":json!({"tilesets":[set]}).to_string()},
        {"url":"https://maps.example.org/metadata?f=json","text":metadata.to_string()}
    ]});
    let listing = Session::new(Project::default()).run("service.layers", &params).unwrap();
    assert_eq!(listing["count"], 1, "{listing}");
    assert!(listing["notes"].as_array().unwrap().is_empty(), "{listing}");
}

#[test]
fn ogc_dataset_rejects_direct_and_contradictory_tileset_projections() {
    let root = "https://maps.example.org/dataset";
    let landing = json!({"links":[{"rel":"tilesets-map","type":"application/json","href":"/tiles"}]}).to_string();
    let mut session = Session::new(Project::default());
    for linked in [false, true] {
        for grid in ["NetherlandsRDNewQuad", "EuropeanETRS89_LAEAQuad"] {
            let metadata = json!({"tileMatrixSetId":grid,"links":[
                {"rel":"item","type":"image/png","href":"/wrong/{tileMatrix}/{tileRow}/{tileCol}.png"}
            ]})
            .to_string();
            let mut params =
                json!({"url":root,"type":"ogcapi","text":landing,"documents":[{"url":"https://maps.example.org/tiles","text":metadata}]});
            if linked {
                params["documents"][0]["text"] = json!(
                    json!({"tilesets":[{"tileMatrixSetId":"WebMercatorQuad","links":[
                    {"rel":"self","type":"application/json","href":"/metadata"}]}]})
                    .to_string()
                );
                params["documents"].as_array_mut().unwrap().push(json!({"url":"https://maps.example.org/metadata?f=json","text":metadata}));
            }
            let listing = session.run("service.layers", &params).unwrap();
            assert_eq!(listing["count"], 0, "{listing}");
            assert!(listing["notes"].as_array().unwrap().iter().any(|n| n.as_str().unwrap().contains("tile matrix set")));
        }
    }
}

#[test]
fn ogc_dataset_linked_scheme_declarations_are_checked() {
    for (grid, count) in [("WebMercatorQuad", 1), ("EuropeanETRS89_LAEAQuad", 0)] {
        let landing = json!({"links":[{"rel":"tilesets-map","type":"application/json","href":"/tiles"}]}).to_string();
        let list = json!({"tilesets":[{"tileMatrixSetId":"WebMercatorQuad","links":[
            {"rel":"self","type":"application/json","href":"/metadata"}]}]})
        .to_string();
        let metadata = json!({"links":[{"rel":"tiling-scheme","href":format!("https://definitions.example.org/{grid}")},
            {"rel":"item","type":"image/png","href":"/map/{tileMatrix}/{tileRow}/{tileCol}.png"}]})
        .to_string();
        let listing = Session::new(Project::default())
            .run(
                "service.layers",
                &json!({
            "url":"https://maps.example.org/dataset","type":"ogcapi","text":landing,"documents":[
                {"url":"https://maps.example.org/tiles","text":list},
                {"url":"https://maps.example.org/metadata?f=json","text":metadata}]}),
            )
            .unwrap();
        assert!(listing["next"].is_null(), "{listing}");
        assert_eq!(listing["count"], count, "{listing}");
        let notes = listing["notes"].as_array().unwrap();
        if count == 1 {
            assert!(notes.is_empty(), "{listing}");
        } else {
            assert_eq!(notes.len(), 1, "{listing}");
            assert!(
                notes[0].as_str().unwrap().starts_with("1 tilesets were left out: unsupported or undeclared tile matrix set"),
                "{listing}"
            );
        }
    }
}

#[test]
fn ogc_dataset_tilesets_have_distinct_addable_names() {
    let root = "https://maps.example.org/dataset";
    let landing = json!({"links":[{"rel":"tilesets-map","type":"application/json","href":"/tiles"}]}).to_string();
    let sets = json!({"tilesets": (1..=2).map(|id| json!({"tileMatrixSetId":"WebMercatorQuad", "links":[
        {"rel":"item","type":"image/png","href":format!("/map-{id}/{{tileMatrix}}/{{tileRow}}/{{tileCol}}.png")}
    ]})).collect::<Vec<_>>()})
    .to_string();
    let mut params = json!({"url":root,"type":"ogcapi","text":landing,"documents":[{"url":"https://maps.example.org/tiles","text":sets}]});
    let mut session = Session::new(Project::default());
    let listing = session.run("service.layers", &params).unwrap();
    assert_eq!(listing["count"], 2, "{listing}");
    assert_eq!(listing["layers"][0]["name"], "map");
    assert_ne!(listing["layers"][0]["name"], listing["layers"][1]["name"]);
    for layer in listing["layers"].as_array().unwrap() {
        params["layer"] = layer["name"].clone();
        params["source_url"] = layer["source_url"].clone();
        let added = session.run("service.add", &params).unwrap();
        let index = added["index"].as_u64().unwrap() as usize;
        assert_eq!(session.map().tiles[index].url, layer["offer"]["url"].as_str().unwrap());
    }
    assert_ne!(session.map().tiles[0].url, session.map().tiles[1].url);
}

#[test]
fn ogc_dataset_linked_tilesets_keep_the_advertised_order() {
    let root = "https://maps.example.org/dataset";
    let text = json!({"links":[{"rel":"tilesets-map","href":"/tiles"}]}).to_string();
    let sets = json!({"tilesets": (1..=2).map(|id| json!({"tileMatrixSetId":"WebMercatorQuad", "links":[
        {"rel":"self","type":"application/json","href":format!("/map-{id}")}
    ]})).collect::<Vec<_>>()})
    .to_string();
    let mut documents = vec![json!({"url":"https://maps.example.org/tiles?f=json","text":sets})];
    for id in 1..=2 {
        let metadata =
            json!({"links":[{"rel":"item","type":"image/png","href":format!("/map-{id}/{{tileMatrix}}/{{tileRow}}/{{tileCol}}.png")}]})
                .to_string();
        documents.push(json!({"url":format!("https://maps.example.org/map-{id}?f=json"),"text":metadata}));
    }
    let mut session = Session::new(Project::default());
    let listing = session.run("service.layers", &json!({"url":root,"type":"ogcapi","text":text,"documents":documents})).unwrap();
    assert_eq!(listing["count"], 2, "{listing}");
    for (index, id) in [1, 2].into_iter().enumerate() {
        assert_eq!(listing["layers"][index]["offer"]["url"], format!("https://maps.example.org/map-{id}/{{z}}/{{y}}/{{x}}.png"));
    }
}

#[test]
fn ogc_dataset_service_discovery_accepts_an_explicit_map_template() {
    let root = "https://maps.example.org/dataset";
    let landing = json!({"title":"Map", "links":[{"rel":"tilesets-map","type":"application/json","href":"/tiles"}]}).to_string();
    let template = "https://maps.example.org/resolved/{z}/{y}/{x}.png";
    let mut params = json!({"url":root,"type":"ogcapi","text":landing,"layer":"map","tiles":template,
        "source_url":"https://maps.example.org/dataset?f=json"});
    let mut session = Session::new(Project::default());
    let added = session.run("service.add", &params).unwrap();
    assert_eq!(session.map().tiles[added["index"].as_u64().unwrap() as usize].url, template);
    params["tiles"] = json!("not-a-template");
    assert!(session.run("service.add", &params).unwrap_err().contains("not a tile template"));

    // A current listing's metadata source must still select the normal validated map offer.
    params["tiles"] = json!(template);
    params["source_url"] = json!("https://maps.example.org/tiles");
    params["documents"] = json!([{"url":"https://maps.example.org/tiles","text":json!({"tilesets":[{
        "tileMatrixSetId":"WebMercatorQuad","dataType":"map","links":[
            {"rel":"item","type":"image/png","href":"/resolved/{tileMatrix}/{tileRow}/{tileCol}.png"}]
    }]}).to_string()}]);
    let added = session.run("service.add", &params).unwrap();
    assert_eq!(session.map().tiles[added["index"].as_u64().unwrap() as usize].url, template);

    // Rejecting an invalid explicit template must not hide keys withheld during discovery.
    params["url"] = json!(format!("{root}?key=fixture-key"));
    params.as_object_mut().unwrap().remove("source_url");
    params["tiles"] = json!("not-a-template");
    session.notices.clear();
    assert!(session.run("service.add", &params).unwrap_err().contains("not a tile template"));
    assert_eq!(session.notices.iter().filter(|n| !n.error && n.text.contains("address(es)")).count(), 1);
}

#[test]
fn ogc_dataset_metadata_bytes_are_bounded_without_discarding_valid_tiles() {
    let mut docs = documents();
    docs[1].1["tilesets"] = json!([{
        "tileMatrixSetId":"WebMercatorQuad","dataType":"vector","vector_layers":[{"id":"roads"}],
        "links":[{"rel":"item","type":"application/vnd.mapbox-vector-tile","href":"/wm/{tileMatrix}/{tileRow}/{tileCol}?f=mvt"}]
    }]);
    docs[2].1["styles"].as_array_mut().unwrap().truncate(1);
    let mut params = json!({"url":"https://maps.example.org/dataset","type":"ogcapi","text":docs[0].1.to_string(),"documents":[
        {"url":"https://maps.example.org/tiles?f=json","text":docs[1].1.to_string()},
        {"url":"https://maps.example.org/styles?f=json","text":docs[2].1.to_string()},
        {"url":"https://maps.example.org/good?f=mapbox","text":docs[3].1.to_string()}
    ]});
    let mut session = Session::new(Project::default());
    let clean = session.run("service.layers", &params).unwrap();
    assert_eq!(clean["count"], 2, "{clean}");
    assert!(clean["notes"].as_array().unwrap().is_empty());
    params["documents"][2]["text"] = json!(format!("{}{}", docs[3].1, " ".repeat(crate::cmd::service::MAX_DATASET_BYTES)));
    let partial = session.run("service.layers", &params).unwrap();
    assert_eq!(partial["count"], 1, "{partial}");
    assert!(partial.get("next").is_none());
    assert!(partial["notes"].as_array().unwrap().iter().any(|n| n.as_str().unwrap().contains("metadata budget")));
}

#[test]
fn ogc_dataset_failed_discovery_keeps_prior_omission_counts() {
    for omitted in [false, true] {
        let mut sets = vec![json!({"tileMatrixSetId":"WebMercatorQuad","links":[{"rel":"self","href":"/metadata?f=json"}]})];
        if omitted {
            sets.insert(0, json!({"tileMatrixSetId":"Unsupported"}));
        }
        let params = json!({"url":"https://maps.example.org/dataset","type":"ogcapi","layer":"map",
        "text":json!({"links":[{"rel":"tilesets-map","href":"/tiles?f=json"},{"rel":"styles","href":"/styles"}]}).to_string(),
        "documents":[
            {"url":"https://maps.example.org/tiles?f=json","text":json!({"tilesets":sets}).to_string()},
            {"url":"https://maps.example.org/metadata?f=json","text":" ".repeat(crate::cmd::service::MAX_DATASET_BYTES)}
        ]});
        let mut session = Session::new(Project::default());
        for command in ["service.layers", "service.add"] {
            let error = session.run(command, &params).unwrap_err();
            assert!(error.contains("metadata budget"), "{error}");
            assert!(error.contains("1 tileset documents were left out: dataset discovery stopped"), "{error}");
            assert!(error.contains("1 style catalogues were left out: dataset discovery stopped"), "{error}");
            assert_eq!(error.contains("1 tilesets were left out"), omitted, "{error}");
            #[cfg(not(target_arch = "wasm32"))]
            {
                use crate::cmd::reads::{read_ahead, read_land};
                let read = read_ahead(&mut session, command, &params).unwrap().run(&crate::analysis::control::Control::new()).unwrap();
                assert_eq!(read_land(&mut session, read).unwrap_err(), error, "worker errors must retain the same counts after rollback");
            }
        }
    }
}

#[test]
fn ogc_dataset_map_tiles_reject_declared_coverage_and_unknown_types() {
    for listed in [false, true] {
        for kind in [
            None,
            Some(json!("map")),
            Some(json!("vector")),
            Some(json!("coverage")),
            Some(json!("unknown")),
            Some(json!(5)),
            Some(Value::Null),
        ] {
            let mut tileset = json!({"tileMatrixSetId":"WebMercatorQuad","links":[
                {"rel":"item","type":"image/png","href":"/tiles/{tileMatrix}/{tileRow}/{tileCol}.png"}
            ]});
            if let Some(kind) = &kind {
                tileset["dataType"] = kind.clone();
            }
            let document = if listed { json!({"tilesets":[tileset]}) } else { tileset };
            let listing = Session::new(Project::default())
                .run(
                    "service.layers",
                    &json!({
                        "url":"https://maps.example.org/dataset","type":"ogcapi",
                        "text":json!({"links":[{"rel":"tilesets-map","href":"/tiles?f=json"}]}).to_string(),
                        "documents":[{"url":"https://maps.example.org/tiles?f=json","text":document.to_string()}]
                    }),
                )
                .unwrap();
            let supported = kind.as_ref().is_none_or(|k| k == "map");
            assert_eq!(listing["count"], usize::from(supported), "{kind:?}: {listing}");
            let notes = listing["notes"].as_array().unwrap();
            if supported {
                assert!(notes.is_empty(), "{listing}");
            } else {
                assert_eq!(notes.len(), 1, "{listing}");
                assert!(notes[0].as_str().unwrap().starts_with("1 tilesets were left out"), "{listing}");
            }
        }
    }
}
