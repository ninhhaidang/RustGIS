//! Open-data portals and web catalogs searched by text or place: CKAN catalogues (data.gov's own
//! catalogue, and any CKAN site such as open.canada.ca), Socrata's discovery API (every Socrata
//! site, or one), ArcGIS Hub, ArcGIS portals (ArcGIS Online, NASA's Earthdata GIS, an
//! organisation's public items), OpenAerialMap, the USGS 3DEP elevation models, IGN's LiDAR HD
//! tiles, GeoLens servers, Hugging Face datasets and the Ocean Data Platform. A search names
//! datasets and the geospatial resources each offers: files RustGIS reads by address (GeoJSON,
//! KML, zipped Shapefiles, GeoParquet, GeoTIFF, LiDAR…) and services it adds (WMS, WFS, ArcGIS
//! REST); a Hugging Face dataset lists its files in a second request ([`Dataset::listing`]).
//! Requests are built and answers read here; the fetching is the caller's.

use serde_json::Value;

/// The portals searched when none is named.
pub const DATA_GOV: &str = "https://catalog.data.gov";
pub const SOCRATA: &str = "https://api.us.socrata.com";
pub const HUB: &str = "https://hub.arcgis.com";
pub const ARCGIS_ONLINE: &str = "https://www.arcgis.com";
pub const EARTHDATA_GIS: &str = "https://gis.earthdata.nasa.gov/portal";
pub const OPENAERIALMAP: &str = "https://api.openaerialmap.org";
pub const TNM: &str = "https://tnmaccess.nationalmap.gov/api/v1";
pub const IGN_WFS: &str = "https://data.geopf.fr/wfs";
pub const GEOLENS: &str = "https://datasets.geolibre.app";
pub const HUGGING_FACE: &str = "https://huggingface.co";
pub const OCEAN_DATA: &str = "https://api.hubocean.earth";
pub const DYNAMICAL: &str = "https://stac.dynamical.org";
/// Where dynamical.org keeps its datasets' Zarr stores.
const DYNAMICAL_DATA: &str = "https://data.dynamical.org";

/// The USGS 3DEP elevation models a search can ask for: a label and The National Map's name.
pub const DEM_DATASETS: [(&str, &str); 5] = [
    ("1 meter", "Digital Elevation Model (DEM) 1 meter"),
    ("1/9 arc-second (about 3 m)", "National Elevation Dataset (NED) 1/9 arc-second"),
    ("1/3 arc-second (about 10 m)", "National Elevation Dataset (NED) 1/3 arc-second"),
    ("1 arc-second (about 30 m)", "National Elevation Dataset (NED) 1 arc-second"),
    ("Alaska 5 meter", "Alaska 5 meter DEM"),
];

/// The largest box IGN's LiDAR HD tiles are searched in, in square degrees (its tiles are 1 km).
pub const IGN_MAX_AREA: f64 = 2.0;

/// How many datasets a search asks for at most.
pub const MAX_RESULTS: usize = 50;
/// How many resources a dataset lists at most.
const MAX_RESOURCES: usize = 20;
/// The longest query sent.
const MAX_QUERY: usize = 200;

/// The kind of portal: its search request and its answer's form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A CKAN site's `package_search`.
    Ckan,
    /// data.gov's catalogue (its own search, DCAT records).
    DataGov,
    /// Socrata's discovery API (every site, or one site's datasets).
    Socrata,
    /// ArcGIS Hub's search (OGC API Records).
    Hub,
    /// An ArcGIS portal's item search (`sharing/rest/search`), its public items or an
    /// organisation's.
    ArcGis,
    /// OpenAerialMap's image catalogue (drone and aerial imagery as COGs).
    OpenAerialMap,
    /// The National Map's products: USGS 3DEP elevation models as GeoTIFFs.
    UsgsDem,
    /// IGN's LiDAR HD tiles (France), COPC point clouds, found through its WFS.
    IgnLidar,
    /// A GeoLens server's datasets, read as OGC API Features.
    GeoLens,
    /// Hugging Face datasets, whose files are listed in a second request.
    HuggingFace,
    /// HUB Ocean's Ocean Data Platform (its STAC catalogue, read as OGC API Features).
    OceanData,
    /// dynamical.org's weather and climate datasets (its STAC catalogue): Zarr stores.
    Dynamical,
}

/// A portal: its kind, its address, and (Socrata) the site searched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Portal {
    pub kind: Kind,
    /// The address searched (no path for Socrata's discovery and Hub; a CKAN site's root).
    pub base: String,
    /// A Socrata site the search keeps to (`data.cityofnewyork.us`).
    pub domain: Option<String>,
    /// What else keeps the search to some datasets: an ArcGIS organisation's id, the USGS
    /// elevation model's name ([`DEM_DATASETS`]).
    pub filter: Option<String>,
    /// The kind of ArcGIS items searched ([`ARCGIS_KINDS`]), all when none.
    pub category: Option<&'static str>,
}

/// What a resource is to RustGIS: a file read by address, or a service of a type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Use {
    File,
    /// A Zarr store, whose variable and steps are picked.
    Zarr,
    /// `wms`, `wfs` or `arcgis`, and the layer an address names (an ArcGIS layer's number).
    Service {
        kind: &'static str,
        layer: Option<String>,
    },
}

/// A dataset's resource RustGIS can add.
#[derive(Clone, Debug, PartialEq)]
pub struct Resource {
    /// What it is, as people say it (`GeoJSON`, `Shapefile`, `ArcGIS REST`…).
    pub format: String,
    pub url: String,
    pub using: Use,
}

/// A dataset found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Dataset {
    pub id: String,
    pub title: String,
    /// Its description's start.
    pub description: String,
    pub organization: String,
    /// Its page on the portal.
    pub page: Option<String>,
    /// Its extent (west, south, east, north), when the portal says.
    pub bbox: Option<[f64; 4]>,
    pub resources: Vec<Resource>,
    /// The address listing its files, when they aren't in the search's answer (Hugging Face), or
    /// its layers (an ArcGIS web map): [`Portal::parse_listing`] reads it.
    pub listing: Option<String>,
    /// What it is, as the portal says (`Image service`, `Web map`, `Point cloud`…).
    pub kind: String,
    /// When it was made or last changed (`2026-10-16`), when the portal says.
    pub date: String,
    /// A small picture of it, by its address.
    pub thumbnail: Option<String>,
}

/// A host name: letters, digits, dots and dashes.
fn host_ok(h: &str) -> bool {
    !h.is_empty() && h.len() <= 253 && h.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
}

/// The host of an address (or a bare host).
fn host_of(url: &str) -> &str {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    rest.split(['/', '?', '#']).next().unwrap_or(rest)
}

impl Portal {
    /// A portal by kind (`ckan`, the default, `socrata` or `arcgis-hub`) and an optional address:
    /// a CKAN site (data.gov's catalogue by default), a Socrata site (`data.cityofnewyork.us`) or
    /// discovery API, an ArcGIS Hub site.
    pub fn new(kind: Option<&str>, url: Option<&str>) -> Result<Portal, String> {
        let url = url.map(str::trim).filter(|u| !u.is_empty());
        let web = |u: &str| -> Result<String, String> {
            let u = if u.contains("://") { u.to_string() } else { format!("https://{u}") };
            crate::address::check(&u, None)?;
            Ok(u)
        };
        match kind.map(|k| k.trim().to_ascii_lowercase()).as_deref() {
            None | Some("") | Some("ckan") | Some("datagov") | Some("data.gov") => {
                let base = match url {
                    Some(u) => web(u)?,
                    None => DATA_GOV.to_string(),
                };
                // The site's root: a pasted search address keeps only it.
                let base = base.split(['?', '#']).next().unwrap_or(&base).trim_end_matches('/').to_string();
                let base = base.split("/api/").next().unwrap_or(&base).to_string();
                let kind = if host_of(&base).eq_ignore_ascii_case("catalog.data.gov") { Kind::DataGov } else { Kind::Ckan };
                Ok(Portal { kind, base, domain: None, filter: None, category: None })
            }
            Some("socrata") => {
                let (base, domain) = match url {
                    None => (SOCRATA.to_string(), None),
                    Some(u) => {
                        let host = host_of(u).to_ascii_lowercase();
                        if !host_ok(&host) {
                            return Err(rustgis_data::diagnostics::message_with_address(
                                u,
                                "a Socrata site is its domain, such as data.cityofnewyork.us",
                            ));
                        }
                        web(&host)?;
                        // A discovery API itself searches every site; a site keeps the search to it.
                        if host.starts_with("api.") && host.ends_with(".socrata.com") {
                            (format!("https://{host}"), None)
                        } else {
                            (SOCRATA.to_string(), Some(host))
                        }
                    }
                };
                Ok(Portal { kind: Kind::Socrata, base, domain, filter: None, category: None })
            }
            Some("arcgis-hub") | Some("hub") | Some("arcgis") => {
                let base = match url {
                    Some(u) => {
                        let host = host_of(u).to_ascii_lowercase();
                        if !host_ok(&host) {
                            return Err(rustgis_data::diagnostics::message_with_address(
                                u,
                                "an ArcGIS Hub site is its address, such as hub.arcgis.com",
                            ));
                        }
                        web(&host)?
                    }
                    None => HUB.to_string(),
                };
                Ok(Portal { kind: Kind::Hub, base, domain: None, filter: None, category: None })
            }
            Some("arcgis-portal") | Some("arcgis-online") | Some("earthdata-gis") => {
                let default =
                    if kind.is_some_and(|k| k.trim().eq_ignore_ascii_case("earthdata-gis")) { EARTHDATA_GIS } else { ARCGIS_ONLINE };
                let base = match url {
                    Some(u) => web(u)?,
                    None => default.to_string(),
                };
                // The portal's root: a pasted search, item or home page address keeps only it.
                let base = base.split(['?', '#']).next().unwrap_or(&base).trim_end_matches('/').to_string();
                let base = base.split("/sharing/").next().unwrap_or(&base).split("/home/").next().unwrap_or(&base).to_string();
                let base = base.trim_end_matches("/home").to_string();
                // An organisation's ArcGIS Online address answers only its own pages' requests:
                // its items are searched on ArcGIS Online.
                let base = if host_of(&base).to_ascii_lowercase().ends_with(".maps.arcgis.com") { ARCGIS_ONLINE.to_string() } else { base };
                Ok(Portal { kind: Kind::ArcGis, base, domain: None, filter: None, category: None })
            }
            Some("geolens") => {
                let base = match url {
                    Some(u) => web(u)?,
                    None => GEOLENS.to_string(),
                };
                let base = base.split(['?', '#']).next().unwrap_or(&base).trim_end_matches('/').to_string();
                let base = base.split("/api/").next().unwrap_or(&base).trim_end_matches("/api").to_string();
                Ok(Portal { kind: Kind::GeoLens, base, domain: None, filter: None, category: None })
            }
            Some(k) => {
                let (kind, base) = match k {
                    "openaerialmap" | "oam" => (Kind::OpenAerialMap, OPENAERIALMAP),
                    "usgs-dem" | "3dep" | "tnm" => (Kind::UsgsDem, TNM),
                    "ign-lidar" | "lidar-hd" => (Kind::IgnLidar, IGN_WFS),
                    "huggingface" | "hugging-face" => (Kind::HuggingFace, HUGGING_FACE),
                    "ocean-data" | "odp" => (Kind::OceanData, OCEAN_DATA),
                    "dynamical" => (Kind::Dynamical, DYNAMICAL),
                    other => {
                        return Err(format!(
                            "unknown portal '{other}' (ckan, socrata, arcgis-hub, arcgis-portal, earthdata-gis, openaerialmap, usgs-dem, ign-lidar, geolens, huggingface, ocean-data or dynamical)"
                        ));
                    }
                };
                Ok(Portal { kind, base: base.to_string(), domain: None, filter: None, category: None })
            }
        }
    }

    /// Keeps the search to some datasets: an ArcGIS portal's to an organisation (its id), the USGS
    /// elevation models' to one ([`DEM_DATASETS`], by label or name). The others take none.
    pub fn with_filter(mut self, filter: Option<&str>) -> Result<Portal, String> {
        let Some(f) = filter.map(str::trim).filter(|f| !f.is_empty()) else { return Ok(self) };
        match self.kind {
            Kind::ArcGis => {
                if f.len() > 40 || !f.bytes().all(|b| b.is_ascii_alphanumeric()) {
                    return Err(format!("'{f}': an ArcGIS organisation's id is letters and digits"));
                }
                self.filter = Some(f.to_string());
            }
            Kind::UsgsDem => {
                let name = DEM_DATASETS
                    .iter()
                    .find(|(label, name)| label.eq_ignore_ascii_case(f) || name.eq_ignore_ascii_case(f))
                    .map(|(_, name)| *name)
                    .ok_or_else(|| {
                        format!("'{f}': the elevation models are {}", DEM_DATASETS.iter().map(|(l, _)| *l).collect::<Vec<_>>().join(", "))
                    })?;
                self.filter = Some(name.to_string());
            }
            _ => return Err(format!("{} takes no filter", self.label())),
        }
        Ok(self)
    }

    /// Keeps an ArcGIS portal's search to a kind of item ([`ARCGIS_KINDS`]: imagery, maps,
    /// features or webmaps); none or `all` searches every kind.
    pub fn with_category(mut self, category: Option<&str>) -> Result<Portal, String> {
        let Some(c) = category.map(str::trim).filter(|c| !c.is_empty() && *c != "all") else { return Ok(self) };
        if self.kind != Kind::ArcGis {
            return Err(format!("{} takes no kind of item", self.label()));
        }
        let found = ARCGIS_KINDS.iter().find(|(k, _)| k.eq_ignore_ascii_case(c)).map(|(k, _)| *k);
        self.category = Some(found.ok_or_else(|| format!("'{c}': the kinds are all, imagery, maps, features and webmaps"))?);
        Ok(self)
    }

    /// How many datasets the search found in all, and the address of its next page, from its
    /// answer (`url`: the page's address), when the portal pages (ArcGIS portals,
    /// OpenAerialMap).
    pub fn paging(&self, url: &str, bytes: &[u8]) -> (Option<u64>, Option<String>) {
        let Ok(v) = serde_json::from_slice::<Value>(bytes) else { return (None, None) };
        match self.kind {
            Kind::ArcGis => {
                let total = v.get("total").and_then(Value::as_u64);
                let next = v.get("nextStart").and_then(Value::as_i64).filter(|n| *n > 1 && *n < 10_000);
                (total, next.map(|n| replace_param(url, "start", &n.to_string())))
            }
            Kind::OpenAerialMap => {
                let total = v.pointer("/meta/found").and_then(Value::as_u64);
                let page = v.pointer("/meta/page").and_then(Value::as_u64).unwrap_or(1);
                let limit = v.pointer("/meta/limit").and_then(Value::as_u64).unwrap_or(MAX_RESULTS as u64).max(1);
                let more = total.is_some_and(|t| page * limit < t) && page < 200;
                (total, more.then(|| replace_param(url, "page", &(page + 1).to_string())))
            }
            _ => (None, None),
        }
    }

    /// Whether a search needs words (the others list what's where the map is, or everything).
    pub fn needs_query(&self) -> bool {
        matches!(self.kind, Kind::Ckan | Kind::DataGov | Kind::Socrata | Kind::Hub | Kind::HuggingFace)
    }

    /// Whether a search is by place: it needs a box (the map's extent by default).
    pub fn needs_place(&self) -> bool {
        matches!(self.kind, Kind::OpenAerialMap | Kind::UsgsDem | Kind::IgnLidar)
    }

    /// Whether the box a search is kept to is too large for the portal (IGN's LiDAR HD).
    pub fn check_area(&self, bbox: Option<[f64; 4]>) -> Result<(), String> {
        match (self.kind, bbox) {
            (Kind::IgnLidar, Some(b)) if (b[2] - b[0]).abs() * (b[3] - b[1]).abs() > IGN_MAX_AREA => {
                Err(format!("LiDAR HD tiles are searched in at most {IGN_MAX_AREA} square degrees: zoom in"))
            }
            (k, None) if self.needs_place() => {
                Err(format!("{}: give a box, or search where the map is", if k == Kind::IgnLidar { "LiDAR HD" } else { "the search" }))
            }
            _ => Ok(()),
        }
    }

    /// What the portal is called in messages.
    pub fn label(&self) -> String {
        match (&self.kind, &self.domain) {
            (Kind::Socrata, Some(d)) => d.clone(),
            (Kind::OpenAerialMap, _) => "OpenAerialMap".into(),
            (Kind::UsgsDem, _) => "USGS 3DEP".into(),
            (Kind::IgnLidar, _) => "IGN LiDAR HD".into(),
            (Kind::HuggingFace, _) => "Hugging Face".into(),
            (Kind::OceanData, _) => "Ocean Data Platform".into(),
            (Kind::Dynamical, _) => "dynamical.org".into(),
            _ => host_of(&self.base).to_string(),
        }
    }

    /// The search for `query`, up to `limit` datasets, in `bbox` (west, south, east, north) when
    /// given and the portal filters by place (CKAN with its spatial extension, Hub; the others'
    /// answers are kept to it after).
    pub fn search_url(&self, query: &str, bbox: Option<[f64; 4]>, limit: usize) -> String {
        let q: String = query.trim().chars().take(MAX_QUERY).collect();
        let q = crate::s3::encode(&q, true);
        let limit = limit.clamp(1, MAX_RESULTS);
        let bb = |b: [f64; 4]| format!("{},{},{},{}", b[0], b[1], b[2], b[3]);
        match self.kind {
            Kind::Ckan => {
                let mut u = format!("{}/api/3/action/package_search?q={q}&rows={limit}", self.base);
                if let Some(b) = bbox {
                    u.push_str(&format!("&ext_bbox={}", bb(b)));
                }
                u
            }
            Kind::DataGov => format!("{}/search?q={q}&per_page={limit}", self.base),
            Kind::Socrata => {
                let mut u = format!("{}/api/catalog/v1?q={q}&only=dataset&limit={limit}", self.base);
                if let Some(d) = &self.domain {
                    u.push_str(&format!("&domains={d}"));
                }
                u
            }
            Kind::Hub => {
                let mut u = format!("{}/api/search/v1/collections/dataset/items?q={q}&type=Feature%20Service&limit={limit}", self.base);
                if let Some(b) = bbox {
                    u.push_str(&format!("&bbox={}", bb(b)));
                }
                u
            }
            Kind::ArcGis => {
                let words = arcgis_words(query);
                let mut q = String::new();
                if !words.is_empty() {
                    q.push_str(&format!("({words}) AND "));
                }
                q.push('(');
                let types: &[&str] =
                    self.category.and_then(|c| ARCGIS_KINDS.iter().find(|(k, _)| *k == c)).map(|(_, t)| *t).unwrap_or(&ARCGIS_TYPES);
                q.push_str(&types.iter().map(|t| format!("type:\"{t}\"")).collect::<Vec<_>>().join(" OR "));
                q.push(')');
                if let Some(org) = &self.filter {
                    q.push_str(&format!(" AND orgid:{org}"));
                }
                let mut u = format!("{}/sharing/rest/search?f=json&num={limit}&start=1&q={}", self.base, crate::s3::encode(&q, true));
                if words.is_empty() {
                    u.push_str("&sortField=modified&sortOrder=desc");
                }
                if let Some(b) = bbox {
                    u.push_str(&format!("&bbox={}", bb(b)));
                }
                u
            }
            Kind::OpenAerialMap => {
                // Its search takes no words: the newest images where asked, kept to the words after
                // (from more of them when there are words).
                let rows = if q.is_empty() { MAX_RESULTS } else { MAX_RESULTS * 4 };
                let mut u = format!("{}/meta?limit={rows}&page=1&order_by=acquisition_end&sort=desc", self.base);
                if let Some(b) = bbox {
                    u.push_str(&format!("&bbox={}", bb(b)));
                }
                u
            }
            Kind::UsgsDem => {
                let dataset = self.filter.as_deref().unwrap_or(DEM_DATASETS[2].1);
                // More than asked for: the products repeat tiles (older editions), kept once after.
                let mut u = format!(
                    "{}/products?datasets={}&prodFormats=GeoTIFF&max={}",
                    self.base,
                    crate::s3::encode(dataset, true),
                    (limit * 3).min(150)
                );
                if !q.is_empty() {
                    u.push_str(&format!("&q={q}"));
                }
                if let Some(b) = bbox {
                    u.push_str(&format!("&bbox={}", bb(b)));
                }
                u
            }
            Kind::IgnLidar => {
                let b = bbox.unwrap_or([0.0; 4]);
                format!(
                    "{}?SERVICE=WFS&VERSION=2.0.0&REQUEST=GetFeature&TYPENAMES=IGNF_LIDAR-HD_METADONNEE:metadata&OUTPUTFORMAT=application/json&SRSNAME=urn:ogc:def:crs:OGC:1.3:CRS84&COUNT={IGN_MAX_TILES}&BBOX={},urn:ogc:def:crs:OGC:1.3:CRS84",
                    self.base,
                    bb(b)
                )
            }
            Kind::GeoLens => format!("{}/api/search/datasets/?q={q}&limit={limit}", self.base),
            Kind::HuggingFace => match hf_repo(query) {
                // `owner/name`: that dataset.
                Some(repo) => format!("{}/api/datasets/{repo}", self.base),
                None => format!("{}/api/datasets?search={q}&limit={limit}", self.base),
            },
            // Its whole catalogue (a few hundred datasets), searched here.
            Kind::OceanData => format!("{}/api/stac/search?limit=1000", self.base),
            // Its catalogue (a few dozen datasets), searched here.
            Kind::Dynamical => format!("{}/catalog.json", self.base),
        }
    }

    /// The datasets in a search's answer (`from`: the search's address, which the resources' own
    /// addresses are judged against), with the resources RustGIS can add; datasets without one are
    /// left out, as are those outside `bbox` when they say where they are.
    pub fn parse(&self, from: &str, bytes: &[u8], bbox: Option<[f64; 4]>, query: &str) -> Result<Vec<Dataset>, String> {
        let v: Value = serde_json::from_slice(bytes).map_err(|_| format!("{}: the answer isn't JSON", self.label()))?;
        // The words a portal whose search takes none is kept to here: every one in the title,
        // description or provider.
        let words: Vec<String> = query.split_whitespace().take(20).map(str::to_lowercase).collect();
        let has_words = |d: &Dataset| {
            let all = format!("{} {} {} {}", d.title, d.description, d.organization, d.id).to_lowercase();
            words.iter().all(|w| all.contains(w.as_str()))
        };
        let mut out = match self.kind {
            Kind::Ckan => {
                if v.get("success").and_then(Value::as_bool) == Some(false) {
                    return Err(format!("{}: {}", self.label(), error_text(&v)));
                }
                let r = v.pointer("/result/results").and_then(Value::as_array).ok_or_else(|| self.not_portal())?;
                r.iter().take(MAX_RESULTS).filter_map(|d| ckan_dataset(&self.base, d)).collect::<Vec<_>>()
            }
            Kind::DataGov => {
                let r = v.get("results").and_then(Value::as_array).ok_or_else(|| self.not_portal())?;
                r.iter().take(MAX_RESULTS).filter_map(|d| datagov_dataset(&self.base, d)).collect()
            }
            Kind::Socrata => {
                let r = v.get("results").and_then(Value::as_array).ok_or_else(|| self.not_portal())?;
                r.iter().take(MAX_RESULTS).filter_map(socrata_dataset).collect()
            }
            Kind::Hub => {
                let r = v.get("features").and_then(Value::as_array).ok_or_else(|| self.not_portal())?;
                r.iter().take(MAX_RESULTS).filter_map(|d| hub_dataset(&self.base, d)).collect()
            }
            Kind::ArcGis => {
                if let Some(e) = v.get("error") {
                    let m = e.get("message").and_then(Value::as_str).unwrap_or("the search failed");
                    return Err(format!("{}: {}", self.label(), m.chars().take(200).collect::<String>()));
                }
                let r = v.get("results").and_then(Value::as_array).ok_or_else(|| self.not_portal())?;
                r.iter().take(MAX_RESULTS).filter_map(|d| arcgis_dataset(&self.base, d)).collect()
            }
            Kind::OpenAerialMap => {
                let r = v.get("results").and_then(Value::as_array).ok_or_else(|| self.not_portal())?;
                // Every match on the page: the next page starts after all the images fetched.
                r.iter().take(MAX_RESULTS * 4).filter_map(oam_dataset).filter(|d| has_words(d)).collect()
            }
            Kind::UsgsDem => {
                let r = v.get("items").and_then(Value::as_array).ok_or_else(|| self.not_portal())?;
                newest_dems(r.iter().take(500).filter_map(dem_dataset).collect())
            }
            Kind::IgnLidar => {
                let r = v.get("features").and_then(Value::as_array).ok_or_else(|| self.not_portal())?;
                r.iter().take(IGN_MAX_TILES).filter_map(ign_dataset).filter(|d| has_words(d)).take(MAX_RESULTS).collect()
            }
            Kind::GeoLens => {
                let r = v.get("features").and_then(Value::as_array).ok_or_else(|| self.not_portal())?;
                r.iter().take(MAX_RESULTS).filter_map(|d| geolens_dataset(&self.base, d)).collect()
            }
            Kind::HuggingFace => {
                // One dataset (`owner/name`) or a list.
                let list: Vec<&Value> = match &v {
                    Value::Array(a) => a.iter().take(MAX_RESULTS).collect(),
                    Value::Object(_) if v.get("id").is_some() => vec![&v],
                    _ => return Err(self.not_portal()),
                };
                list.into_iter().filter_map(|d| hf_dataset(&self.base, d)).collect()
            }
            Kind::Dynamical => {
                let r = v.get("links").and_then(Value::as_array).ok_or_else(|| self.not_portal())?;
                r.iter().take(500).filter_map(dynamical_dataset).filter(|d| has_words(d)).take(MAX_RESULTS).collect()
            }
            Kind::OceanData => {
                let r = v.get("features").and_then(Value::as_array).ok_or_else(|| self.not_portal())?;
                r.iter()
                    .take(2000)
                    .filter_map(|d| ocean_dataset(&self.base, d))
                    .filter(|d| has_words(d) && bbox.is_none_or(|b| d.bbox.is_none_or(|e| overlaps(b, e))))
                    .take(MAX_RESULTS)
                    .collect()
            }
        };
        for d in &mut out {
            // The portal's answer is hostile input: an address it gives may not reach into the
            // local network (unless the portal is there) or a forbidden address.
            d.resources.retain(|r| crate::address::check(&r.url, Some(from)).is_ok());
            // So may its pictures', fetched as results are shown.
            d.thumbnail = d.thumbnail.take().filter(|u| crate::address::check(u, Some(from)).is_ok());
            d.resources.sort_by_key(|r| rank(&r.format));
            d.resources.truncate(MAX_RESOURCES);
        }
        out.retain(|d| (!d.resources.is_empty() || d.listing.is_some()) && bbox.is_none_or(|b| d.bbox.is_none_or(|e| overlaps(b, e))));
        Ok(out)
    }

    fn not_portal(&self) -> String {
        match self.kind {
            Kind::Ckan => format!("{}: not a CKAN catalogue (its package_search answered something else)", self.label()),
            Kind::DataGov => format!("{}: the catalogue's answer has no results", self.label()),
            Kind::Socrata => format!("{}: not a Socrata discovery answer", self.label()),
            Kind::Hub => format!("{}: not an ArcGIS Hub search answer", self.label()),
            Kind::ArcGis => format!("{}: not an ArcGIS portal's search answer", self.label()),
            _ => format!("{}: the answer isn't a list of datasets", self.label()),
        }
    }
}

/// A CKAN error's message.
fn error_text(v: &Value) -> String {
    v.pointer("/error/message").and_then(Value::as_str).unwrap_or("the search failed").chars().take(200).collect()
}

fn text(v: &Value, k: &str, max: usize) -> String {
    v.get(k).and_then(Value::as_str).map(|s| s.trim().chars().take(max).collect()).unwrap_or_default()
}

/// A description's start, on one line, without its HTML tags and common entities.
fn snippet(s: &str) -> String {
    let mut plain = String::with_capacity(s.len());
    let mut tag = false;
    for c in s.chars() {
        match c {
            '<' => tag = true,
            '>' if tag => {
                tag = false;
                plain.push(' ');
            }
            _ if !tag => plain.push(c),
            _ => {}
        }
    }
    let plain = plain
        .replace("&nbsp;", " ")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&");
    let one: String = plain.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() > 240 { format!("{}…", one.chars().take(240).collect::<String>()) } else { one }
}

/// The order resources are offered in: what reads best first.
fn rank(format: &str) -> usize {
    ["GeoJSON", "FlatGeobuf", "GeoParquet", "GeoPackage", "KML", "KMZ", "Shapefile", "GeoTIFF", "ArcGIS REST", "WMS", "WFS", "CSV"]
        .iter()
        .position(|f| *f == format)
        .unwrap_or(usize::MAX)
}

/// What a resource is, from its format label, media type and address; `None` for what RustGIS
/// doesn't add (web pages, PDFs, plain JSON…).
pub fn classify(format: &str, media: &str, url: &str) -> Option<Resource> {
    let url = url.trim();
    let lower_url = url.to_ascii_lowercase();
    if !(lower_url.starts_with("https://") || lower_url.starts_with("http://")) || url.len() > 2048 {
        return None;
    }
    let f = format.trim().to_ascii_lowercase();
    let m = media.trim().to_ascii_lowercase();
    let path = lower_url.split(['?', '#']).next().unwrap_or(&lower_url);
    let ext = path.rsplit('/').next().and_then(|n| n.rsplit_once('.')).map(|(_, e)| e).unwrap_or("");
    let file = |name: &str| Some(Resource { format: name.into(), url: url.to_string(), using: Use::File });
    let service = |name: &str, kind: &'static str, layer: Option<String>| {
        Some(Resource { format: name.into(), url: url.to_string(), using: Use::Service { kind, layer } })
    };
    // Services first: their addresses often end in no extension, and their labels are many.
    if f.contains("arcgis") || f.contains("esri rest") || path.contains("/rest/services/") {
        // `…/FeatureServer` or `…/MapServer`, and a layer's number after it.
        let parts: Vec<&str> = url.split(['?', '#']).next().unwrap_or(url).trim_end_matches('/').rsplitn(2, '/').collect();
        let (layer, root) = match parts.as_slice() {
            [last, root] if last.bytes().all(|b| b.is_ascii_digit()) && !last.is_empty() => (Some(last.to_string()), *root),
            _ => (None, url.split(['?', '#']).next().unwrap_or(url).trim_end_matches('/')),
        };
        let r = root.to_ascii_lowercase();
        if ["/featureserver", "/mapserver", "/imageserver", "/vectortileserver"].iter().any(|k| r.ends_with(k)) {
            return Some(Resource { format: "ArcGIS REST".into(), url: root.to_string(), using: Use::Service { kind: "arcgis", layer } });
        }
        return None;
    }
    if f == "wms" || f.contains("web map service") || lower_url.contains("service=wms") {
        return service("WMS", "wms", None);
    }
    if f == "wfs" || f.contains("web feature service") || lower_url.contains("service=wfs") {
        return service("WFS", "wfs", None);
    }
    match () {
        _ if f == "geojson" || m.contains("geo+json") || m.contains("vnd.geo+json") || ext == "geojson" => file("GeoJSON"),
        _ if f == "kmz" || m.contains("kmz") || ext == "kmz" => file("KMZ"),
        _ if f == "kml" || m.contains("google-earth.kml") || ext == "kml" => file("KML"),
        _ if f.contains("shapefile")
            || f == "shp"
            || (ext == "zip" && (f.contains("shp") || lower_url.contains("shapefile")))
            || (f == "zip" && lower_url.contains("shapefile")) =>
        {
            file("Shapefile")
        }
        _ if f.contains("geoparquet") || f == "parquet" || ext == "parquet" => file("GeoParquet"),
        _ if f.contains("flatgeobuf") || ext == "fgb" => file("FlatGeobuf"),
        _ if f.contains("geopackage") || f == "gpkg" || ext == "gpkg" => file("GeoPackage"),
        _ if f == "geotiff" || f == "tiff" || f == "tif" || m.contains("image/tiff") || ext == "tif" || ext == "tiff" => file("GeoTIFF"),
        // A zip of unknown content: often a Shapefile, read as one (the reader says if it isn't).
        _ if f == "zip" || m.contains("application/zip") || ext == "zip" => file("Shapefile"),
        _ if f == "csv" || m.contains("text/csv") || ext == "csv" => file("CSV"),
        _ => None,
    }
}

/// A box of a GeoJSON geometry's coordinates (bounded walk), in longitude/latitude.
fn bbox_of(g: &Value) -> Option<[f64; 4]> {
    fn walk(v: &Value, depth: usize, b: &mut [f64; 4], n: &mut usize) {
        if depth > 6 || *n > 100_000 {
            return;
        }
        if let Some(a) = v.as_array() {
            if let (Some(x), Some(y)) = (a.first().and_then(Value::as_f64), a.get(1).and_then(Value::as_f64)) {
                if (-180.0..=180.0).contains(&x) && (-90.0..=90.0).contains(&y) {
                    *b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
                }
                *n += 1;
                return;
            }
            for c in a {
                walk(c, depth + 1, b, n);
            }
        }
    }
    let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    let mut n = 0;
    walk(g.get("coordinates")?, 0, &mut b, &mut n);
    (b[0] <= b[2] && b[1] <= b[3]).then_some(b)
}

/// Whether two boxes meet (a box crossing the antimeridian, west past east, as two).
fn overlaps(a: [f64; 4], b: [f64; 4]) -> bool {
    let spans = |w: f64, e: f64| if w <= e { vec![(w, e)] } else { vec![(w, 180.0), (-180.0, e)] };
    a[1] <= b[3] && b[1] <= a[3] && spans(a[0], a[2]).iter().any(|(aw, ae)| spans(b[0], b[2]).iter().any(|(bw, be)| aw <= be && bw <= ae))
}

fn ckan_dataset(base: &str, d: &Value) -> Option<Dataset> {
    let resources: Vec<Resource> = d
        .get("resources")?
        .as_array()?
        .iter()
        .take(200)
        .filter_map(|r| classify(&text(r, "format", 80), &text(r, "mimetype", 120), &text(r, "url", 2048)))
        .collect();
    let name = text(d, "name", 200);
    let bbox = d
        .get("extras")
        .and_then(Value::as_array)
        .and_then(|e| e.iter().take(100).find(|x| x.get("key").and_then(Value::as_str) == Some("spatial")))
        .and_then(|x| x.get("value")?.as_str())
        .filter(|s| s.len() < 1 << 20)
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|g| bbox_of(&g));
    Some(Dataset {
        id: if name.is_empty() { text(d, "id", 200) } else { name.clone() },
        title: text(d, "title", 300),
        description: snippet(&text(d, "notes", 4000)),
        organization: d.get("organization").map(|o| text(o, "title", 200)).unwrap_or_default(),
        page: (!name.is_empty()).then(|| format!("{base}/dataset/{name}")),
        bbox,
        resources,
        listing: None,
        kind: String::new(),
        date: String::new(),
        thumbnail: None,
    })
}

fn datagov_dataset(base: &str, d: &Value) -> Option<Dataset> {
    let dcat = d.get("dcat")?;
    let resources: Vec<Resource> = dcat
        .get("distribution")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(200)
        .filter_map(|r| {
            let url = r.get("downloadURL").or_else(|| r.get("accessURL")).and_then(Value::as_str).unwrap_or("");
            classify(&text(r, "format", 80), &text(r, "mediaType", 120), url)
        })
        .collect();
    let slug = text(d, "slug", 200);
    let title = Some(text(d, "title", 300)).filter(|t| !t.is_empty()).unwrap_or_else(|| text(dcat, "title", 300));
    let description = Some(text(d, "description", 4000)).filter(|t| !t.is_empty()).unwrap_or_else(|| text(dcat, "description", 4000));
    let organization =
        d.get("organization").map(|o| text(o, "name", 200)).filter(|o| !o.is_empty()).unwrap_or_else(|| text(d, "publisher", 200));
    Some(Dataset {
        id: if slug.is_empty() { text(d, "identifier", 300) } else { slug.clone() },
        title,
        description: snippet(&description),
        organization,
        page: (!slug.is_empty()).then(|| format!("{base}/dataset/{slug}")),
        bbox: d.get("spatial_shape").and_then(bbox_of),
        resources,
        listing: None,
        kind: String::new(),
        date: String::new(),
        thumbnail: None,
    })
}

/// The geometry column types Socrata names.
const SOCRATA_GEO: [&str; 7] = ["point", "multipoint", "line", "multiline", "polygon", "multipolygon", "location"];

fn socrata_dataset(r: &Value) -> Option<Dataset> {
    let res = r.get("resource")?;
    let id = text(res, "id", 20);
    // A dataset (four-by-four id) with a geometry column: its rows as GeoJSON.
    let is_id = id.len() == 9 && id.as_bytes().get(4) == Some(&b'-') && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    let geo = res
        .get("columns_datatype")
        .and_then(Value::as_array)
        .is_some_and(|c| c.iter().take(500).filter_map(Value::as_str).any(|t| SOCRATA_GEO.contains(&t.to_ascii_lowercase().as_str())));
    let domain = r.get("metadata").map(|m| text(m, "domain", 253)).unwrap_or_default();
    if !is_id || !geo || !host_ok(&domain) || text(res, "type", 40) != "dataset" {
        return None;
    }
    let url = format!("https://{domain}/resource/{id}.geojson?%24limit={}", SOCRATA_ROWS);
    Some(Dataset {
        title: text(res, "name", 300),
        description: snippet(&text(res, "description", 4000)),
        organization: Some(text(res, "attribution", 200)).filter(|a| !a.is_empty()).unwrap_or_else(|| domain.clone()),
        page: Some(text(r, "permalink", 500)).filter(|p| p.starts_with("https://")),
        bbox: None,
        resources: vec![Resource { format: "GeoJSON".into(), url, using: Use::File }],
        listing: None,
        kind: String::new(),
        date: String::new(),
        thumbnail: None,
        id,
    })
}

/// How many rows a Socrata dataset's GeoJSON asks for (its API answers 1000 when not asked).
pub const SOCRATA_ROWS: usize = 50_000;

/// An ArcGIS Hub item, its page on the Hub site searched (`base`: hub.arcgis.com or an
/// organisation's own).
fn hub_dataset(base: &str, f: &Value) -> Option<Dataset> {
    let p = f.get("properties")?;
    let url = text(p, "url", 2048);
    let resource = classify("ArcGIS REST", "", &url)?;
    let id = Some(text(p, "id", 100)).filter(|i| !i.is_empty()).unwrap_or_else(|| text(f, "id", 100));
    Some(Dataset {
        page: (!id.is_empty()).then(|| format!("{}/datasets/{id}", base.trim_end_matches('/'))),
        title: text(p, "title", 300),
        description: snippet(&Some(text(p, "snippet", 2000)).filter(|s| !s.is_empty()).unwrap_or_else(|| text(p, "description", 2000))),
        organization: text(p, "owner", 200),
        bbox: p.get("extent").and_then(bbox_of),
        resources: vec![resource],
        listing: None,
        kind: String::new(),
        date: String::new(),
        thumbnail: None,
        id,
    })
}

/// The item types an ArcGIS portal's search asks for: those RustGIS adds.
const ARCGIS_TYPES: [&str; 6] = ["Feature Service", "Map Service", "Image Service", "Vector Tile Service", "GeoJson", "Web Map"];

/// The kinds of ArcGIS items a search can keep to: a name and the item types.
pub const ARCGIS_KINDS: [(&str, &[&str]); 4] = [
    ("imagery", &["Image Service"]),
    ("maps", &["Map Service", "Vector Tile Service"]),
    ("features", &["Feature Service", "GeoJson"]),
    ("webmaps", &["Web Map"]),
];

/// The most LiDAR HD tiles a search lists.
pub const IGN_MAX_TILES: usize = 500;

/// The most files a Hugging Face dataset's listing offers.
const MAX_FILES: usize = 200;

/// A search's words for an ArcGIS portal, its query syntax's operators and signs taken out.
fn arcgis_words(q: &str) -> String {
    let plain: String = q.chars().take(MAX_QUERY).map(|c| if c.is_alphanumeric() || c == '_' || c == '.' { c } else { ' ' }).collect();
    plain.split_whitespace().filter(|w| !matches!(*w, "AND" | "OR" | "NOT")).collect::<Vec<_>>().join(" ")
}

/// A web map's layers (its operational layers, those in groups too) RustGIS adds: ArcGIS services,
/// named by their titles.
fn web_map_layers(v: &Value, from: &str) -> Result<Vec<Resource>, String> {
    // A layer in a group is named after the group too (groups often repeat their layers' titles).
    fn walk(layers: &Value, depth: usize, group: &str, out: &mut Vec<Resource>, from: &str) {
        for l in layers.as_array().into_iter().flatten().take(500) {
            if out.len() >= MAX_FILES {
                return;
            }
            let title = text(l, "title", 200);
            let named = match (group.is_empty(), title.is_empty()) {
                (true, _) => title.clone(),
                (false, true) => group.to_string(),
                (false, false) => format!("{group} › {title}"),
            };
            if depth < 6
                && let Some(inner) = l.get("layers")
            {
                walk(inner, depth + 1, &named, out, from);
            }
            let url = text(l, "url", 2048);
            if let Some(mut r) = classify("ArcGIS REST", "", &url).filter(|r| crate::address::check(&r.url, Some(from)).is_ok()) {
                if !named.is_empty() {
                    r.format = named;
                }
                out.push(r);
            }
        }
    }
    let ops = v.get("operationalLayers").ok_or("not an ArcGIS web map (it has no operational layers)")?;
    let mut out = Vec::new();
    walk(ops, 0, "", &mut out, from);
    Ok(out)
}

/// An address with its query parameter `key` set to `value`.
fn replace_param(url: &str, key: &str, value: &str) -> String {
    let (path, query) = url.split_once('?').unwrap_or((url, ""));
    let mut parts: Vec<String> =
        query.split('&').filter(|p| !p.is_empty() && p.split('=').next() != Some(key)).map(str::to_string).collect();
    parts.push(format!("{key}={value}"));
    format!("{path}?{}", parts.join("&"))
}

/// A Hugging Face dataset named whole (`owner/name`, or its page's address).
fn hf_repo(q: &str) -> Option<String> {
    let q = q.trim();
    let q = q.strip_prefix("https://huggingface.co/datasets/").unwrap_or(q).trim_end_matches('/');
    let (owner, name) = q.split_once('/')?;
    let ok = |s: &str| {
        !s.is_empty() && s.len() <= 96 && !s.starts_with('.') && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    };
    (ok(owner) && ok(name)).then(|| format!("{owner}/{name}"))
}

/// A box given as four numbers, if it is one in longitude/latitude.
fn bbox4(v: &Value) -> Option<[f64; 4]> {
    let a = v.as_array()?;
    let n = |i: usize| a.get(i).and_then(Value::as_f64);
    let b = [n(0)?, n(1)?, n(2)?, n(3)?];
    let lon = |x: f64| (-180.0..=180.0).contains(&x);
    let lat = |y: f64| (-90.0..=90.0).contains(&y);
    (lon(b[0]) && lon(b[2]) && lat(b[1]) && lat(b[3]) && b[1] <= b[3]).then_some(b)
}

/// An id safe in an address's path: letters, digits, dashes, underscores and dots.
fn id_ok(id: &str) -> bool {
    !id.is_empty() && id.len() <= 100 && !id.starts_with('.') && id.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

/// A picture's address, if it is a web one.
fn picture(url: String) -> Option<String> {
    (url.starts_with("https://") && url.len() <= 2048).then_some(url)
}

/// The day (`2026-10-16`) of a time in milliseconds since 1970.
fn day_of_ms(ms: f64) -> String {
    if !ms.is_finite() || !(0.0..=1e15).contains(&ms) {
        return String::new();
    }
    // Days to a civil date (Howard Hinnant's algorithm).
    let z = (ms / 86_400_000.0).floor() as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// An ArcGIS item type as a label (`Image Service` → `Image service`).
fn type_label(t: &str) -> String {
    let mut words = t.split_whitespace();
    let first = words.next().unwrap_or("").to_string();
    std::iter::once(first).chain(words.map(str::to_lowercase)).collect::<Vec<_>>().join(" ")
}

/// An ArcGIS portal's item: its service, a GeoJSON file's data, or a web map, whose layers are
/// listed after.
fn arcgis_dataset(base: &str, d: &Value) -> Option<Dataset> {
    let id = text(d, "id", 64);
    if !id.bytes().all(|b| b.is_ascii_alphanumeric()) || id.is_empty() {
        return None;
    }
    let ty = text(d, "type", 80);
    let (resources, listing) = match ty.as_str() {
        "GeoJson" => (
            vec![Resource { format: "GeoJSON".into(), url: format!("{base}/sharing/rest/content/items/{id}/data"), using: Use::File }],
            None,
        ),
        "Web Map" => (Vec::new(), Some(format!("{base}/sharing/rest/content/items/{id}/data?f=json"))),
        _ => (vec![classify("ArcGIS REST", "", &text(d, "url", 2048))?], None),
    };
    // `[[west, south], [east, north]]`.
    let bbox = d.get("extent").and_then(Value::as_array).and_then(|e| {
        let (a, b) = (e.first()?.as_array()?, e.get(1)?.as_array()?);
        bbox4(&Value::Array(vec![a.first()?.clone(), a.get(1)?.clone(), b.first()?.clone(), b.get(1)?.clone()]))
    });
    let thumb = text(d, "thumbnail", 300);
    let snippet_text = text(d, "snippet", 2000);
    let long = snippet(&text(d, "description", 4000));
    Some(Dataset {
        title: text(d, "title", 300),
        description: if long.is_empty() { snippet(&snippet_text) } else { long },
        organization: text(d, "owner", 200),
        page: Some(format!("{base}/home/item.html?id={id}")),
        bbox,
        resources,
        listing,
        kind: type_label(&ty),
        date: d.get("modified").and_then(Value::as_f64).map(day_of_ms).unwrap_or_default(),
        thumbnail: (!thumb.is_empty() && !thumb.contains("..")).then(|| format!("{base}/sharing/rest/content/items/{id}/info/{thumb}")),
        id,
    })
}

/// An OpenAerialMap image: its GeoTIFF (a COG).
fn oam_dataset(d: &Value) -> Option<Dataset> {
    let resource = classify("GeoTIFF", "", &text(d, "uuid", 2048))?;
    let mut about = vec![text(d, "platform", 40)];
    if let Some(g) = d.get("gsd").and_then(Value::as_f64).filter(|g| g.is_finite() && *g > 0.0) {
        about.push(if g < 1.0 { format!("{:.0} cm per pixel", g * 100.0) } else { format!("{g:.1} m per pixel") });
    }
    about.retain(|a| !a.is_empty());
    let p = d.get("properties");
    let sensor = p.map(|p| text(p, "sensor", 100)).unwrap_or_default();
    Some(Dataset {
        id: text(d, "_id", 64),
        title: Some(text(d, "title", 300)).filter(|t| !t.is_empty()).unwrap_or_else(|| "Untitled image".into()),
        description: if sensor.is_empty() { String::new() } else { format!("Sensor: {sensor}") },
        organization: text(d, "provider", 200),
        page: None,
        bbox: d.get("bbox").and_then(bbox4).or_else(|| d.pointer("/geojson").and_then(bbox_of)),
        resources: vec![resource],
        listing: None,
        kind: about.join(" · "),
        date: text(d, "acquisition_end", 10),
        thumbnail: p.and_then(|p| picture(text(p, "thumbnail", 2048))),
    })
}

/// A 3DEP product: the tile it covers (older editions of a tile share it), its date, and the
/// dataset.
fn dem_dataset(i: &Value) -> Option<(String, (bool, String), Dataset)> {
    let url = text(i, "downloadURL", 2048);
    let resource = classify("GeoTIFF", "", &url)?;
    let title = text(i, "title", 300);
    // A 1 m tile's `x55y511`, else the title without its date.
    let tile = title
        .split_whitespace()
        .find(|w| w.starts_with('x') && w.contains('y') && w.bytes().skip(1).all(|b| b.is_ascii_digit() || b == b'y'))
        .map(str::to_string)
        .unwrap_or_else(|| {
            let t = title.trim_end_matches(|c: char| c.is_ascii_digit());
            if title.len() - t.len() == 8 { t.trim_end().to_string() } else { title.clone() }
        });
    let date = Some(text(i, "publicationDate", 30)).filter(|d| !d.is_empty()).unwrap_or_else(|| text(i, "lastUpdated", 30));
    let bbox = i.get("boundingBox").and_then(|b| {
        bbox4(&Value::Array(vec![b.get("minX")?.clone(), b.get("minY")?.clone(), b.get("maxX")?.clone(), b.get("maxY")?.clone()]))
    });
    let mut kind = "GeoTIFF".to_string();
    if let Some(n) = i.get("sizeInBytes").and_then(Value::as_f64).filter(|n| *n > 0.0) {
        kind.push_str(&if n >= 1e6 { format!(" · {:.0} MB", n / 1e6) } else { format!(" · {:.0} KB", (n / 1e3).max(1.0)) });
    }
    let d = Dataset {
        id: Some(text(i, "sourceId", 100)).filter(|s| !s.is_empty()).unwrap_or_else(|| title.clone()),
        title,
        description: text(i, "datasetName", 200),
        organization: "USGS".into(),
        page: Some(text(i, "metaUrl", 500)).filter(|p| p.starts_with("https://")),
        bbox,
        resources: vec![resource],
        listing: None,
        kind,
        date: date.chars().take(10).collect(),
        thumbnail: picture(text(i, "previewGraphicURL", 2048)),
    };
    // A current edition before a historical one, then the newest.
    Some((tile, (!url.contains("/historical/"), date), d))
}

/// One product per tile, its newest current edition, in the order the tiles came.
fn newest_dems(found: Vec<(String, (bool, String), Dataset)>) -> Vec<Dataset> {
    let mut best: Vec<(String, (bool, String), Dataset)> = Vec::new();
    for (tile, rank, d) in found {
        match best.iter_mut().find(|(t, _, _)| *t == tile) {
            Some(b) if rank > b.1 => *b = (tile, rank, d),
            Some(_) => {}
            None => best.push((tile, rank, d)),
        }
    }
    best.into_iter().take(MAX_RESULTS).map(|(_, _, d)| d).collect()
}

/// A LiDAR HD tile: its COPC point cloud.
fn ign_dataset(f: &Value) -> Option<Dataset> {
    let p = f.get("properties")?;
    let url = text(p, "url_npl", 2048);
    let lower = url.to_ascii_lowercase();
    if !lower.starts_with("https://") || !lower.split(['?', '#']).next().is_some_and(|u| u.ends_with(".laz")) {
        return None;
    }
    let tile = text(p, "coordonnees_nw", 40);
    let mut kind = "Point cloud".to_string();
    if let Some(n) = p.get("nombre_points").and_then(Value::as_f64).filter(|n| *n > 0.0) {
        kind.push_str(&format!(" · {:.1} million points", n / 1e6));
    }
    let mission = text(p, "code_mission", 40);
    Some(Dataset {
        id: text(f, "id", 100),
        title: if tile.is_empty() { url.rsplit('/').next().unwrap_or("").to_string() } else { format!("LiDAR HD {tile}") },
        description: if mission.is_empty() { String::new() } else { format!("Mission {mission}, Lambert-93 tile {tile}") },
        organization: "IGN".into(),
        page: None,
        bbox: f.get("geometry").and_then(bbox_of),
        resources: vec![Resource { format: "COPC".into(), url, using: Use::File }],
        listing: None,
        kind,
        date: text(p, "date_fin_acquisition", 20).trim_end_matches('Z').to_string(),
        thumbnail: None,
    })
}

/// A GeoLens dataset: its features (OGC API Features, as GeoJSON). Rasters, drawn from tiles
/// signed for a while, are left out.
fn geolens_dataset(base: &str, f: &Value) -> Option<Dataset> {
    let p = f.get("properties")?;
    let id = text(f, "id", 100);
    let raster = text(p, "record_type", 60).contains("raster") || p.get("band_count").and_then(Value::as_u64).is_some_and(|n| n > 0);
    if !id_ok(&id) || raster {
        return None;
    }
    let geometry = text(p, "geometry_type", 40).to_lowercase();
    let mut kind = if geometry.is_empty() { "Features".to_string() } else { format!("Features ({geometry})") };
    if let Some(n) = p.get("feature_count").and_then(Value::as_u64) {
        kind.push_str(&format!(" · {n}"));
    }
    Some(Dataset {
        title: text(p, "title", 300),
        description: snippet(&text(p, "description", 4000)),
        organization: text(p, "license", 100),
        page: None,
        bbox: f.get("geometry").and_then(bbox_of),
        resources: vec![Resource {
            format: "GeoJSON".into(),
            url: format!("{base}/api/collections/{id}/items?limit={GEOLENS_FEATURES}"),
            using: Use::File,
        }],
        listing: None,
        kind,
        date: String::new(),
        thumbnail: None,
        id,
    })
}

/// How many features a GeoLens dataset is read with: a page of its server's, the most it gives.
pub const GEOLENS_FEATURES: usize = 1000;

/// A Hugging Face dataset (public, not gated): its files are listed after.
fn hf_dataset(base: &str, d: &Value) -> Option<Dataset> {
    let id = text(d, "id", 200);
    let (owner, _) = id.split_once('/')?;
    let gated = d.get("gated").is_some_and(|g| g.as_bool() != Some(false));
    let shut = d.get("private").and_then(Value::as_bool) == Some(true) || d.get("disabled").and_then(Value::as_bool) == Some(true);
    if hf_repo(&id).is_none() || gated || shut {
        return None;
    }
    let tags: Vec<&str> = d
        .get("tags")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(100)
        .filter_map(Value::as_str)
        .filter(|t| t.starts_with("license:") || t.starts_with("task_categories:") || t.starts_with("modality:"))
        .take(6)
        .collect();
    let mut kind = "Dataset".to_string();
    if let Some(n) = d.get("downloads").and_then(Value::as_u64) {
        kind.push_str(&format!(" · {n} downloads"));
    }
    Some(Dataset {
        organization: owner.to_string(),
        title: id.clone(),
        description: tags.iter().map(|t| t.replace(':', ": ")).collect::<Vec<_>>().join(" · "),
        page: Some(format!("{base}/datasets/{id}")),
        bbox: None,
        resources: Vec::new(),
        listing: Some(format!("{base}/api/datasets/{id}/tree/main?recursive=true&limit=1000")),
        kind,
        date: text(d, "lastModified", 10),
        thumbnail: None,
        id,
    })
}

/// An Ocean Data Platform dataset: its features (OGC API Features, as GeoJSON). The browser reads
/// them through GeoLibre's relay, the platform's own answering only its site.
fn ocean_dataset(base: &str, f: &Value) -> Option<Dataset> {
    let id = text(f, "id", 100);
    if !id_ok(&id) {
        return None;
    }
    let p = f.get("properties")?;
    let title = text(p, "title", 300);
    let title = title.trim_start_matches("Published — ").trim_start_matches("Published - ").to_string();
    let url = if cfg!(target_arch = "wasm32") {
        format!("{OCEAN_RELAY}/{id}/items?limit={OCEAN_FEATURES}")
    } else {
        format!("{base}/api/features/collections/{id}/items?f=json&limit={OCEAN_FEATURES}")
    };
    Some(Dataset {
        title,
        description: snippet(&text(p, "description", 4000)),
        organization: "HUB Ocean".into(),
        page: Some(format!("https://app.hubocean.earth/catalog/dataset/{id}")),
        bbox: f.get("bbox").and_then(bbox4),
        resources: vec![Resource { format: "GeoJSON".into(), url, using: Use::File }],
        listing: None,
        kind: "Features".into(),
        date: Some(text(p, "start_datetime", 10)).filter(|d| !d.is_empty()).unwrap_or_else(|| text(p, "datetime", 10)),
        thumbnail: None,
        id,
    })
}

/// A dynamical.org dataset (a catalogue's child link): its Zarr store, `data.dynamical.org`'s
/// provider/model/kind path. The virtual datasets, GRIB read through Icechunk, are left out.
fn dynamical_dataset(l: &Value) -> Option<Dataset> {
    if l.get("rel").and_then(Value::as_str) != Some("child") {
        return None;
    }
    let href = text(l, "href", 500);
    let id = href.trim_end_matches("/collection.json").rsplit('/').next().unwrap_or("").to_string();
    if !id_ok(&id) || id.ends_with("-virtual") {
        return None;
    }
    // `noaa-gfs-forecast-35-day` → `noaa/gfs/forecast-35-day`: the provider, the model up to
    // the kind (forecast or analysis), the rest.
    let parts: Vec<&str> = id.split('-').collect();
    if parts.iter().any(|p| p.is_empty()) {
        return None;
    }
    let k = parts.iter().position(|p| *p == "forecast" || *p == "analysis").filter(|k| *k >= 2)?;
    let path = format!("{}/{}/{}", parts.first()?, parts.get(1..k)?.join("-"), parts.get(k..)?.join("-"));
    let provider = parts.first().map(|p| p.to_uppercase()).unwrap_or_default();
    Some(Dataset {
        title: Some(text(l, "title", 300)).filter(|t| !t.is_empty()).unwrap_or_else(|| id.clone()),
        description: String::new(),
        organization: provider,
        page: Some(format!("https://dynamical.org/catalog/{id}/")),
        bbox: None,
        resources: vec![Resource { format: "Zarr".into(), url: format!("{DYNAMICAL_DATA}/{path}/latest.zarr"), using: Use::Zarr }],
        listing: None,
        kind: if parts.contains(&"forecast") { "Forecast".into() } else { "Analysis".into() },
        date: String::new(),
        thumbnail: None,
        id,
    })
}

/// How many features an Ocean Data Platform dataset is read with at most.
pub const OCEAN_FEATURES: usize = 10_000;
/// The relay the browser reads Ocean Data Platform features through.
const OCEAN_RELAY: &str = "https://tiles.geolibre.app/odp/features";

impl Portal {
    /// The address listing a dataset's files, by its name (Hugging Face's `owner/name`).
    pub fn listing_of(&self, dataset: &str) -> Option<String> {
        match self.kind {
            Kind::HuggingFace => hf_repo(dataset).map(|id| format!("{}/api/datasets/{id}/tree/main?recursive=true&limit=1000", self.base)),
            // A web map's layers.
            Kind::ArcGis => {
                let id = dataset.trim();
                (!id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric()))
                    .then(|| format!("{}/sharing/rest/content/items/{id}/data?f=json", self.base))
            }
            _ => None,
        }
    }

    /// The files a dataset's listing ([`Dataset::listing`], fetched from `from`) offers: those
    /// RustGIS adds, named by their path in the dataset.
    pub fn parse_listing(&self, from: &str, bytes: &[u8]) -> Result<Vec<Resource>, String> {
        let v: Value = serde_json::from_slice(bytes).map_err(|_| format!("{}: the listing isn't JSON", self.label()))?;
        if self.kind == Kind::ArcGis {
            return web_map_layers(&v, from);
        }
        let entries = v.as_array().ok_or_else(|| format!("{}: not a listing of files", self.label()))?;
        // `…/api/datasets/{owner}/{name}/tree/…`: the dataset.
        let repo = from
            .split("/api/datasets/")
            .nth(1)
            .and_then(|r| r.split("/tree/").next())
            .and_then(hf_repo)
            .ok_or_else(|| format!("{from}: not a Hugging Face dataset's listing"))?;
        let mut out: Vec<Resource> = entries
            .iter()
            .take(20_000)
            .filter(|e| e.get("type").and_then(Value::as_str) == Some("file"))
            .filter_map(|e| {
                let path = text(e, "path", 1000);
                if path.split('/').any(|s| s.is_empty() || s == "." || s == "..") {
                    return None;
                }
                let encoded: Vec<String> = path.split('/').map(|s| crate::s3::encode(s, true)).collect();
                let url = format!("{}/datasets/{repo}/resolve/main/{}", self.base, encoded.join("/"));
                let lower = path.to_ascii_lowercase();
                let addable = [
                    ".geojson",
                    ".parquet",
                    ".geoparquet",
                    ".fgb",
                    ".gpkg",
                    ".tif",
                    ".tiff",
                    ".csv",
                    ".kml",
                    ".kmz",
                    ".zip",
                    ".pmtiles",
                    ".laz",
                    ".las",
                    ".gpx",
                ]
                .iter()
                .any(|x| lower.ends_with(x));
                addable.then_some(Resource { format: path, url, using: Use::File })
            })
            .filter(|r| crate::address::check(&r.url, Some(from)).is_ok())
            .take(MAX_FILES)
            .collect();
        out.sort_by(|a, b| a.format.cmp(&b.format));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portals_and_searches() {
        let ckan = Portal::new(None, None).unwrap();
        assert_eq!((ckan.kind, ckan.base.as_str()), (Kind::DataGov, DATA_GOV));
        assert_eq!(ckan.search_url(" city parks ", None, 99), "https://catalog.data.gov/search?q=city%20parks&per_page=50");
        let ca = Portal::new(Some("ckan"), Some("https://open.canada.ca/data/api/3/action/package_search?q=x")).unwrap();
        assert_eq!((ca.kind, ca.base.as_str()), (Kind::Ckan, "https://open.canada.ca/data"));
        assert_eq!(
            ca.search_url("lakes", Some([-80.0, 43.0, -79.0, 44.0]), 5),
            "https://open.canada.ca/data/api/3/action/package_search?q=lakes&rows=5&ext_bbox=-80,43,-79,44"
        );
        let nyc = Portal::new(Some("socrata"), Some("https://data.cityofnewyork.us/browse")).unwrap();
        assert_eq!((nyc.domain.as_deref(), nyc.label().as_str()), (Some("data.cityofnewyork.us"), "data.cityofnewyork.us"));
        assert_eq!(
            nyc.search_url("parks", None, 3),
            "https://api.us.socrata.com/api/catalog/v1?q=parks&only=dataset&limit=3&domains=data.cityofnewyork.us"
        );
        assert_eq!(Portal::new(Some("socrata"), None).unwrap().domain, None);
        let hub = Portal::new(Some("arcgis-hub"), None).unwrap();
        assert_eq!(
            hub.search_url("parks", Some([-74.3, 40.5, -73.7, 40.9]), 4),
            "https://hub.arcgis.com/api/search/v1/collections/dataset/items?q=parks&type=Feature%20Service&limit=4&bbox=-74.3,40.5,-73.7,40.9"
        );
        assert!(Portal::new(Some("google"), None).is_err());
        assert!(Portal::new(Some("socrata"), Some("not a host!")).is_err());
        assert!(Portal::new(Some("ckan"), Some("file:///etc")).is_err());
        assert!(Portal::new(Some("ckan"), Some("http://169.254.169.254/")).is_err());
    }

    #[test]
    fn resources_by_format() {
        let r = |f: &str, m: &str, u: &str| classify(f, m, u).map(|r| (r.format, r.using));
        assert_eq!(r("GeoJSON", "", "https://x.org/a?x=1"), Some(("GeoJSON".into(), Use::File)));
        assert_eq!(r("", "application/geo+json", "https://x.org/q"), Some(("GeoJSON".into(), Use::File)));
        assert_eq!(r("ZIP", "application/zip", "https://x.org/a/shapefile"), Some(("Shapefile".into(), Use::File)));
        assert_eq!(r("KML", "", "https://x.org/a.kml").map(|x| x.0), Some("KML".into()));
        assert_eq!(
            r("ArcGIS GeoServices REST API", "", "https://s.arcgis.com/a/arcgis/rest/services/p/FeatureServer/0"),
            Some(("ArcGIS REST".into(), Use::Service { kind: "arcgis", layer: Some("0".into()) }))
        );
        assert_eq!(
            classify("Esri REST", "", "https://s.arcgis.com/a/arcgis/rest/services/p/MapServer/").map(|x| x.url),
            Some("https://s.arcgis.com/a/arcgis/rest/services/p/MapServer".into())
        );
        assert_eq!(
            r("WMS", "", "https://x.org/wms?service=WMS&request=GetCapabilities").map(|x| x.1),
            Some(Use::Service { kind: "wms", layer: None })
        );
        for none in [
            ("HTML", "text/html", "https://x.org/"),
            ("PDF", "", "https://x.org/a.pdf"),
            ("GeoJSON", "", "ftp://x.org/a.geojson"),
            ("ArcGIS REST", "", "https://x.org/rest/services/a/GPServer"),
        ] {
            assert!(classify(none.0, none.1, none.2).is_none(), "{none:?}");
        }
    }

    #[test]
    fn answers() {
        let from = "https://catalog.data.gov/search?q=parks";
        let dg = br#"{"results":[{"slug":"parks","title":"Parks","description":"Parks of a city","organization":{"name":"City"},"spatial_shape":{"type":"Polygon","coordinates":[[[-122.5,47.5],[-122.2,47.5],[-122.2,47.7],[-122.5,47.5]]]},"dcat":{"distribution":[{"downloadURL":"https://x.org/p.csv","mediaType":"text/csv"},{"downloadURL":"https://x.org/p.geojson","mediaType":"application/geo+json"},{"accessURL":"http://127.0.0.1/p.geojson","format":"GeoJSON"},{"downloadURL":"https://x.org/p.html","format":"HTML"}]}},{"slug":"report","title":"A report","dcat":{"distribution":[{"downloadURL":"https://x.org/r.pdf"}]}}],"sort":"relevance"}"#;
        let p = Portal::new(None, None).unwrap();
        let ds = p.parse(from, dg, None, "").unwrap();
        assert_eq!(ds.len(), 1, "a dataset with nothing to add is left out");
        let d = &ds[0];
        assert_eq!((d.title.as_str(), d.organization.as_str()), ("Parks", "City"));
        assert_eq!(d.page.as_deref(), Some("https://catalog.data.gov/dataset/parks"));
        assert_eq!(d.bbox, Some([-122.5, 47.5, -122.2, 47.7]));
        // GeoJSON first, the local-network address left out, the page not a resource.
        assert_eq!(d.resources.iter().map(|r| r.format.as_str()).collect::<Vec<_>>(), vec!["GeoJSON", "CSV"]);
        // Kept to a box: a dataset elsewhere goes.
        assert!(p.parse(from, dg, Some([0.0, 0.0, 1.0, 1.0]), "").unwrap().is_empty());
        assert_eq!(p.parse(from, dg, Some([-123.0, 47.0, -122.0, 48.0]), "").unwrap().len(), 1);
        assert!(p.parse(from, b"<html>", None, "").is_err() && p.parse(from, b"{}", None, "").is_err());

        let soc = br#"{"results":[{"resource":{"id":"enfh-gkve","name":"Parks Properties","type":"dataset","columns_datatype":["Text","MultiPolygon"]},"metadata":{"domain":"data.cityofnewyork.us"},"permalink":"https://data.cityofnewyork.us/d/enfh-gkve"},{"resource":{"id":"nc67-uf89","name":"Violations","type":"dataset","columns_datatype":["Text"]},"metadata":{"domain":"data.cityofnewyork.us"}},{"resource":{"id":"bad","name":"x","type":"dataset","columns_datatype":["Point"]},"metadata":{"domain":"evil host"}}]}"#;
        let s = Portal::new(Some("socrata"), None).unwrap();
        let ds = s.parse("https://api.us.socrata.com/api/catalog/v1?q=parks", soc, None, "").unwrap();
        assert_eq!(ds.len(), 1);
        assert_eq!(ds[0].resources[0].url, "https://data.cityofnewyork.us/resource/enfh-gkve.geojson?%24limit=50000");

        let hub = br#"{"type":"FeatureCollection","features":[{"id":"c5ef","properties":{"id":"c5ef","title":"parks","type":"Feature Service","owner":"nyc","url":"https://services1.arcgis.com/S/arcgis/rest/services/parks/FeatureServer","extent":{"type":"Polygon","coordinates":[[[-73.9,40.76],[-73.87,40.76],[-73.87,40.74],[-73.9,40.76]]]}}},{"properties":{"title":"no url"}}]}"#;
        let h = Portal::new(Some("arcgis-hub"), None).unwrap();
        let ds = h.parse("https://hub.arcgis.com/api/search/v1/collections/dataset/items?q=parks", hub, None, "").unwrap();
        assert_eq!(ds.len(), 1);
        assert_eq!(ds[0].resources[0].using, Use::Service { kind: "arcgis", layer: None });
        assert_eq!(ds[0].bbox, Some([-73.9, 40.74, -73.87, 40.76]));

        let ckan = br#"{"success":true,"result":{"count":1,"results":[{"name":"lakes","title":"Lakes","notes":"All lakes","organization":{"title":"NRCan"},"extras":[{"key":"spatial","value":"{\"type\":\"Polygon\",\"coordinates\":[[[-80,43],[-79,43],[-79,44],[-80,43]]]}"}],"resources":[{"format":"WMS","url":"https://maps.example.ca/wms?service=WMS"},{"format":"SHP","url":"https://x.ca/lakes.zip"}]}]}}"#;
        let c = Portal::new(Some("ckan"), Some("https://open.canada.ca/data")).unwrap();
        let ds = c.parse("https://open.canada.ca/data/api/3/action/package_search?q=lakes", ckan, None, "").unwrap();
        assert_eq!(ds[0].page.as_deref(), Some("https://open.canada.ca/data/dataset/lakes"));
        assert_eq!(ds[0].resources.iter().map(|r| r.format.as_str()).collect::<Vec<_>>(), vec!["Shapefile", "WMS"]);
        assert!(c.parse("x", br#"{"success":false,"error":{"message":"Bad request"}}"#, None, "").unwrap_err().contains("Bad request"));
    }

    #[test]
    fn web_service_searches() {
        let agol = Portal::new(Some("arcgis-portal"), None).unwrap().with_filter(Some("T4QMspbfLg3qTGWY")).unwrap();
        let u = agol.search_url("fire (perimeters) AND x:\"y\"", Some([-120.0, 35.0, -110.0, 40.0]), 5);
        assert!(
            u.starts_with(
                "https://www.arcgis.com/sharing/rest/search?f=json&num=5&start=1&q=%28fire%20perimeters%20x%20y%29%20AND%20%28type%3A"
            ),
            "{u}"
        );
        assert!(u.contains("AND%20orgid%3AT4QMspbfLg3qTGWY") && u.ends_with("&bbox=-120,35,-110,40"), "{u}");
        // No words: the newest items.
        assert!(agol.search_url("", None, 5).contains("&sortField=modified"));
        assert!(Portal::new(Some("arcgis-portal"), None).unwrap().with_filter(Some("org id!")).is_err());
        let ed = Portal::new(Some("earthdata-gis"), None).unwrap();
        assert!(ed.search_url("flood", None, 3).starts_with("https://gis.earthdata.nasa.gov/portal/sharing/rest/search?"));
        // A pasted item page keeps only the portal; an organisation's address searches ArcGIS Online.
        let p = Portal::new(Some("arcgis-portal"), Some("https://gis.example.org/portal/home/item.html?id=1")).unwrap();
        assert_eq!(p.base, "https://gis.example.org/portal");
        assert_eq!(Portal::new(Some("arcgis-portal"), Some("https://acme.maps.arcgis.com/home")).unwrap().base, ARCGIS_ONLINE);

        let oam = Portal::new(Some("openaerialmap"), None).unwrap();
        assert!(oam.needs_place() && !oam.needs_query());
        assert!(oam.check_area(None).is_err());
        assert!(oam.search_url("x", Some([1.0, 2.0, 3.0, 4.0]), 5).ends_with("&bbox=1,2,3,4"));
        let dem = Portal::new(Some("usgs-dem"), None).unwrap().with_filter(Some("1 meter")).unwrap();
        let u = dem.search_url("", Some([-122.3, 46.1, -122.1, 46.3]), 10);
        assert!(u.contains("datasets=Digital%20Elevation%20Model%20%28DEM%29%201%20meter&prodFormats=GeoTIFF&max=30"), "{u}");
        assert!(Portal::new(Some("usgs-dem"), None).unwrap().with_filter(Some("5 cm")).is_err());
        let ign = Portal::new(Some("ign-lidar"), None).unwrap();
        assert!(ign.check_area(Some([2.0, 48.0, 2.5, 48.5])).is_ok());
        assert!(ign.check_area(Some([0.0, 40.0, 5.0, 45.0])).is_err(), "too large a box");
        assert!(ign.search_url("", Some([2.3, 48.8, 2.4, 48.9]), 5).contains("&BBOX=2.3,48.8,2.4,48.9,urn:ogc:def:crs:OGC:1.3:CRS84"));
        let hf = Portal::new(Some("huggingface"), None).unwrap();
        assert_eq!(hf.search_url("giswqs/geospatial", None, 5), "https://huggingface.co/api/datasets/giswqs/geospatial");
        assert_eq!(hf.search_url("land cover", None, 5), "https://huggingface.co/api/datasets?search=land%20cover&limit=5");
        assert_eq!(hf.search_url("../etc/passwd", None, 5), "https://huggingface.co/api/datasets?search=..%2Fetc%2Fpasswd&limit=5");
        assert!(hf.needs_query());
        let gl = Portal::new(Some("geolens"), Some("https://demo.example.com/api/search/datasets/?q=x")).unwrap();
        assert_eq!(gl.search_url("roads", None, 5), "https://demo.example.com/api/search/datasets/?q=roads&limit=5");
        assert!(Portal::new(Some("geolens"), Some("file:///etc")).is_err());
    }

    #[test]
    fn web_service_answers() {
        let agol = Portal::new(Some("arcgis-portal"), None).unwrap();
        let from = "https://www.arcgis.com/sharing/rest/search?q=parks";
        let a = br#"{"total":3,"results":[{"id":"abc123","type":"Feature Service","title":"Parks","owner":"city","snippet":"City parks","url":"https://services.arcgis.com/x/arcgis/rest/services/Parks/FeatureServer/0","extent":[[-122.5,47.5],[-122.2,47.7]]},{"id":"def456","type":"GeoJson","title":"Trails"},{"id":"bad id","type":"Map Service","url":"https://x.org/arcgis/rest/services/a/MapServer"},{"id":"ghi789","type":"Image Service","title":"Imagery","url":"https://x.org/arcgis/rest/services/i/ImageServer"}]}"#;
        let ds = agol.parse(from, a, None, "").unwrap();
        assert_eq!(ds.len(), 3, "the item with a bad id is left out");
        assert_eq!(ds[0].resources[0].using, Use::Service { kind: "arcgis", layer: Some("0".into()) });
        assert_eq!(ds[0].bbox, Some([-122.5, 47.5, -122.2, 47.7]));
        assert_eq!(ds[0].page.as_deref(), Some("https://www.arcgis.com/home/item.html?id=abc123"));
        assert_eq!(ds[1].resources[0].url, "https://www.arcgis.com/sharing/rest/content/items/def456/data");
        assert_eq!(ds[2].resources[0].url, "https://x.org/arcgis/rest/services/i/ImageServer");
        assert!(agol.parse(from, br#"{"error":{"code":400,"message":"Invalid query"}}"#, None, "").unwrap_err().contains("Invalid query"));

        let oam = Portal::new(Some("openaerialmap"), None).unwrap();
        let o = br#"{"meta":{"found":2},"results":[{"_id":"1","uuid":"https://oin.s3.amazonaws.com/a/b.tif","title":"Village","provider":"HOT","platform":"uav","gsd":0.05,"acquisition_end":"2026-09-20T06:00:00Z","bbox":[-77.3,4.85,-77.2,4.86]},{"_id":"2","uuid":"https://oin.s3.amazonaws.com/a/c.tif","title":"Coast","bbox":[1,2,3]}]}"#;
        let ds = oam.parse("https://api.openaerialmap.org/meta", o, Some([-80.0, 0.0, -70.0, 10.0]), "").unwrap();
        assert_eq!(ds.len(), 2, "an image whose box isn't one is kept");
        assert_eq!((ds[0].kind.as_str(), ds[0].date.as_str()), ("uav · 5 cm per pixel", "2026-09-20"));
        assert_eq!(oam.parse("https://api.openaerialmap.org/meta", o, None, "coast").unwrap()[0].title, "Coast");

        let dem = Portal::new(Some("usgs-dem"), None).unwrap();
        let d = br#"{"total":3,"items":[{"title":"USGS 1/3 Arc Second n47w123 20121001","downloadURL":"https://prd-tnm.s3.amazonaws.com/x/historical/n47w123/a.tif","publicationDate":"2012-10-01","boundingBox":{"minX":-123,"minY":46,"maxX":-122,"maxY":47}},{"title":"USGS 1/3 Arc Second n47w123 20240101","downloadURL":"https://prd-tnm.s3.amazonaws.com/x/current/n47w123/b.tif","publicationDate":"2024-01-01"},{"title":"USGS 1/3 Arc Second n47w123 20250813","downloadURL":"https://prd-tnm.s3.amazonaws.com/x/historical/n47w123/c.tif","publicationDate":"2025-08-13"}]}"#;
        let ds = dem.parse("https://tnmaccess.nationalmap.gov/api/v1/products", d, None, "").unwrap();
        assert_eq!(ds.len(), 1, "one per tile");
        assert!(ds[0].resources[0].url.ends_with("/current/n47w123/b.tif"), "the current edition");

        let ign = Portal::new(Some("ign-lidar"), None).unwrap();
        let i = br#"{"features":[{"id":"metadata.1","geometry":{"type":"Polygon","coordinates":[[[2.3,48.8],[2.31,48.8],[2.31,48.81],[2.3,48.8]]]},"properties":{"coordonnees_nw":"0650-6858","nombre_points":17455527,"date_fin_acquisition":"2023-03-01Z","url_npl":"https://data.geopf.fr/telechargement/download/x/LHD_FXX_0650_6858_PTS_LAMB93_IGN69.copc.laz"}},{"id":"metadata.2","properties":{"url_npl":"http://x.org/a.laz"}}]}"#;
        let ds = ign.parse("https://data.geopf.fr/wfs?x", i, None, "").unwrap();
        assert_eq!(ds.len(), 1);
        assert_eq!(
            (ds[0].title.as_str(), ds[0].kind.as_str(), ds[0].date.as_str()),
            ("LiDAR HD 0650-6858", "Point cloud · 17.5 million points", "2023-03-01")
        );

        let gl = Portal::new(Some("geolens"), None).unwrap();
        let g = br#"{"type":"FeatureCollection","features":[{"id":"8c26-1","properties":{"title":"Tracks","record_type":"vector_dataset","geometry_type":"MULTILINESTRING","feature_count":12}},{"id":"r1","properties":{"title":"DEM","record_type":"raster_dataset","band_count":1}},{"id":"../x","properties":{"title":"Bad"}}]}"#;
        let ds = gl.parse("https://datasets.geolibre.app/api/search/datasets/?q=", g, None, "").unwrap();
        assert_eq!(ds.len(), 1);
        assert_eq!(ds[0].resources[0].url, "https://datasets.geolibre.app/api/collections/8c26-1/items?limit=1000");

        let hf = Portal::new(Some("huggingface"), None).unwrap();
        let h = br#"[{"id":"giswqs/geospatial","downloads":10,"gated":false,"private":false,"tags":["license:mit","region:us"]},{"id":"a/gated","gated":"manual"},{"id":"noslash"}]"#;
        let ds = hf.parse("https://huggingface.co/api/datasets?search=x", h, None, "").unwrap();
        assert_eq!(ds.len(), 1);
        assert_eq!((ds[0].kind.as_str(), ds[0].description.as_str(), ds[0].resources.len()), ("Dataset · 10 downloads", "license: mit", 0));
        let listing = ds[0].listing.clone().unwrap();
        assert_eq!(Some(listing.clone()), hf.listing_of("giswqs/geospatial"));
        let files = br#"[{"type":"directory","path":"naip"},{"type":"file","path":"naip/a b.tif"},{"type":"file","path":"README.md"},{"type":"file","path":"x/../../etc.geojson"},{"type":"file","path":"z.pmtiles"}]"#;
        let r = hf.parse_listing(&listing, files).unwrap();
        assert_eq!(r.iter().map(|r| r.format.as_str()).collect::<Vec<_>>(), vec!["naip/a b.tif", "z.pmtiles"]);
        assert_eq!(r[0].url, "https://huggingface.co/datasets/giswqs/geospatial/resolve/main/naip/a%20b.tif");
        assert!(hf.parse_listing("https://huggingface.co/api/datasets/../tree/main", files).is_err());
        assert!(hf.parse_listing(&listing, b"{}").is_err());

        let odp = Portal::new(Some("ocean-data"), None).unwrap();
        let o = br#"{"features":[{"id":"cd48-1","bbox":[-64.6,17.7,-64.5,17.8],"properties":{"title":"Published - Coral sites","description":"Reef survey"}},{"id":"x2","bbox":[10,50,11,51],"properties":{"title":"Kelp"}}]}"#;
        let ds = odp.parse("https://api.hubocean.earth/api/stac/search?limit=1000", o, None, "coral").unwrap();
        assert_eq!((ds.len(), ds[0].title.as_str()), (1, "Coral sites"));
        assert_eq!(odp.parse("https://api.hubocean.earth/api/stac/search", o, Some([9.0, 49.0, 12.0, 52.0]), "").unwrap()[0].title, "Kelp");

        // Hostile answers: never a panic, an error or nothing.
        for k in ["arcgis-portal", "openaerialmap", "usgs-dem", "ign-lidar", "geolens", "huggingface", "ocean-data"] {
            let p = Portal::new(Some(k), None).unwrap();
            for junk in [
                &b""[..],
                b"null",
                b"[1,2]",
                b"{\"results\":[null,{\"id\":5}]}",
                b"{\"features\":[{\"properties\":7}]}",
                b"{\"items\":\"x\"}",
            ] {
                let _ = p.parse("https://x.org/", junk, Some([0.0, 0.0, 1.0, 1.0]), "a");
                let _ = p.parse_listing("https://huggingface.co/api/datasets/a/b/tree/main", junk);
            }
        }
    }

    #[test]
    fn arcgis_kinds_pages_and_web_maps() {
        let p = Portal::new(Some("earthdata-gis"), None).unwrap().with_category(Some("webmaps")).unwrap();
        let u = p.search_url("", None, 20);
        assert!(u.contains("%28type%3A%22Web%20Map%22%29") && !u.contains("Image%20Service"), "{u}");
        assert!(Portal::new(Some("geolens"), None).unwrap().with_category(Some("maps")).is_err());
        assert!(p.clone().with_category(Some("rasters")).is_err());
        assert_eq!(p.clone().with_category(Some("all")).unwrap().category, Some("webmaps"), "all keeps the portal as it was");
        // Pages.
        let (total, next) = p.paging(&u, br#"{"total":1966,"start":1,"num":20,"nextStart":21,"results":[]}"#);
        assert_eq!(total, Some(1966));
        assert!(next.as_deref().is_some_and(|n| n.ends_with("&start=21") && n.matches("start=").count() == 1), "{next:?}");
        assert_eq!(p.paging(&u, br#"{"total":3,"nextStart":-1}"#).1, None);
        let oam = Portal::new(Some("openaerialmap"), None).unwrap();
        let ou = oam.search_url("", Some([0.0, 0.0, 1.0, 1.0]), 5);
        let (t, n) = oam.paging(&ou, br#"{"meta":{"found":326,"page":1,"limit":50},"results":[]}"#);
        assert_eq!((t, n.as_deref().map(|n| n.contains("page=2"))), (Some(326), Some(true)));
        assert_eq!(oam.paging(&ou, br#"{"meta":{"found":40,"page":1,"limit":50}}"#).1, None);
        assert_eq!(oam.paging(&ou, b"junk"), (None, None));
        // A web map's item, its kind, date and picture, and its layers.
        let a = br#"{"results":[{"id":"w1","type":"Web Map","title":"Isaias","owner":"patrick_rea","modified":1792108800000,"thumbnail":"thumbnail/t.png"}]}"#;
        let ds = p.parse("https://gis.earthdata.nasa.gov/portal/sharing/rest/search", a, None, "").unwrap();
        let d = &ds[0];
        assert_eq!((d.kind.as_str(), d.date.as_str()), ("Web map", "2026-10-16"));
        assert_eq!(
            d.thumbnail.as_deref(),
            Some("https://gis.earthdata.nasa.gov/portal/sharing/rest/content/items/w1/info/thumbnail/t.png")
        );
        let listing = d.listing.clone().unwrap();
        assert_eq!(Some(listing.clone()), p.listing_of("w1"));
        let wm = br#"{"operationalLayers":[{"title":"Optical","url":"https://gis.earthdata.nasa.gov/portal/rest/services/a/ImageServer"},{"layerType":"GroupLayer","title":"Sept","layers":[{"title":"Flood","url":"https://gis.earthdata.nasa.gov/portal/rest/services/b/MapServer/2"}]},{"title":"Notes"},{"title":"Local","url":"http://169.254.169.254/rest/services/c/FeatureServer/0"}]}"#;
        let layers = p.parse_listing(&listing, wm).unwrap();
        assert_eq!(layers.iter().map(|r| r.format.as_str()).collect::<Vec<_>>(), vec!["Optical", "Sept › Flood"]);
        assert_eq!(layers[1].using, Use::Service { kind: "arcgis", layer: Some("2".into()) });
        assert!(p.parse_listing(&listing, b"{}").is_err());
        assert_eq!(day_of_ms(0.0), "1970-01-01");
        assert_eq!(day_of_ms(951_782_400_000.0), "2000-02-29");
        assert_eq!(day_of_ms(f64::NAN), "");
        assert_eq!(replace_param("https://x.org/s?a=1&start=1&b=2", "start", "41"), "https://x.org/s?a=1&b=2&start=41");
    }

    #[test]
    fn dynamical_datasets() {
        let p = Portal::new(Some("dynamical"), None).unwrap();
        assert!(!p.needs_query() && !p.needs_place());
        assert_eq!(p.search_url("", None, 20), "https://stac.dynamical.org/catalog.json");
        let c = br#"{"links":[{"rel":"root","href":"https://stac.dynamical.org/catalog.json"},{"rel":"child","href":"https://stac.dynamical.org/noaa-gfs-forecast/collection.json","title":"NOAA GFS forecast"},{"rel":"child","href":"https://stac.dynamical.org/noaa-gfs-forecast-virtual/collection.json","title":"virtual"},{"rel":"child","href":"https://stac.dynamical.org/ecmwf-ifs-ens-forecast-15-day-0-25-degree/collection.json"},{"rel":"child","href":"https://stac.dynamical.org/odd/collection.json"}]}"#;
        let ds = p.parse("https://stac.dynamical.org/catalog.json", c, None, "").unwrap();
        assert_eq!(
            ds.iter().map(|d| d.resources[0].url.as_str()).collect::<Vec<_>>(),
            vec![
                "https://data.dynamical.org/noaa/gfs/forecast/latest.zarr",
                "https://data.dynamical.org/ecmwf/ifs-ens/forecast-15-day-0-25-degree/latest.zarr"
            ]
        );
        assert_eq!((ds[0].title.as_str(), ds[0].organization.as_str(), ds[0].kind.as_str()), ("NOAA GFS forecast", "NOAA", "Forecast"));
        assert_eq!(ds[0].resources[0].using, Use::Zarr);
        assert_eq!(p.parse("https://stac.dynamical.org/catalog.json", c, None, "ecmwf").unwrap().len(), 1);
    }

    #[test]
    fn us_catalogs() {
        use crate::usgis::{FEDERAL, LOCAL, STATE, Search};
        for set in FEDERAL.iter().chain(STATE).chain(LOCAL) {
            assert!(!set.catalogs.is_empty(), "{}", set.name);
            for c in set.catalogs {
                assert!(c.url.starts_with("https://"), "{}", c.name);
                match c.search {
                    Search::ArcGis(org) => {
                        assert!(Portal::new(Some("arcgis-portal"), None).unwrap().with_filter(Some(org)).is_ok(), "{}", c.name)
                    }
                    Search::Socrata(d) => assert!(Portal::new(Some("socrata"), Some(d)).is_ok(), "{}", c.name),
                }
            }
        }
        assert_eq!(STATE.len(), 51);
    }

    #[test]
    fn boxes_meet() {
        assert!(overlaps([0.0, 0.0, 10.0, 10.0], [5.0, 5.0, 15.0, 15.0]));
        assert!(!overlaps([0.0, 0.0, 10.0, 10.0], [11.0, 0.0, 12.0, 1.0]));
        // Across the antimeridian.
        assert!(overlaps([170.0, -20.0, -170.0, -10.0], [178.0, -18.0, 179.0, -17.0]));
        assert!(overlaps([170.0, -20.0, -170.0, -10.0], [-179.0, -18.0, -178.0, -17.0]));
        assert!(!overlaps([170.0, -20.0, -170.0, -10.0], [0.0, -18.0, 1.0, -17.0]));
    }
}
