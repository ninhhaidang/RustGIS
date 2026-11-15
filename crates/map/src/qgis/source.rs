//! QGIS data sources: a layer's provider and `<datasource>` text, read into where RustGIS finds
//! the data. Paths are made absolute from the project's folder; web addresses are decoded.

/// Where a QGIS layer's data is.
#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    /// A file (`ogr`, `gdal`, `spatialite`): its path, the layer or table in it, and the filter
    /// QGIS's subset string sets (as QGIS has it), and the source's options RustGIS doesn't read
    /// (`layerid=2`, `geometrytype=Point`), for the import to note.
    File { path: String, layer: Option<String>, subset: Option<String>, ignored: Vec<String> },
    /// A delimited text file (`delimitedtext`): its path, and the fields its points (or WKT
    /// geometry) are in.
    Csv { path: String, x: Option<String>, y: Option<String>, wkt: Option<String>, crs: Option<String> },
    /// XYZ tiles (`wms` with `type=xyz`), raster or vector (`vectortile`).
    Xyz { url: String, zmin: Option<u8>, zmax: Option<u8>, vector: bool },
    /// A WMS or WMTS layer.
    Wms { url: String, layer: String, crs: Option<String> },
    /// A WFS or OGC API Features layer.
    Wfs { url: String, layer: String },
    /// A PostGIS table: QGIS's connection text (its password left out) and the table.
    Postgres { connection: String, table: String },
    /// One RustGIS doesn't read.
    Other { provider: String },
}

/// The source of a layer with QGIS's `provider` and `datasource`, relative paths taken from `dir`.
pub fn read(provider: &str, datasource: &str, dir: &str) -> Source {
    match provider {
        "ogr" | "gdal" => file(datasource, dir),
        "spatialite" => {
            // `dbname='…' key='id' srid=4326 type=Point table="t" (geom) sql=…`: the filter, if
            // any, runs to the end.
            let (head, sql) = datasource.rsplit_once(" sql=").unwrap_or((datasource, ""));
            let subset = Some(sql.trim().to_string()).filter(|q| !q.is_empty());
            let kv = conninfo(head);
            // What QGIS always writes and RustGIS reads or needn't.
            const READ_OR_HARMLESS: [&str; 7] = ["dbname", "table", "key", "srid", "type", "estimatedmetadata", "checkPrimaryKeyUnicity"];
            let ignored = kv
                .iter()
                .filter(|(k, _)| !READ_OR_HARMLESS.iter().any(|r| r.eq_ignore_ascii_case(k)))
                .take(32)
                .map(|(k, v)| noted_option(&format!("{k}={v}")))
                .collect();
            match (kv.iter().find(|(k, _)| k == "dbname"), table_of(head)) {
                (Some((_, db)), Some(t)) => Source::File { path: absolute(db, dir), layer: Some(t), subset, ignored },
                _ => Source::Other { provider: provider.into() },
            }
        }
        "delimitedtext" => csv(datasource, dir),
        "wms" | "vectortile" => {
            let q = query(datasource);
            let get = |k: &str| q.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).filter(|v| !v.is_empty());
            let zoom = |k: &str| get(k).and_then(|v| v.parse::<u8>().ok());
            match (get("type").as_deref(), get("url")) {
                (Some("xyz"), Some(url)) => Source::Xyz { url, zmin: zoom("zmin"), zmax: zoom("zmax"), vector: provider == "vectortile" },
                (_, Some(url)) if provider == "wms" => match get("layers") {
                    Some(layer) => Source::Wms { url, layer, crs: get("crs") },
                    None => Source::Other { provider: provider.into() },
                },
                _ => Source::Other { provider: provider.into() },
            }
        }
        "WFS" | "wfs" | "OAPIF" | "oapif" => {
            let kv = conninfo(datasource);
            let get = |k: &str| kv.iter().find(|(n, _)| n.eq_ignore_ascii_case(k)).map(|(_, v)| v.clone());
            match (get("url"), get("typename")) {
                (Some(url), Some(layer)) => Source::Wfs { url, layer },
                _ => Source::Other { provider: provider.into() },
            }
        }
        "postgres" => match table_of(datasource) {
            Some(table) => {
                // The password stays out of what the import keeps or reports.
                let kept: Vec<String> = conninfo(datasource)
                    .into_iter()
                    .filter(|(k, _)| matches!(k.as_str(), "dbname" | "host" | "port" | "user" | "service" | "sslmode"))
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect();
                Source::Postgres { connection: kept.join(" "), table }
            }
            None => Source::Other { provider: provider.into() },
        },
        _ => Source::Other { provider: provider.into() },
    }
}

/// An `ogr` or `gdal` source: `path|layername=…|subset=…` (or `|layerid=…`), or GDAL's
/// `GPKG:path:table`.
fn file(datasource: &str, dir: &str) -> Source {
    if let Some(rest) = datasource.strip_prefix("GPKG:") {
        // GPKG:path:table, the path itself possibly with a drive letter's colon.
        if let Some((path, table)) = rest.rsplit_once(':') {
            return Source::File { path: absolute(path, dir), layer: Some(table.to_string()), subset: None, ignored: Vec::new() };
        }
    }
    let mut parts = datasource.split('|');
    let path = parts.next().unwrap_or_default().trim();
    let (mut layer, mut subset, mut ignored) = (None, None, Vec::new());
    // Every part is read for the layer and the filter; at most 32 others are kept for the note.
    for p in parts {
        if let Some(v) = p.strip_prefix("layername=") {
            layer = Some(v.to_string());
        } else if let Some(v) = p.strip_prefix("subset=") {
            subset = Some(v.to_string()).filter(|s| !s.trim().is_empty());
        } else if !p.trim().is_empty() && p.trim() != "layerid=0" && ignored.len() < 32 {
            // (layerid=0 is the first layer, which is what opens.)
            ignored.push(noted_option(p.trim()));
        }
    }
    // A layer named as well as numbered: the name is what's read.
    if layer.is_some() {
        ignored.retain(|o: &String| !o.starts_with("layerid="));
    }
    Source::File { path: absolute(path, dir), layer, subset, ignored }
}

/// A source option as the import notes it: with its value only for the options known to carry
/// nothing private (`layerid=2`, `geometrytype=Point`), else by its name alone, since a value can
/// be a key, a token or a password under any name.
fn noted_option(option: &str) -> String {
    const SHOWN: [&str; 3] = ["layerid", "geometrytype", "uniquegeometrytype"];
    match option.split_once('=') {
        Some((key, _)) if !SHOWN.iter().any(|k| k.eq_ignore_ascii_case(key.trim())) => {
            format!("{}=…", key.trim().chars().take(100).collect::<String>())
        }
        _ => option.chars().take(100).collect(),
    }
}

/// A `delimitedtext` source: `file:///path.csv?type=csv&xField=…&yField=…&crs=…`.
fn csv(datasource: &str, dir: &str) -> Source {
    let (path, q) = datasource.split_once('?').unwrap_or((datasource, ""));
    // file:///absolute/path, or a relative one as file:./path.
    let path = path.strip_prefix("file://").or_else(|| path.strip_prefix("file:")).unwrap_or(path);
    let q = query(q);
    let get = |k: &str| q.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).filter(|v| !v.is_empty());
    Source::Csv {
        path: absolute(&rustgis_data::address::percent_decode(path), dir),
        x: get("xField"),
        y: get("yField"),
        wkt: get("wktField"),
        crs: get("crs"),
    }
}

/// `key=value` pairs joined by `&`, each value percent-decoded (QGIS's web sources).
fn query(s: &str) -> Vec<(String, String)> {
    s.split('&')
        .take(200)
        .filter_map(|kv| kv.split_once('=').or(Some((kv, ""))))
        .map(|(k, v)| (k.to_string(), rustgis_data::address::percent_decode(v)))
        .collect()
}

/// `key=value` and `key='value'` pairs joined by spaces (QGIS's connection texts).
fn conninfo(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = s.trim();
    while !rest.is_empty() && out.len() < 100 {
        let Some((k, after)) = rest.split_once('=') else { break };
        let k = k.trim().to_string();
        let (v, next) = if let Some(quoted) = after.strip_prefix('\'') {
            match quoted.find('\'') {
                Some(end) => (quoted.get(..end).unwrap_or_default(), quoted.get(end + 1..).unwrap_or_default()),
                None => (quoted, ""),
            }
        } else {
            let end = after.find(' ').unwrap_or(after.len());
            (after.get(..end).unwrap_or_default(), after.get(end..).unwrap_or_default())
        };
        out.push((k, v.to_string()));
        rest = next.trim_start();
    }
    out
}

/// A connection text's `table="schema"."table" (geom)`, as `schema.table` (or `table`).
fn table_of(s: &str) -> Option<String> {
    let at = s.find("table=")?;
    let rest = s.get(at + 6..)?;
    let end = rest.find(" (").or_else(|| rest.find(' ')).unwrap_or(rest.len());
    let t = rest.get(..end)?.replace('"', "");
    (!t.is_empty()).then_some(t)
}

/// `path` from the project's folder `dir`, unless it is absolute or a web address.
pub(super) fn absolute(path: &str, dir: &str) -> String {
    let path = path.trim();
    if path.is_empty() || !is_relative(path) || dir.is_empty() {
        return path.to_string();
    }
    let rel = path.strip_prefix("./").unwrap_or(path);
    format!("{}/{}", dir.trim_end_matches(['/', '\\']), rel)
}

/// Whether `path` is relative to a Windows drive's working directory (`C:roads.gpkg`, not
/// `C:\…` or `C:/…`): no project folder tells where that is.
pub fn is_drive_relative(path: &str) -> bool {
    let b = path.trim().as_bytes();
    b.get(1) == Some(&b':') && b.first().is_some_and(u8::is_ascii_alphabetic) && !matches!(b.get(2), Some(b'/' | b'\\'))
}

/// Whether `path` is relative to the project's folder: not absolute (`/…`, `\…`, `C:\…`), not
/// on a drive (`C:…`, see [`is_drive_relative`]) and not a web address.
pub fn is_relative(path: &str) -> bool {
    let path = path.trim();
    let windows_drive = path.as_bytes().get(1) == Some(&b':') && path.as_bytes().first().is_some_and(u8::is_ascii_alphabetic);
    !(path.starts_with('/') || path.starts_with('\\') || windows_drive || path.contains("://"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_read_as_their_providers_write_them() {
        let dir = "/data/project";
        assert_eq!(
            read("ogr", "./roads.gpkg|layername=roads|subset=\"type\" = 'motorway'", dir),
            Source::File {
                path: "/data/project/roads.gpkg".into(),
                layer: Some("roads".into()),
                subset: Some("\"type\" = 'motorway'".into()),
                ignored: Vec::new()
            }
        );
        assert_eq!(
            read("ogr", "/abs/rivers.shp", dir),
            Source::File { path: "/abs/rivers.shp".into(), layer: None, subset: None, ignored: Vec::new() }
        );
        assert_eq!(
            read("gdal", "C:/maps/dem.tif", dir),
            Source::File { path: "C:/maps/dem.tif".into(), layer: None, subset: None, ignored: Vec::new() }
        );
        // Options RustGIS doesn't read are kept for a note; the first layer's number isn't one.
        let Source::File { ignored, .. } = read("ogr", "./multi.gpkg|layerid=2|geometrytype=Point", dir) else { panic!() };
        assert_eq!(ignored, ["layerid=2", "geometrytype=Point"]);
        let Source::File { ignored, .. } = read("ogr", "./one.gpkg|layerid=0", dir) else { panic!() };
        assert!(ignored.is_empty());
        let Source::File { ignored, .. } = read("ogr", "./multi.gpkg|layername=roads|layerid=3", dir) else { panic!() };
        assert!(ignored.is_empty(), "the name is read: {ignored:?}");
        // An option that may carry a secret is noted by its name only.
        let Source::File { ignored, .. } = read("ogr", "/vsis3/b/x.gpkg|credential:AWS_SECRET_ACCESS_KEY=hunter2|token=abc", dir) else {
            panic!()
        };
        assert_eq!(ignored, ["credential:AWS_SECRET_ACCESS_KEY=…", "token=…"]);
        let Source::File { ignored, .. } = read("ogr", "./x.gpkg|header=Bearer abc|layerid=4", dir) else { panic!() };
        assert_eq!(ignored, ["header=…", "layerid=4"], "values only for options known to be harmless");
        // SpatiaLite: the table, its filter, and the options that matter noted.
        let Source::File { path, layer, subset, ignored } =
            read("spatialite", r#"dbname='./db.sqlite' key='id' srid=4326 type=Point table="towns" (geom) sql="pop" > 1000"#, dir)
        else {
            panic!()
        };
        assert_eq!(
            (path.as_str(), layer.as_deref(), subset.as_deref()),
            ("/data/project/db.sqlite", Some("towns"), Some(r#""pop" > 1000"#))
        );
        assert!(ignored.is_empty(), "{ignored:?}");
        let Source::File { subset, ignored, .. } = read("spatialite", r#"dbname='/a.sqlite' password='s3cret' table="t" (geom) sql="#, dir)
        else {
            panic!()
        };
        assert_eq!((subset, ignored), (None, vec!["password=…".to_string()]));
        // The layer and the filter are read past the options kept for the note.
        let many = format!("./m.gpkg|{}|layername=late|subset=a = 1", vec!["x=1"; 40].join("|"));
        let Source::File { layer, subset, ignored, .. } = read("ogr", &many, dir) else { panic!() };
        assert_eq!((layer.as_deref(), subset.as_deref(), ignored.len()), (Some("late"), Some("a = 1"), 32));
        assert_eq!(
            read("gdal", "GPKG:./m.gpkg:relief", dir),
            Source::File { path: "/data/project/m.gpkg".into(), layer: Some("relief".into()), subset: None, ignored: Vec::new() }
        );
        assert_eq!(
            read("delimitedtext", "file:///data/pts%20a.csv?type=csv&xField=lon&yField=lat&crs=EPSG:4326", dir),
            Source::Csv {
                path: "/data/pts a.csv".into(),
                x: Some("lon".into()),
                y: Some("lat".into()),
                wkt: None,
                crs: Some("EPSG:4326".into())
            }
        );
        assert_eq!(
            read("wms", "type=xyz&url=https://tile.openstreetmap.org/%7Bz%7D/%7Bx%7D/%7By%7D.png&zmax=19&zmin=0", dir),
            Source::Xyz { url: "https://tile.openstreetmap.org/{z}/{x}/{y}.png".into(), zmin: Some(0), zmax: Some(19), vector: false }
        );
        assert_eq!(
            read("wms", "crs=EPSG:2056&format=image/png&layers=ch.swisstopo.pixelkarte-farbe&styles&url=https://wms.geo.admin.ch/", dir),
            Source::Wms {
                url: "https://wms.geo.admin.ch/".into(),
                layer: "ch.swisstopo.pixelkarte-farbe".into(),
                crs: Some("EPSG:2056".into())
            }
        );
        // A password stays behind.
        let pg = read("postgres", "dbname='gis' host=db port=5432 user='me' password='secret' table=\"public\".\"roads\" (geom)", dir);
        assert_eq!(pg, Source::Postgres { connection: "dbname=gis host=db port=5432 user=me".into(), table: "public.roads".into() });
        assert_eq!(read("memory", "Point?crs=EPSG:4326", dir), Source::Other { provider: "memory".into() });
        assert_eq!(
            read("delimitedtext", "file:./pts.csv?wktField=geom", dir),
            Source::Csv { path: "/data/project/pts.csv".into(), x: None, y: None, wkt: Some("geom".into()), crs: None }
        );
    }
}
