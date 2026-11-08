//! The project: maps, standalone tables and geoprocessing history, saved as JSON (`.rgis`).

use std::path::Path;

use rustgis_data::{Dataset, Feature, Field, GeometryType};
use rustgis_map::Map;
use serde::{Deserialize, Serialize};

use crate::GpRecord;

#[path = "project_label_losses.rs"]
pub(crate) mod label_losses;

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
/// Saved-document compatibility version, independent of the RustGIS release version.
/// Advance it when an older reader would lose meaningful saved information on resaving.
pub const FORMAT: u32 = 1;
/// Largest project file read or saved in the browser, or on a computer whose memory isn't known
/// (512 MiB): the floor of [`max_bytes`].
pub const BYTES_FLOOR: u64 = 512 << 20;

/// The most [`max_bytes`] grows to, however much memory the computer has (4 GiB).
pub const BYTES_CEILING: u64 = 4 << 30;

/// The memory an open project takes for each byte of its file, as measured opening projects of
/// GeoJSON layers from the command line:
/// - polygons of 40 vertices and 8 attributes: about 2 (337 MiB took 731 MiB; 674 MiB, 1.41 GiB;
///   the app took 576 MiB more than empty to open and draw the first);
/// - points with 12 short text attributes: about 4.4 (101 MiB took 449 MiB);
/// - points with 30 one-letter attributes, about the most a byte of file costs: about 5.2
///   (87 MiB took 453 MiB).
///
/// Planned at 5: at the limit, even the most measured takes little more than a quarter of the
/// memory, and a project of the size in #1615 (659 MiB) opens on a computer sold as 16 GB, which
/// reports a little less than 16 GiB.
const MEMORY_PER_BYTE: u64 = 5;

/// Largest project file read or saved on this computer (#1615): on the desktop as much as a
/// quarter of its memory holds open at [`MEMORY_PER_BYTE`] a byte, from [`BYTES_FLOOR`] up to
/// [`BYTES_CEILING`], as the other memory limits follow it (#1291 step 2); [`BYTES_FLOOR`] in the
/// browser. Save and open use the same limit, so a project saved here opens here; one saved on a
/// computer with more memory may need one like it.
pub fn max_bytes() -> u64 {
    #[cfg(test)]
    if let Some(max) = TEST_MAX.with(std::cell::Cell::get) {
        return max;
    }
    max_bytes_for(rustgis_data::memory::physical())
}

/// [`max_bytes`] on a computer with `memory` bytes (`None`: not known). The formula is
/// [`rustgis_data::memory::scaled`]'s, worked out here in `u64`, as that one counts in `usize`,
/// which doesn't hold 4 GiB on a 32-bit build.
fn max_bytes_for(memory: Option<u64>) -> u64 {
    memory.map_or(BYTES_FLOOR, |m| (m / rustgis_data::memory::SHARE / MEMORY_PER_BYTE).clamp(BYTES_FLOOR, ceiling()))
}

/// The most [`max_bytes`] grows to in this build: [`BYTES_CEILING`] on a 64-bit build. A 32-bit
/// build stays at [`BYTES_FLOOR`] whatever the memory: an open project takes 2 to 5 times its file
/// in address space, which 4 GiB doesn't hold for a larger one.
fn ceiling() -> u64 {
    if usize::BITS >= 64 { BYTES_CEILING } else { BYTES_FLOOR }
}

/// The memory a 64-bit build needs for [`max_bytes`] to reach `bytes`: the inverse of
/// [`max_bytes_for`] above the floor.
fn memory_for(bytes: u64) -> u64 {
    bytes.saturating_mul(rustgis_data::memory::SHARE).saturating_mul(MEMORY_PER_BYTE)
}

/// `value` as JSON text, or `None` as soon as it passes `max` bytes: the text of a project too
/// large to keep is never built whole, only up to the limit.
pub(crate) fn json_within<T: Serialize + ?Sized>(value: &T, max: u64) -> Result<Option<String>, String> {
    struct Capped {
        buf: Vec<u8>,
        max: usize,
        over: bool,
    }
    impl std::io::Write for Capped {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            if self.buf.len().saturating_add(b.len()) > self.max {
                self.over = true;
                return Err(std::io::Error::other("past the limit"));
            }
            // A machine short of memory gets an error, not an abort.
            self.buf.try_reserve(b.len()).map_err(|_| std::io::Error::other("out of memory"))?;
            self.buf.extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut w = Capped { buf: Vec::new(), max: usize::try_from(max).unwrap_or(usize::MAX), over: false };
    match serde_json::to_writer(&mut w, value) {
        Ok(()) => String::from_utf8(w.buf).map(Some).map_err(|e| e.to_string()),
        Err(_) if w.over => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// The limit as a refusal says it: in MiB and, where it follows the computer's memory, that it
/// does, so a project refused here but opened elsewhere is understood.
pub fn limit_said(max: u64) -> String {
    // A 32-bit build keeps the floor whatever the memory: its limit doesn't follow it.
    if rustgis_data::memory::physical().is_some() && ceiling() > BYTES_FLOOR {
        format!("{} MiB on this computer, a limit that follows its memory", max >> 20)
    } else {
        format!("{} MiB", max >> 20)
    }
}

#[cfg(test)]
thread_local! {
    /// A smaller [`max_bytes`] for the tests on this thread ([`with_max_bytes`]), so a test of
    /// the limit doesn't build a project of gigabytes.
    static TEST_MAX: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// Runs `f` with [`max_bytes`] at `max` on the calling thread only (work `f` hands to another
/// thread sees the real limit); the limit is restored even if `f` panics.
#[cfg(test)]
pub(crate) fn with_max_bytes<T>(max: u64, f: impl FnOnce() -> T) -> T {
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            TEST_MAX.with(|m| m.set(None));
        }
    }
    TEST_MAX.with(|m| m.set(Some(max)));
    let _restore = Restore;
    f()
}

/// Runtime capabilities never survive a project boundary, including an in-process save restore.
/// `None` for an address that holds none, and for a wrapper that doesn't parse (kept as it is,
/// never emptied: its layer then fails to read, saying why).
pub(crate) fn without_login_binding(address: &str) -> Option<String> {
    // A shallow recorded string can contain arbitrarily many template prefixes. Walk its
    // suffixes without recursion, then rebuild the unchanged prefix only once.
    let mut inner = address;
    while let Some((_, archive)) = crate::sources::template_archive(inner) {
        inner = archive;
    }
    let bound = rustgis_data::login_address::parse(inner).ok().flatten()?;
    let prefix = address.get(..address.len() - inner.len())?;
    Some(format!("{prefix}{}", bound.url))
}

/// A recorded value may contain several addresses inside ordinary text. Remove just each
/// capability header, not URL queries or words that happen to match an active credential.
pub(crate) fn without_recorded_login_bindings(text: &str) -> Option<String> {
    let whole = without_login_binding(text);
    let text = whole.as_deref().unwrap_or(text);
    let mut rest = text;
    let mut public = None;
    while let Some(at) = rest.find(rustgis_data::login_address::PREFIX) {
        let candidate = &rest[at..];
        if let Some(url) = recorded_login_url(candidate) {
            let out = public.get_or_insert_with(|| {
                let mut out = String::with_capacity(text.len());
                out.push_str(&text[..text.len() - rest.len()]);
                out
            });
            out.push_str(&rest[..at]);
            rest = url;
        } else {
            // Keep malformed wrappers verbatim, advancing past this prefix so hostile input
            // cannot repeatedly rescan it.
            let end = at + rustgis_data::login_address::PREFIX.len();
            if let Some(out) = &mut public {
                out.push_str(&rest[..end]);
            }
            rest = &rest[end..];
        }
    }
    public
        .map(|mut out| {
            out.push_str(rest);
            out
        })
        .or(whole)
}

/// Finds a header's end without treating the surrounding prose as part of its URL. Validate
/// only through the URL authority with the shared parser; each candidate is size bounded.
fn recorded_login_url(text: &str) -> Option<&str> {
    let rest = text.strip_prefix(rustgis_data::login_address::PREFIX)?;
    let (binding, rest) = rest.split_once(':')?;
    if !(16..=64).contains(&binding.len()) || !binding.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    let (len, rest) = rest.split_once(':')?;
    if len.is_empty() || len.len() > 4 || !len.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let len: usize = len.parse().ok()?;
    let url = rest.get(len..)?.strip_prefix(':')?;
    let scheme = ["https://", "http://"].into_iter().find(|s| url.get(..s.len()).is_some_and(|head| head.eq_ignore_ascii_case(s)))?;
    let header_len = text.len() - url.len();
    let limit = 16384usize.checked_sub(header_len + scheme.len())?;
    let authority = &url[scheme.len()..];
    let end = authority
        .char_indices()
        .take_while(|(at, _)| *at < limit)
        .find(|(_, c)| matches!(*c, '/' | '?' | '#' | '\\') || c.is_whitespace() || c.is_control())
        .map(|(at, _)| at)
        .or_else(|| (authority.len() <= limit).then_some(authority.len()))?;
    let parsed = |end| {
        text.get(..header_len + scheme.len() + end).and_then(|candidate| rustgis_data::login_address::parse(candidate).ok().flatten())
    };
    if parsed(end).is_none() {
        // A bare host can be followed immediately by prose punctuation. Retry without that
        // suffix, but retain the closing bracket of an IPv6 host. Keep the original URL slice:
        // trimming here only chooses what the shared parser validates, not what we preserve.
        let authority = authority.get(..end)?;
        let ipv6_close = authority.strip_prefix('[').and_then(|rest| rest.find(']')).map(|at| at + 1);
        let trimmed = authority
            .char_indices()
            .rev()
            .find(|(at, c)| Some(*at) == ipv6_close || !matches!(*c, ']' | ')' | '}' | '"' | '\'' | ',' | ';' | '!' | '>' | ':' | '.'))
            .map_or(0, |(at, c)| at + c.len_utf8());
        parsed(trimmed)?;
    }
    Some(url)
}

pub(crate) fn clear_login_bindings<'a>(addresses: impl IntoIterator<Item = &'a mut String>) {
    for address in addresses {
        if let Some(public) = without_recorded_login_bindings(address) {
            *address = public;
        }
    }
}

/// Visits recorded strings without recursion; credential-named user data remains untouched.
pub(crate) fn recorded_addresses<'a>(value: &'a mut serde_json::Value, addresses: &mut Vec<&'a mut String>) {
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            serde_json::Value::String(text) => addresses.push(text),
            serde_json::Value::Array(values) => pending.extend(values),
            serde_json::Value::Object(values) => pending.extend(values.values_mut()),
            _ => {}
        }
    }
}

/// A role-independent runtime capability is never part of a recorded parameter's public form.
pub(crate) fn public_login_params(value: &serde_json::Value) -> Option<serde_json::Value> {
    let mut pending = vec![value];
    let mut found = false;
    while let Some(value) = pending.pop() {
        match value {
            serde_json::Value::String(text) if text.contains(rustgis_data::login_address::PREFIX) => {
                found = true;
                break;
            }
            serde_json::Value::Array(values) => pending.extend(values),
            serde_json::Value::Object(values) => pending.extend(values.values()),
            _ => {}
        }
    }
    if !found {
        return None;
    }
    let mut public = value.clone();
    let mut addresses = Vec::new();
    recorded_addresses(&mut public, &mut addresses);
    clear_login_bindings(addresses);
    (public != *value).then_some(public)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Project {
    /// Read-time adjustments (labels, diagrams, layouts), consumed by the session and never saved.
    #[serde(skip)]
    pub(crate) read_notices: Vec<(ReadPart, String)>,
    /// The format read from disk; serialization always declares the current writer's format.
    #[serde(default = "format", serialize_with = "write_format")]
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

/// Every project serialization, including templates and packages, describes the current
/// writer's payload rather than retaining the version of the file originally opened.
fn write_format<S: serde::Serializer>(_: &u32, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_u32(FORMAT)
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

/// Whether `path` is a plain `http://` or `https://` address: not a login or S3 wrapper, which a
/// raster layer downloads whole.
fn plain_web_address(path: &str) -> bool {
    let scheme = path.get(..8).map(str::to_ascii_lowercase).unwrap_or_default();
    scheme.starts_with("https://") || scheme.starts_with("http://")
}

/// Read only compatibility metadata before interpreting any saved payload.
#[derive(Deserialize)]
struct FileFormat {
    #[serde(default)]
    format: u64,
}

/// Refuse future document formats before typed deserialization or normalization. Missing
/// metadata is legacy content; arrays include legacy style files. Writers remain unchanged
/// until a change needs a newer format, at which point it must declare that format.
pub(crate) fn check_format(text: &str, kind: &str) -> Result<(), String> {
    let max = max_bytes();
    if text.len() as u64 > max {
        return Err(format!("the {kind} file is too large (maximum {})", limit_said(max)));
    }
    if text.trim_start().starts_with('[') {
        return Ok(());
    }
    let header: FileFormat = serde_json::from_str(text).map_err(|e| format!("not a RustGIS {kind}: {e}"))?;
    if header.format > u64::from(FORMAT) {
        return Err(format!("this {kind} was saved by a newer RustGIS (format {}). Update RustGIS to open it.", header.format));
    }
    Ok(())
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
            read_notices: Vec::new(),
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

    /// The strings of recorded command parameters (geoprocessing history, tasks and models). They
    /// are not layer addresses, and only login bindings are taken from them when saving or opening.
    pub(crate) fn recorded_mut(&mut self) -> Vec<&mut String> {
        let mut addresses = Vec::new();
        for record in &mut self.gp_history {
            recorded_addresses(&mut record.params, &mut addresses);
            if let Some((_, params)) = &mut record.command {
                recorded_addresses(params, &mut addresses);
            }
        }
        for task in &mut self.tasks {
            for step in &mut task.steps {
                recorded_addresses(&mut step.params, &mut addresses);
            }
        }
        for model in &mut self.models {
            for step in &mut model.steps {
                recorded_addresses(&mut step.params, &mut addresses);
            }
            if let Some(graph) = &mut model.graph {
                for node in &mut graph.nodes {
                    recorded_addresses(&mut node.params, &mut addresses);
                    // node.layer selects a name, including names that resemble runtime URLs.
                }
            }
        }
        addresses
    }

    /// Runtime bindings that this output cannot restore: peer receivers never share a login;
    /// explicit files can resolve layers and tile owners with a recorded saved connection.
    pub(crate) fn bound_address_losses(&self, include_recorded: bool) -> usize {
        let only_bound = |a: &str| {
            without_login_binding(a)
                .is_some_and(|public| matches!(crate::durable::public_address(&public), crate::durable::Kept::Unchanged))
        };
        let mut n = 0usize;
        for map in self.maps.iter().chain(self.offline_areas.iter().map(|area| &area.data)) {
            let mut layers = std::collections::BTreeSet::new();
            let mut tiles = std::collections::BTreeSet::new();
            for slot in map.slots().into_iter().filter(|slot| slot.address && only_bound(slot.value)) {
                match slot.at {
                    rustgis_map::SlotPos::Layer { layer, .. } => {
                        if map.layers.get(layer).is_some_and(|l| include_recorded || l.connection.is_empty()) {
                            layers.insert(layer);
                        }
                    }
                    rustgis_map::SlotPos::Tile { tile, .. } => {
                        if map.tiles.get(tile).is_some_and(|t| include_recorded || t.connection.is_empty()) {
                            tiles.insert(tile);
                        }
                    }
                    rustgis_map::SlotPos::Group { .. } | rustgis_map::SlotPos::Terrain => n = n.saturating_add(1),
                }
            }
            n = n.saturating_add(layers.len()).saturating_add(tiles.len());
        }
        n.saturating_add(self.tables.iter().filter(|table| table.source.as_deref().is_some_and(only_bound)).count())
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

    /// Takes the notices of what reading the project changed, keeping those of the parts given
    /// (an import says only what it takes); the rest are dropped.
    pub(crate) fn take_read_notices(&mut self, parts: &[ReadPart]) -> Vec<String> {
        std::mem::take(&mut self.read_notices).into_iter().filter(|(part, _)| parts.contains(part)).map(|(_, n)| n).collect()
    }

    /// Repairs invariants after loading (at least one map; a valid active index). Returns a
    /// notice for each kind of change made to the project's layouts, for the session to show.
    #[must_use]
    pub fn normalize(&mut self) -> Vec<String> {
        self.repair(false)
    }

    /// [`Project::normalize`] for a project read from its file, which also opens its plain COG
    /// tile layers (a project saved before GeoTIFFs read in place were raster layers, or with
    /// those `map.addTiles` adds) as raster layers read in place: as the project opens only. A
    /// later repair (a layer file added, maps imported) leaves a tile layer added this session
    /// as it is. Returns a notice for each kind of change made to the project's layouts.
    pub(crate) fn normalize_opened(&mut self) -> Vec<String> {
        self.repair(true)
    }

    /// Repairs the project; returns a notice per kind of change made to its layouts.
    fn repair(&mut self, opened: bool) -> Vec<String> {
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
        let layouts_dropped = self.layouts.len().saturating_sub(MAX_LAYOUTS);
        self.layouts.truncate(MAX_LAYOUTS);
        let mut repairs = Vec::new();
        for l in &mut self.layouts {
            let r = l.clamp();
            if !r.is_empty() {
                repairs.push((l.name.clone(), r));
            }
        }
        let when = if opened { "while opening the project" } else { "while checking the project" };
        let notices = layout_notices(&self.name, when, layouts_dropped, &repairs);
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
            // GeoTIFFs read in place are raster layers (a project from before saved tile layers),
            // so they don't count toward the tile layers kept. As the project opens only: a tile
            // layer added this session stays one until the project is opened again.
            if opened {
                crate::cmd::tiles::cog_tiles_to_rasters(m);
                // A raster layer whose path is a plain web address, from no saved connection, is
                // read in place, the one way such a path is read (a login or S3 wrapper is
                // downloaded whole, as is a connection's address, plain here until its login is
                // made again): one saved by an older RustGIS, which didn't know `in_place` (it
                // showed the layer as missing and saved it again without the key), reads in
                // place again here, losing a pending look at most.
                for l in &mut m.layers {
                    if l.connection.is_empty()
                        && let Some(rl) = &mut l.raster
                        && !rl.in_place
                        && rl.embedded.is_none()
                        && rl.path.as_deref().is_some_and(plain_web_address)
                    {
                        rl.in_place = true;
                    }
                }
            }
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
                // Read in place, read together below.
                if let Some(rl) = &mut l.raster
                    && rl.data.is_none()
                    && !rl.in_place
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
            read_in_place(m);
        }
        notices
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

    /// Omits runtime-only reservations on an owned recovery or bridge draft.
    pub(crate) fn omit_pending_imports(&mut self) {
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
    }

    /// Writes controlled durable fields and relative paths in one guarded transaction. Live
    /// fields are restored on every return and native unwind; only explicit saves are remembered.
    pub fn saved_text(&mut self, dir: Option<&Path>) -> Result<(String, crate::durable::Report), String> {
        self.ensure_imports_complete()?;
        let unrecorded = self.bound_address_losses(false);
        let (text, mut report) = crate::durable::output_remembered(self, dir.and_then(Path::to_str), project_text)?;
        report.addresses = report.addresses.saturating_add(u32::try_from(unrecorded).unwrap_or(u32::MAX));
        Ok((text, report))
    }

    /// The recovery text alone, without its counted loss report.
    /// Recovery stores should use [`Self::recovery_text_with_report`] to retain its notices.
    pub fn recovery_text(self) -> Result<String, String> {
        self.recovery_text_with_report().map(|(text, _)| text)
    }

    /// The project as an autosaved recovery copy keeps it: its text without passwords, keys, tokens
    /// or signatures in layer addresses, from a copy of the project (autosave writes it off the
    /// interface thread). Unlike [`Self::saved_text`], local file paths stay as the open project
    /// holds them, absolute: the copy lives in the app's recovery folder and is opened from there,
    /// not from the project's folder. Nor is it remembered as a file this process wrote, so
    /// frequent copies can't push the user's own saves out of the snapshots that restore secrets.
    /// Runtime import reservations are omitted while completed work remains recoverable.
    /// Text too large to open again is refused. Returns the text and its counted output notices.
    pub fn recovery_text_with_report(mut self) -> Result<(String, crate::durable::Report), String> {
        self.omit_pending_imports();
        let unrecorded = self.bound_address_losses(false);
        // Refused as a recovery copy, not as a save, and never built past the limit.
        let (text, mut report) = crate::durable::output_owned(self, |p| {
            let max = max_bytes();
            json_within(p, max)?.ok_or_else(|| format!("the project is too large to keep a recovery copy (maximum {})", limit_said(max)))
        })?;
        report.addresses = report.addresses.saturating_add(u32::try_from(unrecorded).unwrap_or(u32::MAX));
        Ok((text, report))
    }

    /// Writes checked durable text atomically and returns counted losses from the guarded output.
    pub fn save(&mut self, path: &Path) -> Result<crate::durable::Report, String> {
        let dir = std::path::absolute(path).ok().and_then(|p| p.parent().map(Path::to_path_buf));
        let (text, stripped) = self.saved_text(dir.as_deref())?;
        rustgis_data::io::write_atomic(path, text.as_bytes()).map_err(|e| format!("could not save {}: {e}", path.display()))?;
        Ok(stripped)
    }

    /// Reads a project from its JSON text (a browser upload).
    pub fn from_json(text: &str) -> Result<Project, String> {
        Self::read_json(text, true, None, true)
    }

    /// Reads imported JSON (including package contents) without restoring a saved file's secrets.
    pub(crate) fn from_import_json(text: &str, dir: Option<&Path>) -> Result<Project, String> {
        Self::read_json(text, false, dir, true)
    }

    /// Peer census must precede normalization: duplicate ids cannot become proof of a unique
    /// matching slot after the normalizer renumbers or drops them. Saved-text restore is absent.
    pub(crate) fn from_peer_raw(text: &str) -> Result<Project, String> {
        Self::read_json(text, false, None, false)
    }

    /// Validates compatibility before typed decoding. Saved files may restore exact fields;
    /// peer input stays unnormalized until its slot census has rejected ambiguous identities.
    fn read_json(text: &str, restore: bool, dir: Option<&Path>, normalize: bool) -> Result<Project, String> {
        check_format(text, "project")?;
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
            crate::durable::restore(&mut p, text);
        }
        // A binding is this computer's runtime token for a login: one in a file is no authority
        // to use any login here.
        crate::s3_connections::clear_bindings(p.addresses_mut());
        clear_login_bindings(p.addresses_mut());
        clear_login_bindings(p.recorded_mut());
        if let Some(d) = dir.and_then(Path::to_str) {
            p.resolve_paths(d);
        }
        let layout_notices = if normalize { p.normalize_opened() } else { Vec::new() };
        p.read_notices = label_losses::notices(text, &p)?;
        p.read_notices.extend(layout_notices.into_iter().map(|n| (ReadPart::Layouts, n)));
        Ok(p)
    }

    /// Opens a project file (a regular file within [`max_bytes`]; see `read_file`), resolving
    /// its relative paths against its folder.
    pub fn load(path: &Path) -> Result<Project, String> {
        let max = max_bytes();
        // Past the limit: said with its size and the limit here, before anything is read.
        if let Ok(meta) = std::fs::metadata(path)
            && meta.is_file()
            && meta.len() > max
        {
            // A computer with more memory opens it, up to the ceiling; past that none does.
            // The memory a 64-bit build needs to open it (whole GiB, rounded up).
            let needed = memory_for(meta.len()).div_ceil(1 << 30);
            let more = if meta.len() > BYTES_CEILING {
                format!(": no computer opens a project past {} MiB", BYTES_CEILING >> 20)
            } else if meta.len() > ceiling() {
                // Only a 32-bit build stops below the ceiling.
                format!(": a 64-bit RustGIS on a computer with at least {needed} GiB of memory opens it")
            } else if rustgis_data::memory::physical().is_some() {
                format!(": a computer with at least {needed} GiB of memory opens it")
            } else {
                String::new()
            };
            return Err(format!(
                "{} is {} MiB, more than a project can be (maximum {}){more}",
                path.display(),
                // Rounded up: a file a byte past 512 MiB isn't "512 MiB".
                meta.len().div_ceil(1 << 20),
                limit_said(max)
            ));
        }
        let bytes = read_file(path, max)?;
        let text = String::from_utf8(bytes).map_err(|_| format!("{} isn't a RustGIS project", path.display()))?;
        let dir = std::path::absolute(path).ok().and_then(|p| p.parent().map(Path::to_path_buf));
        Self::read_json(&text, true, dir.as_deref(), true)
    }
}

/// Starts reading the overviews of a map's raster layers read in place and not read yet, all at
/// once (see [`rustgis_map::raster::read_together`]): a project or an import of several waits for
/// the slowest, not for each in turn, where the session waits for them
/// ([`crate::Session::wait_in_place_reads`]); elsewhere they land as the map draws. One read from
/// an S3 bucket waits for its connection (see [`crate::s3_connections::resolve_project_s3`]). One
/// that failed is left to [`crate::Session::read_in_place_again`], which reads it again once its
/// wait is over, and one that couldn't start (an address the project keeps that can't be read)
/// stays as it is, said once: a repair of the project (a layer file added, maps imported) doesn't
/// start either over.
pub(crate) fn read_in_place(m: &mut Map) {
    let mut named: Vec<(String, &mut rustgis_map::raster::RasterLayer)> = m
        .layers
        .iter_mut()
        .filter_map(|l| {
            let name = l.name.clone();
            let rl = l.raster.as_mut()?;
            (rl.in_place && rl.error.is_none() && !rl.path.as_deref().is_some_and(crate::s3_connections::is_s3_backed))
                .then_some((name, rl))
        })
        .collect();
    let mut layers: Vec<&mut rustgis_map::raster::RasterLayer> = named.iter_mut().map(|(_, rl)| &mut **rl).collect();
    let failed = rustgis_map::raster::read_together(&mut layers, false);
    for (i, e) in failed {
        if let Some((name, _)) = named.get(i) {
            log::warn!("{name}: {e}");
        }
    }
}

/// A local file's bytes, refused past `max`. Only a regular file is opened: a device such as
/// `/dev/zero` reports no size and never ends, and opening a named pipe waits for a writer.
/// Reading stops a byte past `max`, so a file growing as it is read can't exhaust memory either.
pub(crate) fn read_file(path: &Path, max: u64) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} isn't a file", path.display()));
    }
    let too_large = || format!("{} is too large (maximum {} MiB)", path.display(), max >> 20);
    if meta.len() > max {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(max.saturating_add(1)).read_to_end(&mut bytes))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if bytes.len() as u64 > max {
        return Err(too_large());
    }
    Ok(bytes)
}

fn empty_map() -> &'static Map {
    static M: std::sync::OnceLock<Map> = std::sync::OnceLock::new();
    M.get_or_init(|| Map::new("Map"))
}

/// Most reports, presentations and notebooks a project file may hold.
pub const MAX_ITEMS: usize = 1000;

/// The part of a project a read notice is about, so an import says only what it takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReadPart {
    Labels,
    Diagrams,
    /// Diagrams that won't show for their layer's aggregation or extrusion (#1596).
    HiddenDiagrams,
    Layouts,
}

/// Every part: a whole project opened.
pub(crate) const ALL_PARTS: [ReadPart; 4] = [ReadPart::Labels, ReadPart::Diagrams, ReadPart::HiddenDiagrams, ReadPart::Layouts];

/// The most layouts a project keeps (`layout.*` commands refuse to make more).
pub(crate) const MAX_LAYOUTS: usize = 200;

/// The layouts named in a notice, the first few of them.
/// A name cut to 100 characters for a notice, so a long one can't crowd out the count (a
/// notice too long is withheld whole).
pub(crate) fn short_name(name: &str) -> String {
    match name.char_indices().nth(100) {
        Some((i, _)) => format!("{}…", name.get(..i).unwrap_or_default()),
        None => name.to_string(),
    }
}

fn layout_names(
    repairs: &[(String, rustgis_map::layout::LayoutRepairs)],
    of: impl Fn(&rustgis_map::layout::LayoutRepairs) -> usize,
) -> String {
    let names: Vec<&str> = repairs.iter().filter(|(_, r)| of(r) > 0).map(|(n, _)| n.as_str()).collect();
    let mut text = names.iter().take(3).map(|n| format!("\"{}\"", short_name(n))).collect::<Vec<_>>().join(", ");
    if names.len() > 3 {
        text.push_str(&format!(" and {} more", names.len() - 3));
    }
    text
}

/// One notice per kind of change [`Project::repair`] made to the layouts of a project read from
/// its file: layouts beyond the limit, elements brought into range or removed, page settings.
pub(crate) fn layout_notices(
    name: &str,
    when: &str,
    dropped: usize,
    repairs: &[(String, rustgis_map::layout::LayoutRepairs)],
) -> Vec<String> {
    let name = short_name(name);
    let mut notices = Vec::new();
    if dropped > 0 {
        notices.push(format!("{name}: {dropped} layout(s) beyond the {MAX_LAYOUTS} a project holds removed {when}"));
    }
    let total =
        |of: fn(&rustgis_map::layout::LayoutRepairs) -> usize| repairs.iter().map(|(_, r)| of(r)).fold(0usize, usize::saturating_add);
    let adjusted = total(|r| r.elements_adjusted);
    if adjusted > 0 {
        notices.push(format!(
            "{name}: {adjusted} layout element(s) with a value out of range, a repeated id, an over-long name or a missing map frame reference repaired {when} (in {})",
            layout_names(repairs, |r| r.elements_adjusted)
        ));
    }
    let cut = total(|r| r.elements_cut);
    if cut > 0 {
        notices.push(format!(
            "{name}: {cut} layout element(s) with a name, text, polygon points or legend exclusions cut to their limits {when} (in {})",
            layout_names(repairs, |r| r.elements_cut)
        ));
    }
    let removed = total(|r| r.elements_removed);
    if removed > 0 {
        notices.push(format!(
            "{name}: {removed} layout element(s) beyond the 500 a layout holds or tied to a missing map frame removed {when} (in {})",
            layout_names(repairs, |r| r.elements_removed)
        ));
    }
    let pages = total(|r| r.page_adjusted);
    if pages > 0 {
        notices.push(format!(
            "{name}: the page size, guides or map series margin of {pages} layout(s) adjusted {when} (in {})",
            layout_names(repairs, |r| r.page_adjusted)
        ));
    }
    let series = total(|r| r.series_removed);
    if series > 0 {
        notices.push(format!(
            "{name}: the map series of {series} layout(s) removed, its map frame missing, {when} (in {})",
            layout_names(repairs, |r| r.series_removed)
        ));
    }
    notices
}

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

// Durable ownership is classified here so private fields cannot disappear from the inventory.
use crate::durable::{Durable, FieldMut, Leaf, Owner, Prepare};

impl Durable for Project {
    /// Offers classified whole fields once, before any live mutation.
    fn prepare(&self, p: &mut Prepare<'_>) {
        let Self {
            read_notices: _,
            format: _,
            name: _,
            maps,
            active_map: _,
            tables,
            next_table_id: _,
            gp_history,
            layouts: _,
            folders: _,
            styles: _,
            ramps: _,
            tasks,
            gp_env: _,
            models,
            workspaces: _,
            link_views: _,
            thumbnail: _,
            jobs: _,
            offline_areas,
            reports: _,
            presentations,
            notebooks,
            dl_models: _,
            shared: _,
            data_stores: _,
        } = self;
        for (i, v) in maps.iter().enumerate() {
            p.child(Owner::Map(i), v);
        }
        for (i, v) in offline_areas.iter().enumerate() {
            let OfflineArea { name: _, map: _, extent: _, created: _, synced: _, data } = v;
            p.child(Owner::Area(i), data);
        }
        for (i, v) in tables.iter().enumerate() {
            p.child(Owner::Table(i), v);
        }
        for (i, v) in tasks.iter().enumerate() {
            p.child(Owner::Task(i), v);
        }
        for (i, v) in models.iter().enumerate() {
            p.child(Owner::Model(i), v);
        }
        for (i, v) in gp_history.iter().enumerate() {
            p.child(Owner::History(i), v);
        }
        for (i, v) in notebooks.iter().enumerate() {
            p.child(Owner::Notebook(i), v);
        }
        for (i, v) in presentations.iter().enumerate() {
            p.child(Owner::Presentation(i), v);
        }
    }

    /// Resolves a prepared location through owned fields, without allocation or policy.
    fn field_mut(&mut self, owners: &[Owner], leaf: &Leaf) -> Option<FieldMut<'_>> {
        let (owner, rest) = owners.split_first()?;
        match owner {
            Owner::Map(i) => self.maps.get_mut(*i)?.field_mut(rest, leaf),
            Owner::Area(i) => self.offline_areas.get_mut(*i)?.data.field_mut(rest, leaf),
            Owner::Table(i) => self.tables.get_mut(*i)?.field_mut(rest, leaf),
            Owner::Task(i) => self.tasks.get_mut(*i)?.field_mut(rest, leaf),
            Owner::Model(i) => self.models.get_mut(*i)?.field_mut(rest, leaf),
            Owner::History(i) => self.gp_history.get_mut(*i)?.field_mut(rest, leaf),
            Owner::Notebook(i) => self.notebooks.get_mut(*i)?.field_mut(rest, leaf),
            Owner::Presentation(i) => self.presentations.get_mut(*i)?.field_mut(rest, leaf),
            Owner::Graph => None,
        }
    }
}

impl Durable for Table {
    /// Offers classified whole fields once, before any live mutation.
    fn prepare(&self, p: &mut Prepare<'_>) {
        let Self { id: _, dataset } = self;
        let Dataset {
            name: _,
            geometry_type: _,
            fields: _,
            features: _,
            source: provenance,
            crs: _,
            crs_incomplete: _,
            zm_dropped: _,
            windowed: _,
        } = dataset;
        if let Some(v) = provenance {
            p.string(Leaf::Table, v, true, rustgis_map::paths::is_local(v));
        }
    }

    /// Resolves a prepared location through owned fields, without allocation or policy.
    fn field_mut(&mut self, owners: &[Owner], leaf: &Leaf) -> Option<FieldMut<'_>> {
        if owners.is_empty() && *leaf == Leaf::Table { self.dataset.source.as_mut().map(FieldMut::Text) } else { None }
    }
}

impl Durable for Task {
    /// Offers classified whole fields once, before any live mutation.
    fn prepare(&self, p: &mut Prepare<'_>) {
        let Self { name: _, description: _, steps } = self;
        for (i, TaskStep { title: _, instructions: _, command, params }) in steps.iter().enumerate() {
            p.command(Leaf::Task(i), Leaf::TaskCommand(i), command, params);
        }
    }

    /// Resolves a prepared location through owned fields, without allocation or policy.
    fn field_mut(&mut self, owners: &[Owner], leaf: &Leaf) -> Option<FieldMut<'_>> {
        if !owners.is_empty() {
            return None;
        }
        match leaf {
            Leaf::Task(i) => Some(FieldMut::Json(&mut self.steps.get_mut(*i)?.params)),
            Leaf::TaskCommand(i) => Some(FieldMut::Text(&mut self.steps.get_mut(*i)?.command)),
            _ => None,
        }
    }
}

impl Durable for Model {
    /// Offers classified whole fields once, before any live mutation.
    fn prepare(&self, p: &mut Prepare<'_>) {
        let Self { name: _, steps, graph } = self;
        for (i, ModelStep { tool, params }) in steps.iter().enumerate() {
            p.tool(Leaf::ModelStep(i), Leaf::ModelStepTool(i), tool, params);
        }
        if let Some(g) = graph {
            p.child(Owner::Graph, g);
        }
    }

    /// Resolves a prepared location through owned fields, without allocation or policy.
    fn field_mut(&mut self, owners: &[Owner], leaf: &Leaf) -> Option<FieldMut<'_>> {
        if let Some((Owner::Graph, rest)) = owners.split_first() {
            return self.graph.as_mut()?.field_mut(rest, leaf);
        }
        if !owners.is_empty() {
            return None;
        }
        match leaf {
            Leaf::ModelStep(i) => Some(FieldMut::Json(&mut self.steps.get_mut(*i)?.params)),
            Leaf::ModelStepTool(i) => Some(FieldMut::Text(&mut self.steps.get_mut(*i)?.tool)),
            _ => None,
        }
    }
}

impl Durable for ModelGraph {
    /// Offers classified whole fields once, before any live mutation.
    fn prepare(&self, p: &mut Prepare<'_>) {
        let Self { nodes, edges: _ } = self;
        for (i, ModelNode { id: _, kind: _, x: _, y: _, layer: _, tool, params, name: _ }) in nodes.iter().enumerate() {
            p.tool(Leaf::ModelNode(i), Leaf::ModelNodeTool(i), tool.as_deref().unwrap_or(""), params);
        }
    }

    /// Resolves a prepared location through owned fields, without allocation or policy.
    fn field_mut(&mut self, owners: &[Owner], leaf: &Leaf) -> Option<FieldMut<'_>> {
        if !owners.is_empty() {
            return None;
        }
        match leaf {
            Leaf::ModelNode(i) => Some(FieldMut::Json(&mut self.nodes.get_mut(*i)?.params)),
            Leaf::ModelNodeTool(i) => self.nodes.get_mut(*i)?.tool.as_mut().map(FieldMut::Text),
            _ => None,
        }
    }
}

/// Enforces the opening bound inside the formatter, before any destination is replaced.
fn project_text(p: &Project) -> Result<String, String> {
    let max = max_bytes();
    json_within(p, max)?.ok_or_else(|| format!("the project is too large to save (maximum {})", limit_said(max)))
}

#[cfg(test)]
mod limit_tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    /// A quarter of the memory, at 5 bytes a byte of project: a twentieth of it, from 512 MiB to
    /// 4 GiB; 512 MiB where the memory isn't known (the browser).
    #[test]
    fn the_project_limit_follows_the_memory() {
        assert_eq!(max_bytes_for(None), BYTES_FLOOR);
        assert_eq!(max_bytes_for(Some(8 * GIB)), BYTES_FLOOR, "never below the floor");
        // Within this build's ceiling: a 32-bit build stays at the floor.
        let within = |v: u64| v.clamp(BYTES_FLOOR, ceiling());
        assert_eq!(max_bytes_for(Some(16 * GIB)), within(16 * GIB / 20));
        assert_eq!(max_bytes_for(Some(64 * GIB)), within(64 * GIB / 20));
        assert_eq!(max_bytes_for(Some(80 * GIB)), ceiling());
        assert_eq!(max_bytes_for(Some(1024 * GIB)), ceiling(), "never above the ceiling");
        assert_eq!(max_bytes_for(Some(u64::MAX)), ceiling(), "no overflow");
        // The 659 MiB project of #1615 opens on a computer sold as 16 GB, which reports less (in a
        // 64-bit build, as the app is).
        if usize::BITS >= 64 {
            assert!(max_bytes_for(Some(15 * GIB)) > 659 << 20);
            assert_eq!(ceiling(), BYTES_CEILING);
        } else {
            assert_eq!(ceiling(), BYTES_FLOOR);
        }
        let max = max_bytes();
        assert!((BYTES_FLOOR..=BYTES_CEILING).contains(&max), "{max}");
        // The memory named for a refused file is enough to open it, and not much more.
        let size: u64 = 659 << 20;
        let needed = memory_for(size).div_ceil(GIB) * GIB;
        if usize::BITS >= 64 {
            assert!(max_bytes_for(Some(needed)) >= size && max_bytes_for(Some(needed - GIB)) < size);
        }
        assert_eq!(needed / GIB, 13);
    }

    /// Writing stops at the limit: what comes after it is never serialized.
    #[test]
    fn serializing_stops_at_the_limit() {
        struct Probe<'a>(&'a std::cell::Cell<bool>);
        impl Serialize for Probe<'_> {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                self.0.set(true);
                s.serialize_unit()
            }
        }
        let visited = std::cell::Cell::new(false);
        let v = ("x".repeat(100), Probe(&visited));
        assert_eq!(json_within(&v, 50).ok(), Some(None));
        assert!(!visited.get(), "the value after the limit was serialized");
        assert!(json_within(&v, 1000).ok().flatten().is_some() && visited.get());
    }

    /// The text stops at the limit: whole within it, none past it, never built past it.
    #[test]
    fn json_is_never_built_past_the_limit() {
        let v = serde_json::json!({"a": "x".repeat(100)});
        let whole = serde_json::to_string(&v).unwrap_or_default();
        assert_eq!(json_within(&v, whole.len() as u64).ok().flatten(), Some(whole.clone()));
        assert_eq!(json_within(&v, whole.len() as u64 - 1).ok(), Some(None));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod login_binding_tests {
    use super::*;

    #[test]
    fn saves_and_imports_strip_runtime_capabilities_including_recorded_urls() {
        let url = rustgis_data::login_address::bind(
            "0123456789abcdef",
            "https://project757.example.org",
            "https://project757.example.org/tiles/{z}/{x}/{y}.png",
        )
        .unwrap();
        let mut project = Project::default();
        project.map_mut().tiles.push(rustgis_map::TileLayer::new("Bound", rustgis_map::tiles::TileKind::Raster, &url));
        project.tasks.push(Task {
            name: "Recorded".into(),
            description: String::new(),
            steps: vec![TaskStep {
                title: "Add".into(),
                instructions: String::new(),
                command: "map.addTiles".into(),
                params: serde_json::json!({"url": url, "connection": "saved-id"}),
            }],
        });
        let forged = serde_json::to_string(&project).unwrap();
        let (saved, _) = project.saved_text(None).unwrap();
        assert!(!saved.contains(rustgis_data::login_address::PREFIX));
        assert_eq!(project.map().tiles[0].url, url);
        for text in [&saved, &forged] {
            let opened = Project::from_json(text).unwrap();
            assert_eq!(opened.map().tiles[0].url, rustgis_data::login_address::inner(&url));
            assert_eq!(opened.tasks[0].steps[0].params["url"], rustgis_data::login_address::inner(&url));
            assert_eq!(opened.tasks[0].steps[0].params["connection"], "saved-id");
        }
        let recovery = project.recovery_text().unwrap();
        assert!(!recovery.contains(rustgis_data::login_address::PREFIX));
    }

    fn bound(url: &str) -> String {
        rustgis_data::login_address::bind("0123456789abcdef", "https://project757.example.org", url).unwrap()
    }

    fn tiles(name: &str, url: &str, connection: &str) -> rustgis_map::TileLayer {
        let mut t = rustgis_map::TileLayer::new(name, rustgis_map::tiles::TileKind::Raster, url);
        t.connection = connection.into();
        t
    }

    #[test]
    fn recorded_parameters_are_not_layer_addresses() {
        let mut project = Project::default();
        let note = "https://user@project757.example.org/data.csv#part";
        project.tasks.push(Task {
            name: "Recorded".into(),
            description: String::new(),
            steps: vec![TaskStep {
                title: "Add".into(),
                instructions: String::new(),
                command: "layer.addUrl".into(),
                params: serde_json::json!({"url": note}),
            }],
        });
        assert!(project.addresses_mut().is_empty());
        let (saved, stripped) = project.saved_text(None).unwrap();
        assert_eq!(stripped.layer_addresses(), 0);
        assert_eq!(Project::from_json(&saved).unwrap().tasks[0].steps[0].params["url"], note);
    }

    /// String nesting is independent of JSON depth and must never consume the thread's stack.
    #[test]
    fn hostile_nested_templates_in_recorded_parameters_do_not_recurse() {
        let prefix = "pmtiles:x@".repeat(16_384);
        let plain = "https://project757.example.org/data.csv";
        let clean = format!("{prefix}{plain}");
        let mut project = Project::default();
        project.tasks.push(Task {
            name: "Nested".into(),
            description: String::new(),
            steps: vec![TaskStep {
                title: "Read".into(),
                instructions: String::new(),
                command: "layer.addUrl".into(),
                params: serde_json::json!({"url":format!("{prefix}{}", bound(plain)),"clean":clean}),
            }],
        });
        let text = serde_json::to_string(&project).unwrap();
        let opened = Project::from_json(&text).unwrap();
        assert_eq!(opened.tasks[0].steps[0].params["url"], clean);
        assert_eq!(opened.tasks[0].steps[0].params["clean"], clean);
    }

    #[test]
    fn embedded_recorded_bindings_are_removed_from_saves_recovery_and_exports() {
        let first = "https://project757.example.org/first.csv?token=ordinary#part";
        let second = "https://project757.example.org/second.csv";
        let note = format!("préface [{}], then [{}]; tail", bound(first), bound(second));
        let plain = format!("préface [{first}], then [{second}]; tail");
        let clean = "ordinary abc123 https://user@project757.example.org/data.csv?token=abc123#part";
        let params = serde_json::json!({"nested": [{"note": note}], "clean": clean});
        let mut session = crate::Session::new(Project::default());
        session
            .run(
                "task.new",
                &serde_json::json!({"name": "Recorded", "steps": [{"title": "Read", "command": "layer.addUrl", "params": params}]}),
            )
            .unwrap();
        session.run("model.new", &serde_json::json!({"name": "Recorded", "steps": [{"tool": "buffer", "params": params}]})).unwrap();
        session.project.gp_history.push(GpRecord {
            tool: "Recorded".into(),
            id: "buffer".into(),
            params: params.clone(),
            command: Some(("layer.addUrl".into(), params.clone())),
            at: 0,
            ok: true,
            messages: Vec::new(),
            millis: 0,
            outputs: Vec::new(),
        });
        let forged = serde_json::to_string(&session.project).unwrap();
        let (saved, stripped) = session.project.saved_text(None).unwrap();
        assert_eq!(stripped.layer_addresses(), 0);
        let recovery = session.project.clone().recovery_text().unwrap();
        for text in [&saved, &recovery] {
            assert!(!text.contains(rustgis_data::login_address::PREFIX));
            assert!(!text.contains("0123456789abcdef"));
        }
        for text in [&saved, &recovery, &forged] {
            let mut opened = Project::from_json(text).unwrap();
            for value in opened.recorded_mut() {
                assert!(!value.contains(rustgis_data::login_address::PREFIX));
                assert!(value.as_str() == plain || value.as_str() == clean, "{value}");
            }
            assert_eq!(opened.tasks[0].steps[0].params["nested"][0]["note"], plain);
            assert_eq!(opened.models[0].steps[0].params["nested"][0]["note"], plain);
            assert_eq!(opened.gp_history[0].params["nested"][0]["note"], plain);
            assert_eq!(opened.gp_history[0].command.as_ref().unwrap().1["nested"][0]["note"], plain);
        }
        let path = std::env::temp_dir().join(format!("recorded-text-757-{}.rgistask", std::process::id()));
        session.run("task.save", &serde_json::json!({"task": "Recorded", "path": path.to_string_lossy()})).unwrap();
        let task = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        let model = session.run("model.export", &serde_json::json!({"model": "Recorded"})).unwrap();
        for text in [task.as_str(), model["text"].as_str().unwrap()] {
            assert!(!text.contains(rustgis_data::login_address::PREFIX));
            assert!(!text.contains("0123456789abcdef"));
            assert!(text.contains(&plain));
            assert!(text.contains(clean));
        }
        assert_eq!(session.project.tasks[0].steps[0].params, params);
        assert_eq!(session.project.models[0].steps[0].params, params);
        assert_eq!(session.project.gp_history[0].params, params);
        assert_eq!(session.project.gp_history[0].command.as_ref().unwrap().1, params);
    }

    #[test]
    fn recorded_binding_scanner_preserves_malformed_and_wrapped_values() {
        let public = "https://project757.example.org/archive.pmtiles";
        let url = bound(public);
        for (wrapped, plain) in [
            (format!("pmtiles:{{z}}/{{x}}/{{y}}@{url}"), format!("pmtiles:{{z}}/{{x}}/{{y}}@{public}")),
            (format!("rustgis-s3:0123456789abcdef:0::{url}"), format!("rustgis-s3:0123456789abcdef:0::{public}")),
        ] {
            let mut value = wrapped;
            clear_login_bindings([&mut value]);
            assert_eq!(value, plain);
        }
        let malformed = [
            "rustgis-login:nothex:5:https://project757.example.org/path",
            "rustgis-login:0123456789abcdef:999999999999999999999999:https://project757.example.org/path",
            "rustgis-login:0123456789abcdef:1:é:https://project757.example.org/path",
            "rustgis-login:0123456789abcdef:9999:short",
        ];
        for bad in malformed {
            let mut unchanged = bad.to_string();
            clear_login_bindings([&mut unchanged]);
            assert_eq!(unchanged, bad);
            let mut mixed = format!("before {bad} then {url} after");
            clear_login_bindings([&mut mixed]);
            assert_eq!(mixed, format!("before {bad} then {public} after"));
        }
        let mut long = format!("{} {url} {}", "é".repeat(20_000), "tail ".repeat(4_000));
        clear_login_bindings([&mut long]);
        assert_eq!(long, format!("{} {public} {}", "é".repeat(20_000), "tail ".repeat(4_000)));
    }

    #[test]
    fn recorded_bare_origins_keep_prose_punctuation_and_validate_the_binding() {
        for (origin, url) in [
            ("https://project757.example.org", "https://project757.example.org"),
            ("https://project757.example.org", "HTTPS://PROJECT757.EXAMPLE.ORG.:443"),
            ("https://project757.example.org:8443", "https://project757.example.org:8443"),
            ("https://[::1]", "https://[::1]"),
            ("https://[::1]", "HTTPS://[0:0:0:0:0:0:0:1]:443"),
        ] {
            let bound = rustgis_data::login_address::bind("0123456789abcdef", origin, url).unwrap();
            for (left, right) in [("[", "]"), ("(", ")"), ("\"", "\""), ("[{(", ")}];"), ("", "!")] {
                let mut text = format!("before {left}{bound}{right} after");
                clear_login_bindings([&mut text]);
                assert_eq!(text, format!("before {left}{url}{right} after"));
            }
        }
        let origin = "https://project757.example.org";
        for invalid in [
            format!("rustgis-login:0123456789abcdef:{}:{origin}:https://foreign757.example.org", origin.len()),
            format!("rustgis-login:nothex:{}:{origin}:{origin}", origin.len()),
            format!("rustgis-login:0123456789abcdef:99999999999999999999:{origin}:{origin}"),
        ] {
            let original = format!("before [{invalid}] after");
            let mut text = original.clone();
            clear_login_bindings([&mut text]);
            assert_eq!(text, original);
        }
    }

    #[test]
    fn a_malformed_wrapper_is_withheld_at_durable_output() {
        let bad = "rustgis-login:nothex:5:https://project757.example.org/{z}/{x}/{y}.png";
        let mut project = Project::default();
        project.tasks.push(Task {
            name: "Recorded".into(),
            description: String::new(),
            steps: vec![TaskStep {
                title: "Add".into(),
                instructions: String::new(),
                command: "map.addTiles".into(),
                params: serde_json::json!({"url": bad}),
            }],
        });
        let (saved, report) = project.saved_text(None).unwrap();
        // Inspect the durable bytes: from_json may restore a credential edit from this
        // process's explicit-save cache, including an invalid address that still cannot run.
        let written: serde_json::Value = serde_json::from_str(&saved).unwrap();
        assert_eq!(written["tasks"][0]["steps"][0]["params"]["url"], rustgis_data::login_address::PREFIX);
        assert_eq!((report.withheld, report.steps, report.layer_addresses()), (1, 1, 0));
        assert_eq!(report.notices().len(), 2, "the withheld slot and affected executable step are distinct report dimensions");
        assert_eq!(project.tasks[0].steps[0].params["url"], bad, "the live project is restored");
        let mut address = bad.to_string();
        clear_login_bindings([&mut address]);
        assert_eq!(address, bad);
    }

    #[test]
    fn only_lost_secrets_are_counted_when_saving() {
        let plain = "https://project757.example.org/{z}/{x}/{y}.png";
        let mut project = Project::default();
        project.map_mut().tiles.push(tiles("Recorded", &bound(plain), "saved-id"));
        assert_eq!(
            project.saved_text(None).unwrap().1.layer_addresses(),
            0,
            "a layer that records its connection asks for the login again"
        );

        project.map_mut().tiles.push(tiles("Unrecorded", &bound(plain), ""));
        assert_eq!(project.saved_text(None).unwrap().1.layer_addresses(), 1, "a bound layer with no connection can't ask for one");

        project.map_mut().tiles.clear();
        project.map_mut().tiles.push(tiles("Token", &format!("{plain}?token=abc123"), ""));
        assert_eq!(project.saved_text(None).unwrap().1.layer_addresses(), 1, "a real token is still lost");

        project.map_mut().tiles.clear();
        project.map_mut().tiles.push(tiles("Both", &bound(&format!("{plain}?token=abc123")), "saved-id"));
        assert_eq!(project.saved_text(None).unwrap().1.layer_addresses(), 1, "the token under a binding is lost too");

        project.map_mut().tiles.clear();
        project.map_mut().tiles.push(tiles("Unrecorded token", &bound(&format!("{plain}?token=abc123")), ""));
        assert_eq!(project.saved_text(None).unwrap().1.layer_addresses(), 1, "a bound layer with no connection and a token is one layer");

        project.map_mut().tiles.clear();
        let legend = "https://project757.example.org/legend.png";
        let mut tile = tiles("Legend only", plain, "");
        tile.legends = vec![rustgis_data::ogc::Legend { url: bound(legend), ..Default::default() }; 2];
        project.map_mut().tiles.push(tile);
        let (saved, report) = project.saved_text(None).unwrap();
        assert_eq!(report.layer_addresses(), 1, "several lost legend logins count their tile once");
        assert_eq!(project.map().tiles[0].legends[0].url, bound(legend), "the live login is restored");
        assert_eq!(Project::from_json(&saved).unwrap().map().tiles[0].legends[0].url, legend, "a saved binding is never restored");

        project.map_mut().tiles[0].legends.clear();
        project.map_mut().tiles[0].time_url = Some(bound(plain));
        assert_eq!(project.saved_text(None).unwrap().1.layer_addresses(), 1, "an unrecorded time URL loses its login too");
        project.map_mut().tiles[0].time_url = None;
        project.map_mut().tiles[0].time_urls = vec![(0.0, bound(plain)), (1.0, bound(plain))];
        assert_eq!(project.saved_text(None).unwrap().1.layer_addresses(), 1, "dated URLs count their tile once");
        project.map_mut().tiles[0].time_urls.clear();
        assert_eq!(project.saved_text(None).unwrap().1.layer_addresses(), 0, "a clean tile loses no login");
    }
}
