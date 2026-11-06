//! Geocoding: places and addresses found by name through a Nominatim or Photon server (OSM's
//! public ones, or the user's own), or PDOK's Locatieserver (the Netherlands' official address and
//! place registers). Requests are built and answers read here; the fetching is the caller's
//! (blocking on the command line, off the frame in the app).

use serde_json::Value;

/// The kind of server: its request and its answer's form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Nominatim's `/search` (`format=jsonv2`).
    Nominatim,
    /// Photon's `/api` (GeoJSON).
    Photon,
    /// PDOK's Locatieserver: its `/free` search (Solr's JSON, each place's point as WKT).
    Locatieserver,
}

/// A geocoding server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Geocoder {
    pub kind: Kind,
    /// Its address, without the endpoint's path (`https://nominatim.openstreetmap.org`).
    pub base: String,
}

/// OSM's public Nominatim, the default.
pub const NOMINATIM: &str = "https://nominatim.openstreetmap.org";
/// Komoot's public Photon.
pub const PHOTON: &str = "https://photon.komoot.io";
/// PDOK's Locatieserver (version 3.1 of its search API).
pub const LOCATIESERVER: &str = "https://api.pdok.nl/bzk/locatieserver/search/v3_1";

/// The kinds of place the Locatieserver is asked for: its default set (municipalities, towns,
/// streets, postcodes, addresses) and provinces. Plots, hectometre posts, districts and water
/// boards are left out, as its own default does.
const LOCATIESERVER_TYPES: &str = "type:(provincie OR gemeente OR woonplaats OR weg OR postcode OR adres)";
/// How the Locatieserver ranks them: its own boosts, except that an address outranks its street
/// (its default puts the street first for "Damrak 1, Amsterdam") and a province ranks below the
/// municipality and the town of its name ("Utrecht").
const LOCATIESERVER_BOOSTS: [&str; 6] =
    ["type:provincie^1", "type:gemeente^1.5", "type:woonplaats^1.5", "type:weg^1", "type:postcode^0.5", "type:adres^1.5"];
/// The longest query the Locatieserver takes (its `q` is at most 400 characters).
const LOCATIESERVER_MAX_QUERY: usize = 400;

/// How many addresses a run sends to a public server: their usage policies allow light use only
/// (a server of one's own takes [`MAX_BATCH`]).
pub const PUBLIC_BATCH: usize = 100;
/// How many addresses a run geocodes at most.
pub const MAX_BATCH: usize = 10_000;
/// How many places a search returns at most.
pub const MAX_RESULTS: usize = 20;
/// The longest query sent.
const MAX_QUERY: usize = 500;

/// The places in an answer, and how many of the answer's entries were left out because they had no
/// usable position (no coordinate, an unreadable or out-of-range one).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Found {
    pub places: Vec<Place>,
    pub skipped: usize,
}

/// A place found.
#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    /// What the server calls it (the full address for Nominatim).
    pub name: String,
    pub lon: f64,
    pub lat: f64,
    /// Its extent, west, south, east, north, when the server gives one.
    pub bbox: Option<[f64; 4]>,
}

impl Geocoder {
    /// A server by `provider` (`nominatim`, the default, `photon` or `locatieserver`) and an
    /// optional address of one's own.
    pub fn new(provider: Option<&str>, url: Option<&str>) -> Result<Geocoder, String> {
        let kind = match provider.map(|p| p.trim().to_ascii_lowercase()).as_deref() {
            None | Some("") | Some("nominatim") => Kind::Nominatim,
            Some("photon") => Kind::Photon,
            Some("locatieserver") => Kind::Locatieserver,
            Some(other) => return Err(format!("unknown geocoder '{other}' (nominatim, photon or locatieserver)")),
        };
        let base = match url.map(str::trim).filter(|u| !u.is_empty()) {
            Some(u) => {
                crate::address::check(u, None)?;
                // The endpoint's path is the request's.
                let u = u.trim_end_matches('/');
                let path = match kind {
                    Kind::Nominatim => "/search",
                    Kind::Photon => "/api",
                    Kind::Locatieserver => "/free",
                };
                u.strip_suffix(path).unwrap_or(u).to_string()
            }
            None => match kind {
                Kind::Nominatim => NOMINATIM.to_string(),
                Kind::Photon => PHOTON.to_string(),
                Kind::Locatieserver => LOCATIESERVER.to_string(),
            },
        };
        Ok(Geocoder { kind, base })
    }

    /// The server's host, lowercased: the host itself, not a path or a look-alike host that
    /// contains a name.
    fn host(&self) -> String {
        let b = self.base.to_ascii_lowercase();
        let host = b.split_once("://").map(|(_, r)| r).unwrap_or(&b).split(['/', '?', '#']).next().unwrap_or("");
        let host = host.rsplit_once('@').map(|(_, h)| h).unwrap_or(host);
        host.split(':').next().unwrap_or(host).to_string()
    }

    /// Whether it is a public server shared by everyone: OSM's Nominatim, komoot's Photon or
    /// PDOK's Locatieserver.
    pub fn is_public(&self) -> bool {
        self.asks_light_use() || self.host() == "api.pdok.nl"
    }

    /// Whether its usage policy asks for at most a request a second and light use (OSM's
    /// Nominatim and komoot's Photon). PDOK publishes no such limit for its Locatieserver, a
    /// geocoding service for applications: it is asked a few times a second, with no cap on a run
    /// below [`MAX_BATCH`].
    pub fn asks_light_use(&self) -> bool {
        matches!(self.host().as_str(), "nominatim.openstreetmap.org" | "photon.komoot.io")
    }

    /// How long to wait between requests, in milliseconds.
    pub fn spacing_ms(&self) -> u64 {
        if self.asks_light_use() {
            1100
        } else if self.is_public() {
            250
        } else {
            0
        }
    }

    /// How many addresses a run may send.
    pub fn batch_limit(&self) -> usize {
        if self.asks_light_use() { PUBLIC_BATCH } else { MAX_BATCH }
    }

    /// The longest query the server is sent, in characters.
    fn max_query(&self) -> usize {
        if self.kind == Kind::Locatieserver { LOCATIESERVER_MAX_QUERY } else { MAX_QUERY }
    }

    /// Whether `query` can be sent: not empty, and not longer than the server takes (a query cut
    /// short could find another place).
    pub fn check_query(&self, query: &str) -> Result<(), String> {
        let n = query.trim().chars().count();
        if n == 0 {
            return Err("give a place or an address to find".into());
        }
        if n > self.max_query() {
            return Err(format!("a query of {n} characters: the geocoder takes up to {}", self.max_query()));
        }
        Ok(())
    }

    /// The request for `query`, returning up to `limit` places ([`Geocoder::check_query`] says
    /// whether it can be sent; a longer one is cut to what the server takes).
    pub fn url(&self, query: &str, limit: usize) -> String {
        let q: String = query.trim().chars().take(self.max_query()).collect();
        let limit = limit.clamp(1, MAX_RESULTS);
        match self.kind {
            Kind::Nominatim => format!("{}/search?format=jsonv2&limit={limit}&q={}", self.base, encode(&q)),
            Kind::Photon => format!("{}/api/?limit={limit}&q={}", self.base, encode(&q)),
            Kind::Locatieserver => {
                let boosts: String = LOCATIESERVER_BOOSTS.iter().map(|b| format!("&bq={}", encode(b))).collect();
                format!(
                    "{}/free?q={}&rows={limit}&fl={}&fq={}{boosts}",
                    self.base,
                    encode(&q),
                    encode("weergavenaam,centroide_ll"),
                    encode(LOCATIESERVER_TYPES)
                )
            }
        }
    }

    /// The places in an answer.
    pub fn parse(&self, bytes: &[u8]) -> Result<Vec<Place>, String> {
        self.read(bytes).map(|f| f.places)
    }

    /// The places in an answer, and how many of its entries were left out (no usable position).
    pub fn read(&self, bytes: &[u8]) -> Result<Found, String> {
        let v: Value = serde_json::from_slice(bytes).map_err(|_| "the geocoder's answer isn't JSON".to_string())?;
        let (entries, place): (&Vec<Value>, fn(&Value) -> Option<Place>) = match self.kind {
            Kind::Nominatim => (v.as_array().ok_or("the geocoder's answer isn't a list of places")?, nominatim_place),
            Kind::Photon => (v.get("features").and_then(Value::as_array).ok_or("the geocoder's answer isn't GeoJSON")?, photon_place),
            Kind::Locatieserver => (
                v.get("response")
                    .and_then(|r| r.get("docs"))
                    .and_then(Value::as_array)
                    .ok_or("the geocoder's answer isn't the Locatieserver's (no response.docs)")?,
                locatieserver_place,
            ),
        };
        let mut found = Found::default();
        // The entries past the places asked for aren't read: a server sends what it was asked.
        for e in entries {
            if found.places.len() >= MAX_RESULTS {
                break;
            }
            match place(e) {
                Some(p) => found.places.push(p),
                None => found.skipped += 1,
            }
        }
        // Places given but none usable is a broken answer, not "nothing found".
        if found.places.is_empty() && found.skipped > 0 {
            return Err(format!("the geocoder's answer has {} places but none with a usable position", found.skipped));
        }
        Ok(found)
    }
}

/// A number given as a number or as text (Nominatim's).
fn num(v: Option<&Value>) -> Option<f64> {
    let v = v?;
    v.as_f64().or_else(|| v.as_str()?.trim().parse().ok()).filter(|x: &f64| x.is_finite())
}

fn valid(lon: f64, lat: f64) -> bool {
    (-180.0..=180.0).contains(&lon) && (-90.0..=90.0).contains(&lat)
}

fn nominatim_place(p: &Value) -> Option<Place> {
    let (lon, lat) = (num(p.get("lon"))?, num(p.get("lat"))?);
    if !valid(lon, lat) {
        return None;
    }
    // boundingbox: south, north, west, east.
    let bbox = p.get("boundingbox").and_then(Value::as_array).and_then(|b| {
        let n: Vec<f64> = b.iter().take(4).filter_map(|x| num(Some(x))).collect();
        match n.as_slice() {
            [s, nn, w, e] if valid(*w, *s) && valid(*e, *nn) => Some([*w, *s, *e, *nn]),
            _ => None,
        }
    });
    let name = p.get("display_name").and_then(Value::as_str).unwrap_or("").chars().take(500).collect();
    Some(Place { name, lon, lat, bbox })
}

fn photon_place(f: &Value) -> Option<Place> {
    let c = f.get("geometry")?.get("coordinates")?.as_array()?;
    let (lon, lat) = (num(c.first())?, num(c.get(1))?);
    if !valid(lon, lat) {
        return None;
    }
    let props = f.get("properties");
    let text = |k: &str| props.and_then(|p| p.get(k)).and_then(Value::as_str).filter(|s| !s.is_empty());
    // A name from its parts, most specific first.
    let street = match (text("street"), text("housenumber")) {
        (Some(s), Some(n)) => Some(format!("{s} {n}")),
        (s, _) => s.map(str::to_string),
    };
    let parts: Vec<String> = [text("name").map(str::to_string), street]
        .into_iter()
        .chain(["city", "state", "country"].iter().map(|k| text(k).map(str::to_string)))
        .flatten()
        .collect();
    let mut name: Vec<String> = Vec::new();
    for p in parts {
        if !name.contains(&p) {
            name.push(p);
        }
    }
    // extent: west, north, east, south.
    let bbox = props.and_then(|p| p.get("extent")).and_then(Value::as_array).and_then(|b| {
        let n: Vec<f64> = b.iter().take(4).filter_map(|x| num(Some(x))).collect();
        match n.as_slice() {
            [w, nn, e, s] if valid(*w, *s) && valid(*e, *nn) => Some([*w, s.min(*nn), *e, nn.max(*s)]),
            _ => None,
        }
    });
    Some(Place { name: name.join(", ").chars().take(500).collect(), lon, lat, bbox })
}

fn locatieserver_place(d: &Value) -> Option<Place> {
    let (lon, lat) = wkt_point(d.get("centroide_ll")?.as_str()?)?;
    if !valid(lon, lat) {
        return None;
    }
    let name = d.get("weergavenaam").and_then(Value::as_str).unwrap_or("").chars().take(500).collect();
    Some(Place { name, lon, lat, bbox: None })
}

/// A WKT point's longitude and latitude (`POINT(4.89 52.37)`, WKT's order: x, then y).
fn wkt_point(s: &str) -> Option<(f64, f64)> {
    let s = s.trim();
    let rest = s.get(..5).filter(|p| p.eq_ignore_ascii_case("point")).and(s.get(5..))?;
    let inner = rest.trim().strip_prefix('(')?.strip_suffix(')')?;
    let mut n = inner.split_whitespace();
    let (x, y) = (n.next()?.parse::<f64>().ok()?, n.next()?.parse::<f64>().ok()?);
    (n.next().is_none() && x.is_finite() && y.is_finite()).then_some((x, y))
}

/// Percent-encodes a query parameter.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Finds places, waiting for the answer (the command line; the app searches off the frame).
#[cfg(not(target_arch = "wasm32"))]
pub fn search_blocking(g: &Geocoder, query: &str, limit: usize) -> Result<Found, String> {
    g.check_query(query)?;
    g.read(&crate::cache::download_blocking(&g.url(query, limit))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn servers_and_requests() {
        let g = Geocoder::new(None, None).unwrap();
        assert_eq!(g.kind, Kind::Nominatim);
        assert!(g.is_public() && g.spacing_ms() >= 1000 && g.batch_limit() == PUBLIC_BATCH);
        // The host decides, not a look-alike or a path.
        for own in ["https://nominatim.openstreetmap.org.evil.example", "https://geo.example/nominatim.openstreetmap.org"] {
            assert!(!Geocoder::new(None, Some(own)).unwrap().is_public(), "{own}");
        }
        assert!(Geocoder::new(Some("photon"), Some("https://photon.komoot.io:443/api")).unwrap().is_public());
        assert_eq!(
            g.url(" 10 Downing St, London ", 3),
            "https://nominatim.openstreetmap.org/search?format=jsonv2&limit=3&q=10%20Downing%20St%2C%20London"
        );
        let own = Geocoder::new(Some("Photon"), Some("http://localhost:2322/api/")).unwrap();
        assert_eq!((own.kind, own.base.as_str()), (Kind::Photon, "http://localhost:2322"));
        assert!(!own.is_public() && own.spacing_ms() == 0 && own.batch_limit() == MAX_BATCH);
        assert_eq!(own.url("Zürich", 99), "http://localhost:2322/api/?limit=20&q=Z%C3%BCrich");
        assert!(Geocoder::new(Some("google"), None).is_err());
        assert!(Geocoder::new(None, Some("file:///etc/passwd")).is_err());
        assert!(Geocoder::new(None, Some("http://169.254.169.254/")).is_err());
    }

    #[test]
    fn answers() {
        let g = Geocoder::new(None, None).unwrap();
        let a = br#"[{"lat":"51.5033","lon":"-0.1276","display_name":"10 Downing Street, London","boundingbox":["51.50","51.51","-0.13","-0.12"]},{"lat":"x","lon":"1"},{"lat":"95","lon":"1"}]"#;
        let p = g.parse(a).unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(
            p[0],
            Place { name: "10 Downing Street, London".into(), lon: -0.1276, lat: 51.5033, bbox: Some([-0.13, 51.50, -0.12, 51.51]) }
        );
        assert!(g.parse(b"{}").is_err() && g.parse(b"<html>").is_err());
        let ph = Geocoder::new(Some("photon"), None).unwrap();
        let b = br#"{"type":"FeatureCollection","features":[{"geometry":{"type":"Point","coordinates":[8.54,47.37]},"properties":{"name":"Zurich","city":"Zurich","country":"Switzerland","extent":[8.44,47.43,8.62,47.32]}},{"geometry":null}]}"#;
        let p = ph.parse(b).unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].name, "Zurich, Switzerland");
        assert_eq!(p[0].bbox, Some([8.44, 47.32, 8.62, 47.43]));
        assert!(ph.parse(b"[]").is_err());
    }

    /// An answer as PDOK's Locatieserver gives it (trimmed to the fields asked for).
    const LOCATIESERVER_ANSWER: &[u8] = br#"{"response":{"numFound":943552,"start":0,"maxScore":13.47,"numFoundExact":true,"docs":[
        {"weergavenaam":"Damrak 1, 1012LG Amsterdam","centroide_ll":"POINT(4.89803846 52.37714446)"},
        {"weergavenaam":"Damrak, Amsterdam","centroide_ll":"POINT(4.89580599 52.37513402)"},
        {"weergavenaam":"Damrak 18-1, Amsterdam","centroide_ll":" point ( 4.89727884   52.37658789 ) "}]}}"#;

    #[test]
    fn locatieserver_requests() {
        let g = Geocoder::new(Some(" LocatieServer "), None).unwrap();
        assert_eq!((g.kind, g.base.as_str()), (Kind::Locatieserver, LOCATIESERVER));
        // PDOK's server: public, but without the OSM servers' request a second and 100 a run.
        assert!(g.is_public() && !g.asks_light_use());
        assert_eq!((g.spacing_ms(), g.batch_limit()), (250, MAX_BATCH));
        assert_eq!(
            g.url(" Damrak 1, Amsterdam ", 5),
            "https://api.pdok.nl/bzk/locatieserver/search/v3_1/free?q=Damrak%201%2C%20Amsterdam&rows=5\
             &fl=weergavenaam%2Ccentroide_ll\
             &fq=type%3A%28provincie%20OR%20gemeente%20OR%20woonplaats%20OR%20weg%20OR%20postcode%20OR%20adres%29\
             &bq=type%3Aprovincie%5E1&bq=type%3Agemeente%5E1.5&bq=type%3Awoonplaats%5E1.5&bq=type%3Aweg%5E1\
             &bq=type%3Apostcode%5E0.5&bq=type%3Aadres%5E1.5"
        );
        // Rows within 1–20 (the server refuses more than 100); a query within its 400 characters.
        assert!(g.url("x", 99).contains("&rows=20&") && g.url("x", 0).contains("&rows=1&"));
        let long = g.url(&"a".repeat(1000), 1);
        let q = long.split("q=").nth(1).and_then(|r| r.split('&').next()).unwrap();
        assert_eq!(q.len(), 400);
        // A longer query isn't sent cut short: it is refused before it is sent.
        assert!(g.check_query(&"ä".repeat(400)).is_ok());
        assert!(g.check_query(&"a".repeat(401)).unwrap_err().contains("401 characters"));
        assert!(g.check_query("  ").is_err());
        let osm = Geocoder::new(None, None).unwrap();
        assert!(osm.check_query(&"a".repeat(500)).is_ok() && osm.check_query(&"a".repeat(501)).is_err());
        // An address of one's own (a proxy or PDOK's acceptance server), with or without `/free`.
        let own = Geocoder::new(Some("locatieserver"), Some("https://geo.example/locatieserver/v3_1/free/")).unwrap();
        assert_eq!(own.base, "https://geo.example/locatieserver/v3_1");
        assert!(own.url("Utrecht", 1).starts_with("https://geo.example/locatieserver/v3_1/free?q=Utrecht&rows=1&"));
        assert!(!own.is_public() && own.spacing_ms() == 0);
        // The host decides, not a look-alike.
        assert!(!Geocoder::new(Some("locatieserver"), Some("https://api.pdok.nl.evil.example/free")).unwrap().is_public());
        assert!(Geocoder::new(Some("locatieserver"), Some("https://API.pdok.nl:443/bzk/locatieserver/search/v3_1")).unwrap().is_public());
        assert!(Geocoder::new(Some("pdok"), None).unwrap_err().contains("locatieserver"));
    }

    #[test]
    fn locatieserver_answers() {
        let g = Geocoder::new(Some("locatieserver"), None).unwrap();
        // Several places, in the server's order, longitude first as WKT has it.
        let f = g.read(LOCATIESERVER_ANSWER).unwrap();
        assert_eq!(f.skipped, 0);
        assert_eq!(f.places.len(), 3);
        assert_eq!(f.places[0], Place { name: "Damrak 1, 1012LG Amsterdam".into(), lon: 4.89803846, lat: 52.37714446, bbox: None });
        assert_eq!(f.places[1].name, "Damrak, Amsterdam");
        assert_eq!((f.places[2].lon, f.places[2].lat), (4.89727884, 52.37658789));
        // One place.
        let one = br#"{"response":{"docs":[{"weergavenaam":"Gemeente Utrecht","centroide_ll":"POINT(5.0747543 52.09113798)","type":"gemeente","score":9.78}]}}"#;
        assert_eq!(
            g.read(one).unwrap(),
            Found { places: vec![Place { name: "Gemeente Utrecht".into(), lon: 5.0747543, lat: 52.09113798, bbox: None }], skipped: 0 }
        );
        // Nothing found: no places, nothing left out.
        let none = br#"{"response":{"numFound":0,"start":0,"maxScore":0.0,"numFoundExact":true,"docs":[ ]}}"#;
        assert_eq!(g.read(none).unwrap(), Found::default());
        // Entries without a usable point are counted, not dropped quietly; a place without a name
        // keeps its point.
        let bad = br#"{"response":{"docs":[
            {"weergavenaam":"no point"},
            {"weergavenaam":"not text","centroide_ll":7},
            {"weergavenaam":"empty","centroide_ll":""},
            {"weergavenaam":"a line","centroide_ll":"LINESTRING(4 52, 5 53)"},
            {"weergavenaam":"one number","centroide_ll":"POINT(4.9)"},
            {"weergavenaam":"three numbers","centroide_ll":"POINT(4.9 52.3 1)"},
            {"weergavenaam":"words","centroide_ll":"POINT(east north)"},
            {"weergavenaam":"unclosed","centroide_ll":"POINT(4.9 52.3"},
            {"weergavenaam":"not a number","centroide_ll":"POINT(NaN 52.3)"},
            {"weergavenaam":"infinite","centroide_ll":"POINT(inf 52.3)"},
            {"weergavenaam":"Dutch grid metres","centroide_ll":"POINT(121537.612 487586.916)"},
            {"weergavenaam":"swapped beyond the pole","centroide_ll":"POINT(4.9 152.3)"},
            {"weergavenaam":"POINT","centroide_ll":"POINTPOINT"},
            {"weergavenaam":"\u00fc","centroide_ll":"POIN\u00fcT(4.9 52.3)"},
            "a string",
            null,
            {"centroide_ll":"POINT(4.9 52.3)","weergavenaam":42}
        ]}}"#;
        let f = g.read(bad).unwrap();
        assert_eq!(f.skipped, 16, "{f:?}");
        assert_eq!(f.places, vec![Place { name: String::new(), lon: 4.9, lat: 52.3, bbox: None }]);
        // Places given but none usable: an error, not "nothing found".
        let unusable = br#"{"response":{"docs":[{"weergavenaam":"a","centroide_ll":"POINT(4.9)"},{"weergavenaam":"b"}]}}"#;
        assert!(g.read(unusable).unwrap_err().contains("2 places but none"));
        // Answers that aren't the Locatieserver's are errors, not "nothing found".
        for not in [&b"<html>"[..], b"[]", b"{}", br#"{"response":{}}"#, br#"{"response":{"docs":{}}}"#, br#"{"response":[1]}"#, b"null"] {
            assert!(g.read(not).is_err(), "{}", String::from_utf8_lossy(not));
        }
        // A huge answer: twenty places read, the rest not looked at.
        let many =
            format!(r#"{{"response":{{"docs":[{}]}}}}"#, vec![r#"{"weergavenaam":"x","centroide_ll":"POINT(5 52)"}"#; 5000].join(","));
        let f = g.read(many.as_bytes()).unwrap();
        assert_eq!((f.places.len(), f.skipped), (MAX_RESULTS, 0));
        // `parse` gives the places alone.
        assert_eq!(g.parse(LOCATIESERVER_ANSWER).unwrap().len(), 3);
    }

    /// The other servers' entries without a usable position are counted too.
    #[test]
    fn skipped_entries_are_counted() {
        let g = Geocoder::new(None, None).unwrap();
        let a = br#"[{"lat":"51.5","lon":"-0.12","display_name":"London"},{"lat":"x","lon":"1"},{"lat":"95","lon":"1"}]"#;
        assert_eq!(g.read(a).unwrap().skipped, 2);
        let ph = Geocoder::new(Some("photon"), None).unwrap();
        let b = br#"{"features":[{"geometry":{"coordinates":[8.54,47.37]},"properties":{"name":"Zurich"}},{"geometry":null}]}"#;
        assert_eq!(ph.read(b).unwrap().skipped, 1);
        let clean = br#"[{"lat":"51.5","lon":"-0.12","display_name":"London"}]"#;
        assert_eq!(g.read(clean).unwrap().skipped, 0);
    }
}
