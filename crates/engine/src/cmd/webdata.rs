//! Home › Add › Web Data services that are a fixed set of web addresses rather than a service to
//! browse: USGS The National Map's map services (a catalog by category, each added as its
//! ArcGIS tiles or exported images) and Fields of the World's field boundaries (PMTiles archives
//! on Source Cooperative, one a year, drawn by score). The others (NASA GIBS, EnviroAtlas, FEMA's
//! flood hazard layer) are services Add From Service browses.

use serde_json::{Value, json};

use super::{f, ps};
use crate::{CmdResult, CommandSpec as C, Session};

pub fn commands() -> Vec<C> {
    vec![
        C::new("nationalMap.services", "List The National Map Services", "Home › Add › Web Data › United States", |_, _| {
            Ok(national_map_list())
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .view(),
        C::new("nationalMap.add", "Add The National Map Service", "Home › Add › Web Data › United States", national_map_add).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network)).params(
            r#"{"service": "a service's id or title (nationalMap.services lists them), e.g. \"basemap/USGSTopo\"", "name"?: text}"#,
        ),
        C::new("fieldsOfTheWorld.add", "Add Fields of the World", "Home › Add › Web Data", fields_add).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network)).params(
            r#"{"year"?: 2017–2025 (2025), "draw"?: "both"|"fields"|"density" (both: field density zoomed out, fields from zoom 9), "minScore"?: 0–100 (fields scoring below are hidden; 0)}"#,
        ),
    ]
}

// ---------------------------------------------------------------------------------------------
// The National Map

/// How a National Map service is drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    /// Its tile cache (`/tile/{z}/{y}/{x}`), a Web Mercator pyramid.
    Tile,
    /// A MapServer's exported images.
    Export,
    /// An ImageServer's exported images, with a raster function.
    Image(Option<&'static str>),
}

/// A service of The National Map's catalog.
struct NmService {
    id: &'static str,
    category: &'static str,
    title: &'static str,
    /// The service's ArcGIS REST address.
    url: &'static str,
    mode: Mode,
    max_zoom: u8,
    attribution: &'static str,
}

const NM: &str = "USGS The National Map";

const fn nm(id: &'static str, category: &'static str, title: &'static str, url: &'static str, mode: Mode) -> NmService {
    NmService { id, category, title, url, mode, max_zoom: 19, attribution: NM }
}

/// The National Map's map services, by category (GeoLibre's catalog of them, less the Special
/// Edition 250K Maps index, which the server no longer has).
const NATIONAL_MAP: &[NmService] = &[
    NmService {
        max_zoom: 16,
        ..nm(
            "basemap/USGSTopo",
            "Basemaps",
            "USGS Topo",
            "https://basemap.nationalmap.gov/arcgis/rest/services/USGSTopo/MapServer",
            Mode::Tile,
        )
    },
    NmService {
        max_zoom: 16,
        ..nm(
            "basemap/USGSImageryOnly",
            "Basemaps",
            "USGS Imagery Only",
            "https://basemap.nationalmap.gov/arcgis/rest/services/USGSImageryOnly/MapServer",
            Mode::Tile,
        )
    },
    NmService {
        max_zoom: 16,
        ..nm(
            "basemap/USGSImageryTopo",
            "Basemaps",
            "USGS Imagery Topo",
            "https://basemap.nationalmap.gov/arcgis/rest/services/USGSImageryTopo/MapServer",
            Mode::Tile,
        )
    },
    NmService {
        max_zoom: 16,
        ..nm(
            "basemap/USGSShadedReliefOnly",
            "Basemaps",
            "USGS Shaded Relief",
            "https://basemap.nationalmap.gov/arcgis/rest/services/USGSShadedReliefOnly/MapServer",
            Mode::Tile,
        )
    },
    NmService {
        max_zoom: 16,
        ..nm(
            "basemap/USGSHydroCached",
            "Basemaps",
            "USGS Hydro Cached",
            "https://basemap.nationalmap.gov/arcgis/rest/services/USGSHydroCached/MapServer",
            Mode::Tile,
        )
    },
    nm(
        "hydro/3DHP_all",
        "Hydrography",
        "3D Hydrography Program (3DHP)",
        "https://hydro.nationalmap.gov/arcgis/rest/services/3DHP_all/MapServer",
        Mode::Export,
    ),
    nm(
        "hydro/nhd",
        "Hydrography",
        "National Hydrography Dataset (NHD)",
        "https://hydro.nationalmap.gov/arcgis/rest/services/nhd/MapServer",
        Mode::Export,
    ),
    nm(
        "hydro/NHDPlus_HR",
        "Hydrography",
        "NHDPlus High Resolution",
        "https://hydro.nationalmap.gov/arcgis/rest/services/NHDPlus_HR/MapServer",
        Mode::Export,
    ),
    nm(
        "hydro/wbd",
        "Hydrography",
        "Watershed Boundary Dataset (WBD)",
        "https://hydro.nationalmap.gov/arcgis/rest/services/wbd/MapServer",
        Mode::Export,
    ),
    nm(
        "elevation/3DEPElevation",
        "Elevation",
        "3DEP Elevation (Hillshade)",
        "https://elevation.nationalmap.gov/arcgis/rest/services/3DEPElevation/ImageServer",
        Mode::Image(Some(r#"{"rasterFunction":"Hillshade Gray"}"#)),
    ),
    nm(
        "imagery/USGSNAIPPlus",
        "Imagery",
        "NAIP Plus Imagery",
        "https://imagery.nationalmap.gov/arcgis/rest/services/USGSNAIPPlus/ImageServer",
        Mode::Image(None),
    ),
    nm(
        "imagery/USGSNAIPImagery",
        "Imagery",
        "NAIP False Color Imagery",
        "https://imagery.nationalmap.gov/arcgis/rest/services/USGSNAIPImagery/ImageServer",
        Mode::Image(Some(r#"{"rasterFunction":"FalseColorComposite"}"#)),
    ),
    nm(
        "imagery/USGSNAIPImagery-NDVI",
        "Imagery",
        "NAIP NDVI",
        "https://imagery.nationalmap.gov/arcgis/rest/services/USGSNAIPImagery/ImageServer",
        Mode::Image(Some(r#"{"rasterFunction":"NDVI_Color"}"#)),
    ),
    nm(
        "carto/contours",
        "Cartography",
        "Elevation Contours",
        "https://carto.nationalmap.gov/arcgis/rest/services/contours/MapServer",
        Mode::Export,
    ),
    nm(
        "partnerships/USGSTrails",
        "Cartography",
        "USGS Trails",
        "https://partnerships.nationalmap.gov/arcgis/rest/services/USGSTrails/MapServer",
        Mode::Export,
    ),
    nm(
        "carto/geonames",
        "Cartography",
        "Geographic Names (GNIS)",
        "https://carto.nationalmap.gov/arcgis/rest/services/geonames/MapServer",
        Mode::Export,
    ),
    nm(
        "carto/govunits",
        "Cartography",
        "Governmental Unit Boundaries",
        "https://carto.nationalmap.gov/arcgis/rest/services/govunits/MapServer",
        Mode::Export,
    ),
    nm(
        "carto/map_indices",
        "Cartography",
        "Map Indices",
        "https://carto.nationalmap.gov/arcgis/rest/services/map_indices/MapServer",
        Mode::Export,
    ),
    nm(
        "carto/selectable_polygons",
        "Cartography",
        "Selectable Polygons",
        "https://carto.nationalmap.gov/arcgis/rest/services/selectable_polygons/MapServer",
        Mode::Export,
    ),
    nm(
        "carto/structures",
        "Cartography",
        "Structures",
        "https://carto.nationalmap.gov/arcgis/rest/services/structures/MapServer",
        Mode::Export,
    ),
    nm(
        "carto/transportation",
        "Cartography",
        "Transportation",
        "https://carto.nationalmap.gov/arcgis/rest/services/transportation/MapServer",
        Mode::Export,
    ),
    NmService {
        attribution: "FEMA",
        ..nm(
            "fema/NFHL",
            "Hazards",
            "FEMA National Flood Hazard Layer",
            "https://hazards.fema.gov/arcgis/rest/services/public/NFHL/MapServer",
            Mode::Export,
        )
    },
    NmService {
        attribution: "USGS, Esri",
        max_zoom: 15,
        ..nm(
            "esri/USA_Topo_Maps",
            "Other Data",
            "Scanned USA Topo Maps",
            "https://services.arcgisonline.com/arcgis/rest/services/USA_Topo_Maps/MapServer",
            Mode::Tile,
        )
    },
    NmService {
        attribution: "BLM",
        ..nm(
            "blm/PLSS",
            "Other Data",
            "BLM Public Land Survey System (PLSS)",
            "https://gis.blm.gov/arcgis/rest/services/Cadastral/BLM_Natl_PLSS_CadNSDI/MapServer",
            Mode::Export,
        )
    },
    NmService {
        attribution: "USFWS",
        ..nm(
            "fws/Wetlands",
            "Other Data",
            "FWS National Wetlands Inventory",
            "https://fwspublicservices.wim.usgs.gov/wetlandsmapservice/rest/services/Wetlands/MapServer",
            Mode::Export,
        )
    },
    nm(
        "index/3DEPElevationIndex",
        "Indexes",
        "3DEP Elevation Index",
        "https://index.nationalmap.gov/arcgis/rest/services/3DEPElevationIndex/MapServer",
        Mode::Export,
    ),
    nm(
        "index/NHDPlus_HR_Index",
        "Indexes",
        "NHDPlus HR Index",
        "https://index.nationalmap.gov/arcgis/rest/services/NHDPlus_HR_Index/MapServer",
        Mode::Export,
    ),
    nm(
        "index/USGS_Seamless1m_Index",
        "Indexes",
        "Seamless 1m DEM Index",
        "https://index.nationalmap.gov/arcgis/rest/services/USGS_Seamless1m_Index/MapServer",
        Mode::Export,
    ),
    nm(
        "index/USGSNAIPImageryIndex",
        "Indexes",
        "NAIP Imagery Index",
        "https://index.nationalmap.gov/arcgis/rest/services/USGSNAIPImageryIndex/MapServer",
        Mode::Export,
    ),
    nm(
        "index/USTopoAvailability",
        "Indexes",
        "US Topo Availability",
        "https://index.nationalmap.gov/arcgis/rest/services/USTopoAvailability/MapServer",
        Mode::Export,
    ),
    nm(
        "partnerships/3DEPDataAcquisition_1KMGrid",
        "Indexes",
        "3DEP Acquisition Grid (1 km)",
        "https://partnerships.nationalmap.gov/arcgis/rest/services/3DEPDataAcquisition_1KMGrid/MapServer",
        Mode::Export,
    ),
];

/// The categories of [`NATIONAL_MAP`], in their order.
pub const NATIONAL_MAP_CATEGORIES: [&str; 8] =
    ["Basemaps", "Hydrography", "Elevation", "Imagery", "Cartography", "Hazards", "Other Data", "Indexes"];

/// The tile template a service is drawn with.
fn national_map_template(s: &NmService) -> String {
    let url = s.url;
    match s.mode {
        Mode::Tile => format!("{url}/tile/{{z}}/{{y}}/{{x}}"),
        Mode::Export => {
            format!("{url}/export?bbox={{bbox-epsg-3857}}&bboxSR=3857&imageSR=3857&size=256,256&format=png32&transparent=true&f=image")
        }
        Mode::Image(rule) => {
            let rule = rule.map(|r| format!("&renderingRule={}", percent_encode(r)));
            format!(
                "{url}/exportImage?bbox={{bbox-epsg-3857}}&bboxSR=3857&imageSR=3857&size=256,256&format=png{}&f=image",
                rule.unwrap_or_default()
            )
        }
    }
}

/// A query value with everything but letters, digits and `-_.~` percent-encoded.
fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// The catalog as the app and scripts see it.
fn national_map_list() -> Value {
    let services: Vec<Value> = NATIONAL_MAP
        .iter()
        .map(|s| json!({"id": s.id, "title": s.title, "category": s.category, "url": s.url, "tiles": national_map_template(s)}))
        .collect();
    json!({"count": services.len(), "categories": NATIONAL_MAP_CATEGORIES, "services": services})
}

fn national_map_add(s: &mut Session, p: &Value) -> CmdResult {
    let want = ps(p, "service").map(str::trim).filter(|w| !w.is_empty()).ok_or("service is required (nationalMap.services lists them)")?;
    let svc = NATIONAL_MAP
        .iter()
        .find(|s| s.id == want)
        .or_else(|| NATIONAL_MAP.iter().find(|s| s.id.eq_ignore_ascii_case(want) || s.title.eq_ignore_ascii_case(want)))
        .ok_or_else(|| format!("no National Map service '{want}' (nationalMap.services lists them)"))?;
    let name = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).unwrap_or(svc.title);
    // Basemaps go with the basemaps; the rest are transparent overlays on top.
    let basemap = svc.category == "Basemaps";
    // A drawn map (labels, symbols) is asked for as one picture of the view, so they aren't cut
    // at tile edges (#1162); imagery in tiles.
    let single = matches!(svc.mode, Mode::Export);
    super::tiles::add(
        s,
        &json!({"url": national_map_template(svc), "kind": "raster", "name": name, "attribution": svc.attribution,
            "maxZoom": svc.max_zoom, "overlay": !basemap, "singleImage": single}),
    )
}

// ---------------------------------------------------------------------------------------------
// Fields of the World

/// The years Fields of the World (2nd Edition) has field boundaries for.
pub const FIELDS_YEARS: std::ops::RangeInclusive<i64> = 2017..=2025;

/// A year's archive: field polygons (`fields`, zoom 9–13) and field-density cells (`cells`,
/// zoom 0–8).
pub fn fields_url(year: i64) -> String {
    format!("https://data.source.coop/ftw/global-data-2e/vector/{year}/fields-{year}.pmtiles")
}

/// The zoom the field polygons start at (the density cells end there).
const FIELDS_ZOOM: u8 = 9;

/// The style layers that draw the archive as Fields of the World does: cells by the share of
/// them covered by fields, fields by the model's score, those below `min_score` hidden.
fn fields_style(density: bool, fields: bool, min_score: f64) -> Value {
    let score = json!(["to-number", ["get", "score"], 0]);
    let score_color = json!(["step", score, "#d73027", 45, "#fdae61", 55, "#ffffbf", 65, "#a6d96a", 80, "#1a9850"]);
    let mut layers = Vec::new();
    if density {
        layers.push(json!({"id": "ftw-cells", "type": "fill", "source-layer": "cells", "maxzoom": FIELDS_ZOOM,
            "paint": {"fill-color": ["step", ["to-number", ["get", "pct_covered"], 0], "#ffffd9", 2, "#edf8b1", 10, "#7fcdbb", 25, "#41b6c4", 50, "#225ea8", 75, "#081d58"],
                "fill-opacity": 0.8}}));
    }
    if fields {
        let filter = if min_score > 0.0 { Some(json!([">=", score, min_score])) } else { None };
        let mut fill = json!({"id": "ftw-fields", "type": "fill", "source-layer": "fields", "minzoom": FIELDS_ZOOM,
            "paint": {"fill-color": score_color, "fill-opacity": 0.2}});
        let mut line = json!({"id": "ftw-fields-line", "type": "line", "source-layer": "fields", "minzoom": FIELDS_ZOOM,
            "paint": {"line-color": score_color, "line-width": 1.25}});
        if let Some(f) = filter {
            fill["filter"] = f.clone();
            line["filter"] = f;
        }
        layers.push(fill);
        layers.push(line);
    }
    Value::Array(layers)
}

fn fields_add(s: &mut Session, p: &Value) -> CmdResult {
    let year = match p.get("year") {
        None | Some(Value::Null) => *FIELDS_YEARS.end(),
        Some(v) => v
            .as_i64()
            .or_else(|| v.as_str().and_then(|t| t.trim().parse().ok()))
            .filter(|y| FIELDS_YEARS.contains(y))
            .ok_or_else(|| format!("year is {} to {}", FIELDS_YEARS.start(), FIELDS_YEARS.end()))?,
    };
    let (density, fields) = match ps(p, "draw").unwrap_or("both") {
        "both" => (true, true),
        "fields" => (false, true),
        "density" => (true, false),
        other => return Err(format!("unknown draw '{other}' (both, fields or density)")),
    };
    let min_score = f(p, "minScore").unwrap_or(0.0).clamp(0.0, 100.0).round();
    let name = match (density, fields) {
        (true, true) => format!("Fields of the World {year}"),
        (false, _) => format!("Fields of the World {year} fields"),
        (true, false) => format!("Fields of the World {year} density"),
    };
    let url = fields_url(year);
    let both = format!("Fields of the World {year}");
    let mut r = super::tiles::add(
        s,
        &json!({"url": url, "kind": "pmtiles", "name": name, "overlay": true, "maxZoom": 16,
            "attribution": "Fields of the World (CC BY 4.0)", "styleLayers": fields_style(density, fields, min_score)}),
    )?;
    // The same look again was replaced by `tiles::add` (same name). Once the new layer is in, the
    // year's both and its parts, which draw over one another, don't stay together; another
    // overlay of the archive, named by the user, stays.
    let parts = [format!("{both} fields"), format!("{both} density")];
    s.map_mut()
        .tiles
        .retain(|t| !(t.url == url && t.overlay && t.name != name && (t.name == both || (density && fields && parts.contains(&t.name)))));
    // The view's Web Mercator zoom (known on the Web Mercator map and the globe, whose camera
    // counts Web Mercator metres; another projection's units differ).
    let mercator = matches!(s.map().projection, rustgis_map::globe::Projection::WebMercator | rustgis_map::globe::Projection::Globe);
    let zoom = (156_543.033_928 / s.map().camera.res.max(1e-9)).log2();
    if fields && mercator && zoom < f64::from(FIELDS_ZOOM) - 0.5 {
        r["note"] = json!(format!("fields are drawn from zoom {FIELDS_ZOOM}: zoom in to see them"));
    }
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn national_map_catalog() {
        assert_eq!(NATIONAL_MAP.len(), 31);
        for s in NATIONAL_MAP {
            assert!(NATIONAL_MAP_CATEGORIES.contains(&s.category), "{}", s.id);
            assert!(s.url.starts_with("https://") && s.url.ends_with("Server"), "{}", s.id);
            let t = national_map_template(s);
            assert!(t.contains("{z}") || t.contains("{bbox-epsg-3857}"), "{t}");
        }
        let ids: std::collections::HashSet<_> = NATIONAL_MAP.iter().map(|s| s.id).collect();
        assert_eq!(ids.len(), NATIONAL_MAP.len(), "ids are unique");
        let hill = NATIONAL_MAP.iter().find(|s| s.id == "elevation/3DEPElevation").map(national_map_template).unwrap_or_default();
        assert!(
            hill.contains("/exportImage?") && hill.contains("&renderingRule=%7B%22rasterFunction%22%3A%22Hillshade%20Gray%22%7D"),
            "{hill}"
        );
    }

    #[test]
    fn style_layer_limits_agree() {
        assert_eq!(rustgis_map::tiles::MAX_STYLE_LAYER_COUNT, crate::render::rustgis_tiles::source::MAX_OWN_STYLE_LAYERS);
    }

    #[test]
    fn fields_style_by_score() {
        let both = fields_style(true, true, 50.0);
        assert_eq!(both.as_array().map(Vec::len), Some(3));
        assert_eq!(both[1]["filter"], json!([">=", ["to-number", ["get", "score"], 0], 50.0]));
        assert!(fields_style(false, true, 0.0)[0].get("filter").is_none());
        assert_eq!(fields_style(true, false, 0.0)[0]["source-layer"], "cells");
    }
}
