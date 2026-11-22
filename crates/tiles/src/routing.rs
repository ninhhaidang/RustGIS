//! Routing: directions between places through a Valhalla server (the public one run by FOSSGIS
//! on OpenStreetMap data, or the user's own). Requests are built and answers read here; the
//! fetching is the caller's (blocking on the command line, off the frame in the app).
//!
//! Answers are asked for in OSRM's form, whose steps carry a maneuver (its type and direction),
//! an instruction, the street, and spoken announcements.

use rustgis_geom::{Coord, wgs84_step_m};
use serde_json::{Value, json};

/// The public Valhalla server (FOSSGIS), the default.
pub const VALHALLA: &str = "https://valhalla1.openstreetmap.de";
/// The most places a route passes through (start, stops and destination).
pub const MAX_POINTS: usize = 20;
/// The most steps read from a route.
const MAX_STEPS: usize = 5_000;
/// The most points read from a route's line.
const MAX_LINE: usize = 500_000;

/// How the route is travelled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Drive,
    Bike,
    Walk,
}

impl Mode {
    pub const ALL: [Mode; 3] = [Mode::Drive, Mode::Bike, Mode::Walk];

    /// A mode by name: `drive` (`auto`, `car`), `bike` (`bicycle`) or `walk` (`pedestrian`,
    /// `foot`).
    pub fn parse(s: &str) -> Result<Mode, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "drive" | "driving" | "auto" | "car" => Ok(Mode::Drive),
            "bike" | "bicycle" | "cycling" => Ok(Mode::Bike),
            "walk" | "walking" | "pedestrian" | "foot" => Ok(Mode::Walk),
            other => Err(format!("unknown travel mode '{other}' (drive, bike or walk)")),
        }
    }

    /// Its name, as [`Mode::parse`] reads it.
    pub fn id(self) -> &'static str {
        match self {
            Mode::Drive => "drive",
            Mode::Bike => "bike",
            Mode::Walk => "walk",
        }
    }

    /// Valhalla's costing model.
    fn costing(self) -> &'static str {
        match self {
            Mode::Drive => "auto",
            Mode::Bike => "bicycle",
            Mode::Walk => "pedestrian",
        }
    }
}

/// Roads a drive keeps off where it can.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Avoid {
    pub tolls: bool,
    pub highways: bool,
    pub ferries: bool,
}

/// What to ask for.
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    /// The start, any stops, and the destination, as longitude/latitude.
    pub points: Vec<Coord>,
    pub mode: Mode,
    /// Only for driving.
    pub avoid: Avoid,
    /// Other ways there too (only between two places).
    pub alternates: bool,
    /// Distances in the instructions in miles and feet rather than kilometres and metres.
    pub imperial: bool,
    /// The instructions' language, as a tag (`en-US`, `de`); English when the server doesn't
    /// know it.
    pub language: String,
}

/// A routing server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Router {
    /// Its address, without the endpoint's path (`https://valhalla1.openstreetmap.de`).
    pub base: String,
}

/// A way from the start to the destination.
#[derive(Clone, Debug, PartialEq)]
pub struct Route {
    /// The line followed, as longitude/latitude.
    pub line: Vec<Coord>,
    /// How far along [`Route::line`] each of its points is, in metres.
    pub along: Vec<f64>,
    pub distance_m: f64,
    pub duration_s: f64,
    /// The main roads taken, as the server names them.
    pub summary: String,
    pub steps: Vec<Step>,
    /// How far along the line each stop and the destination are, in metres.
    pub stops_m: Vec<f64>,
}

/// One step: a maneuver, then the stretch to the next.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    /// The maneuver: `depart`, `turn`, `continue`, `merge`, `fork`, `roundabout`, `arrive`…
    pub kind: String,
    /// Its direction: `left`, `slight right`, `uturn`, `straight`… (empty when none).
    pub modifier: String,
    /// What to do, as the server words it.
    pub instruction: String,
    /// The street the step follows.
    pub road: String,
    /// Where the maneuver is.
    pub location: Coord,
    /// How far along the route the maneuver is, in metres.
    pub at_m: f64,
    pub distance_m: f64,
    pub duration_s: f64,
    /// What to say, and how far before the next maneuver (metres) to say it.
    pub voice: Vec<(f64, String)>,
}

impl Router {
    /// The server at `url`, or the public one.
    pub fn new(url: Option<&str>) -> Result<Router, String> {
        let base = match url.map(str::trim).filter(|u| !u.is_empty()) {
            Some(u) => {
                crate::address::check(u, None)?;
                let u = u.trim_end_matches('/');
                u.strip_suffix("/route").unwrap_or(u).to_string()
            }
            None => VALHALLA.to_string(),
        };
        Ok(Router { base })
    }

    /// Whether it is the public server, whose coordinates leave for FOSSGIS's.
    pub fn is_public(&self) -> bool {
        self.base.trim_end_matches('/').eq_ignore_ascii_case(VALHALLA)
    }

    /// The request (a GET, so it fetches like any download, in the browser too).
    pub fn url(&self, r: &Request) -> Result<String, String> {
        if r.points.len() < 2 {
            return Err("a route needs a start and a destination".into());
        }
        if r.points.len() > MAX_POINTS {
            return Err(format!("a route passes through at most {MAX_POINTS} places"));
        }
        if let Some(p) = r.points.iter().find(|p| !valid(p.x, p.y)) {
            return Err(format!("{}, {} isn't a longitude and latitude", p.x, p.y));
        }
        let locations: Vec<Value> = r.points.iter().map(|p| json!({"lon": round6(p.x), "lat": round6(p.y)})).collect();
        let language: String = r.language.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(12).collect();
        let mut body = json!({
            "locations": locations,
            "costing": r.mode.costing(),
            "format": "osrm",
            "voice_instructions": true,
            "language": if language.is_empty() { "en-US".to_string() } else { language },
            "units": if r.imperial { "miles" } else { "kilometers" },
        });
        if r.alternates && r.points.len() == 2 {
            body["alternates"] = json!(2);
        }
        if r.mode == Mode::Drive && (r.avoid.tolls || r.avoid.highways || r.avoid.ferries) {
            let mut o = serde_json::Map::new();
            for (on, key) in [(r.avoid.tolls, "use_tolls"), (r.avoid.highways, "use_highways"), (r.avoid.ferries, "use_ferry")] {
                if on {
                    o.insert(key.into(), json!(0));
                }
            }
            body["costing_options"] = json!({"auto": o});
        }
        Ok(format!("{}/route?json={}", self.base, encode(&body.to_string())))
    }

    /// The routes in an answer, the best first.
    pub fn parse(&self, bytes: &[u8]) -> Result<Vec<Route>, String> {
        let v: Value = serde_json::from_slice(bytes).map_err(|_| "the routing server's answer isn't JSON".to_string())?;
        if let Some(code) = v.get("code").and_then(Value::as_str).filter(|c| *c != "Ok") {
            return Err(v.get("message").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| code.to_string()));
        }
        let routes = v.get("routes").and_then(Value::as_array).ok_or("the routing server's answer has no routes")?;
        let out: Vec<Route> = routes.iter().take(4).filter_map(route).collect();
        if out.is_empty() {
            return Err("no route was found between these places".into());
        }
        Ok(out)
    }
}

/// What a failed request means to the user.
pub fn explain(error: &str) -> String {
    match crate::cache::http_status(error) {
        Some(400) => "no route was found between these places (one may be too far from a road)".into(),
        Some(429) => "the routing server is busy: try again in a moment".into(),
        Some(s) if s >= 500 => "the routing server didn't answer: try again in a moment".into(),
        _ => error.to_string(),
    }
}

fn route(r: &Value) -> Option<Route> {
    let mut line: Vec<Coord> = Vec::new();
    let mut along: Vec<f64> = Vec::new();
    let mut steps = Vec::new();
    let mut stops_m = Vec::new();
    let mut summary: Vec<String> = Vec::new();
    for leg in r.get("legs")?.as_array()? {
        if let Some(s) = leg.get("summary").and_then(Value::as_str).filter(|s| !s.is_empty())
            && !summary.iter().any(|x| x == s)
        {
            summary.push(s.chars().take(200).collect());
        }
        for s in leg.get("steps").and_then(Value::as_array).into_iter().flatten() {
            if steps.len() >= MAX_STEPS {
                break;
            }
            let pts = s
                .get("geometry")
                .and_then(Value::as_str)
                .and_then(|g| rustgis_data::interop::decode_polyline(g, 6).ok())
                .unwrap_or_default();
            let at_m = along.last().copied().unwrap_or(0.0);
            for p in pts {
                if !valid(p.x, p.y) || line.len() >= MAX_LINE {
                    continue;
                }
                match line.last() {
                    Some(last) if *last == p => {}
                    Some(last) => {
                        // On the ellipsoid, a few operations a step: a route can hold half a million.
                        let d = along.last().copied().unwrap_or(0.0) + wgs84_step_m(*last, p);
                        line.push(p);
                        along.push(d);
                    }
                    None => {
                        line.push(p);
                        along.push(0.0);
                    }
                }
            }
            let m = s.get("maneuver");
            let text = |v: Option<&Value>, k: &str| -> String {
                v.and_then(|v| v.get(k)).and_then(Value::as_str).unwrap_or("").chars().take(300).collect()
            };
            // A step without a place of its own (and no line yet to stand in) is left out.
            let Some(location) = m
                .and_then(|m| m.get("location"))
                .and_then(Value::as_array)
                .and_then(|c| Some(Coord::new(c.first()?.as_f64()?, c.get(1)?.as_f64()?)))
                .filter(|c| valid(c.x, c.y))
                .or_else(|| line.last().copied())
            else {
                continue;
            };
            let voice = s
                .get("voiceInstructions")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .take(20)
                .filter_map(|v| {
                    let d = v.get("distanceAlongGeometry")?.as_f64().filter(|d| d.is_finite())?;
                    Some((d.max(0.0), v.get("announcement")?.as_str()?.chars().take(300).collect()))
                })
                .collect();
            let kind = text(m, "type");
            let road = match (text(Some(s), "name"), text(Some(s), "ref")) {
                (n, r) if n.is_empty() => r,
                (n, r) if r.is_empty() || n.contains(&r) => n,
                (n, r) => format!("{n} ({r})"),
            };
            steps.push(Step {
                instruction: text(m, "instruction"),
                modifier: text(m, "modifier"),
                road,
                location,
                at_m,
                distance_m: num(s.get("distance")),
                duration_s: num(s.get("duration")),
                voice,
                kind: if kind.is_empty() { "turn".into() } else { kind },
            });
        }
        stops_m.push(along.last().copied().unwrap_or(0.0));
    }
    if line.len() < 2 {
        return None;
    }
    Some(Route {
        distance_m: Some(num(r.get("distance"))).filter(|d| *d > 0.0).unwrap_or_else(|| along.last().copied().unwrap_or(0.0)),
        duration_s: num(r.get("duration")),
        summary: summary.join(" · "),
        line,
        along,
        steps,
        stops_m,
    })
}

impl Route {
    /// The point `d` metres along the route, and the direction of travel there (degrees clockwise
    /// from north).
    pub fn at(&self, d: f64) -> Option<(Coord, f64)> {
        let (first, last) = (*self.line.first()?, *self.line.last()?);
        let end = self.along.last().copied().unwrap_or(0.0);
        if d <= 0.0 || d.is_nan() || self.line.len() < 2 {
            return Some((first, bearing(first, *self.line.get(1).unwrap_or(&first))));
        }
        if d >= end {
            let before = self.line.get(self.line.len().saturating_sub(2)).copied().unwrap_or(last);
            return Some((last, bearing(before, last)));
        }
        // The segment the distance falls on.
        let i = self.along.partition_point(|&a| a <= d).clamp(1, self.line.len() - 1);
        let (a, b) = (self.line[i - 1], self.line[i]);
        let (da, db) = (self.along[i - 1], self.along[i]);
        let t = if db > da { (d - da) / (db - da) } else { 0.0 };
        Some((Coord::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t), bearing(a, b)))
    }

    /// The step under way `d` metres along the route: the last whose maneuver is behind.
    pub fn step_at(&self, d: f64) -> usize {
        self.steps.partition_point(|s| s.at_m <= d).saturating_sub(1)
    }

    /// The route's extent: west, south, east, north.
    pub fn extent(&self) -> [f64; 4] {
        let mut e = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
        for p in &self.line {
            e = [e[0].min(p.x), e[1].min(p.y), e[2].max(p.x), e[3].max(p.y)];
        }
        e
    }
}

/// The direction from `a` to `b`, in degrees clockwise from north.
fn bearing(a: Coord, b: Coord) -> f64 {
    let (la, lb) = (a.y.to_radians(), b.y.to_radians());
    let dl = (b.x - a.x).to_radians();
    let y = dl.sin() * lb.cos();
    let x = la.cos() * lb.sin() - la.sin() * lb.cos() * dl.cos();
    (y.atan2(x).to_degrees() + 360.0) % 360.0
}

/// A distance as a traveller reads it: whole kilometres from 10 km, tenths from 1 km, then
/// metres rounded to 50 or 10 (miles and feet when `imperial`).
pub fn format_distance(m: f64, imperial: bool) -> String {
    let m = if m.is_finite() { m.max(0.0) } else { 0.0 };
    // Each unit is picked by the value rounded as it would show, so 975 m reads 1.0 km, not 1000 m.
    let round = |x: f64, step: f64| (x / step).round() * step;
    if imperial {
        let mi = m / 1609.344;
        let ft = m / 0.3048;
        let ft_step = if round(ft, 10.0) >= 100.0 { 50.0 } else { 10.0 };
        if round(mi, 0.1) >= 10.0 {
            format!("{mi:.0} mi")
        } else if round(ft, ft_step) >= 528.0 {
            format!("{mi:.1} mi")
        } else {
            format!("{:.0} ft", round(ft, ft_step))
        }
    } else {
        let step = if round(m, 10.0) >= 100.0 { 50.0 } else { 10.0 };
        if round(m, 100.0) >= 10_000.0 {
            format!("{:.0} km", m / 1000.0)
        } else if round(m, step) >= 1000.0 {
            format!("{:.1} km", m / 1000.0)
        } else {
            format!("{:.0} m", round(m, step))
        }
    }
}

/// A duration as minutes, or hours and minutes.
pub fn format_duration(s: f64) -> String {
    let min = if s.is_finite() { (s.max(0.0) / 60.0).round() as u64 } else { 0 };
    match (min / 60, min % 60) {
        (0, m) => format!("{} min", m.max(1)),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// A place typed as `lat, lon` (as a map app shows it), or `lon, lat` when the first number can't
/// be a latitude.
pub fn parse_latlon(s: &str) -> Option<Coord> {
    let parts: Vec<f64> =
        s.split([',', ' ', ';']).filter(|t| !t.is_empty()).map(|t| t.trim().parse::<f64>()).collect::<Result<_, _>>().ok()?;
    let [a, b] = parts.as_slice() else { return None };
    let c = if a.abs() <= 90.0 { Coord::new(*b, *a) } else { Coord::new(*a, *b) };
    valid(c.x, c.y).then_some(c)
}

/// Finds routes, waiting for the answer (the command line; the app routes off the frame).
#[cfg(not(target_arch = "wasm32"))]
pub fn route_blocking(router: &Router, r: &Request) -> Result<Vec<Route>, String> {
    let url = router.url(r)?;
    let bytes = crate::cache::download_blocking(&url).map_err(|e| explain(&e))?;
    router.parse(&bytes)
}

fn num(v: Option<&Value>) -> f64 {
    v.and_then(Value::as_f64).filter(|x| x.is_finite() && *x >= 0.0).unwrap_or(0.0)
}

fn valid(lon: f64, lat: f64) -> bool {
    (-180.0..=180.0).contains(&lon) && (-90.0..=90.0).contains(&lat)
}

fn round6(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
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

#[cfg(test)]
mod tests {
    use super::*;

    fn request(points: Vec<Coord>) -> Request {
        Request { points, mode: Mode::Drive, avoid: Avoid::default(), alternates: true, imperial: false, language: "en-US".into() }
    }

    #[test]
    fn requests() {
        let r = Router::new(None).unwrap();
        assert!(r.is_public());
        let mut q = request(vec![Coord::new(13.3777, 52.5163), Coord::new(13.405, 52.52)]);
        let url = r.url(&q).unwrap();
        assert!(url.starts_with("https://valhalla1.openstreetmap.de/route?json=%7B"), "{url}");
        let decoded = url.replace("%22", "\"").replace("%3A", ":").replace("%2C", ",");
        for part in ["\"costing\":\"auto\"", "\"format\":\"osrm\"", "\"alternates\":2", "\"units\":\"kilometers\"", "\"lon\":13.3777"] {
            assert!(decoded.contains(part), "{part} in {decoded}");
        }
        assert!(!decoded.contains("costing_options"));
        q.avoid = Avoid { tolls: true, highways: false, ferries: true };
        q.imperial = true;
        q.points.insert(1, Coord::new(13.39, 52.51));
        let d = r.url(&q).unwrap().replace("%22", "\"").replace("%3A", ":").replace("%2C", ",");
        assert!(d.contains("\"use_tolls\":0") && d.contains("\"use_ferry\":0") && !d.contains("use_highways"), "{d}");
        assert!(d.contains("\"units\":\"miles\"") && !d.contains("alternates"), "stops: no alternates");
        // Walking ignores the drive's avoidances.
        q.mode = Mode::Walk;
        assert!(r.url(&q).unwrap().contains("pedestrian") && !r.url(&q).unwrap().contains("use_tolls"));
        assert!(r.url(&request(vec![Coord::new(1.0, 1.0)])).is_err());
        assert!(r.url(&request(vec![Coord::new(1.0, 1.0); MAX_POINTS + 1])).is_err());
        assert!(r.url(&request(vec![Coord::new(1.0, 1.0), Coord::new(200.0, 1.0)])).is_err());
        assert!(r.url(&request(vec![Coord::new(f64::NAN, 1.0), Coord::new(1.0, 1.0)])).is_err());
        let own = Router::new(Some("http://localhost:8002/route/")).unwrap();
        assert_eq!(own.base, "http://localhost:8002");
        assert!(!own.is_public());
        assert!(Router::new(Some("file:///etc/passwd")).is_err());
        assert_eq!(Mode::parse("Bicycle"), Ok(Mode::Bike));
        assert!(Mode::parse("teleport").is_err());
    }

    /// A trimmed answer from the public server (Brandenburg Gate to Alexanderplatz).
    const ANSWER: &str = r#"{"code":"Ok","waypoints":[],"routes":[{"duration":302.776,"distance":1921.553,"legs":[{"summary":"Pariser Platz","steps":[
        {"geometry":"g~idcBwjpoXl@InGo@","name":"Pariser Platz","distance":17.553,"duration":6.327,"maneuver":{"instruction":"Drive south on Pariser Platz.","type":"depart","location":[13.378236,52.51634]}},
        {"geometry":"itidcBqlpoXiD}mAYsKe@cS","name":"Pariser Platz","ref":"B 2","distance":1904.0,"duration":296.448,"voiceInstructions":[{"announcement":"Continue for 2 kilometers.","distanceAlongGeometry":1894.0},{"announcement":"bad"}],"maneuver":{"modifier":"left","instruction":"Turn left to stay on Pariser Platz.","type":"continue","location":[13.378265,52.516181]}},
        {"geometry":"ycqdcBurdqX??","name":"","distance":0.0,"duration":0.0,"maneuver":{"instruction":"You have arrived at your destination.","type":"arrive","location":[13.404987,52.520013]}}]}]}]}"#;

    #[test]
    fn answers() {
        let r = Router::new(None).unwrap();
        let routes = r.parse(ANSWER.as_bytes()).unwrap();
        assert_eq!(routes.len(), 1);
        let a = &routes[0];
        assert_eq!((a.distance_m, a.duration_s, a.summary.as_str()), (1921.553, 302.776, "Pariser Platz"));
        assert_eq!(a.steps.len(), 3);
        assert_eq!((a.steps[0].kind.as_str(), a.steps[1].modifier.as_str()), ("depart", "left"));
        assert_eq!(a.steps[1].road, "Pariser Platz (B 2)");
        assert_eq!(a.steps[1].voice, vec![(1894.0, "Continue for 2 kilometers.".to_string())]);
        assert_eq!(a.steps[0].at_m, 0.0);
        assert!(a.steps[1].at_m > 10.0 && a.steps[1].at_m < 30.0, "{}", a.steps[1].at_m);
        // No repeated vertices where steps meet; distances along the line only grow.
        assert!(a.line.windows(2).all(|w| w[0] != w[1]));
        assert!(a.along.windows(2).all(|w| w[1] > w[0]));
        assert_eq!(a.stops_m.len(), 1);
        // Walking along it.
        let (p, _) = a.at(a.steps[1].at_m + 1.0).unwrap();
        assert!((p.y - 52.516).abs() < 0.01);
        assert_eq!(a.step_at(0.0), 0);
        assert_eq!(a.step_at(a.steps[1].at_m + 1.0), 1);
        assert_eq!(a.step_at(1e9), 2);
        assert!(a.at(-5.0).is_some() && a.at(1e12).is_some());
        assert_eq!(a.at(f64::NAN).map(|(c, _)| c), a.line.first().copied());
        // Errors, and hostile answers.
        assert_eq!(
            r.parse(br#"{"code":"NoSegment","message":"One of the supplied input coordinates could not snap to street segment."}"#)
                .unwrap_err(),
            "One of the supplied input coordinates could not snap to street segment."
        );
        assert!(r.parse(b"<html>").is_err() && r.parse(b"{}").is_err());
        assert!(r.parse(br#"{"routes":[{"legs":[{"steps":[{"geometry":"!!!"}]}]}]}"#).is_err());
        assert!(r.parse(br#"{"routes":[{"legs":"x"},7,null]}"#).is_err());
        // A step without a place is left out, not the route.
        let odd = ANSWER
            .replace(
                r#""maneuver":{"instruction":"Drive south on Pariser Platz.","type":"depart","location":[13.378236,52.51634]}"#,
                r#""maneuver":{"type":"depart","location":"x"}"#,
            )
            .replace(r#""geometry":"g~idcBwjpoXl@InGo@""#, r#""geometry":"""#);
        let routes = r.parse(odd.as_bytes()).unwrap();
        assert_eq!(routes[0].steps.len(), 2);
        assert!(explain("http status: 400").contains("no route"));
    }

    /// Distances along a route are on the WGS 84 ellipsoid, as the Measure tool's are, and so is
    /// the route's length when the server doesn't give one.
    #[test]
    fn a_route_without_a_distance_measures_its_line_on_the_ellipsoid() {
        let r = Router::new(None).unwrap();
        let routes = r.parse(ANSWER.replace(r#""duration":302.776,"distance":1921.553,"#, r#""duration":302.776,"#).as_bytes()).unwrap();
        let a = &routes[0];
        let ellipsoid = rustgis_geom::wgs84_length_m(&a.line);
        assert!((a.distance_m / ellipsoid - 1.0).abs() < 1e-6, "{} m, {ellipsoid} m on the ellipsoid", a.distance_m);
        assert_eq!(a.along.last().copied(), Some(a.distance_m));
    }

    #[test]
    fn typed_places() {
        assert_eq!(parse_latlon("52.52, 13.405"), Some(Coord::new(13.405, 52.52)));
        assert_eq!(parse_latlon("-122.4 37.8"), Some(Coord::new(-122.4, 37.8)));
        assert_eq!(parse_latlon("Berlin"), None);
        assert_eq!(parse_latlon("1, 2, 3"), None);
        assert_eq!(parse_latlon("95, 200"), None);
    }

    #[test]
    fn formats() {
        assert_eq!(format_distance(12_345.0, false), "12 km");
        assert_eq!(format_distance(1_234.0, false), "1.2 km");
        assert_eq!(format_distance(237.0, false), "250 m");
        assert_eq!(format_distance(37.0, false), "40 m");
        assert_eq!(format_distance(f64::NAN, false), "0 m");
        // Rounding never shows a unit past its own range.
        assert_eq!(format_distance(975.0, false), "1.0 km");
        assert_eq!(format_distance(9_999.0, false), "10 km");
        assert_eq!(format_distance(96.0, false), "100 m");
        assert_eq!(format_distance(160.0, true), "500 ft");
        assert_eq!(format_distance(165.0, true), "0.1 mi");
        assert_eq!(format_distance(20_000.0, true), "12 mi");
        assert_eq!(format_distance(100.0, true), "350 ft");
        assert_eq!(format_duration(30.0), "1 min");
        assert_eq!(format_duration(302.0), "5 min");
        assert_eq!(format_duration(3600.0), "1 h");
        assert_eq!(format_duration(5_000.0), "1 h 23 min");
    }
}
