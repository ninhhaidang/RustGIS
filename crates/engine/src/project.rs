//! The project: maps, standalone tables and geoprocessing history, saved as JSON (`.rgis`).

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, OnceLock};

use rustgis_data::{Dataset, Feature, Field, GeometryType};
use rustgis_map::Map;
use serde::{Deserialize, Serialize};

use crate::GpRecord;

/// A map export cannot keep runtime-only download reservations, even in a larger project.
pub(crate) fn ensure_map_imports_complete(map: &Map) -> Result<(), String> {
    if map.layers.iter().any(|layer| layer.dataset.source.as_deref().is_some_and(|source| source.starts_with("geolibre-pending:")))
        || map.tiles.iter().any(|tile| tile.id.starts_with("geolibre-pending:"))
    {
        return Err("GeoLibre layers are still downloading; wait for them to finish before saving".into());
    }
    Ok(())
}

pub const EXTENSION: &str = "rgis";
/// The project extension earlier versions wrote; such files still open.
pub const LEGACY_EXTENSION: &str = "rgisx";

/// Whether a file name or path names a project file (`.rgis`, or the legacy `.rgisx`), ignoring case.
pub fn is_project_file(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    [EXTENSION, LEGACY_EXTENSION].iter().any(|x| n.strip_suffix(x).is_some_and(|s| s.ends_with('.')))
}

/// The extension of a project package (`project.package`): a project with its data, which opens
/// as a new, unsaved project.
pub const PACKAGE_EXTENSION: &str = "rgispkg";

/// Whether a file name or path opens as a project: a project file or a project package, ignoring
/// case. Unlike [`is_project_file`], this includes packages, which a project never saves to.
pub fn opens_as_project(name: &str) -> bool {
    is_project_file(name) || name.to_ascii_lowercase().strip_suffix(PACKAGE_EXTENSION).is_some_and(|s| s.ends_with('.'))
}

/// The command that opens a file given by its path, as the desktop app's command line and a file
/// dropped on its window do: native projects replace it, GeoLibre projects import a map, and
/// other files are added as data.
pub fn open_command(path: &str) -> &'static str {
    if rustgis_map::geolibre::is_project_file(path) {
        "project.importGeolibre"
    } else if opens_as_project(path) {
        "project.open"
    } else {
        "layer.addData"
    }
}
/// Format version written to files.
pub const FORMAT: u32 = 1;
/// Largest project file read (512 MiB).
pub const MAX_BYTES: u64 = 512 << 20;

/// The address a saved file keeps. Only web addresses can contain layer credentials. A tile
/// template (`pmtiles:…@archive`, `cog:…@archive`) is judged by its archive, and an address read
/// through a saved S3 connection by the address it wraps, the wrapper rebuilt around it (its
/// binding and endpoint are provenance, not secrets); a COG's look after either is kept.
fn saved_address(address: &str) -> Option<String> {
    use rustgis_data::s3_address;
    if let Some((head, archive)) = crate::sources::template_archive(address) {
        let (file, look) = split_look(archive);
        return saved_address(file).map(|saved| format!("{head}{saved}{look}"));
    }
    if s3_address::is_wrapper(address) {
        let (file, look) = split_look(address);
        // One that doesn't parse stays as it is (and blocked): it names no connection.
        let b = s3_address::parse(file).ok().flatten()?;
        let inner = saved_address(b.url)?;
        return s3_address::bind(b.binding, b.endpoint, &inner).ok().map(|saved| format!("{saved}{look}"));
    }
    let inner = address.strip_prefix(crate::render::rustgis_tiles::s3::ANONYMOUS).unwrap_or(address);
    let (protocol, _) = inner.split_once("://")?;
    if !["http", "https", "s3"].iter().any(|s| protocol.eq_ignore_ascii_case(s)) {
        return None;
    }
    let (scheme, mut rest) = address.split_once("://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    if let Some(at) = rest.get(..end).and_then(|authority| authority.rfind('@')) {
        rest = rest.get(at + 1..)?;
    }
    let rest = rest.split('#').next().unwrap_or(rest);
    let mut out = format!("{scheme}://{rest}");
    rustgis_data::source::credentials::redact_query(&mut out);
    (out != address).then_some(out)
}

/// A COG address without its look (`…#bands=…`), and the look as written.
fn split_look(address: &str) -> (&str, &str) {
    let file = crate::render::rustgis_tiles::cog::split(address).0;
    (file, address.get(file.len()..).unwrap_or(""))
}

/// Strips layer credentials, keeping originals in the visitor's order for the caller to restore.
/// Packaging calls this without registering the resulting text as a file this process wrote.
pub(crate) fn strip_addresses(addresses: Vec<&mut String>) -> Vec<Option<String>> {
    addresses.into_iter().map(|a| saved_address(a).map(|redacted| std::mem::replace(a, redacted))).collect()
}

struct Written {
    len: usize,
    hash: u64,
    was: Vec<Option<String>>,
}

const MAX_WRITTEN: usize = 32;

fn kept() -> MutexGuard<'static, Vec<Written>> {
    static KEPT: OnceLock<Mutex<Vec<Written>>> = OnceLock::new();
    KEPT.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

fn identity(text: &str) -> (usize, u64) {
    static KEY: OnceLock<RandomState> = OnceLock::new();
    (text.len(), KEY.get_or_init(RandomState::new).hash_one(text.as_bytes()))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Project {
    #[serde(default = "format")]
    pub format: u32,
    pub name: String,
    pub maps: Vec<Map>,
    pub active_map: usize,
    #[serde(default)]
    pub tables: Vec<Table>,
    /// The id the next standalone table gets: removing a table, or undoing its addition, doesn't
    /// free its id.
    #[serde(default)]
    next_table_id: u32,
    #[serde(default)]
    pub gp_history: Vec<GpRecord>,
    #[serde(default)]
    pub layouts: Vec<rustgis_map::layout::Layout>,
    /// Folder connections shown in the Browser.
    #[serde(default)]
    pub folders: Vec<String>,
    /// Saved symbology, applied to layers by name.
    #[serde(default)]
    pub styles: Vec<StyleItem>,
    /// Custom colour ramps, offered beside the built-in ones (renderers keep their own copy).
    #[serde(default)]
    pub ramps: Vec<rustgis_map::color::CustomRamp>,
    /// Step-by-step tasks: guided command sequences.
    #[serde(default)]
    pub tasks: Vec<Task>,
    /// Geoprocessing environment settings.
    #[serde(default)]
    pub gp_env: GpEnvironment,
    /// Models: chains of tools, each step able to use an earlier step's output.
    #[serde(default)]
    pub models: Vec<Model>,
    /// Ortho and reality mapping workspaces.
    #[serde(default)]
    pub workspaces: Vec<crate::cmd::imagery::Workspace>,
    /// Maps move together (View › Link Views).
    #[serde(default)]
    pub link_views: bool,
    /// The project's thumbnail (base64 PNG).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<String>,
    /// Workflow Manager jobs.
    #[serde(default)]
    pub jobs: Vec<Job>,
    /// Offline areas: snapshots of a map's data around an extent, synced back by feature id.
    #[serde(default)]
    pub offline_areas: Vec<OfflineArea>,
    /// Reports on layers' records.
    #[serde(default)]
    pub reports: Vec<rustgis_map::report::Report>,
    /// Presentations: pages of maps, text, images and videos.
    #[serde(default)]
    pub presentations: Vec<rustgis_map::presentation::Presentation>,
    /// Notebooks of text and command cells.
    #[serde(default)]
    pub notebooks: Vec<crate::cmd::notebook::Notebook>,
    /// Trained deep learning models.
    #[serde(default)]
    pub dl_models: Vec<crate::cmd::deeplearning::DlModel>,
    /// Shares made (Publish › Share As › Jobs) and the folders they go to.
    #[serde(default)]
    pub shared: Vec<crate::cmd::sharing::SharedItem>,
    #[serde(default)]
    pub data_stores: Vec<crate::cmd::sharing::DataStore>,
}

/// A standalone table: a dataset without geometry, under an id that stays with it. Names can
/// repeat and positions change, so the id is what says which table is meant.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(from = "TableFile")]
pub struct Table {
    /// Unique among the project's tables, and never 0 once the project is loaded.
    pub id: u32,
    #[serde(flatten)]
    pub dataset: Dataset,
}

/// A table as a file holds it: the dataset's fields beside the id (absent from files written
/// before tables had ids), with what its read lost, as a layer keeps it: the coordinate system,
/// geometry lost to it and Z or M values dropped (absent from files written before tables kept
/// them). Read field by field, since `flatten` would buffer every row first.
#[derive(Deserialize)]
struct TableFile {
    #[serde(default)]
    id: u32,
    name: String,
    geometry_type: GeometryType,
    fields: Vec<Field>,
    features: Vec<Feature>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    crs: Option<rustgis_data::raster::Crs>,
    #[serde(default)]
    crs_incomplete: bool,
    #[serde(default)]
    zm_dropped: bool,
}

impl From<TableFile> for Table {
    fn from(t: TableFile) -> Self {
        let TableFile { id, name, geometry_type, fields, features, source, crs, crs_incomplete, zm_dropped } = t;
        Table {
            id,
            dataset: Dataset {
                name,
                geometry_type,
                fields,
                features: features.into(),
                source,
                crs,
                crs_incomplete,
                zm_dropped,
                windowed: false,
            },
        }
    }
}

/// The table a name or an id written as text stands for, as its position among `tables`: the
/// first of that name (names can repeat), else the first of that name ignoring case, else the
/// one of that id. The one rule commands and tools find a table by.
pub fn find_table(tables: &[Table], text: &str) -> Option<usize> {
    tables
        .iter()
        .position(|t| t.name == text)
        .or_else(|| tables.iter().position(|t| t.name.eq_ignore_ascii_case(text)))
        .or_else(|| text.trim().parse::<u32>().ok().and_then(|id| tables.iter().position(|t| t.id == id)))
}

impl std::ops::Deref for Table {
    type Target = Dataset;
    fn deref(&self) -> &Dataset {
        &self.dataset
    }
}

impl std::ops::DerefMut for Table {
    fn deref_mut(&mut self) -> &mut Dataset {
        &mut self.dataset
    }
}

/// A unit of work tracked in the Workflow Manager.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: u32,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// "new", "in progress", "on hold" or "done".
    pub status: String,
    #[serde(default)]
    pub assigned: String,
    /// The area the job covers (longitude/latitude), if any.
    #[serde(default)]
    pub extent: Option<rustgis_geom::Extent>,
    /// Seconds since 1970.
    pub created: f64,
    #[serde(default)]
    pub updated: f64,
}

/// A map's data clipped to an extent, to take into the field.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OfflineArea {
    pub name: String,
    /// The map it came from (by name).
    pub map: String,
    pub extent: rustgis_geom::Extent,
    pub created: f64,
    #[serde(default)]
    pub synced: f64,
    /// The snapshot: the map's feature layers holding the features meeting the extent.
    pub data: Map,
}

/// Settings every geoprocessing run honours.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GpEnvironment {
    /// Tools only process features meeting this longitude/latitude extent (when no features are
    /// selected).
    #[serde(default)]
    pub extent: Option<rustgis_geom::Extent>,
    /// The cell size (degrees) of rasters tools create, when they don't say.
    #[serde(default)]
    pub cell_size: Option<f64>,
}

/// A model: geoprocessing steps run in order. A parameter value `%n` stands for the output layer
/// of step n (1-based). A model drawn in ModelBuilder keeps its `graph` too, which is what runs;
/// `steps` then holds the same tools in run order, for older versions and web tools.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Model {
    pub name: String,
    #[serde(default)]
    pub steps: Vec<ModelStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph: Option<ModelGraph>,
}

/// A model as ModelBuilder draws it: input, tool and output nodes joined by edges from an output
/// port to an input port (see `crate::model`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelGraph {
    #[serde(default)]
    pub nodes: Vec<ModelNode>,
    #[serde(default)]
    pub edges: Vec<ModelEdge>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelNodeKind {
    /// A layer of the map the model reads.
    Input,
    /// A geoprocessing tool.
    #[default]
    Tool,
    /// A result the model keeps, under a name.
    Output,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelNode {
    pub id: String,
    pub kind: ModelNodeKind,
    /// Where the node sits on the canvas (its top-left corner).
    #[serde(default)]
    pub x: f32,
    #[serde(default)]
    pub y: f32,
    /// Input nodes: the layer (or standalone table) by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
    /// Tool nodes: the tool id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// Tool nodes: parameter values not supplied by an edge.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub params: serde_json::Value,
    /// Output nodes: the name of the layer the result becomes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// An edge from node `from`'s output port to node `to`'s input port (a tool parameter's name).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelEdge {
    pub from: String,
    #[serde(rename = "fromPort", default)]
    pub from_port: String,
    pub to: String,
    #[serde(rename = "toPort", default)]
    pub to_port: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelStep {
    pub tool: String,
    pub params: serde_json::Value,
}

/// A saved style: a layer's symbology and labels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StyleItem {
    pub name: String,
    pub geometry: rustgis_data::GeometryType,
    pub renderer: rustgis_map::Renderer,
    pub labels: rustgis_map::Labeling,
    #[serde(default)]
    pub transparency: f32,
}

/// A task: steps that each run a command, in order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub steps: Vec<TaskStep>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskStep {
    pub title: String,
    #[serde(default)]
    pub instructions: String,
    pub command: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

fn format() -> u32 {
    FORMAT
}

impl Default for Project {
    fn default() -> Self {
        Project::new("Untitled")
    }
}

/// Resolve linked files and explicitly relative provenance against their containing document.
pub(crate) fn resolve_file_paths<'a>(dir: &str, paths: impl IntoIterator<Item = (&'a mut String, bool)>) {
    for (p, record) in paths {
        if (!record || ["./", "../", ".\\", "..\\"].iter().any(|prefix| p.starts_with(prefix)))
            && rustgis_map::paths::is_project_relative(p)
            && let Some(full) = rustgis_map::paths::resolve(dir, p)
        {
            *p = full;
        }
    }
}

#[derive(Deserialize)]
struct ProjectFormat {
    #[serde(default)]
    format: Option<u32>,
}

impl Project {
    fn file_paths_mut(&mut self) -> Vec<(&mut String, bool)> {
        let mut paths = Vec::new();
        for map in &mut self.maps {
            paths.extend(map.file_paths_mut());
        }
        for area in &mut self.offline_areas {
            paths.extend(area.data.file_paths_mut());
        }
        for table in &mut self.tables {
            if let Some(p) = &mut table.dataset.source
                && rustgis_map::paths::is_local(p)
            {
                paths.push((p, true));
            }
        }
        paths
    }

    fn resolve_paths(&mut self, dir: &str) {
        resolve_file_paths(dir, self.file_paths_mut());
    }

    pub fn new(name: impl Into<String>) -> Self {
        Project {
            format: FORMAT,
            name: name.into(),
            maps: vec![Map::new("Map")],
            active_map: 0,
            tables: Vec::new(),
            next_table_id: 1,
            gp_history: Vec::new(),
            layouts: Vec::new(),
            folders: Vec::new(),
            styles: Vec::new(),
            ramps: Vec::new(),
            tasks: Vec::new(),
            gp_env: GpEnvironment::default(),
            models: Vec::new(),
            workspaces: Vec::new(),
            link_views: false,
            thumbnail: None,
            jobs: Vec::new(),
            offline_areas: Vec::new(),
            reports: Vec::new(),
            presentations: Vec::new(),
            notebooks: Vec::new(),
            dl_models: Vec::new(),
            shared: Vec::new(),
            data_stores: Vec::new(),
        }
    }

    /// Every map and offline snapshot's data addresses, followed by standalone tables' sources.
    pub fn addresses_mut(&mut self) -> Vec<&mut String> {
        let mut addresses = Vec::new();
        for map in &mut self.maps {
            addresses.extend(map.addresses_mut());
        }
        for area in &mut self.offline_areas {
            addresses.extend(area.data.addresses_mut());
        }
        for table in &mut self.tables {
            if let Some(source) = &mut table.dataset.source {
                addresses.push(source);
            }
        }
        addresses
    }

    /// The standalone table of an id.
    pub fn table(&self, id: u32) -> Option<&Table> {
        self.tables.iter().find(|t| t.id == id)
    }

    /// An id no table has, for the next one.
    fn new_table_id(&mut self) -> u32 {
        let mut id = self.next_table_id.max(1);
        while self.tables.iter().any(|t| t.id == id) {
            id = id.wrapping_add(1).max(1);
        }
        self.next_table_id = id.wrapping_add(1).max(1);
        id
    }

    /// Keeps the ids given in another state of the project from being given again in this one,
    /// which takes its place (undo, redo): the counter never goes back.
    pub(crate) fn keep_table_ids(&mut self, other: &Project) {
        self.next_table_id = self.next_table_id.max(other.next_table_id);
    }

    /// Adds a standalone table, and returns the id it got.
    pub fn add_table(&mut self, dataset: Dataset) -> u32 {
        let id = self.new_table_id();
        self.tables.push(Table { id, dataset });
        id
    }

    /// Adds a tool's output table in place of the tables of its name, as running the tool again
    /// replaces its result: the first of them keeps its id, so a view open on it stays on it.
    pub fn put_table(&mut self, dataset: Dataset) -> u32 {
        let id = match self.tables.iter().find(|t| t.name == dataset.name) {
            Some(t) => t.id,
            None => self.new_table_id(),
        };
        self.tables.retain(|t| t.name != dataset.name);
        self.tables.push(Table { id, dataset });
        id
    }

    /// Repairs invariants after loading (at least one map; a valid active index).
    pub fn normalize(&mut self) {
        if self.maps.is_empty() {
            self.maps.push(Map::new("Map"));
        }
        if self.active_map >= self.maps.len() {
            self.active_map = 0;
        }
        // Tables from before they had ids (0), or with an id repeated by a hand edit, get new
        // ones; the others keep theirs, which commands and views may hold. The new ones come
        // after every id given so far, so none is that of a table removed earlier.
        let mut ids = std::collections::HashSet::new();
        let stale: Vec<usize> = self.tables.iter().enumerate().filter(|(_, t)| t.id == 0 || !ids.insert(t.id)).map(|(i, _)| i).collect();
        let top = self.tables.iter().map(|t| t.id).max().unwrap_or(0);
        let mut next = self.next_table_id.max(top.wrapping_add(1)).max(1);
        for i in stale {
            while !ids.insert(next) {
                next = next.wrapping_add(1).max(1);
            }
            if let Some(t) = self.tables.get_mut(i) {
                t.id = next;
            }
        }
        let top = self.tables.iter().map(|t| t.id).max().unwrap_or(0);
        self.next_table_id = self.next_table_id.max(top.wrapping_add(1)).max(1);
        self.layouts.truncate(200);
        for l in &mut self.layouts {
            l.clamp();
        }
        // Files skip the commands' limits, and notebooks and presentations are found (and
        // notebooks trusted) by name, so names are made unique.
        for r in &mut self.reports {
            r.clamp();
        }
        self.reports.truncate(MAX_ITEMS);
        self.presentations.truncate(MAX_ITEMS);
        self.notebooks.truncate(MAX_ITEMS);
        self.dl_models.truncate(MAX_ITEMS);
        self.dl_models.retain(|m| m.model.validate().is_ok());
        self.shared.truncate(MAX_ITEMS);
        self.data_stores.truncate(MAX_ITEMS);
        for p in &mut self.presentations {
            p.clamp();
        }
        for nb in &mut self.notebooks {
            nb.clamp();
        }
        unique_names(self.presentations.iter_mut().map(|p| &mut p.name));
        unique_names(self.notebooks.iter_mut().map(|nb| &mut nb.name));
        unique_names(self.dl_models.iter_mut().map(|m| &mut m.name));
        // Custom ramps are found by name, before them the built-in schemes: a hand edit's repeated
        // or built-in name gets a suffix, so every ramp stays reachable.
        self.ramps.truncate(MAX_ITEMS);
        for r in &mut self.ramps {
            if rustgis_map::Ramp::from_name(r.name()).is_some() {
                r.name_mut().push_str(" (custom)");
            }
        }
        unique_names(self.ramps.iter_mut().map(|r| r.name_mut()));
        for m in &mut self.maps {
            // Groups from project files must stay shallow, bounded and unambiguous. Children
            // refer to existing ordinary layers; no layer belongs to two groups.
            m.service_groups.truncate(2000);
            let layer_ids: std::collections::HashSet<u32> = m.layers.iter().map(|l| l.id).collect();
            let mut ids = layer_ids.clone();
            let mut children = std::collections::HashSet::new();
            m.service_groups.retain_mut(|g| {
                if g.id == 0 || g.url.len() > 8192 || !ids.insert(g.id) {
                    return false;
                }
                g.name = g.name.chars().take(200).collect();
                g.children.truncate(2000);
                g.children.retain(|c| layer_ids.contains(&c.layer) && children.insert(c.layer));
                true
            });
            // Layer groups likewise: ids of their own, children that exist and are in no other
            // group, numbers in range, parents that exist without a loop or too many levels;
            // each group's layers kept together in the drawing order.
            m.groups.truncate(rustgis_map::MAX_GROUPS);
            m.groups.retain_mut(|g| {
                if g.id == 0 || !ids.insert(g.id) {
                    return false;
                }
                g.name = g.name.chars().take(200).collect();
                g.transparency = if g.transparency.is_finite() { g.transparency.clamp(0.0, 100.0) } else { 0.0 };
                g.min_scale = g.min_scale.filter(|v| v.is_finite() && *v > 0.0);
                g.max_scale = g.max_scale.filter(|v| v.is_finite() && *v > 0.0);
                g.children.retain(|c| layer_ids.contains(c) && children.insert(*c));
                true
            });
            m.tidy_groups();
            // A projection this version can't use (a hand edit, or a newer file) falls back to
            // Web Mercator rather than being named while another is drawn.
            if let rustgis_map::Projection::Custom(code) = &m.projection
                && let Err(e) = rustgis_map::projection::Projector::new(code)
            {
                log::warn!("{}: {e}; using Web Mercator", m.name);
                m.projection = rustgis_map::Projection::WebMercator;
                // The camera is in the lost projection's units: start from the whole world.
                m.camera = rustgis_map::Camera::world();
            }
            m.camera.clamp();
            // The time slider's window and dated tile addresses, before the addresses are checked.
            m.apply_time();
            for t in &mut m.tiles {
                if let Some(f) = t.archive_file().filter(|f| rustgis_map::paths::is_project_relative(f)) {
                    t.error = Some(rustgis_map::paths::unknown_folder(f));
                }
            }
            // Tile layers with an address that can't be fetched (a hand edit) are dropped.
            m.tiles.retain_mut(|t| t.repair().map_err(|e| log::warn!("{}: {e}", m.name)).is_ok());
            m.tiles.truncate(rustgis_map::tiles::MAX_TILE_LAYERS);
            #[cfg(not(target_arch = "wasm32"))]
            for t in &mut m.tiles {
                if t.error.is_none()
                    && let Some(f) = t.archive_file()
                    && !Path::new(f.strip_prefix("file://").unwrap_or(f)).is_file()
                {
                    t.error = Some(format!("{f}: the file isn't there"));
                }
            }
            for l in &mut m.layers {
                if let Some(z) = l.raster.as_mut().and_then(|r| r.time_zarr.as_mut()) {
                    z.repair();
                }
                let unresolved = l
                    .file_paths_mut()
                    .into_iter()
                    .find(|(p, record)| !record && rustgis_map::paths::is_project_relative(p))
                    .map(|(p, _)| p.clone());
                if let Some(p) = &unresolved {
                    let error = rustgis_map::paths::unknown_folder(p);
                    if let Some(rl) = &mut l.raster {
                        rl.error = Some(error.clone());
                        rl.data = None;
                    }
                    if let Some(src) = &mut l.source {
                        src.error = Some(error);
                    }
                }
                if let Some(rl) = &mut l.raster
                    && rl.data.is_none()
                    && !rl.path.as_deref().is_some_and(crate::s3_connections::is_s3_backed)
                    && unresolved.is_none()
                    && let Err(e) = rl.reload()
                {
                    log::warn!("{}: {e}", l.name);
                }
                // A streamed layer's features are read again from its source. One read from an S3
                // bucket waits for [`crate::s3_connections::resolve_project_s3`], which opens it
                // with the saved connection that is to read it (or, with none to choose, as
                // before): reading a project fetches nothing with another session's keys.
                if let Some(src) = &mut l.source {
                    l.dataset.windowed = true;
                    if !src.is_open()
                        && src.error.is_none()
                        && !src.path.as_deref().is_some_and(crate::s3_connections::is_s3_backed)
                        && let Err(e) = src.open()
                    {
                        log::warn!("{}: {e}", l.name);
                    }
                }
                l.normalize_selection();
                let fields = l.dataset.fields.len();
                for f in &mut l.dataset.features {
                    f.attrs.resize(fields, rustgis_data::Value::Null);
                }
                l.touch_geometry();
            }
        }
    }

    pub fn map(&self) -> &Map {
        // `normalize` guarantees a map; fall back to the first for safety.
        self.maps.get(self.active_map).or_else(|| self.maps.first()).unwrap_or_else(|| empty_map())
    }

    pub fn map_mut(&mut self) -> &mut Map {
        if self.maps.is_empty() {
            self.maps.push(Map::new("Map"));
        }
        let i = if self.active_map < self.maps.len() { self.active_map } else { 0 };
        self.active_map = i;
        &mut self.maps[i]
    }

    /// A consumed download cannot run again when an older undo snapshot is restored.
    pub(crate) fn discard_import_reservation(&mut self, token: &str) {
        for map in &mut self.maps {
            let removed: std::collections::HashSet<_> =
                map.layers.iter().filter(|layer| layer.dataset.source.as_deref() == Some(token)).map(|layer| layer.id).collect();
            map.layers.retain(|layer| !removed.contains(&layer.id));
            map.tiles.retain(|tile| tile.id != token);
            for group in &mut map.groups {
                group.children.retain(|id| !removed.contains(id));
            }
        }
    }

    /// Runtime download reservations cannot be reopened; refuse snapshots until they finish.
    pub(crate) fn ensure_imports_complete(&self) -> Result<(), String> {
        for map in self.maps.iter().chain(self.offline_areas.iter().map(|area| &area.data)) {
            ensure_map_imports_complete(map)?;
        }
        Ok(())
    }

    /// The file keeps no passwords, keys, tokens or signatures in layer addresses. The open project
    /// keeps them. This process remembers the 32 most recent distinct saved snapshots with stripped
    /// addresses: opening identical plain project text restores them until that snapshot is evicted
    /// or RustGIS closes. Packages and imports never restore them.
    pub fn saved_text(&mut self, dir: Option<&Path>) -> Result<(String, usize), String> {
        self.ensure_imports_complete()?;
        let was = strip_addresses(self.addresses_mut());
        let n = was.iter().flatten().count();
        let originals: Vec<Option<String>> = if let Some(d) = dir.and_then(Path::to_str) {
            self.file_paths_mut()
                .into_iter()
                .map(|(p, _)| {
                    let relative = rustgis_map::paths::is_absolute_local(p)
                        .then(|| rustgis_map::paths::relative_to(d, p))
                        .flatten()
                        // Keep the traversal stable during restoration, even for an unusually deep
                        // destination whose relative spelling exceeds the local-path size bound.
                        .filter(|r| rustgis_map::paths::is_local(r));
                    relative.map(|r| std::mem::replace(p, r))
                })
                .collect()
        } else {
            Vec::new()
        };
        let text = serde_json::to_string(self).map_err(|e| e.to_string());
        if !originals.is_empty() {
            for ((p, _), original) in self.file_paths_mut().into_iter().zip(originals) {
                if let Some(original) = original {
                    *p = original;
                }
            }
        }
        // Serialization can fail: put the live addresses back before handling its result.
        for (address, original) in self.addresses_mut().into_iter().zip(&was) {
            if let Some(original) = original {
                *address = original.clone();
            }
        }
        if let Ok(text) = &text
            && n > 0
        {
            let (len, hash) = identity(text);
            let mut kept = kept();
            kept.retain(|entry| (entry.len, entry.hash) != (len, hash));
            if kept.len() >= MAX_WRITTEN {
                kept.remove(0);
            }
            kept.push(Written { len, hash, was });
        }
        text.map(|text| (text, n))
    }

    /// The project as an autosaved recovery copy keeps it: its text without passwords, keys, tokens
    /// or signatures in layer addresses, from a copy of the project (autosave writes it off the
    /// interface thread). Unlike [`Self::saved_text`], local file paths stay as the open project
    /// holds them, absolute: the copy lives in the app's recovery folder and is opened from there,
    /// not from the project's folder. Nor is it remembered as a file this process wrote, so
    /// frequent copies can't push the user's own saves out of the snapshots that restore secrets.
    /// Runtime import reservations are omitted while completed work remains recoverable.
    /// Text too large to open again is refused.
    pub fn recovery_text(mut self) -> Result<String, String> {
        // A recovery copy preserves completed work without persisting runtime-only reservations.
        for map in self.maps.iter_mut().chain(self.offline_areas.iter_mut().map(|area| &mut area.data)) {
            let pending: std::collections::HashSet<_> = map
                .layers
                .iter()
                .filter(|layer| layer.dataset.source.as_deref().is_some_and(|source| source.starts_with("geolibre-pending:")))
                .map(|layer| layer.id)
                .collect();
            map.layers.retain(|layer| !pending.contains(&layer.id));
            for group in &mut map.groups {
                group.children.retain(|id| !pending.contains(id));
            }
            map.tiles.retain(|tile| !tile.id.starts_with("geolibre-pending:"));
        }
        strip_addresses(self.addresses_mut());
        let text = serde_json::to_string(&self).map_err(|e| e.to_string())?;
        if text.len() as u64 > MAX_BYTES {
            return Err("the project is too large to keep a recovery copy".into());
        }
        Ok(text)
    }

    fn restore_addresses(&mut self, text: &str) {
        let id = identity(text);
        let kept = kept();
        let Some(entry) = kept.iter().find(|entry| (entry.len, entry.hash) == id) else { return };
        for (address, original) in self.addresses_mut().into_iter().zip(&entry.was) {
            if let Some(full) = original
                && saved_address(full).as_deref() == Some(address.as_str())
            {
                *address = full.clone();
            }
        }
    }

    pub fn save(&mut self, path: &Path) -> Result<usize, String> {
        let dir = std::path::absolute(path).ok().and_then(|p| p.parent().map(Path::to_path_buf));
        let (text, stripped) = self.saved_text(dir.as_deref())?;
        rustgis_data::io::write_atomic(path, text.as_bytes()).map_err(|e| format!("could not save {}: {e}", path.display()))?;
        Ok(stripped)
    }

    /// Reads a project from its JSON text (a browser upload).
    pub fn from_json(text: &str) -> Result<Project, String> {
        Self::read_json(text, true, None)
    }

    /// Reads imported JSON (including package contents) without restoring a saved file's secrets.
    pub(crate) fn from_import_json(text: &str, dir: Option<&Path>) -> Result<Project, String> {
        Self::read_json(text, false, dir)
    }

    fn read_json(text: &str, restore: bool, dir: Option<&Path>) -> Result<Project, String> {
        if text.len() as u64 > MAX_BYTES {
            return Err("the project file is too large".into());
        }
        if let Ok(pf) = serde_json::from_str::<ProjectFormat>(text)
            && let Some(format) = pf.format
            && format > FORMAT
        {
            return Err(format!("this project was saved by a newer RustGIS (format {format}). Update RustGIS to open it."));
        }
        let mut p: Project = serde_json::from_str(text).map_err(|e| {
            let msg = e.to_string();
            if msg.contains("unknown variant") {
                format!("this project may have been saved by a newer RustGIS: {msg}. Update RustGIS to open it.")
            } else {
                format!("not a RustGIS project: {msg}")
            }
        })?;
        p.ensure_imports_complete().map_err(|_| "a project file cannot contain runtime GeoLibre download reservations".to_string())?;
        if p.format > FORMAT {
            return Err(format!("this project was saved by a newer RustGIS (format {}). Update RustGIS to open it.", p.format));
        }
        if restore {
            p.restore_addresses(text);
        }
        // A binding is this computer's runtime token for a login: one in a file is no authority
        // to use any login here.
        crate::s3_connections::clear_bindings(p.addresses_mut());
        if let Some(d) = dir.and_then(Path::to_str) {
            p.resolve_paths(d);
        }
        p.normalize();
        Ok(p)
    }

    pub fn load(path: &Path) -> Result<Project, String> {
        let len = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?.len();
        if len > MAX_BYTES {
            return Err("the project file is too large".into());
        }
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let dir = std::path::absolute(path).ok().and_then(|p| p.parent().map(Path::to_path_buf));
        Self::read_json(&text, true, dir.as_deref())
    }
}

fn empty_map() -> &'static Map {
    static M: std::sync::OnceLock<Map> = std::sync::OnceLock::new();
    M.get_or_init(|| Map::new("Map"))
}

/// Most reports, presentations and notebooks a project file may hold.
pub const MAX_ITEMS: usize = 1000;

/// Renames names repeated (ignoring case) after their first use: "Tour", "Tour 2", …. Each base
/// name remembers the last number it gave, so many repeats cost no more than a few each.
fn unique_names<'a>(names: impl Iterator<Item = &'a mut String>) {
    let mut seen = std::collections::HashSet::new();
    let mut next: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for name in names {
        if seen.insert(name.to_ascii_lowercase()) {
            continue;
        }
        let base = name.clone();
        let k = next.entry(base.to_ascii_lowercase()).or_insert(1);
        loop {
            *k += 1;
            *name = format!("{base} {k}");
            if seen.insert(name.to_ascii_lowercase()) {
                break;
            }
        }
    }
}
