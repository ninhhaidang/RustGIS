use super::*;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::esripbf::tests::encode::{answer, feature, field, geometry, lonlat, message, number, text, transform, uint};

/// A points layer of 400 features on a 20 by 20 grid, 0.05° apart from (-100, 40): object id `i`
/// (from 1) at column `(i - 1) % 20`, row `(i - 1) / 20`.
const FEATURES: u64 = 400;

fn position(id: u64) -> (f64, f64) {
    (-100.0 + ((id - 1) % 20) as f64 * 0.05, 40.0 + ((id - 1) / 20) as f64 * 0.05)
}

/// What a fake service was asked.
#[derive(Default)]
struct Asked {
    count: AtomicUsize,
    urls: Mutex<Vec<String>>,
}

impl Asked {
    fn since(&self, n: usize) -> Vec<String> {
        self.urls.lock().unwrap().get(n..).unwrap_or_default().to_vec()
    }
}

/// How a fake service answers.
#[derive(Clone, Copy)]
struct Opts {
    /// Its pages' length.
    page: usize,
    /// Its queries take standardized SQL (`MOD`).
    sql: bool,
    /// Its description, when not the usual one.
    description: Option<&'static str>,
    /// Its points have Z values (100).
    z: bool,
    /// It answers every page from the start, whatever its offset.
    ignores_offset: bool,
    /// An empty page names no fields.
    empty_without_fields: bool,
    /// Each feature has a text field of 1.5 to 10.5 KB, by its object id.
    padded: bool,
    /// With `padded`, the first feature's text is short (its answer says little of the rest).
    small_first: bool,
    /// Each feature has a text field whose value isn't valid UTF-8.
    bad_text: bool,
    /// It sends at most this many features a page, whatever is asked for.
    caps: Option<usize>,
    /// Its counts are the largest number there is.
    huge_counts: bool,
    /// Its last this many features are counted but never sent.
    hidden: u64,
    /// It sends this many features more a page than asked for.
    over: usize,
    /// An answer of no features is no bytes at all.
    empty_body: bool,
    /// Its layer is in Web Mercator (its description says so), and it answers in the system
    /// asked for (`outSR`), Web Mercator or longitude/latitude.
    mercator: bool,
    /// With `mercator`, its first feature asked for in Web Mercator is an answer too large to
    /// take.
    probe_too_large: bool,
    /// With `mercator`, it answers the first feature asked for in Web Mercator with an empty page
    /// in longitude/latitude.
    probe_empty_lonlat: bool,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            page: 30,
            sql: true,
            description: None,
            z: false,
            ignores_offset: false,
            empty_without_fields: false,
            padded: false,
            small_first: false,
            bad_text: false,
            caps: None,
            huge_counts: false,
            hidden: 0,
            over: 0,
            empty_body: false,
            mercator: false,
            probe_too_large: false,
            probe_empty_lonlat: false,
        }
    }
}

/// Serves the layer at `https://<host>/arcgis/rest/services/P/FeatureServer/0` of [`FEATURES`]
/// points with [`service_with`]'s usual answers, its pages `page` long, its queries taking
/// standardized SQL when `sql`, its description `description` when given; returns its query and
/// what it was asked.
fn service(host: &str, page: usize, sql: bool, description: Option<&'static str>) -> (String, Arc<Asked>) {
    service_with(host, Opts { page, sql, description, ..Opts::default() })
}

/// [`service`], answering as `opts` says.
fn service_with(host: &str, opts: Opts) -> (String, Arc<Asked>) {
    let Opts {
        page,
        sql,
        description,
        z,
        ignores_offset,
        empty_without_fields,
        padded,
        small_first,
        bad_text,
        caps,
        huge_counts,
        hidden,
        over,
        empty_body,
        mercator,
        probe_too_large,
        probe_empty_lonlat,
    } = opts;
    let layer = format!("https://{host}/arcgis/rest/services/P/FeatureServer/0");
    let asked = Arc::new(Asked::default());
    let a = asked.clone();
    remote::testing::handle(&format!("https://{host}/"), move |url| {
        a.count.fetch_add(1, Ordering::Relaxed);
        a.urls.lock().unwrap().push(url.to_string());
        let value = |k: &str| crate::ogc::query_value(url, k).map(crate::address::percent_decode);
        if !url.contains("/query") {
            return Some(match description {
                Some(d) => d.as_bytes().to_vec(),
                None => format!(
                    r#"{{"name":"Points","objectIdField":"OBJECTID","maxRecordCount":{page},"supportedQueryFormats":"JSON, geoJSON, PBF","geometryType":"esriGeometryPoint",
                    "useStandardizedQueries":{sql},"advancedQueryCapabilities":{{"supportsPagination":true}}{}}}"#,
                    if mercator { r#","extent":{"xmin":0,"ymin":0,"xmax":1,"ymax":1,"spatialReference":{"wkid":102100,"latestWkid":3857}}"# } else { "" }
                )
                .into_bytes(),
            });
        }
        let within = value("geometry").map(|g| g.split(',').map(|v| v.parse::<f64>().unwrap()).collect::<Vec<_>>());
        let step = value("where")
            .and_then(|w| w.split("MOD(OBJECTID,").nth(1).map(|r| r.split(')').next().unwrap().parse::<u64>().unwrap()))
            .unwrap_or(1);
        if step > 1 && !sql {
            return Some(br#"{"error":{"code":400,"message":"Unable to complete operation."}}"#.to_vec());
        }
        let none = value("where").is_some_and(|w| w.contains("1=0"));
        let ids: Vec<u64> = (1..=FEATURES)
            .filter(|_| !none)
            .filter(|id| id % step == 0)
            .filter(|id| {
                let (x, y) = position(*id);
                within.as_ref().is_none_or(|g| x >= g[0] && y >= g[1] && x <= g[2] && y <= g[3])
            })
            .collect();
        if value("returnCountOnly").as_deref() == Some("true") {
            let extent = r#","extent":{"xmin":-100,"ymin":40,"xmax":-99.05,"ymax":40.95,"spatialReference":{"wkid":4326}}"#;
            let extent = if value("returnExtentOnly").is_some() { extent } else { "" };
            let n = if huge_counts { u64::MAX } else { ids.len() as u64 };
            return Some(format!(r#"{{"count":{n}{extent}}}"#).into_bytes());
        }
        let offset: usize = if ignores_offset { 0 } else { value("resultOffset").map_or(0, |o| o.parse().unwrap()) };
        let size: usize =
            value("resultRecordCount").map_or(page, |o| o.parse().unwrap()).min(page).min(caps.unwrap_or(usize::MAX)).saturating_add(over);
        let ids: Vec<u64> = ids.into_iter().filter(|id| *id <= FEATURES - hidden).collect();
        let sent: Vec<u64> = ids.iter().skip(offset).take(size).copied().collect();
        if sent.is_empty() && empty_body {
            return Some(Vec::new());
        }
        let geometry_too = value("returnGeometry").as_deref() != Some("false");
        let in_mercator = mercator && value("outSR").as_deref() == Some("3857");
        let probe = in_mercator && value("resultRecordCount").as_deref() == Some("1") && value("resultOffset").is_none();
        if probe && probe_too_large {
            return Some(vec![0; usize::try_from(remote::MAX_ANSWER).unwrap() + 1]);
        }
        if probe && probe_empty_lonlat {
            return Some(answer(&[number(7, 0), lonlat(), transform(false, 0.01, 0.0, 0.0), field("OBJECTID", 6)]));
        }
        let reference = if in_mercator { message(8, &number(1, 3857)) } else { lonlat() };
        let mut parts = vec![number(7, 0), reference, transform(false, 0.01, 0.0, 0.0)];
        if !(sent.is_empty() && empty_without_fields) {
            parts.push(field("OBJECTID", 6));
            if padded {
                parts.push(field("NOTE", 4));
            }
            if bad_text {
                parts.push(field("BAD", 4));
            }
        }
        if z {
            parts.insert(1, number(10, 1));
        }
        if ignores_offset || offset + sent.len() < ids.len() {
            parts.push(number(9, 1));
        }
        for id in sent {
            let (x, y) = position(id);
            let (x, y) = if in_mercator {
                let m = rustgis_geom::lonlat_to_merc(rustgis_geom::Coord::new(x, y));
                (m.x, m.y)
            } else {
                (x, y)
            };
            let mut at = vec![(x * 100.0).round() as i64, (y * 100.0).round() as i64];
            if z {
                at.push(100);
            }
            let mut values = vec![uint(id)];
            if padded {
                let len = if small_first && id == 1 { 10 } else { (id % 7 + 1) as usize * 1500 };
                values.push(text(&"x".repeat(len)));
            }
            if bad_text {
                values.push(message(1, &message(1, &[0xff, 0xfe, b'a'])));
            }
            if geometry_too {
                values.push(geometry(&[], &at));
            }
            parts.push(feature(&values));
        }
        Some(answer(&parts))
    });
    (format!("{layer}/query?where=1%3D1&outFields=*&f=geojson"), asked)
}

fn ids(batch: &Batch) -> Vec<u64> {
    let mut ids: Vec<u64> = batch.features.iter().map(|f| f.id).collect();
    ids.sort_unstable();
    ids
}

fn read(src: &ArcgisSource, req: Request) -> Result<Batch> {
    let src: Arc<dyn FeatureSource> = Arc::new(open_blocking(src.remote.url()).unwrap());
    super::super::read_blocking(&src, &req)
}

/// The view of columns 2..=5 and rows 3..=6: 16 features.
fn small_view() -> Extent {
    Extent::new(-99.91, 40.14, -99.74, 40.31)
}

#[test]
fn a_view_is_read_whole_over_several_pages_with_object_ids_as_ids() {
    let (query, _) = service("live-view.test", 3, true, None);
    let src = open_blocking(&query).unwrap();
    assert_eq!((src.info.format, src.info.count, src.info.crs.as_str()), (FORMAT, Some(FEATURES), "EPSG:4326"));
    assert_eq!(src.info.extent, Extent::new(-100.0, 40.0, -99.05, 40.95));
    let b = read(&src, Request { extent: Some(small_view()), limit: 1000, own: None }).unwrap();
    let want: Vec<u64> = (3..=6).flat_map(|row| (2..=5).map(move |col| row * 20 + col + 1)).collect();
    assert_eq!(ids(&b), want);
    assert!(b.complete && b.matched == 16);
}

#[test]
fn a_view_holding_more_than_the_limit_reads_every_kth_feature_spread_over_it() {
    let (query, _) = service("live-sample.test", 30, true, None);
    let src = open_blocking(&query).unwrap();
    let b = read(&src, Request { extent: Some(Extent::new(-101.0, 39.0, -99.0, 41.0)), limit: 50, own: None }).unwrap();
    assert!(!b.complete && b.matched == FEATURES);
    // 400 over 50 is 8: every 8th object id, across the whole grid.
    assert!(b.features.len() == 50 && b.features.iter().all(|f| f.id % 8 == 0), "{:?}", ids(&b));
    let rows: HashSet<u64> = b.features.iter().map(|f| (f.id - 1) / 20).collect();
    assert_eq!(rows.len(), 20);
    // Every feature, without an extent, is read in object id order.
    let all = read(&src, Request::all()).unwrap();
    assert!(all.complete && all.features.iter().map(|f| f.id).eq(1..=FEATURES));
}

#[test]
fn a_view_read_again_asks_the_service_nothing() {
    let (query, asked) = service("live-again.test", 5, true, None);
    let src: Arc<dyn FeatureSource> = Arc::new(open_blocking(&query).unwrap());
    let req = Request { extent: Some(small_view()), limit: 1000, own: None };
    let first = super::super::read_blocking(&src, &req).unwrap();
    let before = asked.count.load(Ordering::Relaxed);
    let again = super::super::read_blocking(&src, &req).unwrap();
    assert_eq!(asked.since(before), Vec::<String>::new());
    assert_eq!(ids(&first), ids(&again));
}

#[test]
fn a_service_that_cant_sample_gives_its_first_features_and_says_so() {
    let (query, _) = service("live-nosql.test", 30, false, None);
    let src = open_blocking(&query).unwrap();
    assert!(src.info.notes.iter().any(|n| n.contains("standardized SQL")), "{:?}", src.info.notes);
    let b = read(&src, Request { extent: Some(Extent::new(-101.0, 39.0, -99.0, 41.0)), limit: 50, own: None }).unwrap();
    assert!(!b.complete && b.matched == FEATURES && !b.features.is_empty() && b.features.len() <= 50);
    // A view within the limit is read whole, whatever its cells hold.
    let all = read(&src, Request { extent: Some(Extent::new(-101.0, 39.0, -99.0, 41.0)), limit: 1000, own: None }).unwrap();
    assert!(all.complete && all.features.len() == FEATURES as usize, "{}", all.features.len());
}

#[test]
fn every_feature_is_visited_a_page_at_a_time_in_object_id_order() {
    let (query, _) = service("live-visit.test", 30, true, None);
    let src = open_blocking(&query).unwrap();
    let mut seen = Vec::new();
    let mut chunks = 0;
    let b = src
        .visit_chunks(None, 4 << 10, &mut |c| {
            chunks += 1;
            seen.extend(c.iter().map(|f| f.id));
            Ok(())
        })
        .unwrap();
    assert!(b.complete && b.matched == FEATURES && chunks > 1);
    assert!(seen.into_iter().eq(1..=FEATURES));
    // Within an extent, only the features in it.
    let mut inside = Vec::new();
    src.visit_chunks(Some(small_view()), 4 << 10, &mut |c| {
        inside.extend(c.iter().map(|f| f.id));
        Ok(())
    })
    .unwrap();
    assert_eq!(inside.len(), 16);
}

#[test]
fn layers_that_cant_be_read_a_view_at_a_time_are_refused_to_be_downloaded() {
    let unpaged = r#"{"objectIdField":"OBJECTID","maxRecordCount":5,"supportedQueryFormats":"JSON, PBF"}"#;
    let geojson_only = r#"{"objectIdField":"OBJECTID","maxRecordCount":5,"supportedQueryFormats":"JSON, geoJSON",
        "advancedQueryCapabilities":{"supportsPagination":true}}"#;
    let table = r#"{"objectIdField":"OBJECTID","maxRecordCount":5,"supportedQueryFormats":"JSON, PBF",
        "advancedQueryCapabilities":{"supportsPagination":true}}"#;
    for (host, d) in [("live-unpaged.test", unpaged), ("live-geojson.test", geojson_only), ("live-table.test", table)] {
        let (query, _) = service(host, 5, true, Some(d));
        let e = open_blocking(&query).unwrap_err();
        assert!(matches!(&e, DataError::Unsupported(m) if m.contains(NOT_LIVE)), "{e}");
    }
    let (query, _) = service("live-window.test", 5, true, None);
    for extra in ["&resultOffset=10", "&maxAllowableOffset=0.1", "&geometryPrecision=2"] {
        let e = open_blocking(&format!("{query}{extra}")).unwrap_err();
        assert!(e.to_string().contains(NOT_LIVE), "{extra}: {e}");
    }
}

#[test]
fn hostile_answers_are_errors_and_other_services_are_never_asked() {
    for (host, d) in [("live-junk.test", "not json"), ("live-refused.test", r#"{"error":{"code":499,"message":"Token Required"}}"#)] {
        let (query, _) = service(host, 5, true, Some(d));
        assert!(open_blocking(&query).is_err());
    }
    let (query, _) = service("live-origin.test", 5, true, None);
    let file = remote::file(&query);
    assert!(file.answer("https://elsewhere.test/arcgis/rest/services/P/FeatureServer/0/query?f=pbf").is_err());
    assert!(file.answer("http://live-origin.test/arcgis/rest/services/P/FeatureServer/0/query?f=pbf").is_err());
}

#[test]
fn views_anywhere_never_panic_and_ask_for_a_few_cells() {
    let (query, asked) = service("live-views.test", 30, true, None);
    let src: Arc<dyn FeatureSource> = Arc::new(open_blocking(&query).unwrap());
    let views = [
        Extent::new(-180.0, -90.0, 180.0, 90.0),
        Extent::new(-1e300, -1e300, 1e300, 1e300),
        Extent::new(-99.9, 40.1, -99.9, 40.1),
        Extent::new(179.9999999, 89.9999999, 180.0, 90.0),
        Extent::new(200.0, 0.0, 210.0, 10.0),
    ];
    for v in views {
        let before = asked.count.load(Ordering::Relaxed);
        let b = super::super::read_blocking(&src, &Request { extent: Some(v), limit: 100, own: None });
        assert!(b.is_ok(), "{v:?}: {b:?}");
        assert!(asked.count.load(Ordering::Relaxed) - before <= 64, "{v:?}");
    }
    for v in views.iter().filter_map(|v| world_view(v).unwrap()) {
        assert!(cells(&v).len() <= 25, "{v:?}");
    }
    // Outside the world: nothing, said to be all of it.
    let outside =
        super::super::read_blocking(&src, &Request { extent: Some(Extent::new(200.0, 0.0, 210.0, 10.0)), limit: 100, own: None }).unwrap();
    assert!(outside.complete && outside.features.is_empty());
    // No view at all: an error, never an empty answer, and nothing asked.
    let before = asked.count.load(Ordering::Relaxed);
    for v in [Extent::new(f64::NAN, 0.0, 1.0, 1.0), Extent::new(0.0, f64::INFINITY, 1.0, 1.0), Extent::new(5.0, 5.0, -5.0, -5.0)] {
        assert!(super::super::read_blocking(&src, &Request { extent: Some(v), limit: 100, own: None }).is_err(), "{v:?}");
        let mut visit = |_: Vec<Feature>| Ok(());
        assert!(src.visit_chunks(Some(v), 4 << 10, &mut visit).is_err(), "{v:?}");
    }
    assert_eq!(asked.count.load(Ordering::Relaxed), before);
}

#[test]
fn what_pages_lose_is_said_by_each_read_and_none_when_nothing_is() {
    // Z values: said once, by the layer, as other sources say them; not again by its reads.
    let (query, _) = service_with("live-z.test", Opts { z: true, ..Opts::default() });
    let src = open_blocking(&query).unwrap();
    assert!(
        src.info.zm_dropped && src.info.notes == vec![crate::loss::Note::loss(crate::loss::LossCode::DroppedZm, crate::zm_note(None))],
        "{:?}",
        src.info.notes
    );
    assert!(read(&src, Request { extent: Some(small_view()), limit: 1000, own: None }).unwrap().notes.is_empty());
    // Values each page loses: said by each read, a view's counted in its pages.
    let (query, _) = service_with("live-utf8.test", Opts { bad_text: true, ..Opts::default() });
    let src = open_blocking(&query).unwrap();
    assert!(src.info.notes.is_empty(), "{:?}", src.info.notes);
    let b = read(&src, Request { extent: Some(small_view()), limit: 1000, own: None }).unwrap();
    assert!(
        b.notes.len() == 1 && b.notes[0].contains("UTF-8") && b.notes[0].ends_with("counted in the pages read for the view"),
        "{:?}",
        b.notes
    );
    let mut visit = |_: Vec<Feature>| Ok(());
    let all = src.visit_chunks(None, 4 << 10, &mut visit).unwrap();
    assert!(all.notes.len() == 1 && all.notes[0].starts_with(&format!("{FEATURES} values")), "{:?}", all.notes);
    // Clean pages: no notes.
    let (query, _) = service("live-clean.test", 30, true, None);
    let src = open_blocking(&query).unwrap();
    assert!(src.info.notes.is_empty(), "{:?}", src.info.notes);
    assert!(read(&src, Request { extent: Some(small_view()), limit: 1000, own: None }).unwrap().notes.is_empty());
}

#[test]
fn a_service_that_ignores_offsets_stops_a_visit_with_an_error() {
    let (query, _) = service_with("live-offsets.test", Opts { ignores_offset: true, ..Opts::default() });
    let src = open_blocking(&query).unwrap();
    let mut pages = 0;
    let e = src
        .visit_chunks(None, 4 << 10, &mut |_| {
            pages += 1;
            Ok(())
        })
        .unwrap_err();
    assert!(e.to_string().contains("past the 400 features"), "{e}");
    assert!(pages < 100);
}

#[test]
fn sampling_steps_are_powers_of_two() {
    assert_eq!([step_for(0, 10), step_for(10, 10), step_for(11, 10), step_for(400, 50), step_for(401, 50)], [1, 1, 2, 8, 16]);
    assert_eq!(step_for(u64::MAX, 1), MAX_STEP);
}

#[test]
fn a_query_without_geometry_still_reads_its_views() {
    let (query, _) = service("live-nogeometry.test", 30, true, None);
    let src = open_blocking(&format!("{query}&returnGeometry=false")).unwrap();
    let b = read(&src, Request { extent: Some(small_view()), limit: 1000, own: None }).unwrap();
    assert!(b.complete && b.features.len() == 16 && b.features.iter().all(|f| f.geometry.is_some()));
}

#[test]
fn empty_pages_naming_no_fields_are_read_as_empty() {
    let (query, _) = service_with("live-emptypages.test", Opts { empty_without_fields: true, ..Opts::default() });
    let src = open_blocking(&query).unwrap();
    // Every 256th: one feature (256) in all, so most cells' sampled pages are empty.
    let b = read(&src, Request { extent: Some(Extent::new(-101.0, 39.0, -99.0, 41.0)), limit: 2, own: None }).unwrap();
    assert_eq!(ids(&b), vec![256]);
    assert!(!b.complete);
}

#[test]
fn a_budgeted_view_stays_within_its_bytes_as_it_is_read_and_spread_over_it() {
    let (query, _) = service("live-budget.test", 30, true, None);
    let src = open_blocking(&query).unwrap();
    let whole = Request { extent: Some(Extent::new(-101.0, 39.0, -99.0, 41.0)), limit: 1000, own: None };
    let all = read(&src, Request { extent: whole.extent, limit: 1000, own: None }).unwrap();
    let bytes = super::super::memory::features_bytes(&all.features) / 5;
    let shared: Arc<dyn FeatureSource> = Arc::new(open_blocking(&query).unwrap());
    // Fetched first, as the map's reads are, then read within the budget.
    super::super::read_blocking(&shared, &whole).unwrap();
    let b = src.read_budgeted(&whole, bytes).unwrap();
    assert!(!b.complete && !b.features.is_empty());
    assert!(
        super::super::memory::features_bytes(&b.features) <= bytes,
        "{} features, {} bytes of {bytes}; all {} in {}",
        b.features.len(),
        super::super::memory::features_bytes(&b.features),
        all.features.len(),
        super::super::memory::features_bytes(&all.features)
    );
    let rows: HashSet<u64> = b.features.iter().map(|f| (f.id - 1) / 20).collect();
    assert!(rows.len() >= 10, "{rows:?}");
}

#[test]
fn a_layer_matching_nothing_opens_and_reads_empty() {
    let (query, _) = service_with("live-empty.test", Opts { empty_without_fields: true, ..Opts::default() });
    let src = open_blocking(&query.replace("where=1%3D1", "where=1%3D0")).unwrap();
    // Its geometry from the layer's description, as no feature says it.
    assert_eq!((src.info.count, src.info.geometry_type), (Some(0), crate::GeometryType::Point));
    let b = read(&src, Request { extent: Some(small_view()), limit: 1000, own: None }).unwrap();
    assert!(b.complete && b.features.is_empty() && b.matched == 0);
    let mut visited = 0;
    let all = src
        .visit_chunks(None, 4 << 10, &mut |c| {
            visited += c.len();
            Ok(())
        })
        .unwrap();
    assert!(all.complete && visited == 0);
}

#[test]
fn a_view_larger_than_the_answers_kept_is_read_in_part_without_running_out_of_rounds() {
    // 400 features of 1.5 to 10.5 KB: about 2.4 MB of uneven answers, past what a service keeps
    // in tests (2 MB). The service offers pages of 30, up to 315 KB; they are asked for shorter,
    // under the largest answer (64 KB).
    // With and without sampling SQL (a service without it reads each cell's first features).
    for (host, sql) in [("live-large.test", true), ("live-large-nosql.test", false)] {
        let (query, _) = service_with(host, Opts { page: 30, padded: true, sql, ..Opts::default() });
        let src = open_blocking(&query).unwrap();
        assert!(src.page() < 30 && src.page() * 10_600 < remote::MAX_ANSWER as usize, "{}", src.page());
        let whole = Request { extent: Some(Extent::new(-101.0, 39.0, -99.0, 41.0)), limit: 1000, own: None };
        let b = read(&src, whole).unwrap();
        assert!(!b.complete && b.matched == FEATURES && !b.features.is_empty() && b.features.len() < FEATURES as usize, "{host}");
        assert_eq!(b.features.iter().map(|f| f.id).collect::<HashSet<_>>().len(), b.features.len());
        // Read again, it asks for what it read before.
        assert_eq!(ids(&read(&src, whole).unwrap()), ids(&b), "{host}");
        // A view whose answers fit (its cells hold a few of the large features) is read whole.
        let fits = read(&src, Request { extent: Some(small_view()), limit: 1000, own: None }).unwrap();
        assert!(fits.complete && fits.features.len() == 16, "{host}: {}", fits.features.len());
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn an_export_says_what_the_pages_lost_and_nothing_when_they_lost_nothing() {
    let dir = std::env::temp_dir().join(format!("rustgis-arcgis-tests-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (host, lossy) in [("live-export-utf8.test", true), ("live-export-clean.test", false)] {
        let (query, _) = service_with(host, Opts { bad_text: lossy, ..Opts::default() });
        let src = open_blocking(&query).unwrap();
        let layer = crate::Dataset::new(src.info.name.clone(), src.info.geometry_type, src.info.fields.clone());
        let path = dir.join(format!("{host}.geojson"));
        let e = crate::export::write_source_in(&src, &layer, &path, crate::raster::Crs::LonLat).unwrap();
        assert_eq!(e.written, FEATURES as usize);
        assert_eq!(e.read_notes.len(), usize::from(lossy), "{host}: {:?}", e.read_notes);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pages_whose_answers_are_too_large_are_asked_for_again_shorter() {
    // The first feature is small, so the page is planned long (30); the others make an answer of
    // 30 up to 315 KB, past the largest taken in tests (64 KB).
    let (query, _) = service_with("live-shorten.test", Opts { page: 30, padded: true, small_first: true, ..Opts::default() });
    let src = open_blocking(&query).unwrap();
    assert_eq!(src.page(), 30);
    // The whole grid: cells of many features, whose pages of 30 are too large (read by a source
    // of its own, which shortens its own pages).
    let b = read(&src, Request { extent: Some(Extent::new(-101.0, 39.0, -99.0, 41.0)), limit: 1000, own: None }).unwrap();
    assert!(!b.features.is_empty());
    let mut visited = Vec::new();
    src.visit_chunks(None, 64 << 10, &mut |c| {
        visited.extend(c.iter().map(|f| f.id));
        Ok(())
    })
    .unwrap();
    assert!(visited.into_iter().eq(1..=FEATURES));
    assert!(src.page() < 30 && src.page() >= 1, "{}", src.page());
}

#[test]
fn a_materialized_layer_says_each_loss_once() {
    for (host, opts, want) in [
        ("live-once-z.test", Opts { z: true, ..Opts::default() }, "Z or M"),
        ("live-once-utf8.test", Opts { bad_text: true, ..Opts::default() }, "UTF-8"),
    ] {
        let (query, _) = service_with(host, opts);
        let src = open_blocking(&query).unwrap();
        let (ds, report) = crate::source::materialize(&src).unwrap();
        assert_eq!(ds.features.len(), FEATURES as usize);
        let said: Vec<&crate::loss::Note> = report.messages.iter().filter(|m| m.contains(want)).collect();
        assert_eq!(said.len(), 1, "{host}: {:?}", report.messages);
    }
    let (query, _) = service("live-once-clean.test", 30, true, None);
    let (_, report) = crate::source::materialize(&open_blocking(&query).unwrap()).unwrap();
    assert!(report.messages.is_empty(), "{:?}", report.messages);
}

#[test]
fn a_visit_reads_short_pages_in_order() {
    // Pages of 30 asked for a window at a time, 7 sent: each window starts again where a short
    // page ended.
    let (query, asked) = service_with("live-short.test", Opts { caps: Some(7), ..Opts::default() });
    let src = open_blocking(&query).unwrap();
    let before = asked.urls.lock().unwrap().len();
    let mut seen = Vec::new();
    src.visit_chunks(None, 4 << 10, &mut |c| {
        seen.extend(c.iter().map(|f| f.id));
        Ok(())
    })
    .unwrap();
    assert!(seen.into_iter().eq(1..=FEATURES));
    // After the first short page, pages are asked for as long as the service sends them: only the
    // first window's answers past it are thrown away (#2639).
    let pages = asked.since(before).iter().filter(|u| u.contains("resultOffset")).count();
    let needed = (FEATURES as usize).div_ceil(7);
    assert!(pages < needed + remote::ROUND_ANSWERS / 2, "{pages} pages asked for, {needed} needed");
}

#[test]
fn counts_past_any_layer_never_overflow() {
    let (query, _) = service_with("live-huge.test", Opts { huge_counts: true, ..Opts::default() });
    let src = open_blocking(&query).unwrap();
    assert_eq!(src.info.count, Some(u64::MAX));
    // An answer, read or refused, whatever it is: never a panic.
    let _ = read(&src, Request { extent: Some(Extent::new(-101.0, 39.0, -99.0, 41.0)), limit: 1000, own: None });
    let _ = src.visit_chunks(None, 4 << 10, &mut |_| Ok(()));
}

#[test]
fn features_counted_but_not_sent_are_said_and_leave_the_read_incomplete() {
    let (query, _) = service_with("live-hidden.test", Opts { hidden: 10, ..Opts::default() });
    let src = open_blocking(&query).unwrap();
    let mut visited = 0;
    let all = src
        .visit_chunks(None, 4 << 10, &mut |c| {
            visited += c.len();
            Ok(())
        })
        .unwrap();
    assert!(!all.complete && visited == 390, "{visited}");
    assert!(all.notes.iter().any(|n| n.contains("10 features more than it sent")), "{:?}", all.notes);
    // A view read whole, but for what the service didn't send.
    let b = read(&src, Request { extent: Some(Extent::new(-101.0, 39.0, -99.0, 41.0)), limit: 1000, own: None }).unwrap();
    assert!(!b.complete && b.features.len() == 390, "{}", b.features.len());
    assert!(b.notes.iter().any(|n| n.contains("more than it sent")), "{:?}", b.notes);
}

#[test]
fn a_visit_of_pages_longer_than_asked_for_reads_each_feature_once() {
    let (query, _) = service_with("live-over.test", Opts { over: 4, ..Opts::default() });
    let src = open_blocking(&query).unwrap();
    let mut seen = Vec::new();
    let all = src
        .visit_chunks(None, 4 << 10, &mut |c| {
            seen.extend(c.iter().map(|f| f.id));
            Ok(())
        })
        .unwrap();
    assert!(all.complete && seen.into_iter().eq(1..=FEATURES));
}

#[test]
fn a_layer_matching_nothing_may_answer_with_no_bytes() {
    let (query, _) = service_with("live-nobytes.test", Opts { empty_body: true, ..Opts::default() });
    let src = open_blocking(&query.replace("where=1%3D1", "where=1%3D0")).unwrap();
    assert_eq!(src.info.count, Some(0));
    let b = read(&src, Request { extent: Some(small_view()), limit: 1000, own: None }).unwrap();
    assert!(b.complete && b.features.is_empty());
}

/// A layer in a system RustGIS reads is asked for and held in it (`outSR` its code), each feature
/// as the service sends it, and a view still takes the features it holds; one in a system RustGIS
/// can't read is asked for in longitude/latitude, as before.
#[test]
fn a_layer_is_read_in_its_own_system() {
    let (query, asked) = service_with("own-system.test", Opts { page: 30, mercator: true, ..Opts::default() });
    let src = open_blocking(&query).unwrap();
    assert_eq!((src.coord_crs(), src.info.crs.as_str()), (crate::raster::Crs::WebMercator, "EPSG:3857"));
    let before = asked.count.load(Ordering::Relaxed);
    let b = read(&src, Request { extent: Some(small_view()), limit: 1000, own: None }).unwrap();
    let want: Vec<u64> = (3..=6).flat_map(|row| (2..=5).map(move |col| row * 20 + col + 1)).collect();
    assert_eq!(ids(&b), want);
    let pages: Vec<String> = asked.since(before).into_iter().filter(|u| u.contains("resultOffset")).collect();
    assert!(!pages.is_empty() && pages.iter().all(|u| u.contains("outSR=3857")), "{pages:?}");
    for f in &b.features {
        let (x, y) = position(f.id);
        let m = rustgis_geom::lonlat_to_merc(rustgis_geom::Coord::new(x, y));
        let Some(crate::Geometry::Point(c)) = &f.geometry else { panic!() };
        assert!((c.x - m.x).abs() < 0.01 && (c.y - m.y).abs() < 0.01, "{c:?} vs {m:?}");
    }
    // A whole-layer read within the view takes the same features, tested in longitude/latitude.
    let mut inside = Vec::new();
    src.visit_chunks(Some(small_view()), 4 << 10, &mut |c| {
        inside.extend(c.iter().map(|f| f.id));
        Ok(())
    })
    .unwrap();
    inside.sort_unstable();
    assert_eq!(inside, want);
    // A first page in Web Mercator too large to take, or empty and in another system: asked for
    // in longitude/latitude, as before.
    for (host, opts) in [
        ("probe-large.test", Opts { page: 30, mercator: true, probe_too_large: true, ..Opts::default() }),
        ("probe-empty.test", Opts { page: 30, mercator: true, probe_empty_lonlat: true, ..Opts::default() }),
    ] {
        let (query, _) = service_with(host, opts);
        let src = open_blocking(&query).unwrap();
        assert_eq!(src.coord_crs(), crate::raster::Crs::LonLat, "{host}");
        assert_eq!(ids(&read(&src, Request { extent: Some(small_view()), limit: 1000, own: None }).unwrap()), want, "{host}");
    }
    // A code RustGIS doesn't read: longitude/latitude, as before.
    let description = r#"{"name":"Points","objectIdField":"OBJECTID","maxRecordCount":30,"supportedQueryFormats":"PBF","geometryType":"esriGeometryPoint",
        "useStandardizedQueries":true,"advancedQueryCapabilities":{"supportsPagination":true},"extent":{"spatialReference":{"wkid":999999}}}"#;
    let (query, asked) = service("unknown-system.test", 30, true, Some(description));
    let src = open_blocking(&query).unwrap();
    assert_eq!(src.coord_crs(), crate::raster::Crs::LonLat);
    let b = read(&src, Request { extent: Some(small_view()), limit: 1000, own: None }).unwrap();
    assert_eq!(ids(&b), want);
    assert!(asked.since(0).iter().filter(|u| u.contains("resultOffset")).all(|u| u.contains("outSR=4326")));
}
