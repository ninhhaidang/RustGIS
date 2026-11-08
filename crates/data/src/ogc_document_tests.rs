use super::*;

#[path = "../../../tests/support/ogc_document_logins.rs"]
mod documents;

const BASE: &str = "https://maps.example.org/folder/caps.xml";
const ORIGIN: &str = "https://maps.example.org";
const BASE_BINDING: &str = "0123456789abcdef";
const FOREIGN_BINDING: &str = "fedcba9876543210";

fn requests(service: &Service) -> Vec<String> {
    assert_eq!(service.layers.len(), 1);
    let layer = &service.layers[0];
    let mut requests = match &layer.offer {
        Offer::Tiles { template } => vec![template.clone()],
        Offer::Features { url } => vec![url.clone()],
        Offer::Coverage(c) => vec![
            describe_coverage_url(c),
            get_coverage_url(c, &CoverageInfo { format: "image/tiff".into(), axes: None, bbox: None }, [1.0, 2.0, 3.0, 4.0]),
        ],
        other => panic!("unexpected offer: {other:?}"),
    };
    if let Some(info) = &layer.feature_info {
        assert!(info.queryable(), "trusted inherited wrappers remain usable internally");
        requests.push(info.request(0.0, 0.0, 1.0).unwrap());
    }
    requests.extend(layer.legends.iter().map(|legend| legend.url.clone()));
    requests
}

fn assert_authority(request: &str, binding: Option<&str>, target: &str, context: &str) {
    let bound = crate::login_address::parse(request).unwrap();
    assert_eq!(bound.map(|b| b.binding), binding, "{context}: {request}");
    if let Some(bound) = bound {
        assert_eq!(bound.origin, ORIGIN, "{context}");
    }
    let inner = crate::login_address::inner(request);
    assert!(!inner.contains(crate::login_address::PREFIX), "{context}: nested runtime authority");
    assert!(inner.starts_with(target), "{context}: {inner} should target {target}");
}

#[test]
fn document_endpoints_inherit_only_the_trusted_base_login() {
    let bound_base = crate::login_address::bind(BASE_BINDING, ORIGIN, BASE).unwrap();
    let foreign =
        crate::login_address::bind(FOREIGN_BINDING, "https://foreign.example.org", "https://foreign.example.org/advertised").unwrap();
    let same_origin_foreign = crate::login_address::bind(FOREIGN_BINDING, ORIGIN, "https://maps.example.org/advertised").unwrap();
    for (href, target, same_origin) in [
        ("https://maps.example.org/advertised", "https://maps.example.org/advertised", true),
        ("https://MAPS.example.org:443/advertised", "https://MAPS.example.org:443/advertised", true),
        ("http://maps.example.org/advertised", "http://maps.example.org/advertised", false),
        ("https://maps.example.org:444/advertised", "https://maps.example.org:444/advertised", false),
        ("https://foreign.example.org/advertised", "https://foreign.example.org/advertised", false),
        ("//foreign.example.org/advertised", "https://foreign.example.org/advertised", false),
        ("/advertised", "https://maps.example.org/advertised", true),
        ("advertised", "https://maps.example.org/folder/advertised", true),
        ("?map=advertised", "https://maps.example.org/folder/caps.xml?map=advertised", true),
        (
            "?upstream=https://foreign.example.org/tiles",
            "https://maps.example.org/folder/caps.xml?upstream=https://foreign.example.org/tiles",
            true,
        ),
        ("proxy?src=https://data.example/t&sig=a%2Bb", "https://maps.example.org/folder/proxy?src=https://data.example/t&sig=a%2Bb", true),
        ("/proxy?src=https://data.example/t&sig=a%2Bb", "https://maps.example.org/proxy?src=https://data.example/t&sig=a%2Bb", true),
        ("//maps.example.org/proxy?src=https://data.example/t", "https://maps.example.org/proxy?src=https://data.example/t", true),
        ("//foreign.example.org/proxy?src=https://data.example/t", "https://foreign.example.org/proxy?src=https://data.example/t", false),
        ("proxy/https://data.example/t", "https://maps.example.org/folder/proxy/https://data.example/t", true),
        (foreign.as_str(), "https://foreign.example.org/advertised", false),
        (same_origin_foreign.as_str(), "https://maps.example.org/advertised", true),
    ] {
        for (base, has_login) in [(BASE, false), (bound_base.as_str(), true)] {
            for (boundary, document) in documents::documents(href) {
                let service = parse(base, document.as_bytes()).unwrap();
                let expected = (has_login && same_origin).then_some(BASE_BINDING);
                let requests = requests(&service);
                for request in &requests {
                    assert_authority(request, expected, target, boundary);
                }
                let inner = crate::login_address::inner(&requests[0]);
                match &service.layers[0].offer {
                    Offer::Tiles { .. } if boundary == "WMTS ResourceURL" => {
                        assert_eq!(inner, format!("{target}/{{z}}/{{y}}/{{x}}.png"));
                    }
                    Offer::Tiles { .. } => {
                        assert!(inner.contains(if service.kind == "WMS" { "REQUEST=GetMap" } else { "REQUEST=GetTile" }));
                    }
                    Offer::Features { .. } => assert!(inner.contains("REQUEST=GetFeature")),
                    Offer::Coverage(_) => assert!(inner.contains("REQUEST=DescribeCoverage")),
                    other => panic!("unexpected offer: {other:?}"),
                }
            }
        }
    }
}

#[test]
fn fragment_only_document_links_retain_the_endpoint_and_query() {
    let source = format!("{BASE}?map=kept#old");
    let bound_source = crate::login_address::bind(BASE_BINDING, ORIGIN, &source).unwrap();
    for href in ["#frag", "#"] {
        for (source, has_login) in [(source.as_str(), false), (bound_source.as_str(), true)] {
            for (boundary, document) in documents::documents(href) {
                let service = parse(source, document.as_bytes()).unwrap();
                for request in requests(&service) {
                    assert_authority(&request, has_login.then_some(BASE_BINDING), BASE, boundary);
                    let target = url::Url::parse(crate::login_address::inner(&request)).unwrap();
                    assert!(target.query_pairs().any(|(key, value)| key == "map" && value == "kept"), "{boundary}: {request}");
                }
            }
        }
    }
}

#[test]
fn malformed_document_wrappers_cannot_supply_authority() {
    let base = crate::login_address::bind(BASE_BINDING, ORIGIN, BASE).unwrap();
    for bad in [
        "rustgis-login:secret",
        "rustgis-login:fedcba9876543210:1:é:https://foreign.example.org/advertised",
        "rustgis-login:fedcba9876543210:0::https://foreign.example.org/advertised",
        "httpfoo://foreign.example.org/advertised",
        "file:///etc/passwd",
        "file:/etc/passwd",
        "mailto:x",
        "CuStOm+v1.0:payload",
    ] {
        for (boundary, document) in documents::documents(bad) {
            let service = parse(&base, document.as_bytes()).unwrap();
            for request in requests(&service) {
                assert_authority(&request, Some(BASE_BINDING), BASE, boundary);
            }
        }
    }
}

#[test]
fn oversized_same_origin_document_targets_fail_closed() {
    let base = crate::login_address::bind(BASE_BINDING, ORIGIN, BASE).unwrap();
    let target = format!("{ORIGIN}/{}", "a".repeat(17000));
    let prefix = format!("rustgis-login:{BASE_BINDING}:{}:{ORIGIN}:", ORIGIN.len());
    for (boundary, document) in documents::documents(&target) {
        let service = parse(&base, document.as_bytes()).unwrap();
        assert_eq!(service.layers.len(), 1, "{boundary}");
        let requests = match &service.layers[0].offer {
            Offer::Tiles { template } => vec![template.clone()],
            Offer::Features { url } => vec![url.clone()],
            Offer::Coverage(c) => vec![
                describe_coverage_url(c),
                get_coverage_url(c, &CoverageInfo { format: "image/tiff".into(), axes: None, bbox: None }, [1.0, 2.0, 3.0, 4.0]),
            ],
            other => panic!("unexpected offer: {other:?}"),
        };
        for request in requests {
            assert!(request.starts_with(&prefix), "{boundary}: must not downgrade to anonymous");
            assert!(request.contains(&target), "{boundary}: must not replace the target with the base endpoint");
            assert!(crate::login_address::parse(&request).is_err(), "{boundary}");
            assert!(crate::login_address::check(&request).is_err(), "{boundary}");
        }
    }
}

#[test]
fn same_origin_document_links_with_unsafe_characters_keep_the_login() {
    let bound_base = crate::login_address::bind(BASE_BINDING, ORIGIN, BASE).unwrap();
    for (href, target, same_origin) in [
        ("https://maps.example.org/a b/advertised", "https://maps.example.org/a%20b/advertised", true),
        ("/a b/advertised", "https://maps.example.org/a%20b/advertised", true),
        ("a b/advertised", "https://maps.example.org/folder/a%20b/advertised", true),
        ("a\\b/advertised", "https://maps.example.org/folder/a/b/advertised", true),
        ("?map=a b", "https://maps.example.org/folder/caps.xml?map=a%20b", true),
        ("https://foreign.example.org/a b/advertised", "https://foreign.example.org/a b/advertised", false),
    ] {
        for (base, has_login) in [(BASE, false), (bound_base.as_str(), true)] {
            for (boundary, document) in documents::documents(href) {
                let service = parse(base, document.as_bytes()).unwrap();
                let context = format!("{boundary} {href}");
                let expected = (has_login && same_origin).then_some(BASE_BINDING);
                for request in requests(&service) {
                    let bound = crate::login_address::parse(&request).unwrap();
                    assert_eq!(bound.map(|b| b.binding), expected, "{context}: {request}");
                    // An unbound link is used as written, like before; a bound one as it is sent.
                    let target = if expected.is_some() { target.to_string() } else { target.replace("%20", " ").replace("a/b", "a\\b") };
                    assert!(crate::login_address::inner(&request).starts_with(&target), "{context}: {request}");
                }
            }
        }
    }
}

fn assert_operation(request: &str, service: &str, operation: &str, version: &str) {
    let inner = crate::login_address::inner(request);
    let url = url::Url::parse(inner).unwrap();
    for (key, expected) in [("SERVICE", service), ("REQUEST", operation), ("VERSION", version)] {
        let values: Vec<_> = url.query_pairs().filter(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v.into_owned()).collect();
        assert_eq!(values, [expected], "{key}: {request}");
    }
    let query = inner.split_once('?').unwrap().1.split('#').next().unwrap();
    for pair in ["map=kept", "token=a%2Bb"] {
        assert_eq!(query.split('&').filter(|p| *p == pair).count(), 1, "{pair}: {request}");
    }
}

#[test]
fn fragment_endpoints_replace_capabilities_operation_parameters() {
    for (boundary, document) in documents::documents("#frag") {
        let (kind, version, operation) = match boundary {
            "WMS 1.3 GetMap" => ("WMS", "1.3.0", "GetMap"),
            "WMS 1.1 GetMap" => ("WMS", "1.1.1", "GetMap"),
            "WMTS KVP GetTile" => ("WMTS", "1.0.0", "GetTile"),
            "WFS 1.0 GetFeature" => ("WFS", "1.0.0", "GetFeature"),
            "WFS 2.0 GetFeature" => ("WFS", "2.0.0", "GetFeature"),
            "WCS 1.0 GetCoverage" => ("WCS", "1.0.0", "DescribeCoverage"),
            "WCS 2.0 GetCoverage" => ("WCS", "2.0.1", "DescribeCoverage"),
            "WMTS ResourceURL" => continue,
            other => panic!("unexpected fixture: {other}"),
        };
        let source = capabilities_url("https://maps.example.org/mapserv?map=kept&token=a%2Bb&VERSION=obsolete", Some(kind));
        let bound_source = crate::login_address::bind(BASE_BINDING, ORIGIN, &source).unwrap();
        for (source, has_login) in [(source.as_str(), false), (bound_source.as_str(), true)] {
            let service = parse(source, document.as_bytes()).unwrap();
            // Legends and REST templates have their own query semantics, not operation keys.
            for (i, request) in requests(&service).iter().take(if kind == "WCS" { 2 } else { 1 }).enumerate() {
                assert_authority(request, has_login.then_some(BASE_BINDING), "https://maps.example.org/mapserv", boundary);
                assert_operation(request, kind, if i == 1 { "GetCoverage" } else { operation }, version);
            }
        }
    }
}

#[test]
fn parsed_fragment_wfs_offer_reads_all_offset_pages() {
    let document = documents::documents("#frag").into_iter().find(|(name, _)| *name == "WFS 2.0 GetFeature").unwrap().1;
    let source = capabilities_url("https://maps.example.org/mapserv?map=kept&token=a%2Bb&VERSION=obsolete", Some("WFS"));
    let bound_source = crate::login_address::bind(BASE_BINDING, ORIGIN, &source).unwrap();
    for (source, has_login) in [(source.as_str(), false), (bound_source.as_str(), true)] {
        let service = parse(source, document.as_bytes()).unwrap();
        let Offer::Features { url } = &service.layers[0].offer else { panic!("expected WFS features") };
        let mut calls = 0;
        let (joined, note) = super::join_pages(url, 100, |next| {
            calls += 1;
            assert_authority(next, has_login.then_some(BASE_BINDING), "https://maps.example.org/mapserv", "WFS paging");
            assert_operation(next, "WFS", "GetFeature", "2.0.0");
            let parsed = url::Url::parse(crate::login_address::inner(next)).unwrap();
            let values = |key: &str| {
                parsed.query_pairs().filter(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v.into_owned()).collect::<Vec<_>>()
            };
            match calls {
                1 => {
                    assert_eq!(next, url);
                    assert_eq!(values("COUNT"), [FEATURE_LIMIT.to_string()]);
                    assert!(values("STARTINDEX").is_empty());
                    assert!(values("RESULTTYPE").is_empty());
                    Ok(br#"{"type":"FeatureCollection","features":[{"id":"a.1"},{"id":"a.2"}]}"#.to_vec())
                }
                2 => {
                    assert_eq!(values("RESULTTYPE"), ["hits"]);
                    assert!(values("OUTPUTFORMAT").is_empty());
                    Ok(br#"<wfs:FeatureCollection xmlns:wfs="http://www.opengis.net/wfs/2.0" numberMatched="3" numberReturned="0"/>"#
                        .to_vec())
                }
                3 => {
                    assert_eq!(values("STARTINDEX"), ["2"]);
                    assert_eq!(values("COUNT"), ["2"]);
                    assert!(values("RESULTTYPE").is_empty());
                    Ok(br#"{"type":"FeatureCollection","features":[{"id":"a.3"}]}"#.to_vec())
                }
                _ => panic!("unexpected WFS request: {next}"),
            }
        })
        .unwrap();
        assert_eq!(calls, 3);
        let joined: serde_json::Value = serde_json::from_slice(&joined).unwrap();
        let ids: Vec<_> = joined["features"].as_array().unwrap().iter().map(|f| f["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["a.1", "a.2", "a.3"]);
        assert_eq!(note, None);
    }
}

fn resource_document(template: &str) -> String {
    let document = documents::documents("RESOURCE_TEMPLATE").into_iter().find(|(name, _)| *name == "WMTS ResourceURL").unwrap().1;
    let escaped = template.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;");
    document.replace("RESOURCE_TEMPLATE/{TileMatrix}/{TileRow}/{TileCol}.png", &escaped)
}

#[test]
fn relative_resource_templates_resolve_literal_dot_segments_without_rewriting_bytes() {
    let bound_base = crate::login_address::bind(BASE_BINDING, ORIGIN, BASE).unwrap();
    for (href, expected, same_origin) in [
        (
            "../tiles/{TileMatrix}/{TileRow}/{TileCol}.png?sig=a%2Bb&x=../kept",
            "https://maps.example.org/tiles/{z}/{y}/{x}.png?sig=a%2Bb&x=../kept",
            true,
        ),
        ("../../../../tiles/{TileMatrix}/{TileRow}/{TileCol}.png", "https://maps.example.org/tiles/{z}/{y}/{x}.png", true),
        ("/../tiles/./{TileMatrix}/{TileRow}/{TileCol}.png", "https://maps.example.org/tiles/{z}/{y}/{x}.png", true),
        ("../tiles/{TileMatrix}/{TileRow}/{TileCol}/.", "https://maps.example.org/tiles/{z}/{y}/{x}/", true),
        ("../tiles/{TileMatrix}/{TileRow}/{TileCol}/..", "https://maps.example.org/tiles/{z}/{y}/", true),
        ("..", "https://maps.example.org/", true),
        (".", "https://maps.example.org/folder/", true),
        ("../tiles//./{TileMatrix}//{TileRow}/{TileCol}.png/", "https://maps.example.org/tiles//{z}//{y}/{x}.png/", true),
        ("../tiles//../{TileMatrix}/{TileRow}/{TileCol}.png", "https://maps.example.org/tiles/{z}/{y}/{x}.png", true),
        (
            "../%2e/%2E%2e/{TileMatrix}/{TileRow}/{TileCol}.png?b=2&a=%2f#../fragment",
            "https://maps.example.org/%2e/%2E%2e/{z}/{y}/{x}.png?b=2&a=%2f#../fragment",
            true,
        ),
        (
            "tiles/{TileMatrix}/{TileRow}/{TileCol}.png?x=../kept&sig=a%2Bb",
            "https://maps.example.org/folder/tiles/{z}/{y}/{x}.png?x=../kept&sig=a%2Bb",
            true,
        ),
        (
            "tiles/{TileMatrix}/{TileRow}/{TileCol}.png?src=https://data.example/t&sig=a%2Bb",
            "https://maps.example.org/folder/tiles/{z}/{y}/{x}.png?src=https://data.example/t&sig=a%2Bb",
            true,
        ),
        (
            "/tiles/{TileMatrix}/{TileRow}/{TileCol}.png?src=https://data.example/t",
            "https://maps.example.org/tiles/{z}/{y}/{x}.png?src=https://data.example/t",
            true,
        ),
        (
            "//maps.example.org/tiles/{TileMatrix}/{TileRow}/{TileCol}.png?src=https://data.example/t",
            "https://maps.example.org/tiles/{z}/{y}/{x}.png?src=https://data.example/t",
            true,
        ),
        (
            "//foreign.example.org/tiles/{TileMatrix}/{TileRow}/{TileCol}.png?src=https://data.example/t",
            "https://foreign.example.org/tiles/{z}/{y}/{x}.png?src=https://data.example/t",
            false,
        ),
        (
            "proxy/https://data.example/{TileMatrix}/{TileRow}/{TileCol}.png",
            "https://maps.example.org/folder/proxy/https://data.example/{z}/{y}/{x}.png",
            true,
        ),
        ("//maps.example.org/folder/../tiles/{TileMatrix}/{TileRow}/{TileCol}.png", "https://maps.example.org/tiles/{z}/{y}/{x}.png", true),
        (
            "//foreign.example.org/folder/../tiles/{TileMatrix}/{TileRow}/{TileCol}.png",
            "https://foreign.example.org/tiles/{z}/{y}/{x}.png",
            false,
        ),
        (
            "https://maps.example.org/folder/../tiles/{TileMatrix}/{TileRow}/{TileCol}.png",
            "https://maps.example.org/folder/../tiles/{z}/{y}/{x}.png",
            true,
        ),
    ] {
        let document = resource_document(href);
        for (source, has_login) in [(BASE, false), (bound_base.as_str(), true)] {
            let service = parse(source, document.as_bytes()).unwrap();
            let Offer::Tiles { template } = &service.layers[0].offer else { panic!("expected WMTS tiles") };
            assert_authority(template, (has_login && same_origin).then_some(BASE_BINDING), expected, href);
            assert_eq!(crate::login_address::inner(template), expected, "{href}");
        }
    }
}

#[test]
fn advertised_absolute_dotpaths_keep_bytes_but_never_supply_login_authority() {
    let source = crate::login_address::bind(BASE_BINDING, ORIGIN, BASE).unwrap();
    for (target_origin, target, expected_binding) in [
        (ORIGIN, "https://maps.example.org/folder/../tiles/{TileMatrix}/{TileRow}/{TileCol}.png", Some(BASE_BINDING)),
        ("https://foreign.example.org", "https://foreign.example.org/folder/../tiles/{TileMatrix}/{TileRow}/{TileCol}.png", None),
    ] {
        let href = crate::login_address::bind(FOREIGN_BINDING, target_origin, target).unwrap();
        let service = parse(&source, resource_document(&href).as_bytes()).unwrap();
        let Offer::Tiles { template } = &service.layers[0].offer else { panic!("expected WMTS tiles") };
        let expected = target.replace("{TileMatrix}", "{z}").replace("{TileRow}", "{y}").replace("{TileCol}", "{x}");
        assert_authority(template, expected_binding, &expected, target);
        assert_eq!(crate::login_address::inner(template), expected);
    }
}
