//! L2: the map model — layers with symbology and labels, the camera, colour ramps and basemaps.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod antimeridian;
pub mod camera;
pub mod color;
pub mod content;
pub mod design;
pub mod diagram;
pub mod effects;
pub mod fills;
pub mod geolibre;
mod geometry_kinds;
pub mod globe;
pub mod graphics;
pub mod label_by;
pub mod layout;
pub mod lod;
pub mod palette;
pub mod paths;
pub mod planets;
pub mod presentation;
pub mod projection;
pub mod qgis;
pub mod raster;
pub mod report;
pub mod slots;
pub mod source;
pub use slots::{LayerRole, Slot, SlotAt, SlotPos, TileRole};
pub mod symbol_by;
pub mod symbology;
pub mod tiles;
pub mod time;
pub mod view;
mod view_memory;
pub mod world;

pub use camera::Camera;
pub use color::{Ramp, RampGroup, Rgba};
pub use diagram::{DiagramCategory, DiagramKind, DiagramLabel, DiagramSettings, DiagramSize};
pub use globe::{Globe, Projection};
pub use raster::{RasterDefaults, RasterLayer, RasterRenderer, RasterStyle, Resampling, Stretch};
pub use symbology::{
    ClassMethod, Classer, FontName, Glyph, LabelBackground, LabelClass, LabelKey, LabelMatcher, LabelPlacement, Labeling, LineCap,
    LineJoin, LineMark, LineMarkers, LineStroke, Marker, MarkerRotation, Placements, Renderer, Symbol,
};
pub use tiles::{TileKind, TileLayer};

/// Where a locked label stays.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LockedPlace {
    /// The place it took: 0 its own, else one of its alternatives (#1011), numbered from 1.
    pub place: u8,
    /// How far a callout moved it from there (points, right and down), if one did.
    pub callout: Option<[f32; 2]>,
}

use rustgis_data::{Dataset, Geometry, GeometryType};
use rustgis_geom::{Coord, Extent};
use serde::{Deserialize, Serialize};

/// The backdrop drawn under every layer. Each is drawn from the built-in world outline.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Basemap {
    #[default]
    Paper,
    Midnight,
    Ocean,
    Atlas,
    Blank,
}

impl Basemap {
    pub const ALL: [Basemap; 5] = [Basemap::Paper, Basemap::Midnight, Basemap::Ocean, Basemap::Atlas, Basemap::Blank];
    pub fn label(self) -> &'static str {
        match self {
            Basemap::Paper => "Paper",
            Basemap::Midnight => "Midnight",
            Basemap::Ocean => "Ocean",
            Basemap::Atlas => "Atlas",
            Basemap::Blank => "Blank",
        }
    }
    pub fn code(self) -> &'static str {
        match self {
            Basemap::Paper => "paper",
            Basemap::Midnight => "midnight",
            Basemap::Ocean => "ocean",
            Basemap::Atlas => "atlas",
            Basemap::Blank => "blank",
        }
    }
    pub fn from_code(s: &str) -> Option<Basemap> {
        Basemap::ALL.into_iter().find(|b| b.code().eq_ignore_ascii_case(s.trim()) || b.label().eq_ignore_ascii_case(s.trim()))
    }
    /// (water, land, land outline, graticule) colours.
    pub fn palette(self) -> (Rgba, Rgba, Rgba, Rgba) {
        match self {
            Basemap::Paper => {
                (Rgba::rgb(0xDD, 0xE4, 0xEA), Rgba::rgb(0xF6, 0xF5, 0xF1), Rgba::rgb(0xC9, 0xC9, 0xC4), Rgba::new(0x9A, 0xA8, 0xB4, 60))
            }
            Basemap::Midnight => {
                (Rgba::rgb(0x1A, 0x1F, 0x26), Rgba::rgb(0x2E, 0x33, 0x3A), Rgba::rgb(0x47, 0x4D, 0x55), Rgba::new(0x80, 0x90, 0xA0, 40))
            }
            Basemap::Ocean => {
                (Rgba::rgb(0x9C, 0xC6, 0xE0), Rgba::rgb(0xE9, 0xE4, 0xD4), Rgba::rgb(0xB8, 0xB0, 0x98), Rgba::new(0xFF, 0xFF, 0xFF, 90))
            }
            Basemap::Atlas => {
                (Rgba::rgb(0xB5, 0xD5, 0xE6), Rgba::rgb(0xDC, 0xE8, 0xC8), Rgba::rgb(0xA8, 0xB4, 0x90), Rgba::new(0x60, 0x80, 0x90, 50))
            }
            Basemap::Blank => (Rgba::rgb(0xFF, 0xFF, 0xFF), Rgba::rgb(0xFF, 0xFF, 0xFF), Rgba::new(0, 0, 0, 0), Rgba::new(0, 0, 0, 0)),
        }
    }
}

/// How No Fill hid a layer's polygon fills ([`Layer::no_fill`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NoFill {
    /// One transparent fill over every symbol: on streamed layers and layers with points or lines,
    /// whose symbols those share.
    Override,
    /// Every symbol's fill made transparent.
    Symbols,
}

/// A layer: a dataset drawn with a renderer and labels.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Layer {
    pub id: u32,
    pub name: String,
    pub visible: bool,
    pub dataset: Dataset,
    pub renderer: Renderer,
    pub labels: Labeling,
    /// 0 = opaque, 100 = invisible.
    pub transparency: f32,
    /// Drawn only when zoomed in beyond 1:`min_scale` (out-beyond limit), if set.
    #[serde(default)]
    pub min_scale: Option<f64>,
    /// Drawn only when zoomed out beyond 1:`max_scale` (in-beyond limit), if set.
    #[serde(default)]
    pub max_scale: Option<f64>,
    /// Definition query: only matching features draw.
    #[serde(default)]
    pub definition_query: Option<String>,
    /// Bound to the map's time slider: only features whose date is in its window draw.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<time::TimeBinding>,
    /// Selected rows (indices into `dataset.features`).
    #[serde(default)]
    pub selection: Vec<usize>,
    /// Whether the Layers pane shows the legend classes.
    #[serde(default = "yes")]
    pub expanded: bool,
    #[serde(default = "yes")]
    pub selectable: bool,
    /// A graphics layer: each feature carries its own style (see [`graphics`]).
    #[serde(default)]
    pub graphics: bool,
    /// How the layer blends with what's beneath.
    #[serde(default)]
    pub blend: effects::Blend,
    /// A polygon layer (by name) outside whose shapes this layer is hidden.
    #[serde(default)]
    pub mask: Option<String>,
    /// Scale-dependent feature filters.
    #[serde(default)]
    pub display_filters: Vec<effects::DisplayFilter>,
    /// Points drawn as counted bins.
    #[serde(default)]
    pub aggregation: Option<effects::Aggregation>,
    /// Polygons drawn extruded.
    #[serde(default)]
    pub extrusion: Option<effects::Extrusion>,
    /// A line layer's stacked strokes, drawn bottom first in place of its single stroke (none:
    /// the renderer's stroke alone). A point layer's are its marks: each stroke of markers draws
    /// its marker at the point, bottom first, in place of the symbol's (see
    /// [`LineStroke`]); a stroke without markers draws nothing there.
    #[serde(default, skip_serializing_if = "Vec::is_empty", deserialize_with = "symbology::de_strokes")]
    pub line_strokes: Vec<symbology::LineStroke>,
    /// A polygon layer's stacked fills, drawn bottom first inside each polygon in place of its
    /// symbol's fill (none: that fill alone); the outline is drawn over them (see
    /// [`fills::FillLayer`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty", deserialize_with = "fills::de_fills")]
    pub fill_layers: Vec<fills::FillLayer>,
    /// How No Fill hides the polygon fills, while it is on: a new symbology, a fill colour or a
    /// fill in the symbol's colour shows them again. A fill colour chosen transparent isn't this.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "de_no_fill")]
    pub no_fill: Option<NoFill>,
    /// Draws the stacked strokes in levels (symbol levels): every feature's strokes of one
    /// [`LineStroke::level`] before any of the next, so road casings join under the cores
    /// instead of cutting across them where lines meet.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stroke_levels: bool,
    /// Turns each point's marker (and its marks, their offsets with them) by the angle in a
    /// field, as QGIS's data-defined symbol angle does.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "symbology::de_marker_rotation")]
    pub marker_rotation: Option<symbology::MarkerRotation>,
    /// A small chart drawn on each feature from several numeric fields (pie or bars), as ArcGIS
    /// Pro's Chart symbology and QGIS's Diagrams draw, over the renderer's own symbol.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "diagram::de_diagram")]
    pub diagram: Option<diagram::DiagramSettings>,
    /// Its symbol's size, colours, outline width and marker angle per feature, each by an
    /// expression (none set: the renderer's symbol alone); drawn through [`Layer::styler`].
    #[serde(default, skip_serializing_if = "symbol_by::SymbolBy::is_empty", deserialize_with = "symbol_by::de_symbol_by")]
    pub symbol_by: symbol_by::SymbolBy,
    /// The scale (1:N) its symbol and label sizes are set for: drawn at another scale they grow
    /// and shrink with the map, as on paper (QGIS's reference scale). `None`: they keep their
    /// size on screen.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "de_reference_scale")]
    pub reference_scale: Option<f64>,
    /// Layers with a higher priority draw above lower ones, whatever their order (the scene's
    /// depth priority).
    #[serde(default)]
    pub priority: i32,
    /// Domains, subtypes, attribute rules, contingent values, archive and templates.
    #[serde(default, skip_serializing_if = "design::Design::is_empty")]
    pub design: design::Design,
    /// Set for raster layers (whose `dataset` is empty).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raster: Option<RasterLayer>,
    /// Set for streamed layers, whose `dataset` holds the features of the last view read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<source::SourceLayer>,
    /// Set for a layer loaded whole from a PostGIS table to be edited: the table its edits are
    /// saved to, and the rows' versions as loaded (desktop only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub postgis: Option<source::PostgisEdit>,
    /// The GeoPackage table this layer's edits save to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpkg: Option<source::GpkgEdit>,
    /// The saved connection it was added from (its id, in the app's preferences): a layer whose
    /// source is read again logs in with that connection's login, and a project opened where
    /// the connection or its login is missing asks for one. Empty for a layer that came from no
    /// connection. Never a secret.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub connection: String,
    /// Added by an anonymous plugin command: never adopt a saved web-service login, including
    /// after saving and reopening the project.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub anonymous: bool,
    /// Process-local identity, preserved by snapshots but never stored in a project.
    #[serde(skip, default = "next_layer_instance")]
    instance_id: u64,
    /// Process-local replacement identity; ordinary geometry edits leave chosen vertices valid.
    #[serde(skip)]
    replacement_rev: u64,
    /// Shared, like the features: an undo snapshot or other copy of the layer doesn't copy it.
    #[serde(skip)]
    projected: std::sync::Arc<Vec<Option<Geometry>>>,
    /// The active Mercator display level; full projection stays lazy for interaction.
    #[serde(skip)]
    display: Option<std::sync::Arc<lod::Display>>,
    /// Recent unsuccessful display attempts, keyed by their geometry, level and budget inputs.
    #[serde(skip)]
    display_rejected: Vec<lod::Rejected>,
    /// Recent display levels, shared by snapshots and bounded together with `thinned`.
    #[serde(skip)]
    display_levels: Vec<std::sync::Arc<lod::Display>>,
    /// Each projected geometry's extent, for culling without walking its vertices.
    #[serde(skip)]
    projected_extents: std::sync::Arc<Vec<Extent>>,
    /// The projected geometry's line and ring vertices (those thinning can drop).
    #[serde(skip)]
    projected_vertices: usize,
    /// Thinned copies of the projected geometry by level, the latest used first (see [`lod`]);
    /// `None` for a level that draws from the full geometry, thinning saving too little there.
    #[serde(skip)]
    thinned: std::sync::Arc<Vec<(i32, Option<std::sync::Arc<Vec<Option<Geometry>>>>)>>,
    #[serde(skip)]
    projected_rev: u64,
    /// The projection the cache is in.
    #[serde(skip)]
    projected_for: String,
    /// Allocation identities and byte totals, shared with render snapshots without retaining rows.
    #[serde(skip)]
    view_memory: view_memory::Cache,
    /// Geometry families in the current rows, invalidated by copy-on-write edits and window reads.
    #[serde(skip)]
    geometry_kinds: geometry_kinds::Cache,
    /// Bumped whenever geometry changes so caches rebuild.
    #[serde(default)]
    pub geometry_rev: u64,
}

fn yes() -> bool {
    true
}

fn next_layer_instance() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

impl Layer {
    /// Identifies this live layer, independently of its reusable map-local ID.
    pub fn instance_id(&self) -> u64 {
        self.instance_id
    }

    /// Identifies the current source contents independently of ordinary vertex edits.
    pub fn replacement_revision(&self) -> u64 {
        self.replacement_rev
    }

    /// Marks a wholesale data replacement, not an edit to the existing features.
    pub fn touch_replacement(&mut self) {
        self.replacement_rev = next_layer_instance();
    }

    /// Starts a distinct live instance after copying a layer or its owning map.
    /// Undo and render snapshots must retain the original identity instead.
    pub fn renew_instance_id(&mut self) {
        self.instance_id = next_layer_instance();
    }

    /// Baseline rows for a layer loaded for source editing.
    pub fn edit_versions(&self) -> Option<&source::Versions> {
        self.postgis.as_ref().map(|e| &e.versions).or_else(|| self.gpkg.as_ref().map(|e| &e.versions))
    }

    /// Local linked paths, followed by the dataset's record-only provenance.
    pub fn file_paths_mut(&mut self) -> Vec<(&mut String, bool)> {
        let Layer { dataset, source, raster, gpkg, .. } = self;
        let mut paths = Vec::new();
        if let Some(src) = source {
            if let Some(g) = &mut src.gdal {
                paths.push((&mut g.path, false));
            } else if let Some(d) = &mut src.duckdb {
                if let Some(p) = &mut d.database {
                    paths.push((p, false));
                }
            } else if src.embedded.is_none()
                && src.postgis.is_none()
                && src.mysql.is_none()
                && src.sqlserver.is_none()
                && let Some(p) = &mut src.path
            {
                paths.push((p, false));
            }
        }
        if let Some(r) = raster {
            if r.embedded.is_none()
                && let Some(p) = &mut r.path
            {
                paths.push((p, false));
            }
            if let Some(z) = &mut r.time_zarr {
                paths.push((&mut z.store, false));
            }
        }
        if let Some(g) = gpkg {
            paths.push((&mut g.path, false));
        }
        if let Some(p) = &mut dataset.source {
            paths.push((p, true));
        }
        paths.retain(|(p, _)| paths::is_local(p));
        paths
    }

    /// The original local file replaced by Repair Data Source.
    pub fn linked_file(&self) -> Option<&str> {
        let p = if let Some(r) = &self.raster {
            if r.embedded.is_some() {
                return None;
            }
            r.time_zarr.as_ref().map(|z| z.store.as_str()).or(r.path.as_deref())
        } else {
            self.source.as_ref().and_then(source::SourceLayer::linked_file)
        };
        p.filter(|p| paths::is_local(p))
    }

    /// The dataset within the linked container.
    pub fn linked_table(&self) -> Option<&str> {
        self.raster.as_ref().and_then(|r| r.subdataset.as_deref()).or_else(|| {
            self.source.as_ref().and_then(|s| {
                s.table
                    .as_deref()
                    .or_else(|| s.gdal.as_ref().map(|g| g.layer.as_str()))
                    .or_else(|| s.duckdb.as_ref().and_then(|d| d.table.as_deref()))
            })
        })
    }

    /// Installs a fresh source, retaining styling except an untouched no-geometry default.
    /// Clears feature-specific state whose ids may identify different features, returning the
    /// number of saved label moves reset for a notice.
    pub fn replace_source(&mut self, mut src: source::SourceLayer) -> usize {
        self.selection.clear();
        let moved = self.labels.in_order().map(|class| class.moved.len()).sum();
        self.labels.default.moved.clear();
        self.labels.classes.iter_mut().for_each(|class| class.moved.clear());
        src.reads = self.source.as_ref().map_or(0, |old| old.reads);
        if let Some(info) = src.info() {
            if self.dataset.geometry_type == rustgis_data::GeometryType::None
                && info.geometry_type != rustgis_data::GeometryType::None
                && let Renderer::Single(sym) = &self.renderer
                && let Some(seed) = sym.default_seed(rustgis_data::GeometryType::None)
            {
                self.renderer = Renderer::Single(Symbol::default_for(info.geometry_type, seed));
            }
            self.dataset.geometry_type = info.geometry_type;
            self.dataset.fields = info.fields.clone();
            self.dataset.crs = info.coordinate_system();
            // The new source's Z and M, which reading it drops (see `Dataset::zm_dropped`).
            self.dataset.zm_dropped = info.zm_dropped;
        }
        self.dataset.source = src.linked_file().map(String::from);
        self.source = Some(src);
        self.touch_replacement();
        self.install(Vec::new());
        moved
    }

    /// The dataset, streamed, raster and time-following Zarr addresses used to read this layer
    /// again, in that order. Local paths are included; database origins stay password-free.
    pub fn addresses_mut(&mut self) -> impl Iterator<Item = &mut String> {
        self.dataset.source.iter_mut().chain(self.source.iter_mut().filter_map(|source| source.path.as_mut())).chain(
            self.raster.iter_mut().flat_map(|raster| raster.path.iter_mut().chain(raster.time_zarr.iter_mut().map(|zarr| &mut zarr.store))),
        )
    }

    pub fn new(id: u32, dataset: Dataset) -> Self {
        let renderer = Renderer::Single(Symbol::default_for(dataset.geometry_type, id));
        Layer {
            id,
            instance_id: next_layer_instance(),
            replacement_rev: 0,
            name: dataset.name.clone(),
            visible: true,
            labels: Labeling::default_for(&dataset),
            dataset,
            renderer,
            transparency: 0.0,
            min_scale: None,
            max_scale: None,
            definition_query: None,
            time: None,
            selection: Vec::new(),
            expanded: true,
            selectable: true,
            graphics: false,
            blend: effects::Blend::Normal,
            mask: None,
            display_filters: Vec::new(),
            aggregation: None,
            extrusion: None,
            line_strokes: Vec::new(),
            fill_layers: Vec::new(),
            no_fill: None,
            stroke_levels: false,
            marker_rotation: None,
            diagram: None,
            symbol_by: Default::default(),
            reference_scale: None,
            priority: 0,
            design: design::Design::default(),
            raster: None,
            source: None,
            postgis: None,
            gpkg: None,
            connection: String::new(),
            anonymous: false,
            projected: Default::default(),
            display: None,
            display_rejected: Vec::new(),
            display_levels: Vec::new(),
            projected_extents: Default::default(),
            projected_vertices: 0,
            thinned: Default::default(),
            projected_rev: u64::MAX,
            projected_for: String::new(),
            geometry_rev: 0,
            view_memory: Default::default(),
            geometry_kinds: Default::default(),
        }
    }

    /// A raster layer.
    pub fn new_raster(id: u32, name: impl Into<String>, raster: RasterLayer) -> Self {
        let name = name.into();
        let mut l = Layer::new(id, Dataset::new(name.clone(), GeometryType::None, Vec::new()));
        l.selectable = false;
        l.labels.enabled = false;
        l.raster = Some(raster);
        l
    }

    /// A graphics layer.
    pub fn new_graphics(id: u32, name: &str) -> Self {
        let mut l = Layer::new(id, graphics::dataset(name));
        l.graphics = true;
        l.labels.enabled = false;
        l
    }

    /// A streamed layer: a large file read in place, a view at a time.
    pub fn new_streamed(id: u32, source: source::SourceLayer) -> Self {
        let (name, ty, fields, crs) = match source.info() {
            Some(i) => (i.name.clone(), i.geometry_type, i.fields.clone(), i.coordinate_system()),
            None => ("layer".to_string(), GeometryType::None, Vec::new(), None),
        };
        let mut ds = Dataset::new(name, ty, fields);
        ds.crs = crs;
        ds.windowed = true;
        ds.source = source
            .path
            .clone()
            .or_else(|| source.file_name.clone())
            .or_else(|| source.postgis.as_ref().map(|p| p.database.clone()))
            .or_else(|| source.mysql.as_ref().map(|m| m.database.clone()))
            .or_else(|| source.sqlserver.as_ref().map(|m| m.database.clone()));
        let mut l = Layer::new(id, ds);
        l.source = Some(source);
        l
    }

    pub fn is_streamed(&self) -> bool {
        self.source.is_some()
    }

    /// Keeps a streamed layer's features up with the view (longitude/latitude): takes a finished
    /// read, and starts one when the view has moved beyond what was read. With `block`, reads in
    /// place and waits (for exports). Returns whether a read is still running.
    pub fn sync_source(&mut self, view: &Extent, block: bool) -> bool {
        let Some(src) = &mut self.source else { return false };
        if !src.is_open() {
            // Opened once after a project loads; a failure is kept, not retried every frame.
            if src.error.is_some() || src.open().is_err() {
                return false;
            }
            // A source by its web address opens off the frame: an export waits for it.
            if block {
                src.wait_open(std::time::Duration::from_secs(60));
            }
            if !src.is_open() {
                return src.is_pending();
            }
        }
        // A project that lists no fields for a streamed layer (one written by another app, which
        // can't know which columns the reader takes) gets the source's: reads keep the attributes
        // in that order, and a renderer on a field finds it.
        if self.dataset.fields.is_empty()
            && let Some(info) = self.source.as_ref().and_then(|s| s.info())
        {
            self.dataset.fields = info.fields.clone();
        }
        // An opened source's dropped Z or M values, for a project saved without the mark. Added to,
        // not replaced: a source that couldn't tell (a check that timed out) mustn't clear a mark the
        // project kept, and a stale one gives one notice too many rather than one too few.
        if let Some(info) = self.source.as_ref().and_then(|s| s.info()) {
            self.dataset.zm_dropped |= info.zm_dropped;
        }
        let Some(src) = &mut self.source else { return false };
        src.window_fields = Some(self.dataset.fields.len());
        // The view keeps up to its limit of the features the query keeps (#1456).
        src.set_query(self.definition_query.as_deref(), &self.dataset.fields);
        // A failed read leaves nothing drawn for it (its error shows in Layers), not the
        // previous view's features.
        match src.poll(block) {
            source::Polled::Done(_, batch) => self.install(batch.features),
            source::Polled::Failed(_) => self.install(Vec::new()),
            source::Polled::Idle | source::Polled::Waiting => {}
        }
        let Some(src) = &mut self.source else { return false };
        if let Some(e) = src.wanted(view) {
            src.start(e, block);
            match src.poll(block) {
                source::Polled::Done(_, batch) => self.install(batch.features),
                source::Polled::Failed(_) => self.install(Vec::new()),
                // A thread that couldn't start leaves an error and no read.
                source::Polled::Idle if src.error.is_some() => self.install(Vec::new()),
                source::Polled::Idle | source::Polled::Waiting => {}
            }
        }
        self.source.as_ref().is_some_and(|s| s.is_pending())
    }

    /// Replaces a streamed layer's features, keeping the selection (by feature id).
    fn install(&mut self, features: Vec<rustgis_data::Feature>) {
        let areas = self.source.as_ref().and_then(|source| source.ready_cache.as_ref()).map(|cache| cache.areas).unwrap_or_default();
        let mut ids: std::collections::HashSet<u64> =
            self.selection.iter().filter_map(|r| self.dataset.features.get(*r)).map(|f| f.id).collect();
        if let Some(src) = &mut self.source {
            ids.extend(std::mem::take(&mut src.saved_selection));
        }
        let fields = self.dataset.fields.len();
        self.clear_cache();
        self.dataset.features = features.into();
        let mut padded = false;
        for f in &mut self.dataset.features {
            if f.attrs.len() != fields {
                f.attrs.resize(fields, rustgis_data::Value::Null);
                padded = true;
            }
        }
        self.dataset.windowed = true;
        if let Some(src) = &mut self.source {
            src.reads = src.reads.wrapping_add(1);
        }
        self.selection = if ids.is_empty() {
            Vec::new()
        } else {
            self.dataset.features.iter().enumerate().filter(|(_, f)| ids.contains(&f.id)).map(|(i, _)| i).collect()
        };
        self.touch_geometry();
        if let Some(cache) = self.source.as_mut().and_then(|s| s.ready_cache.take()) {
            self.projected = cache.projected;
            self.projected_extents = cache.extents;
            self.projected_vertices = cache.vertices;
            self.projected_for = cache.key;
            self.projected_rev = self.geometry_rev;
            let feature_bytes =
                if padded { rustgis_data::source::memory::features_bytes(&self.dataset.features) } else { cache.feature_bytes };
            self.view_memory.seed(&self.dataset.features, &self.projected, feature_bytes, cache.projected_bytes);
        }
        if let Some(source) = &mut self.source {
            source.area_counts = areas;
            source.area_features = Some(self.dataset.features.allocation_key());
        }
    }

    /// Allocated row, projected-geometry, extent and optional display storage for this layer.
    /// Snapshots report the same shared allocations; totals are memoized until mutable access
    /// changes an allocation identity, including edits through the public dataset API. Stale
    /// allocations still count until the next preparation or explicit cache clear releases them.
    pub fn view_cache_bytes(&self) -> usize {
        self.view_memory
            .total(&self.dataset.features, &self.projected, &self.projected_extents, &self.thinned)
            .saturating_add(self.display_levels.iter().map(|d| d.bytes).sum::<usize>())
    }

    /// Every feature of a streamed layer's source (up to the read limit), for tools and for
    /// loading the layer into memory.
    pub fn load_all(&self) -> Result<Dataset, String> {
        self.load_all_counted().map(|(ds, _)| ds)
    }

    /// [`Layer::load_all`], with the number of rows of the source that couldn't be read.
    pub fn load_all_counted(&self) -> Result<(Dataset, usize), String> {
        let Some(copy) = self.open_source()? else { return Ok((self.dataset.clone(), 0)) };
        let (mut ds, report) = copy.read_all()?;
        ds.name = self.dataset.name.clone();
        ds.zm_dropped |= self.dataset.zm_dropped;
        if ds.features.is_empty() && ds.geometry_type == rustgis_data::GeometryType::None {
            ds.geometry_type = if self.dataset.geometry_type != rustgis_data::GeometryType::None {
                self.dataset.geometry_type
            } else {
                copy.info().map_or(rustgis_data::GeometryType::None, |info| info.geometry_type)
            };
        }
        // (A project that lists no fields keeps the source's, as `sync_source` gives it.)
        if !self.dataset.fields.is_empty() {
            ds.fields = self.dataset.fields.clone();
        }
        ds.source = self.dataset.source.clone();
        Ok((ds, report.skipped))
    }

    /// A streamed layer's source, to export whole a chunk at a time, with a dataset of no
    /// features naming them as [`Layer::load_all`] would. `None` for a layer not
    /// read in place, or whose source can't be read so (one by its web address).
    pub fn chunk_export(&self) -> Result<Option<(std::sync::Arc<dyn rustgis_data::source::FeatureSource>, Dataset)>, String> {
        let Some(copy) = self.open_source()? else { return Ok(None) };
        let Some(src) = copy.visit_source() else { return Ok(None) };
        let info = src.info();
        // (A reload that read no rows leaves the reader without a type: the layer's declared one
        // stands, as in `load_all_counted`.)
        let geometry_type =
            if info.geometry_type == rustgis_data::GeometryType::None && self.dataset.geometry_type != rustgis_data::GeometryType::None {
                self.dataset.geometry_type
            } else {
                info.geometry_type
            };
        let mut ds = Dataset::new(self.dataset.name.clone(), geometry_type, info.fields.clone());
        ds.crs = info.coordinate_system();
        // What reading the source drops, the export lacks (see `Dataset::zm_dropped`).
        ds.zm_dropped = info.zm_dropped || self.dataset.zm_dropped;
        if !self.dataset.fields.is_empty() {
            ds.fields = self.dataset.fields.clone();
        }
        // (A source declaring no geometry type takes its first geometry's when exported, as a
        // whole read does.)
        ds.source = self.dataset.source.clone();
        Ok(Some((src, ds)))
    }

    /// A copy of a streamed layer's source, opened (waiting for one still opening, as an export
    /// does); `None` for a layer not read in place.
    fn open_source(&self) -> Result<Option<source::SourceLayer>, String> {
        let Some(src) = &self.source else { return Ok(None) };
        let mut copy = src.clone();
        if !copy.is_open() {
            copy.open()?;
            // A source by its web address may still be opening in the background (the view or the
            // project started it): wait for it, as an export does, rather than read nothing.
            copy.wait_open(std::time::Duration::from_secs(30));
            if !copy.is_open() {
                return Err(copy
                    .error
                    .clone()
                    .unwrap_or_else(|| "the layer's source is still opening: try again once it has opened".into()));
            }
        }
        Ok(Some(copy))
    }

    /// The symbol a row draws with.
    pub fn symbol(&self, row: usize) -> Symbol {
        if self.graphics { graphics::symbol(&self.dataset, row) } else { self.renderer.symbol_for(&self.dataset, row) }
    }

    /// How far a point row's marker is turned, in degrees clockwise ([`Layer::marker_rotation`]): 0
    /// without, or where the row's value isn't a number.
    pub fn marker_turn(&self, row: usize) -> f32 {
        let Some(r) = self.marker_rotation.as_ref().filter(|_| !self.graphics) else { return 0.0 };
        r.turn(&self.dataset, row)
    }

    /// Its per-row style for one drawing at the map scale 1:`map_scale` (none: no map, as in an
    /// export of the features): [`Layer::symbol`] and the rest with [`Layer::symbol_by`]'s
    /// expressions applied, compiled once here.
    pub fn styler(&self, map_scale: Option<f64>) -> symbol_by::Styler<'_> {
        symbol_by::Styler::new(self, map_scale)
    }

    /// The stacked strokes a line feature row is drawn with: its class's own
    /// ([`Renderer::strokes_for`]), else the layer's `line_strokes`; none (the single stroke) for
    /// a graphics layer.
    pub fn strokes(&self, row: usize) -> &[LineStroke] {
        if self.graphics {
            return &[];
        }
        // Most layers have no class with strokes of its own: no need to class the feature.
        if !self.renderer.has_class_strokes() {
            return &self.line_strokes;
        }
        self.renderer.strokes_for(&self.dataset, row).unwrap_or(&self.line_strokes)
    }

    /// A row's symbol and stacked strokes ([`Layer::symbol`], [`Layer::strokes`]), the feature
    /// classed once for both.
    pub fn style(&self, row: usize) -> (Symbol, &[LineStroke]) {
        if self.graphics || !self.renderer.has_class_strokes() {
            return (self.symbol(row), self.strokes(row));
        }
        let (symbol, strokes) = self.renderer.style_for(&self.dataset, row);
        (symbol, strokes.unwrap_or(&self.line_strokes))
    }

    /// How much its symbol and label sizes are multiplied by at the map scale 1:`scale`: its
    /// reference scale over that ([`Layer::reference_scale`]), kept within [`SIZE_FACTORS`]; 1
    /// without one.
    pub fn size_factor(&self, scale: f64) -> f32 {
        match self.reference_scale {
            Some(r) if scale.is_finite() && scale > 0.0 => (r / scale).clamp(SIZE_FACTORS.0, SIZE_FACTORS.1) as f32,
            _ => 1.0,
        }
    }

    /// Whether some feature may be drawn with stacked strokes ([`Layer::strokes`]).
    pub fn has_strokes(&self) -> bool {
        !self.graphics && (!self.line_strokes.is_empty() || self.renderer.has_class_strokes())
    }

    /// A polygon row's stacked fills: its class's own, else the layer's `fill_layers` (none: its
    /// symbol's single fill).
    pub fn fills(&self, row: usize) -> &[fills::FillLayer] {
        if self.graphics {
            return &[];
        }
        // A class without fills of its own takes the layer's: a layer with no class fills isn't
        // classed for them.
        if !self.renderer.has_class_fills() {
            return &self.fill_layers;
        }
        self.renderer.fills_for(&self.dataset, row).unwrap_or(&self.fill_layers)
    }

    /// A polygon row's symbol, stacked strokes and stacked fills ([`Layer::style`],
    /// [`Layer::fills`]), the feature classed once for all three.
    pub fn style_and_fills(&self, row: usize) -> (Symbol, &[LineStroke], &[fills::FillLayer]) {
        if self.graphics || !self.renderer.has_class_fills() {
            let (symbol, strokes) = self.style(row);
            return (symbol, strokes, self.fills(row));
        }
        let (symbol, strokes, fills) = self.renderer.style_fills_for(&self.dataset, row);
        (symbol, strokes.unwrap_or(&self.line_strokes), fills.unwrap_or(&self.fill_layers))
    }

    /// [`Layer::style_and_fills`] with its renderer's class expression compiled (`c`, from
    /// [`Renderer::classer`]), as a drawing of many features takes it.
    pub fn style_and_fills_with(&self, row: usize, c: &Classer) -> (Symbol, &[LineStroke], &[fills::FillLayer]) {
        if self.graphics || self.renderer.expression().is_none() {
            return self.style_and_fills(row);
        }
        let (symbol, strokes, fills) = self.renderer.style_fills_with(&self.dataset, row, c);
        (symbol, strokes.unwrap_or(&self.line_strokes), fills.unwrap_or(&self.fill_layers))
    }

    /// Whether some feature may be drawn with stacked fills ([`Layer::fills`]).
    pub fn has_fills(&self) -> bool {
        !self.graphics && (!self.fill_layers.is_empty() || self.renderer.has_class_fills())
    }

    pub fn is_raster(&self) -> bool {
        self.raster.is_some()
    }

    pub fn geometry_type(&self) -> GeometryType {
        self.dataset.geometry_type
    }

    /// The declared family or one present in the loaded features. A mixed layer's dominant
    /// family does not describe every feature; streamed layers also expose their current window.
    /// Derived from row allocation identity, so repeated UI checks do not rescan the layer.
    pub fn has_geometry(&self, kind: GeometryType) -> bool {
        kind != GeometryType::None && (self.geometry_type() == kind || self.geometry_kinds.contains(&self.dataset.features, kind))
    }

    /// Copies of every geometry in a map projection's coordinates (`key` names the projection),
    /// rebuilt lazily when the geometry or the projection changes. Points a projection can't show
    /// become NaN, which the renderer skips; features across a world projection's edge are cut
    /// there ([`projection::Flat::project`]). There is one entry per feature, row for row, but its
    /// parts and vertices needn't match the source's (a cut feature has more parts, and a point
    /// the projection can't use breaks a line): hit-testing and snapping use the shape, never a
    /// vertex index, which only the source geometry has.
    pub fn projected_in(&mut self, flat: &projection::Flat, key: &str) -> &[Option<Geometry>] {
        if self.projected_rev != self.geometry_rev || self.projected.len() != self.dataset.features.len() || self.projected_for != key {
            self.display = None;
            if !matches!(flat, projection::Flat::Mercator) {
                self.display_levels.clear();
            }
            let projected: Vec<Option<Geometry>> =
                self.dataset.features.iter().map(|ft| ft.geometry.as_ref().map(|g| flat.project(g))).collect();
            self.projected_extents =
                std::sync::Arc::new(projected.iter().map(|g| g.as_ref().map_or(Extent::EMPTY, Geometry::extent)).collect());
            self.projected_vertices = projected.iter().flatten().map(lod::thinnable).sum();
            self.projected = std::sync::Arc::new(projected);
            self.thinned = Default::default();
            self.projected_rev = self.geometry_rev;
            self.projected_for = key.to_string();
            self.trim_display_levels(self.lod_budget());
        }
        &self.projected
    }

    /// Web Mercator copies of every geometry (see [`Layer::projected_in`]).
    pub fn projected(&mut self) -> &[Option<Geometry>] {
        self.projected_in(&projection::Flat::Mercator, "mercator")
    }

    /// Prepares a bounded Mercator display copy without filling the exact projection cache.
    /// Callers needing labels, selection or other exact positions must use `thinned_in` instead.
    /// `whole_layer_eligible` applies the complete source's LOD decision to each streamed chunk.
    /// Returns false for unsupported projections, small layers, weak savings or a full budget.
    /// Every failure clears the active display; recent reusable levels may remain cached.
    pub fn prepare_display(&mut self, flat: &projection::Flat, tol: f64, whole_layer_eligible: bool) -> bool {
        self.display = None;
        let Some(level) = lod::level(tol).filter(|_| matches!(flat, projection::Flat::Mercator)) else { return false };
        self.trim_thinned_levels(self.lod_budget());
        let used: usize = self.thinned.iter().filter_map(|(_, c)| c.as_ref()).map(|c| lod::cache_bytes(c)).sum();
        let budget = self.lod_budget().saturating_sub(used);
        self.trim_display_levels(budget);
        if let Some(i) = self.display_levels.iter().position(|d| d.level == level && d.whole_layer_eligible == whole_layer_eligible) {
            let hit = self.display_levels.remove(i);
            self.display = Some(hit.clone());
            self.display_levels.insert(0, hit);
            return true;
        }
        let attempt = lod::Rejected { revision: self.geometry_rev, level, rows: self.dataset.features.len(), budget, whole_layer_eligible };
        if let Some(i) = self.display_rejected.iter().position(|a| *a == attempt) {
            let hit = self.display_rejected.remove(i);
            self.display_rejected.insert(0, hit);
            return false;
        }
        let Some(display) = lod::display(&self.dataset.features, self.geometry_rev, level, budget, whole_layer_eligible) else {
            self.display_rejected.insert(0, attempt);
            self.display_rejected.truncate(lod::LEVELS_KEPT);
            return false;
        };
        let display = std::sync::Arc::new(display);
        self.display_levels.insert(0, display.clone());
        self.display_levels.truncate(lod::LEVELS_KEPT);
        self.trim_display_levels(budget);
        self.display = Some(display);
        true
    }

    /// Shared optional LOD storage remaining after original rows and exact projection caches.
    fn lod_budget(&self) -> usize {
        self.source.as_ref().map_or(lod::CACHE_BYTES, |s| {
            s.budget()
                .saturating_sub(self.view_memory.base(&self.dataset.features, &self.projected, &self.projected_extents))
                .min(lod::CACHE_BYTES)
        })
    }

    /// Drops stale levels and evicts the least recently used copies to fit the supplied budget.
    fn trim_display_levels(&mut self, budget: usize) {
        let mut bytes = 0usize;
        self.display_levels.retain(|d| {
            if d.revision != self.geometry_rev
                || d.geometries.len() != self.dataset.features.len()
                || bytes.saturating_add(d.bytes) > budget
            {
                return false;
            }
            bytes += d.bytes;
            true
        });
    }

    /// Reclaims optional exact-consumer LOD copies when the shared remaining budget shrinks.
    fn trim_thinned_levels(&mut self, budget: usize) {
        let used = self.thinned.iter().filter_map(|(_, c)| c.as_ref()).fold(0usize, |n, c| n.saturating_add(lod::cache_bytes(c)));
        if used <= budget {
            return;
        }
        let mut bytes = 0usize;
        std::sync::Arc::make_mut(&mut self.thinned).retain(|(_, copy)| {
            let extra = copy.as_ref().map_or(0, |c| lod::cache_bytes(c));
            if bytes.saturating_add(extra) > budget {
                return false;
            }
            bytes += extra;
            true
        });
    }

    /// The copy and original extents from a successful `prepare_display`, row for row.
    pub fn display_cache(&self) -> Option<(&[Option<Geometry>], &[Extent])> {
        self.display
            .as_ref()
            .filter(|d| d.revision == self.geometry_rev && d.geometries.len() == self.dataset.features.len())
            .map(|d| (d.geometries.as_slice(), d.extents.as_slice()))
    }

    /// The projected geometry (see [`Layer::projected_in`]) as drawn at a tolerance in projection
    /// units: a thinned copy at the level just under it (see [`lod`]), made on first use, or the
    /// geometry itself for a small layer. Read it back with [`Layer::thinned_cache`].
    pub fn thinned_in(&mut self, flat: &projection::Flat, key: &str, tol: f64) -> &[Option<Geometry>] {
        self.display = None;
        self.projected_in(flat, key);
        self.trim_display_levels(self.lod_budget());
        let display_bytes: usize = self.display_levels.iter().map(|d| d.bytes).sum();
        let budget = self.lod_budget().saturating_sub(display_bytes);
        self.trim_thinned_levels(budget);
        let Some(level) = lod::level(tol).filter(|_| self.projected_vertices > lod::THIN_ABOVE) else { return &self.projected };
        let levels = std::sync::Arc::make_mut(&mut self.thinned);
        match levels.iter().position(|(l, _)| *l == level) {
            Some(i) => {
                let hit = levels.remove(i);
                levels.insert(0, hit);
            }
            None => {
                let t = lod::tolerance(level);
                let copy = if matches!(flat, projection::Flat::Mercator) {
                    lod::project_budgeted(&self.dataset.features, level, budget)
                } else {
                    lod::thin_budgeted(&self.projected, t, budget)
                };
                // The saving counts against every vertex the copy holds, points included.
                let all: usize = self.projected.iter().flatten().map(lod::vertices).sum();
                let copy = copy.filter(|c| {
                    let kept: usize = c.iter().flatten().map(lod::vertices).sum();
                    (kept as f64) <= all as f64 * (1.0 - lod::MIN_SAVING)
                });
                levels.insert(0, (level, copy.map(std::sync::Arc::new)));
                levels.truncate(lod::LEVELS_KEPT);
            }
        }
        self.trim_thinned_levels(budget);
        self.thinned_cache(tol)
    }

    /// The geometry from the last [`Layer::thinned_in`] call at this tolerance, without making
    /// it: the full projected geometry when there is no thinned copy for it.
    pub fn thinned_cache(&self, tol: f64) -> &[Option<Geometry>] {
        let level = lod::level(tol).filter(|_| self.projected_vertices > lod::THIN_ABOVE);
        match level.and_then(|level| self.thinned.iter().find(|(l, _)| *l == level)) {
            Some((_, Some(g))) if g.len() == self.projected.len() => g,
            _ => &self.projected,
        }
    }

    /// Each projected geometry's extent, row for row with [`Layer::projected_cache`].
    pub fn projected_extents(&self) -> &[Extent] {
        &self.projected_extents
    }

    /// The Web Mercator copies from the last [`Layer::projected`] call, without refreshing them:
    /// call `projected` first (it needs `&mut self`) whenever the geometry may have changed.
    pub fn projected_cache(&self) -> &[Option<Geometry>] {
        &self.projected
    }

    /// Drops the projected geometry cache (the feature cache).
    pub fn clear_cache(&mut self) {
        self.display = None;
        self.display_levels.clear();
        self.display_rejected.clear();
        self.projected = Default::default();
        self.projected_extents = Default::default();
        self.projected_vertices = 0;
        self.thinned = Default::default();
        self.projected_rev = u64::MAX;
        self.projected_for.clear();
    }

    /// Takes the projected geometry cache `other` (a copy of this layer, such as one drawn off
    /// the UI thread) filled, when it was filled for the same rows and geometry: sharing it costs
    /// a pointer. Display-only results preserve this layer's exact cache; optional copies are
    /// trimmed to its remaining budget. False when the rows or source settings don't match.
    pub fn adopt_cache(&mut self, other: &Layer) -> bool {
        let fits = other.dataset.features.shares(&self.dataset.features)
            && other.geometry_rev == self.geometry_rev
            && (other.is_cached(&other.projected_for) || other.display_cache().is_some())
            && self.source.as_ref().is_none_or(|source| {
                !source.is_pending()
                    && other.source.as_ref().is_some_and(|old| {
                        old.budget() <= source.budget() && old.projection == source.projection && old.full_read == source.full_read
                    })
            });
        if fits {
            self.display = other.display.clone();
            self.display_rejected = other.display_rejected.clone();
            self.display_levels = other.display_levels.clone();
            if other.is_cached(&other.projected_for) {
                self.projected = other.projected.clone();
                self.projected_extents = other.projected_extents.clone();
                self.projected_vertices = other.projected_vertices;
                self.thinned = other.thinned.clone();
                self.projected_rev = other.projected_rev;
                self.projected_for.clone_from(&other.projected_for);
            }
            let budget = self.lod_budget();
            self.trim_thinned_levels(budget);
            let used: usize = self.thinned.iter().filter_map(|(_, c)| c.as_ref()).map(|c| lod::cache_bytes(c)).sum();
            self.trim_display_levels(budget.saturating_sub(used));
            self.display = self.display.take().filter(|d| self.display_levels.iter().any(|kept| std::sync::Arc::ptr_eq(d, kept)));
        }
        fits
    }

    /// Whether the projected geometry cache is filled for a projection.
    pub fn is_cached(&self, key: &str) -> bool {
        self.projected_for == key && self.projected_rev == self.geometry_rev && self.projected.len() == self.dataset.features.len()
    }

    /// Marks geometry as changed.
    pub fn touch_geometry(&mut self) {
        self.geometry_rev = self.geometry_rev.wrapping_add(1);
    }

    /// Puts the selection in range, in order and with each row once: the state selecting leaves
    /// it in. A selection read from a file goes through this too, or a repeated row would be
    /// counted, and run, twice.
    pub fn normalize_selection(&mut self) {
        let n = self.dataset.features.len();
        self.selection.retain(|r| *r < n);
        self.selection.sort_unstable();
        self.selection.dedup();
    }

    /// Whether the layer draws at a scale denominator.
    pub fn in_scale_range(&self, scale: f64) -> bool {
        self.min_scale.is_none_or(|m| scale <= m) && self.max_scale.is_none_or(|m| scale >= m)
    }

    /// Rows drawn at a scale denominator: those passing the definition query, the time slider's
    /// window and every display filter that applies at that scale (a filter that no longer reads
    /// is skipped).
    pub fn drawn_rows(&self, scale: f64) -> Vec<bool> {
        let mut rows = self.visible_rows();
        if let Some(keep) = self.time.as_ref().and_then(|t| t.keeps(&self.dataset)) {
            for (r, k) in rows.iter_mut().zip(keep.iter()) {
                *r = *r && *k;
            }
        }
        for f in self.display_filters.iter().filter(|f| f.applies(scale)) {
            if let Ok(e) = rustgis_data::query::parse_fields(&f.where_, &self.dataset) {
                for (r, keep) in rows.iter_mut().enumerate() {
                    *keep = *keep && rustgis_data::query::eval(&e, &self.dataset, r);
                }
            }
        }
        rows
    }

    /// The legend rows for what this layer draws at a scale denominator (#207): the classes of the
    /// features its definition query and display filters keep. `None` when it draws nothing there
    /// (out of its scale range, or no feature kept, as in an empty layer), so a legend can leave it out.
    pub fn shown_legend(&self, scale: f64) -> Option<Vec<symbology::LegendEntry>> {
        if !self.in_scale_range(scale) {
            return None;
        }
        // Nothing to filter: every feature is drawn, and no row list needs building.
        let query = self.definition_query.as_deref().is_some_and(|q| !q.trim().is_empty());
        let timed = self.time.as_ref().is_some_and(|t| t.window.is_some());
        if !query && !timed && self.display_filters.is_empty() {
            return (!self.dataset.features.is_empty()).then(|| self.renderer.legend_at(&self.dataset, &[], Some(scale)));
        }
        let rows = self.drawn_rows(scale);
        rows.contains(&true).then(|| self.renderer.legend_at(&self.dataset, &rows, Some(scale)))
    }

    /// Rows passing the definition query (all rows when none or invalid).
    pub fn visible_rows(&self) -> Vec<bool> {
        let n = self.dataset.features.len();
        match self.definition_query.as_deref().filter(|q| !q.trim().is_empty()) {
            Some(q) => match rustgis_data::query::parse_fields(q, &self.dataset) {
                Ok(e) => (0..n).map(|r| rustgis_data::query::eval(&e, &self.dataset, r)).collect(),
                Err(_) => vec![true; n],
            },
            None => vec![true; n],
        }
    }

    /// Extent in longitude/latitude.
    pub fn extent(&self) -> Extent {
        match (&self.raster, &self.source) {
            (Some(r), _) => r.data.as_ref().map(|d| d.extent()).unwrap_or(Extent::EMPTY),
            // A streamed layer's whole extent, not just the part read.
            (None, Some(s)) => s.info().map(|i| i.extent).filter(|e| !e.is_empty()).unwrap_or_else(|| self.dataset.extent()),
            (None, None) => self.dataset.extent(),
        }
    }

    /// Extent in longitude/latitude of the features the definition query lets through (the whole
    /// extent without a query, for rasters and for streamed layers, whose rows are only those read).
    pub fn shown_extent(&self) -> Extent {
        let query = self.definition_query.as_deref().is_some_and(|q| !q.trim().is_empty());
        if !query || self.raster.is_some() || self.source.is_some() {
            return self.extent();
        }
        self.visible_rows()
            .iter()
            .zip(&self.dataset.features)
            .filter(|(v, _)| **v)
            .filter_map(|(_, f)| f.geometry.as_ref())
            .fold(Extent::EMPTY, |e, g| e.union(&g.extent()))
    }

    pub fn selection_extent(&self) -> Extent {
        self.selection
            .iter()
            .filter_map(|r| self.dataset.features.get(*r))
            .filter_map(|f| f.geometry.as_ref())
            .fold(Extent::EMPTY, |e, g| e.union(&g.extent()))
    }

    pub fn opacity(&self) -> f32 {
        (1.0 - self.transparency / 100.0).clamp(0.0, 1.0)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bookmark {
    pub name: String,
    pub camera: Camera,
    /// What the place is, in the user's words (#351).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

impl Bookmark {
    pub fn new(name: impl Into<String>, camera: Camera) -> Self {
        Bookmark { name: name.into(), camera, description: String::new() }
    }
}

/// What [`Map::layer_mark`] records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayerMark {
    ids: Vec<u32>,
    next: u32,
}

/// Feature collections imported together from one OGC service. Children remain ordinary
/// layers, sharing rendering, legends, tables and editing with file imports.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServiceGroup {
    pub id: u32,
    pub name: String,
    pub url: String,
    pub kind: String,
    /// Plugin-selected service: never adopt a saved login, even without children or after reopen.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub anonymous: bool,
    #[serde(default = "yes")]
    pub expanded: bool,
    pub children: Vec<ServiceChild>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServiceChild {
    pub layer: u32,
    /// The collection identifier, independent of its display name.
    pub collection: String,
    pub source_url: String,
}

/// Most layer groups a map keeps (files are hostile).
pub const MAX_GROUPS: usize = 2000;

/// Most levels layer groups nest (#836): a top-level group is level 1.
pub const MAX_GROUP_DEPTH: usize = 8;

/// Related layers under one name (#413): a collapsible folder in the Layers pane whose
/// visibility, transparency and scale range apply on top of each child's own. Children stay
/// ordinary layers, kept together in the drawing order; a layer belongs to one group at most.
/// A group may sit in another (#836, `parent`): its layers, and its subgroups' at any depth,
/// stay together in the drawing order, and every group above it applies too.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LayerGroup {
    pub id: u32,
    pub name: String,
    #[serde(default = "yes")]
    pub visible: bool,
    #[serde(default = "yes")]
    pub expanded: bool,
    /// 0 = opaque, 100 = invisible: multiplies each child's own opacity.
    #[serde(default)]
    pub transparency: f32,
    /// The children draw only when zoomed in beyond 1:`min_scale`, if set.
    #[serde(default)]
    pub min_scale: Option<f64>,
    /// The children draw only when zoomed out beyond 1:`max_scale`, if set.
    #[serde(default)]
    pub max_scale: Option<f64>,
    /// The children's layer ids (the layers directly in it, not its subgroups').
    #[serde(default)]
    pub children: Vec<u32>,
    /// The group this one is in (#836); `None` at the top level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<u32>,
}

impl LayerGroup {
    pub fn new(name: impl Into<String>) -> Self {
        LayerGroup {
            id: 0,
            name: name.into(),
            visible: true,
            expanded: true,
            transparency: 0.0,
            min_scale: None,
            max_scale: None,
            children: Vec::new(),
            parent: None,
        }
    }

    /// Whether the group's children may draw at a scale denominator.
    pub fn in_scale_range(&self, scale: f64) -> bool {
        self.min_scale.is_none_or(|m| scale <= m) && self.max_scale.is_none_or(|m| scale >= m)
    }

    pub fn opacity(&self) -> f32 {
        (1.0 - self.transparency / 100.0).clamp(0.0, 1.0)
    }
}

/// An item at one level of the Layers pane's outline (#836, [`Map::outline`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Outline {
    /// A map layer, by id.
    Layer(u32),
    /// A layer group, by id.
    Group(u32),
}

/// An entry in the drawing order of all a map's layers ([`Map::stack`]), by index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drawn {
    /// A map layer: `Map::layers[i]`.
    Layer(usize),
    /// A web tile layer: `Map::tiles[i]`.
    Tile(usize),
}

/// A map: an ordered list of layers (index 0 draws on top), a basemap and a camera.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Map {
    pub name: String,
    pub layers: Vec<Layer>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub service_groups: Vec<ServiceGroup>,
    /// Layer groups (#413).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<LayerGroup>,
    pub basemap: Basemap,
    /// Web tile basemaps and overlays (index 0 on top), each drawn over the map layer it names
    /// (`TileLayer::over`), or under all of them; see [`Map::stack`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tiles: Vec<TileLayer>,
    pub camera: Camera,
    #[serde(default)]
    pub bookmarks: Vec<Bookmark>,
    #[serde(default = "yes")]
    pub graticule: bool,
    /// Flat map or 3D globe.
    #[serde(default)]
    pub projection: Projection,
    /// Which layers a click identifies.
    #[serde(default)]
    pub popup_mode: PopupMode,
    /// Map-wide label display switches.
    #[serde(default)]
    pub label_display: LabelDisplay,
    /// The labels placed when labels were locked, each with the place it took: only they are
    /// drawn while the lock holds, there, wherever the map moves. Filled by the renderer.
    #[serde(skip)]
    pub label_lock: Option<Vec<(LabelKey, LockedPlace)>>,
    /// Geographic anchors of locked non-repeated line labels. Render cache only, never saved.
    #[serde(skip)]
    pub label_lock_line_anchors: std::collections::HashMap<LabelKey, Coord>,
    /// Relationships, snapping, topology rules and edit settings.
    #[serde(default)]
    pub editing: design::MapEditing,
    /// A layer shown only left of a divider (0–1 across the view): the swipe comparison.
    #[serde(default)]
    pub swipe: Option<(u32, f64)>,
    /// Effects, colour vision, drawing mode, clipping and navigation aids.
    #[serde(default)]
    pub view: view::ViewSettings,
    /// The map's thumbnail (base64 PNG).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<String>,
    /// Animation keyframes: views the animation moves through.
    #[serde(default)]
    pub animation: Vec<view::Keyframe>,
    /// The time slider, when it is on: bound layers show only what falls in its window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<time::TimeSlider>,
    /// The celestial body the map is on (a [`rustgis_geom::Body`] id): its size sets the ground
    /// distances, areas and scale (#223).
    #[serde(default = "earth", skip_serializing_if = "is_earth")]
    pub body: String,
    #[serde(default)]
    next_layer_id: u32,
    /// Process-local identity, kept by undo snapshots but never stored in a project.
    #[serde(skip, default = "next_layer_instance")]
    instance_id: u64,
}

fn earth() -> String {
    rustgis_geom::body::EARTH.id.to_string()
}

/// Only the Earth's own id is left out of a saved map: an id this build doesn't know (a newer
/// version's body) is kept, though the map is drawn on the Earth meanwhile.
fn is_earth(id: &str) -> bool {
    id == rustgis_geom::body::EARTH.id
}

/// Which layers a click in the map identifies.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PopupMode {
    /// The topmost feature under the pointer.
    #[default]
    Topmost,
    /// Every visible layer's feature under the pointer.
    Visible,
    /// Every selectable layer's features under the pointer.
    Selectable,
    /// Only the layer selected in the Layers pane.
    SelectedInContents,
    /// No pop-ups.
    None,
}

impl PopupMode {
    pub const ALL: [PopupMode; 5] =
        [PopupMode::Topmost, PopupMode::Visible, PopupMode::Selectable, PopupMode::SelectedInContents, PopupMode::None];
    pub fn code(self) -> &'static str {
        match self {
            PopupMode::Topmost => "topmost",
            PopupMode::Visible => "visible",
            PopupMode::Selectable => "selectable",
            PopupMode::SelectedInContents => "selectedInContents",
            PopupMode::None => "none",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            PopupMode::Topmost => "Topmost Layer",
            PopupMode::Visible => "Visible Layers",
            PopupMode::Selectable => "Selectable Layers",
            PopupMode::SelectedInContents => "Selected in Layers",
            PopupMode::None => "No Pop-ups",
        }
    }
    pub fn from_code(s: &str) -> Option<PopupMode> {
        PopupMode::ALL.into_iter().find(|m| m.code().eq_ignore_ascii_case(s.trim()))
    }
}

/// Map-wide label switches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabelDisplay {
    /// No labels are drawn (faster navigation).
    #[serde(default)]
    pub paused: bool,
    /// The labelled features stay fixed as the map moves.
    #[serde(default)]
    pub locked: bool,
    /// Labels that didn't fit are drawn too, in red.
    #[serde(default)]
    pub show_unplaced: bool,
}

impl Map {
    /// All local linked files and record-only provenance in serialization order.
    pub fn file_paths_mut(&mut self) -> Vec<(&mut String, bool)> {
        let mut paths = Vec::new();
        for tile in &mut self.tiles {
            if tile.archive_file().is_some() {
                paths.push((&mut tile.url, false));
            }
        }
        for layer in &mut self.layers {
            paths.extend(layer.file_paths_mut());
        }
        paths
    }

    pub fn new(name: impl Into<String>) -> Self {
        Map {
            name: name.into(),
            layers: Vec::new(),
            service_groups: Vec::new(),
            groups: Vec::new(),
            basemap: Basemap::Paper,
            tiles: Vec::new(),
            camera: Camera::world(),
            bookmarks: Vec::new(),
            graticule: true,
            projection: Projection::WebMercator,
            popup_mode: PopupMode::Topmost,
            label_display: LabelDisplay::default(),
            label_lock: None,
            label_lock_line_anchors: Default::default(),
            editing: design::MapEditing::default(),
            swipe: None,
            view: view::ViewSettings::default(),
            thumbnail: None,
            animation: Vec::new(),
            time: None,
            body: earth(),
            next_layer_id: 1,
            instance_id: next_layer_instance(),
        }
    }

    /// A copy to draw elsewhere (an export on a thread of its own): its layers keep this map's
    /// identities, and its streamed layers read for themselves (see [`source::SourceLayer::detach_read`]).
    pub fn detached(&self) -> Map {
        let mut copy = self.clone();
        copy.detach_reads();
        copy
    }

    /// [`Map::detached`] for a map that is already a copy: its streamed layers leave a read under
    /// way to the map it was copied from.
    pub fn detach_reads(&mut self) {
        for source in self.layers.iter_mut().filter_map(|l| l.source.as_mut()) {
            source.detach_read();
        }
    }

    /// Identifies this live map, whatever its name or place among the project's maps.
    pub fn instance_id(&self) -> u64 {
        self.instance_id
    }

    /// Starts a distinct live map after copying one. Undo snapshots keep the original identity.
    pub fn renew_instance_id(&mut self) {
        self.instance_id = next_layer_instance();
    }

    /// Every address the map keeps to read its data again (service groups and their layers'
    /// sources, tile layers and their dated, feature-info and legend addresses, terrain tiles,
    /// layers' dataset, streamed and raster sources, time-following Zarr stores). Local paths come too.
    pub fn addresses_mut(&mut self) -> Vec<&mut String> {
        let mut addresses = Vec::new();
        for group in &mut self.service_groups {
            addresses.push(&mut group.url);
            for child in &mut group.children {
                addresses.push(&mut child.source_url);
            }
        }
        for tile in &mut self.tiles {
            addresses.push(&mut tile.url);
            if let Some(url) = &mut tile.time_url {
                addresses.push(url);
            }
            for (_, url) in &mut tile.time_urls {
                addresses.push(url);
            }
            if let Some(info) = &mut tile.feature_info {
                addresses.push(&mut info.url);
            }
            // (A legend's address is built from the service's, its key or password included.)
            for legend in &mut tile.legends {
                addresses.push(&mut legend.url);
            }
        }
        if let Some(view::Terrain { source: view::TerrainSource::Tiles { url, .. }, .. }) = &mut self.view.terrain {
            addresses.push(url);
        }
        for layer in &mut self.layers {
            addresses.extend(layer.addresses_mut());
        }
        addresses
    }

    /// A nonzero id no layer, service group or layer group has: past the highest in use, or (when ids reach
    /// `u32::MAX`, as a project file may have them) the lowest free one.
    fn next_id(&mut self) -> u32 {
        let used =
            || self.layers.iter().map(|l| l.id).chain(self.service_groups.iter().map(|g| g.id)).chain(self.groups.iter().map(|g| g.id));
        let preferred = self.next_layer_id.max(used().max().unwrap_or(0).saturating_add(1));
        let id = if preferred != 0 && !used().any(|id| id == preferred) {
            preferred
        } else {
            let taken: std::collections::HashSet<u32> = used().collect();
            // Among the first `taken.len() + 1` nonzero ids one is free.
            (1..=u32::MAX).take(taken.len().saturating_add(1)).find(|id| !taken.contains(id)).unwrap_or(1)
        };
        self.next_layer_id = id.saturating_add(1);
        id
    }

    /// Applies the time slider: bound layers keep the features in its window (all of them when it
    /// is off), and tile layers with a dated URL show the current step's tiles.
    pub fn apply_time(&mut self) {
        if let Some(t) = self.time.as_mut() {
            t.normalize();
        }
        let window = self.time.as_ref().map(time::TimeSlider::window);
        for l in &mut self.layers {
            let Layer { time, dataset, .. } = l;
            if let Some(b) = time.as_mut() {
                b.apply(dataset, window);
            }
        }
        for t in &mut self.tiles {
            t.apply_time(self.time.as_ref());
        }
    }

    pub fn add_service_group(&mut self, mut group: ServiceGroup) -> u32 {
        group.id = self.next_id();
        let id = group.id;
        self.service_groups.push(group);
        id
    }

    /// Adds a layer group (its children are taken from any other group and gathered together,
    /// in its parent when it has one); returns its id. A parent that's missing, or with no room
    /// for another level, is dropped.
    pub fn add_group(&mut self, mut group: LayerGroup) -> u32 {
        group.id = self.next_id();
        let id = group.id;
        if group.parent.is_some_and(|p| self.group(p).is_none() || self.group_depth(p) >= MAX_GROUP_DEPTH) {
            group.parent = None;
        }
        self.settle_tiles();
        for g in &mut self.groups {
            g.children.retain(|c| !group.children.contains(c));
        }
        let mut inside = vec![id];
        inside.extend(self.chain_ids(group.parent));
        for c in &group.children {
            self.lift_out(&[*c], &inside);
        }
        self.groups.push(group);
        self.gather_chain(id);
        self.settle_tiles();
        id
    }

    /// Moves layers that sit together inside a group they aren't in (`inside`: the groups they
    /// are in, innermost first) to just above that group, so its layers stay together.
    fn lift_out(&mut self, ids: &[u32], inside: &[u32]) {
        let Some(first) = self.layers.iter().position(|l| ids.contains(&l.id)) else { return };
        let order: Vec<u32> = self.layers.iter().map(|l| l.id).filter(|id| !ids.contains(id)).collect();
        let mut at = first.min(order.len());
        while at > 0 && self.splits_between(order.get(at - 1).copied(), order.get(at).copied(), inside) {
            at -= 1;
        }
        if at != first {
            // Moving: the tile layers drawn over them keep their place.
            self.release_tiles(ids);
            let (members, rest): (Vec<Layer>, Vec<Layer>) = std::mem::take(&mut self.layers).into_iter().partition(|l| ids.contains(&l.id));
            self.layers = rest;
            let tail = self.layers.split_off(at.min(self.layers.len()));
            self.layers.extend(members);
            self.layers.extend(tail);
        }
    }

    pub fn group(&self, id: u32) -> Option<&LayerGroup> {
        self.groups.iter().find(|g| g.id == id)
    }

    /// The layer group a layer is directly in, if any.
    pub fn group_of(&self, layer: u32) -> Option<&LayerGroup> {
        self.groups.iter().find(|g| g.children.contains(&layer))
    }

    /// A group and the groups holding it, innermost first: at most [`MAX_GROUP_DEPTH`], ending at
    /// a parent that's missing or already listed (a loop, as a project file may have).
    pub fn group_chain(&self, group: u32) -> Vec<&LayerGroup> {
        let mut out: Vec<&LayerGroup> = Vec::new();
        let mut next = Some(group);
        while let Some(id) = next {
            if out.len() >= MAX_GROUP_DEPTH || out.iter().any(|g| g.id == id) {
                break;
            }
            let Some(g) = self.group(id) else { break };
            out.push(g);
            next = g.parent;
        }
        out
    }

    /// The groups a layer is in, its own first (none outside a group).
    pub fn groups_of(&self, layer: u32) -> Vec<&LayerGroup> {
        self.group_of(layer).map_or_else(Vec::new, |g| self.group_chain(g.id))
    }

    /// The ids of a group's chain ([`Map::group_chain`]); none for `None`.
    fn chain_ids(&self, group: Option<u32>) -> Vec<u32> {
        group.map_or_else(Vec::new, |g| self.group_chain(g).iter().map(|g| g.id).collect())
    }

    /// How many levels deep a group is: 1 at the top level.
    pub fn group_depth(&self, group: u32) -> usize {
        self.group_chain(group).len()
    }

    /// Whether a layer is in a group, directly or in one of its subgroups.
    pub fn layer_in(&self, layer: u32, group: u32) -> bool {
        self.groups_of(layer).iter().any(|g| g.id == group)
    }

    /// Each group's chain of ids, itself first: for loops over many layers or groups.
    fn chains(&self) -> std::collections::HashMap<u32, Vec<u32>> {
        let parent: std::collections::HashMap<u32, Option<u32>> = self.groups.iter().map(|g| (g.id, g.parent)).collect();
        self.groups
            .iter()
            .map(|g| {
                let mut chain = vec![g.id];
                let mut next = g.parent;
                while let Some(p) = next {
                    if chain.len() >= MAX_GROUP_DEPTH || chain.contains(&p) {
                        break;
                    }
                    let Some(up) = parent.get(&p) else { break };
                    chain.push(p);
                    next = *up;
                }
                (g.id, chain)
            })
            .collect()
    }

    /// Each grouped layer's chain of groups, its own first (the first group listing it, as
    /// [`Map::group_of`] finds it).
    fn layer_chains(&self) -> std::collections::HashMap<u32, Vec<u32>> {
        let chains = self.chains();
        let mut out = std::collections::HashMap::new();
        for g in &self.groups {
            let Some(c) = chains.get(&g.id) else { continue };
            for l in &g.children {
                out.entry(*l).or_insert_with(|| c.clone());
            }
        }
        out
    }

    /// The layers in a group and its subgroups at any depth, in drawing order.
    pub fn group_layers(&self, group: u32) -> Vec<u32> {
        let chains = self.layer_chains();
        self.layers.iter().map(|l| l.id).filter(|id| chains.get(id).is_some_and(|c| c.contains(&group))).collect()
    }

    /// A group's subgroups at any depth (not itself).
    pub fn subgroups(&self, group: u32) -> Vec<u32> {
        let chains = self.chains();
        self.groups.iter().map(|g| g.id).filter(|id| *id != group && chains.get(id).is_some_and(|c| c.contains(&group))).collect()
    }

    /// How many levels a group and its subgroups take: 1 without subgroups.
    fn group_height(&self, group: u32) -> usize {
        self.chains().values().filter_map(|c| c.iter().position(|g| *g == group)).max().map_or(1, |p| p + 1)
    }

    /// The layers and groups directly in `parent` (`None`: the top level), as the Layers pane
    /// lists them: in drawing order, top first, then the groups with no layers.
    pub fn outline(&self, parent: Option<u32>) -> Vec<Outline> {
        let chains = self.layer_chains();
        let mut out: Vec<Outline> = Vec::new();
        let mut seen: std::collections::HashSet<Outline> = std::collections::HashSet::new();
        for l in &self.layers {
            let chain = chains.get(&l.id).map_or(&[][..], Vec::as_slice);
            let item = match parent {
                None => chain.last().map_or(Outline::Layer(l.id), |g| Outline::Group(*g)),
                Some(p) => match chain.iter().position(|g| *g == p) {
                    Some(0) => Outline::Layer(l.id),
                    Some(k) => match chain.get(k - 1) {
                        Some(g) => Outline::Group(*g),
                        None => continue,
                    },
                    None => continue,
                },
            };
            if seen.insert(item) {
                out.push(item);
            }
        }
        let tops = self.chains();
        for g in &self.groups {
            // At the top level, a group whose chain ends at it (a missing parent leaves it there).
            let here = match parent {
                None => tops.get(&g.id).and_then(|c| c.last()) == Some(&g.id),
                Some(p) => g.parent == Some(p) && g.id != p,
            };
            if here && seen.insert(Outline::Group(g.id)) {
                out.push(Outline::Group(g.id));
            }
        }
        out
    }

    /// Whether a layer is shown: its own box ticked, and every group's it is in.
    pub fn shows(&self, l: &Layer) -> bool {
        l.visible && self.groups_of(l.id).iter().all(|g| g.visible)
    }

    /// Whether a layer draws at a scale denominator: shown, and within its own scale range and
    /// its groups'.
    pub fn draws_at(&self, l: &Layer, scale: f64) -> bool {
        l.visible && l.in_scale_range(scale) && self.groups_of(l.id).iter().all(|g| g.visible && g.in_scale_range(scale))
    }

    /// Whether a layer is within its own scale range and its groups'.
    pub fn in_scale_range(&self, l: &Layer, scale: f64) -> bool {
        l.in_scale_range(scale) && self.groups_of(l.id).iter().all(|g| g.in_scale_range(scale))
    }

    /// A layer's opacity with its groups' applied.
    pub fn layer_opacity(&self, l: &Layer) -> f32 {
        l.opacity() * self.groups_of(l.id).iter().map(|g| g.opacity()).product::<f32>()
    }

    /// The opacity the groups give each layer in a see-through one (all its groups' multiplied),
    /// by layer id; a layer left out is opaque as far as its groups go.
    pub fn group_opacities(&self) -> std::collections::HashMap<u32, f32> {
        if !self.groups.iter().any(|g| g.transparency > 0.0) {
            return std::collections::HashMap::new();
        }
        let opacity: std::collections::HashMap<u32, f32> = self.groups.iter().map(|g| (g.id, g.opacity())).collect();
        self.layer_chains()
            .into_iter()
            .map(|(l, c)| (l, c.iter().map(|g| opacity.get(g).copied().unwrap_or(1.0)).product::<f32>()))
            .filter(|(_, o)| *o < 1.0)
            .collect()
    }

    /// Ids of the layers whose groups keep them from drawing at a scale denominator (one hidden,
    /// or out of its scale range): for loops that change the layers as they go.
    pub fn held_by_group(&self, scale: f64) -> std::collections::HashSet<u32> {
        let held: std::collections::HashSet<u32> =
            self.groups.iter().filter(|g| !g.visible || !g.in_scale_range(scale)).map(|g| g.id).collect();
        if held.is_empty() {
            return std::collections::HashSet::new();
        }
        // Every group listing a layer counts (a hand-edited file may list it twice).
        let chains = self.chains();
        self.groups
            .iter()
            .filter(|g| chains.get(&g.id).is_some_and(|c| c.iter().any(|x| held.contains(x))))
            .flat_map(|g| g.children.iter().copied())
            .collect()
    }

    /// Folds each group's visibility, transparency and scale range into its layers (its
    /// subgroups' too) and drops the groups: for exports that know only layers.
    pub fn bake_groups(&mut self) {
        self.settle_tiles();
        let chains = self.layer_chains();
        let groups = std::mem::take(&mut self.groups);
        for l in &mut self.layers {
            let Some(chain) = chains.get(&l.id) else { continue };
            for g in chain.iter().filter_map(|id| groups.iter().find(|g| g.id == *id)) {
                l.visible &= g.visible;
                l.transparency = 100.0 * (1.0 - l.opacity() * g.opacity());
                l.min_scale = match (l.min_scale, g.min_scale) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                };
                l.max_scale = match (l.max_scale, g.max_scale) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    (a, b) => a.or(b),
                };
            }
        }
    }

    /// Puts a layer in a group (out of any other), next to the group's other layers in the
    /// drawing order (at its end nearer to the layer); `None` takes it out of its group (above
    /// the outermost group when it was between two of its layers).
    pub fn set_group(&mut self, layer: u32, group: Option<u32>) {
        if group.is_some() && self.group_of(layer).map(|g| g.id) == group {
            return;
        }
        if let Some(gid) = group
            && let Some(before) = self.nearest_end(gid, &[layer])
        {
            // (Checked: the layer and the group exist, and `before` is one of its layers.)
            let _ = self.place_layer(layer, group, before);
            return;
        }
        self.settle_tiles();
        for g in &mut self.groups {
            g.children.retain(|c| *c != layer);
        }
        let inside = self.chain_ids(group);
        self.lift_out(&[layer], &inside);
        if let Some(gid) = group
            && let Some(g) = self.groups.iter_mut().find(|g| g.id == gid)
        {
            g.children.push(layer);
            self.gather_chain(gid);
        }
        self.settle_tiles();
    }

    /// Where layers `moving` (together in the drawing order) join a group at its end nearer to
    /// them: above its topmost other layer (`Some(Some(id))`) when they are above it, else at its
    /// end (`Some(None)`). `None` when the group has no other layers, or they sit among them
    /// already (in a subgroup): they stay there.
    fn nearest_end(&self, group: u32, moving: &[u32]) -> Option<Option<u32>> {
        let others: Vec<u32> = self.group_layers(group).into_iter().filter(|id| !moving.contains(id)).collect();
        let (first, last) = (self.layer_index(*others.first()?)?, self.layer_index(*others.last()?)?);
        let at = self.layers.iter().position(|l| moving.contains(&l.id))?;
        match at {
            a if a < first => Some(others.first().copied()),
            a if a > last => Some(None),
            _ => None,
        }
    }

    /// Below the lowest layer of the first group of `chain` (innermost first) that has any: the
    /// end of a group, or of the nearest group holding it when it has no layers.
    fn end_of(&self, chain: &[u32]) -> Option<usize> {
        chain.iter().find_map(|g| {
            let members = self.group_layers(*g);
            self.layers.iter().rposition(|l| members.contains(&l.id)).map(|i| i + 1)
        })
    }

    /// Puts a layer in a group, or out of any (`None`), and in the drawing order just above the
    /// layer `before` (`None`: below the group's other layers, its subgroups' too, or at the
    /// bottom out of a group). In a group, `before` must be one of its layers or its subgroups';
    /// a place inside a group the layer isn't going in moves up past that group.
    pub fn place_layer(&mut self, layer: u32, group: Option<u32>, before: Option<u32>) -> Result<(), String> {
        if self.layer_index(layer).is_none() {
            return Err("no such layer".into());
        }
        if before == Some(layer) {
            return Err("the layer can't go before itself".into());
        }
        let others: Vec<u32> = match group {
            Some(g) => {
                self.group(g).ok_or("no such group")?;
                self.group_layers(g).into_iter().filter(|c| *c != layer).collect()
            }
            None => Vec::new(),
        };
        if let Some(b) = before {
            if self.layer_index(b).is_none() {
                return Err("no layer to go before".into());
            }
            if group.is_some() && !others.contains(&b) {
                return Err("the layer to go before isn't in the group".into());
            }
        }
        let inside = self.chain_ids(group);
        let places = self.pin_tile_places();
        self.release_tiles(&[layer]);
        for g in &mut self.groups {
            g.children.retain(|c| *c != layer);
        }
        let Some(from) = self.layer_index(layer) else { return Err("no such layer".into()) };
        let l = self.layers.remove(from);
        let mut at = match (before, group) {
            (Some(b), _) => self.layer_index(b).unwrap_or(self.layers.len()),
            // Below the group's lowest layer; in a group with none, at the end of the nearest one
            // holding it that has some, or where it was.
            (None, Some(_)) => self.end_of(&inside).unwrap_or(from.min(self.layers.len())),
            (None, None) => self.layers.len(),
        };
        while at > 0 && self.splits(at, &inside) {
            at -= 1;
        }
        let at = at.min(self.layers.len());
        self.layers.insert(at, l);
        if let Some(g) = group.and_then(|g| self.groups.iter_mut().find(|x| x.id == g)) {
            g.children.push(layer);
        }
        // (Back where it was taken from: nothing moved.)
        self.keep_tiles_if_unmoved(at == from, places);
        self.settle_tiles();
        Ok(())
    }

    /// Moves a group's layers (its subgroups' too), together, to position `to` among the other
    /// layers (past the end, the bottom); a position inside another group's run moves up above
    /// that group. A subgroup stays in its group: at most to its top or its bottom.
    pub fn move_group(&mut self, group: u32, to: usize) -> Result<(), String> {
        let parent = self.group(group).ok_or("no such group")?.parent;
        let children = self.group_layers(group);
        let inside = self.chain_ids(parent);
        let places = self.pin_tile_places();
        // Where the group's layers are now: put back at the start of their run, unbroken, they
        // haven't moved.
        let run: Vec<usize> = self.layers.iter().enumerate().filter(|(_, l)| children.contains(&l.id)).map(|(i, _)| i).collect();
        let start = run.first().copied().filter(|s| run.iter().enumerate().all(|(k, i)| *i == s + k));
        self.release_tiles(&children);
        let (members, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.layers).into_iter().partition(|l| children.contains(&l.id));
        self.layers = rest;
        if members.is_empty() {
            return Err("the group has no layers to move".into());
        }
        let mut at = to.min(self.layers.len());
        if let Some(p) = parent {
            let siblings = self.group_layers(p);
            let lo = self.layers.iter().position(|l| siblings.contains(&l.id));
            let hi = self.layers.iter().rposition(|l| siblings.contains(&l.id));
            at = match (lo, hi) {
                (Some(lo), Some(hi)) => at.clamp(lo, hi + 1),
                // Alone in its group: it moves with the group, not in it.
                _ => run.first().copied().unwrap_or(at).min(self.layers.len()),
            };
        }
        while at > 0 && self.splits(at, &inside) {
            at -= 1;
        }
        let tail = self.layers.split_off(at);
        self.layers.extend(members);
        self.layers.extend(tail);
        self.keep_tiles_if_unmoved(start == Some(at), places);
        self.settle_tiles();
        Ok(())
    }

    /// Whether `group` can go in `parent` (`None`: the top level): not in itself or one of its
    /// subgroups, and no deeper than [`MAX_GROUP_DEPTH`] levels.
    fn can_nest(&self, group: u32, parent: Option<u32>) -> Result<(), String> {
        self.group(group).ok_or("no such group")?;
        let Some(p) = parent else { return Ok(()) };
        self.group(p).ok_or("no such group to go in")?;
        if self.group_chain(p).iter().any(|g| g.id == group) {
            return Err("a group can't go in itself or one of its subgroups".into());
        }
        if self.group_depth(p).saturating_add(self.group_height(group)) > MAX_GROUP_DEPTH {
            return Err(format!("groups nest at most {MAX_GROUP_DEPTH} levels deep"));
        }
        Ok(())
    }

    fn set_parent_of(&mut self, group: u32, parent: Option<u32>) {
        if let Some(g) = self.groups.iter_mut().find(|g| g.id == group) {
            g.parent = parent;
        }
    }

    /// Puts a group, with its layers and subgroups, in another group or at the top level
    /// (`None`), in the drawing order just above the layer `before` (`None`: below the other
    /// group's layers, or at the bottom at the top level). In a group, `before` must be one of
    /// its layers or its subgroups'; a place inside a group it isn't going in moves up past that
    /// group.
    pub fn place_group(&mut self, group: u32, parent: Option<u32>, before: Option<u32>) -> Result<(), String> {
        self.can_nest(group, parent)?;
        let members = self.group_layers(group);
        if let Some(b) = before {
            if self.layer_index(b).is_none() {
                return Err("no layer to go before".into());
            }
            if members.contains(&b) {
                return Err("the group can't go before one of its own layers".into());
            }
            if parent.is_some_and(|p| !self.layer_in(b, p)) {
                return Err("the layer to go before isn't in the group".into());
            }
        }
        if members.is_empty() {
            // Nothing to place: a group with no layers lists at the end of its level, in the
            // order of the map's groups; at the end (`before` none), after the others there.
            self.set_parent_of(group, parent);
            if before.is_none()
                && let Some(i) = self.groups.iter().position(|g| g.id == group)
            {
                let g = self.groups.remove(i);
                self.groups.push(g);
            }
            return Ok(());
        }
        let places = self.pin_tile_places();
        let run: Vec<usize> = self.layers.iter().enumerate().filter(|(_, l)| members.contains(&l.id)).map(|(i, _)| i).collect();
        let start = run.first().copied().filter(|s| run.iter().enumerate().all(|(k, i)| *i == s + k));
        self.release_tiles(&members);
        self.set_parent_of(group, parent);
        let inside = self.chain_ids(parent);
        let (moving, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.layers).into_iter().partition(|l| members.contains(&l.id));
        self.layers = rest;
        let mut at = match (before, parent) {
            (Some(b), _) => self.layer_index(b).unwrap_or(self.layers.len()),
            (None, Some(_)) => self.end_of(&inside).unwrap_or_else(|| run.first().copied().unwrap_or(self.layers.len())),
            (None, None) => self.layers.len(),
        };
        at = at.min(self.layers.len());
        while at > 0 && self.splits(at, &inside) {
            at -= 1;
        }
        let tail = self.layers.split_off(at);
        self.layers.extend(moving);
        self.layers.extend(tail);
        self.keep_tiles_if_unmoved(start == Some(at), places);
        self.settle_tiles();
        Ok(())
    }

    /// Puts a group, with its layers and subgroups, in another group or at the top level
    /// (`None`), next to that group's other layers (at the end nearer to where it is), or where
    /// it is at the top level (above the group it was inside).
    pub fn set_group_parent(&mut self, group: u32, parent: Option<u32>) -> Result<(), String> {
        self.can_nest(group, parent)?;
        if self.group(group).is_some_and(|g| g.parent == parent) {
            return Ok(());
        }
        let members = self.group_layers(group);
        if let Some(p) = parent
            && let Some(before) = self.nearest_end(p, &members)
        {
            return self.place_group(group, parent, before);
        }
        self.settle_tiles();
        self.set_parent_of(group, parent);
        let inside = self.chain_ids(parent);
        self.lift_out(&members, &inside);
        if let Some(p) = parent {
            self.gather_chain(p);
        }
        self.settle_tiles();
        Ok(())
    }

    /// Dissolves a group: its layers and subgroups go up one level, where they are.
    pub fn ungroup(&mut self, group: u32) -> Result<(), String> {
        let g = self.group(group).ok_or("no such group")?;
        let (parent, children) = (g.parent.filter(|p| *p != group), g.children.clone());
        // The tile layers over the group stay over its layers, where they were.
        self.settle_tiles();
        for x in &mut self.groups {
            if Some(x.id) == parent {
                x.children.extend(children.iter().copied());
            }
            if x.parent == Some(group) {
                x.parent = parent;
            }
        }
        self.groups.retain(|x| x.id != group);
        Ok(())
    }

    /// Gathers a group's layers, and those of every group holding it, the outermost first.
    fn gather_chain(&mut self, group: u32) {
        for id in self.chain_ids(Some(group)).into_iter().rev() {
            self.gather_group(id);
        }
    }

    /// Moves a group's layers, its subgroups' too, together in the drawing order at the topmost,
    /// keeping their order; children with no layer are dropped.
    pub fn gather_group(&mut self, group: u32) {
        let ids: std::collections::HashSet<u32> = self.layers.iter().map(|l| l.id).collect();
        let Some(g) = self.groups.iter_mut().find(|g| g.id == group) else { return };
        g.children.retain(|c| ids.contains(c));
        let members: std::collections::HashSet<u32> = self.group_layers(group).into_iter().collect();
        let Some(top) = self.layers.iter().position(|l| members.contains(&l.id)) else { return };
        let mut inner = Vec::new();
        let mut rest = Vec::new();
        for l in std::mem::take(&mut self.layers) {
            if members.contains(&l.id) { inner.push(l) } else { rest.push(l) }
        }
        // Layers above the topmost child stay above.
        let tail = rest.split_off(top.min(rest.len()));
        rest.extend(inner);
        rest.extend(tail);
        self.layers = rest;
        self.settle_tiles();
    }

    /// Makes a project file's groups whole (#836): a parent that is the group itself, missing,
    /// in a loop or past [`MAX_GROUP_DEPTH`] levels is dropped (the group goes to the top
    /// level), and each group's layers are gathered together, the outermost groups first.
    /// Returns the number of invalid parent links removed.
    pub fn tidy_groups(&mut self) -> usize {
        let mut removed = 0;
        let ids: std::collections::HashSet<u32> = self.groups.iter().map(|g| g.id).collect();
        for g in &mut self.groups {
            if g.parent.is_some_and(|p| p == g.id || !ids.contains(&p)) {
                g.parent = None;
                removed += 1;
            }
        }
        let mut parent: std::collections::HashMap<u32, Option<u32>> = self.groups.iter().map(|g| (g.id, g.parent)).collect();
        for g in &mut self.groups {
            let mut seen = vec![g.id];
            let mut next = g.parent;
            while let Some(p) = next {
                if seen.contains(&p) || seen.len() >= MAX_GROUP_DEPTH {
                    g.parent = None;
                    removed += 1;
                    parent.insert(g.id, None);
                    break;
                }
                seen.push(p);
                next = parent.get(&p).copied().flatten();
            }
        }
        let mut order: Vec<(usize, u32)> = self.groups.iter().map(|g| (self.group_depth(g.id), g.id)).collect();
        order.sort_by_key(|(depth, _)| *depth);
        for (_, id) in order {
            self.gather_group(id);
        }
        removed
    }

    /// The map's layers as they stand (their ids in order, and the next id), cheap to take before
    /// adding several layers that must all go in or none: see [`Map::restore_layers`].
    pub fn layer_mark(&self) -> LayerMark {
        LayerMark { ids: self.layers.iter().map(|l| l.id).collect(), next: self.next_layer_id }
    }

    /// Takes out the layers added since `mark` and puts the others back in its order, with the
    /// layer ids it had (the layers' data is never copied).
    pub fn restore_layers(&mut self, mark: &LayerMark) {
        self.layers.retain(|l| mark.ids.contains(&l.id));
        self.layers.sort_by_key(|l| mark.ids.iter().position(|id| *id == l.id).unwrap_or(usize::MAX));
        self.next_layer_id = mark.next;
    }

    /// Inserts a layer on top of the drawing order, whatever its kind, as GeoLibre adds them
    /// (#808): above every layer, and above the tile layers drawn over them.
    fn insert_on_top(&mut self, layer: Layer) {
        self.layers.insert(0, layer);
    }

    /// The map's layers as the Layers pane lists them at the top level, top first: each layer
    /// outside a group alone, each top-level group's layers together (its subgroups' too, in
    /// order), service groups aside.
    fn units(&self) -> Vec<Vec<u32>> {
        let group: std::collections::HashMap<u32, u32> =
            self.layer_chains().into_iter().filter_map(|(l, c)| c.last().map(|top| (l, *top))).collect();
        let mut units: Vec<Vec<u32>> = Vec::new();
        // A group's unit, by group.
        let mut at: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
        for l in &self.layers {
            match group.get(&l.id) {
                Some(g) => match at.get(g).and_then(|k| units.get_mut(*k)) {
                    Some(u) => u.push(l.id),
                    None => {
                        at.insert(*g, units.len());
                        units.push(vec![l.id]);
                    }
                },
                None => units.push(vec![l.id]),
            }
        }
        units
    }

    /// Where each tile layer is drawn, by index: just over this map layer (the topmost of its
    /// group, as a tile layer never goes inside one), or under all of them (None, as for a layer
    /// it names that the map no longer has).
    pub fn tile_slots(&self) -> Vec<Option<u32>> {
        if self.tiles.iter().all(|t| t.over.is_none()) {
            return vec![None; self.tiles.len()];
        }
        let top: std::collections::HashMap<u32, u32> = self
            .units()
            .into_iter()
            .filter_map(|u| u.first().copied().map(|t| (u, t)))
            .flat_map(|(u, t)| u.into_iter().map(move |l| (l, t)))
            .collect();
        self.tiles.iter().map(|t| t.over.and_then(|o| top.get(&o).copied())).collect()
    }

    /// Where `tile` is drawn (see [`Map::tile_slots`]).
    pub fn tile_slot(&self, tile: &TileLayer) -> Option<u32> {
        let over = tile.over?;
        self.units().into_iter().find(|u| u.contains(&over)).and_then(|u| u.first().copied())
    }

    /// The drawing order of all the map's layers, top first: the map layers, each with the
    /// tile layers drawn over it above it, and the tile layers under them all last (#808).
    pub fn stack(&self) -> Vec<Drawn> {
        let slots = self.tile_slots();
        let over = |id: Option<u32>| slots.iter().enumerate().filter(move |(_, s)| **s == id).map(|(i, _)| Drawn::Tile(i));
        let mut out = Vec::with_capacity(self.layers.len() + self.tiles.len());
        for (i, l) in self.layers.iter().enumerate() {
            out.extend(over(Some(l.id)));
            out.push(Drawn::Layer(i));
        }
        out.extend(over(None));
        out
    }

    /// The tile layers drawn just over the map layer `over` (None: under all of them), top
    /// first, by index.
    pub fn tiles_over(&self, over: Option<u32>) -> Vec<usize> {
        self.tile_slots().into_iter().enumerate().filter(|(_, s)| *s == over).map(|(i, _)| i).collect()
    }

    /// Stores each tile layer's place as the layer it's drawn over now (a group's topmost), so a
    /// change to the groups later leaves it where it is.
    fn pin_tiles(&mut self) {
        let slots = self.tile_slots();
        for (t, s) in self.tiles.iter_mut().zip(slots) {
            t.over = s;
        }
    }

    /// Puts the tile layers in their drawing order (index 0 on top, as the pop-ups, the AI
    /// Assistant and `map.tiles` read them). Returns where `index` went.
    fn sort_tiles(&mut self, index: usize) -> usize {
        let order: Vec<usize> = self.stack().into_iter().filter_map(|d| if let Drawn::Tile(i) = d { Some(i) } else { None }).collect();
        let mut old: Vec<Option<TileLayer>> = std::mem::take(&mut self.tiles).into_iter().map(Some).collect();
        self.tiles = order.iter().filter_map(|i| old.get_mut(*i).and_then(Option::take)).collect();
        // (none is left out: the stack lists every tile layer once)
        self.tiles.extend(old.into_iter().flatten());
        order.iter().position(|i| *i == index).unwrap_or(index)
    }

    /// Moves the tile layer at `index` in the drawing order of all the map's layers: past the
    /// next tile layer, or past the next map layer (a group's layers at once); `"top"` and
    /// `"bottom"` take it to the ends. Returns its new index among the tile layers.
    pub fn move_tile(&mut self, index: usize, to: &str) -> Result<usize, String> {
        if index >= self.tiles.len() {
            return Err("no such tile layer".into());
        }
        self.pin_tiles();
        let index = self.sort_tiles(index);
        // The places a tile layer can be, top first: over each group or layer, then under all.
        let mut places: Vec<Option<u32>> = self.units().iter().filter_map(|u| u.first().copied()).map(Some).collect();
        places.push(None);
        // Sorted, the tile layers of a place are next to each other, in their order.
        let here = self.tiles.get(index).and_then(|t| t.over);
        let k = places.iter().position(|p| *p == here).unwrap_or(places.len() - 1);
        let mates = self.tiles_over(here);
        let at = mates.iter().position(|i| *i == index).unwrap_or(0);
        // (the place, and whether it goes on top of the tile layers already there)
        let (place, on_top, swap) = match to {
            "up" if at > 0 => (here, false, mates.get(at - 1).copied()),
            "up" if k > 0 => (places.get(k - 1).copied().flatten(), false, None),
            "down" if at + 1 < mates.len() => (here, false, mates.get(at + 1).copied()),
            "down" if k + 1 < places.len() => (places.get(k + 1).copied().flatten(), true, None),
            "top" => (places.first().copied().flatten(), true, None),
            "bottom" => (None, false, None),
            "up" | "down" => return Err("the tile layer can't move further".into()),
            _ => return Err(format!("unknown move {to:?}: up, down, top or bottom")),
        };
        if let Some(other) = swap {
            self.tiles.swap(index, other);
            return Ok(other);
        }
        let mut t = self.tiles.remove(index);
        t.over = place;
        let mates = self.tiles_over(place);
        let at = if on_top { mates.first().copied().unwrap_or(0) } else { mates.last().map_or(self.tiles.len(), |i| i + 1) };
        let at = at.min(self.tiles.len());
        self.tiles.insert(at, t);
        Ok(self.sort_tiles(at))
    }

    /// Puts the tile layer at `index` at place `to` in the drawing order of all the map's
    /// layers without it ([`Map::stack`], 0 = top; past the end, the bottom): over the map layer
    /// below that place (above its group, as a tile layer never goes inside one). Returns its
    /// new index among the tile layers.
    pub fn place_tile(&mut self, index: usize, to: usize) -> Result<usize, String> {
        if index >= self.tiles.len() {
            return Err("no such tile layer".into());
        }
        self.pin_tiles();
        let mut t = self.tiles.remove(index);
        let stack = self.stack();
        let to = to.min(stack.len());
        let (above, below) = stack.split_at(to);
        t.over = below.iter().find_map(|d| match d {
            Drawn::Layer(i) => self.layers.get(*i).map(|l| l.id),
            Drawn::Tile(_) => None,
        });
        t.over = self.tile_slot(&t);
        let mates = self.tiles_over(t.over);
        // After the tile layers of its place drawn above it; else before the others there.
        let higher = above.iter().filter_map(|d| if let Drawn::Tile(i) = d { Some(*i) } else { None }).filter(|i| mates.contains(i)).max();
        let at = higher.map_or_else(|| mates.first().copied().unwrap_or(0), |i| i + 1).min(self.tiles.len());
        self.tiles.insert(at, t);
        Ok(self.sort_tiles(at))
    }

    /// Hands the tile layers drawn over any of the layers `moving` (about to move or go) to the
    /// layer they then sit over: the next one down that stays, so they keep their place.
    pub fn release_tiles(&mut self, moving: &[u32]) {
        if self.tiles.iter().all(|t| t.over.is_none()) {
            return;
        }
        self.pin_tiles();
        let layers = &self.layers;
        for t in &mut self.tiles {
            let Some(over) = t.over.filter(|o| moving.contains(o)) else { continue };
            let from = layers.iter().position(|l| l.id == over).unwrap_or(layers.len());
            t.over = layers.iter().skip(from).map(|l| l.id).find(|id| !moving.contains(id));
        }
        self.sort_tiles(0);
    }

    /// The tile layers in their drawing order and places, each pinned to the layer it is drawn over now
    /// under the groups as they are (a move inside a group leaves `over` on a layer that is no
    /// longer the group's top), as [`Map::release_tiles`] pins them, and sorted into the drawing
    /// order (`self.tiles` itself, so tile layer indices may change, as `release_tiles` changes
    /// them too). `None` when no tile layer has a place. Kept whole: a tile layer's `id` may be empty or shared, so it can't tell them
    /// apart.
    fn pin_tile_places(&mut self) -> Option<Vec<TileLayer>> {
        if self.tiles.iter().all(|t| t.over.is_none()) {
            return None;
        }
        self.pin_tiles();
        // In the drawing order as it is, which a later merge of their places (a layer put in a
        // group) keeps: the list may be out of it (a change to the groups re-seats tile layers
        // without sorting them).
        self.sort_tiles(0);
        Some(self.tiles.clone())
    }

    /// Puts the tile layers back as `places` had them, in their drawing order, when the layers
    /// were `unmoved`: put back where they were taken from (a layer placed on its own spot, a
    /// group moved to where it is), told by their places, not their ids (which a project may
    /// repeat). Such a move doesn't hand the tile layers over them to the layer below, as
    /// [`Map::release_tiles`] did. A layer that only changed group (put in the group just above
    /// it, say) keeps its tile layers too, and the caller's [`Map::settle_tiles`] then seats them
    /// over that group's topmost layer, as [`Map::set_group`] does.
    fn keep_tiles_if_unmoved(&mut self, unmoved: bool, places: Option<Vec<TileLayer>>) {
        let Some(places) = places else { return };
        if unmoved {
            self.tiles = places;
            self.sort_tiles(0);
        }
    }

    /// After a change to the groups: each tile layer's place is a group's topmost layer, or
    /// one outside a group (see [`Map::pin_tiles`]).
    pub fn settle_tiles(&mut self) {
        if self.tiles.iter().any(|t| t.over.is_some()) {
            self.pin_tiles();
        }
    }

    /// Whether a layer (or a group's layers) put at `pos` in the drawing order would sit inside a
    /// group other than those of `inside` (the groups it is going in, innermost first): between
    /// two of that group's layers.
    fn splits(&self, pos: usize, inside: &[u32]) -> bool {
        let above = pos.checked_sub(1).and_then(|i| self.layers.get(i)).map(|l| l.id);
        self.splits_between(above, self.layers.get(pos).map(|l| l.id), inside)
    }

    /// Whether layers `above` and `below` share a group that isn't one of `inside`.
    fn splits_between(&self, above: Option<u32>, below: Option<u32>, inside: &[u32]) -> bool {
        let (Some(above), Some(below)) = (above, below) else { return false };
        let below = self.groups_of(below);
        self.groups_of(above).iter().any(|g| !inside.contains(&g.id) && below.iter().any(|b| b.id == g.id))
    }

    /// Moves the layer at index `from` to index `to` in the drawing order, keeping groups
    /// together: a group's layer moves only among its group's (its subgroups' included), and a
    /// layer moving into the run of a group it isn't in goes past it (above moving up, below
    /// moving down).
    pub fn move_layer(&mut self, from: usize, to: usize) -> Result<(), String> {
        let id = self.layers.get(from).map(|l| l.id).ok_or("no such layer")?;
        let to = to.min(self.layers.len().saturating_sub(1));
        let inside: Vec<u32> = self.groups_of(id).iter().map(|g| g.id).collect();
        if let Some(g) = inside.first() {
            let members = self.group_layers(*g);
            let lo = self.layers.iter().position(|l| members.contains(&l.id)).unwrap_or(from);
            let hi = self.layers.iter().rposition(|l| members.contains(&l.id)).unwrap_or(from);
            if to < lo || to > hi {
                return Err("the layer can't move out of its group (take it out of the group first)".into());
            }
        }
        if to == from {
            return Err("the layer can't move further".into());
        }
        self.release_tiles(&[id]);
        let l = self.layers.remove(from);
        let mut to = to;
        // `to` is now the index the layer goes in at, among the others.
        while to > 0 && to < self.layers.len() && self.splits(to, &inside) {
            if to < from { to -= 1 } else { to += 1 }
        }
        self.layers.insert(to.min(self.layers.len()), l);
        Ok(())
    }

    /// Adds a dataset as a new layer; returns its id.
    pub fn add_dataset(&mut self, ds: Dataset) -> u32 {
        let id = self.next_id();
        self.insert_on_top(Layer::new(id, ds));
        id
    }

    /// Adds a layer (from a layer file) with a fresh id on top of the drawing order; returns the id.
    pub fn add_layer(&mut self, mut layer: Layer) -> u32 {
        let id = self.next_id();
        layer.id = id;
        layer.renew_instance_id();
        layer.selection.clear();
        layer.touch_geometry();
        self.insert_on_top(layer);
        id
    }

    /// Adds an empty graphics layer on top; returns its id.
    pub fn add_graphics(&mut self, name: &str) -> u32 {
        let id = self.next_id();
        self.layers.insert(0, Layer::new_graphics(id, name));
        id
    }

    /// Adds a streamed layer (a large file read in place); returns its id.
    pub fn add_streamed(&mut self, name: impl Into<String>, source: source::SourceLayer) -> u32 {
        let id = self.next_id();
        let mut l = Layer::new_streamed(id, source);
        l.name = name.into();
        l.dataset.name = l.name.clone();
        self.insert_on_top(l);
        id
    }

    /// Keeps streamed layers up with a view (longitude/latitude; see [`Layer::sync_source`]) at a
    /// scale denominator. Only layers drawn there read: visible and in their zoom range. Returns
    /// whether a read is still running.
    pub fn sync_sources(&mut self, view: &Extent, scale: f64, block: bool) -> bool {
        let mut pending = false;
        let held = self.held_by_group(scale);
        // Hidden resident layers share the budget too; toggling visibility cannot multiply it.
        let sources = self.layers.iter().filter(|l| l.source.is_some()).count().max(1);
        let bytes = source::VIEW_BYTES / sources;
        for layer in &mut self.layers {
            if let Some(raster) = &mut layer.raster {
                pending |= raster.poll_reload();
            }
            if let Some(src) = &mut layer.source {
                let changed = src.projection.as_ref().is_some_and(|p| p != &self.projection);
                let mode_changed = src.full_read != block;
                let budget_changed = src.byte_budget != bytes;
                src.projection = Some(self.projection.clone());
                src.byte_budget = bytes;
                src.full_read = block;
                if changed || mode_changed || budget_changed {
                    src.reread();
                }
                if changed || (!block && layer.view_cache_bytes() > bytes) {
                    // Preserve ids while discarding a window that no longer fits. The next
                    // installed window resolves the selection against its authoritative ids.
                    let mut selected: Vec<_> =
                        layer.selection.iter().filter_map(|r| layer.dataset.features.get(*r)).map(|f| f.id).collect();
                    if let Some(src) = &mut layer.source {
                        selected.append(&mut src.saved_selection);
                        src.reset_window();
                    }
                    layer.install(Vec::new());
                    if let Some(src) = &mut layer.source {
                        src.saved_selection = selected;
                    }
                }
            }
        }
        for l in self.layers.iter_mut().filter(|l| l.is_streamed() && l.visible && l.in_scale_range(scale) && !held.contains(&l.id)) {
            pending |= l.sync_source(view, block);
        }
        pending
    }

    /// Adds a raster layer; returns its id.
    pub fn add_raster(&mut self, name: impl Into<String>, raster: RasterLayer) -> u32 {
        let id = self.next_id();
        self.insert_on_top(Layer::new_raster(id, name, raster));
        id
    }

    pub fn layer(&self, id: u32) -> Option<&Layer> {
        self.layers.iter().find(|l| l.id == id)
    }
    pub fn layer_mut(&mut self, id: u32) -> Option<&mut Layer> {
        self.layers.iter_mut().find(|l| l.id == id)
    }
    pub fn layer_index(&self, id: u32) -> Option<usize> {
        self.layers.iter().position(|l| l.id == id)
    }

    /// Takes the projected geometry caches a copy of this map filled (see
    /// [`Layer::adopt_cache`]), layer by layer.
    pub fn adopt_caches(&mut self, other: &Map) {
        for l in &mut self.layers {
            if let Some(o) = other.layer(l.id) {
                // Complete rendering uses temporary chunk layers, so a failed worker need not
                // have a projected cache to adopt. Transfer its diagnostic only for the same
                // source and unchanged owning rows; stale workers must not overwrite edits.
                if l.dataset.features.shares(&o.dataset.features)
                    && l.geometry_rev == o.geometry_rev
                    && let (Some(own), Some(drawn)) = (&mut l.source, &o.source)
                    && let (Some(a), Some(b)) = (own.chunk_source(), drawn.chunk_source())
                    && std::sync::Arc::ptr_eq(&a, &b)
                {
                    own.error.clone_from(&drawn.error);
                }
                l.adopt_cache(o);
            }
        }
    }
    pub fn layer_by_name(&self, name: &str) -> Option<&Layer> {
        self.layers.iter().find(|l| l.name.eq_ignore_ascii_case(name))
    }

    /// The flat projection of the camera's coordinates (Web Mercator for the globe).
    pub fn flat(&self) -> projection::Flat {
        projection::Flat::of(&self.projection)
    }

    /// Longitude/latitude to the camera's coordinates; `None` outside the projection's area of
    /// use or where it folds.
    pub fn to_map(&self, c: Coord) -> Option<Coord> {
        self.flat().forward_checked(c)
    }

    /// The camera's coordinates to longitude/latitude.
    pub fn to_lonlat(&self, m: Coord) -> Option<Coord> {
        self.flat().inverse(m)
    }

    /// The longitude/latitude at the centre of view.
    pub fn center_lonlat(&self) -> Coord {
        self.to_lonlat(self.camera.center).unwrap_or(Coord::new(0.0, 0.0))
    }

    /// Ground metres per logical pixel at the centre of view.
    pub fn ground_res(&self) -> f64 {
        self.ground_res_at(&self.camera)
    }

    /// Ground metres per logical pixel for another camera on this map (an export's view).
    pub fn ground_res_at(&self, camera: &Camera) -> f64 {
        camera.res * self.ground_per_unit(camera.center)
    }

    /// The celestial body the map is on (the Earth for an unknown id).
    pub fn body(&self) -> &'static rustgis_geom::Body {
        rustgis_geom::Body::get(&self.body)
    }

    /// The built-in basemap's (background, land, land outline, graticule) colours. The world
    /// outline is the Earth's, so on another body the background is plain: the land's grey, with
    /// no land drawn on it (#223).
    pub fn palette(&self) -> (Rgba, Rgba, Rgba, Rgba) {
        let (water, land, outline, grat) = self.basemap.palette();
        if self.body().is_earth() {
            return (water, land, outline, grat);
        }
        let l = ((u16::from(land.r()) * 3 + u16::from(land.g()) * 6 + u16::from(land.b())) / 10) as u8;
        let grey = Rgba::rgb(l, l, l);
        (grey, grey, Rgba::new(0, 0, 0, 0), grat)
    }

    /// Whether the built-in world outline is drawn: where the palette's land and water differ, so
    /// not on the blank basemap or another body.
    pub fn draws_world_outline(&self) -> bool {
        let (water, land, ..) = self.palette();
        land != water
    }

    /// Ground metres on the map's body per map unit at a point in map coordinates.
    pub fn ground_per_unit(&self, at: Coord) -> f64 {
        self.flat().ground_per_unit(at) * self.body().ratio()
    }

    /// Map scale denominator at the centre of view.
    pub fn scale(&self) -> f64 {
        self.ground_res() / rustgis_geom::PIXEL_M
    }

    /// Sets the resolution from a scale denominator.
    pub fn set_scale(&mut self, scale: f64) {
        if !scale.is_finite() || scale <= 0.0 {
            return;
        }
        let per = self.ground_per_unit(self.camera.center).max(1e-9);
        self.camera.res = scale * rustgis_geom::PIXEL_M / per;
        self.camera.clamp();
    }

    /// Points the flat map at a longitude/latitude extent with a margin.
    pub fn fit_lonlat(&mut self, e: &Extent, w: f64, h: f64) {
        let flat = self.flat();
        if let projection::Flat::Mercator = flat {
            self.camera.fit_lonlat(e, w, h);
            return;
        }
        if e.is_empty() || w <= 1.0 || h <= 1.0 {
            return;
        }
        let min_pad = if e.width() > 0.0 || e.height() > 0.0 { 1e-6 } else { 0.02 };
        let m = flat.extent(&e.padded(0.06, min_pad));
        if m.is_empty() {
            return;
        }
        self.camera.fit_extent(&m, w, h);
    }

    /// Switches projection, keeping the centre and the ground scale.
    /// Bookmarks move with it, so they still show the same place.
    pub fn set_projection(&mut self, p: Projection) {
        let old = self.flat();
        let ground_of = |c: &Camera| (old.inverse(c.center).unwrap_or(Coord::new(0.0, 0.0)), c.res * old.ground_per_unit(c.center));
        let camera = ground_of(&self.camera);
        let marks: Vec<_> = self.bookmarks.iter().map(|b| ground_of(&b.camera)).collect();
        self.projection = p;
        let flat = self.flat();
        let place = |cam: &mut Camera, (center, ground): (Coord, f64)| {
            // A centre the new projection can't show moves to its origin.
            cam.center = flat.forward_checked(center).or_else(|| flat.forward(Coord::new(0.0, 0.0))).unwrap_or(Coord::new(0.0, 0.0));
            cam.res = ground / flat.ground_per_unit(cam.center).max(1e-9);
            cam.clamp();
        };
        place(&mut self.camera, camera);
        for (b, m) in self.bookmarks.iter_mut().zip(marks) {
            place(&mut b.camera, m);
        }
    }

    /// Union of all layers' extents (lon/lat).
    pub fn full_extent(&self) -> Extent {
        self.layers.iter().filter(|l| self.shows(l)).fold(Extent::EMPTY, |e, l| e.union(&l.extent()))
    }

    /// Union of the visible layers' shown extents (lon/lat): what Full Extent zooms to, narrowed by
    /// each layer's definition query.
    pub fn shown_extent(&self) -> Extent {
        self.layers.iter().filter(|l| self.shows(l)).fold(Extent::EMPTY, |e, l| e.union(&l.shown_extent()))
    }

    pub fn selection_count(&self) -> usize {
        self.layers.iter().map(|l| l.selection.len()).sum()
    }

    pub fn selection_extent(&self) -> Extent {
        self.layers.iter().fold(Extent::EMPTY, |e, l| e.union(&l.selection_extent()))
    }

    pub fn clear_selection(&mut self) {
        for l in &mut self.layers {
            l.selection.clear();
        }
    }

    /// The feature drawn on top under a point in the camera's coordinates, among the visible layers
    /// `keep` accepts: (layer id, row). Draw order and the rows drawn are those of [`Map::hits_at`].
    pub fn hit(&mut self, p: Coord, tol: f64, scale: f64, keep: impl Fn(&Layer) -> bool) -> Option<(u32, usize)> {
        self.hits_at(p, tol, scale, keep, true, 1).into_iter().next()
    }

    /// Every feature under a point in the visible layers `keep` accepts: top layer first and, in
    /// a layer, the one drawn on top first. At most `limit` of them; with `first_layer`, only
    /// those of the top layer with any.
    pub fn hits_at(
        &mut self,
        p: Coord,
        tol: f64,
        scale: f64,
        keep: impl Fn(&Layer) -> bool,
        first_layer: bool,
        limit: usize,
    ) -> Vec<(u32, usize)> {
        let (flat, key) = (self.flat(), self.projection.code());
        let mut out = Vec::new();
        // Top first, as drawn: a higher depth priority is above whatever the order.
        let mut order: Vec<usize> = (0..self.layers.len()).collect();
        order.sort_by_key(|i| std::cmp::Reverse(self.layers.get(*i).map_or(0, |l| l.priority)));
        let held = self.held_by_group(scale);
        for i in order {
            let Some(l) = self.layers.get_mut(i) else { continue };
            if out.len() >= limit || (first_layer && !out.is_empty()) {
                break;
            }
            if !l.visible || !l.in_scale_range(scale) || held.contains(&l.id) || !keep(l) {
                continue;
            }
            // Only features drawn: the definition query and the display filters let them through.
            let rows = l.drawn_rows(scale);
            let id = l.id;
            let proj = l.projected_in(&flat, &key);
            let hits = proj
                .iter()
                .enumerate()
                .rev()
                .filter(|(row, g)| rows.get(*row) == Some(&true) && g.as_ref().is_some_and(|g| g.hit(p, tol)))
                .map(|(row, _)| (id, row));
            out.extend(hits.take(limit - out.len()));
        }
        out
    }

    /// Layers eligible for map selection, including the visibility and scale of their groups.
    pub fn selectable_layers(&mut self, scale: f64) -> impl Iterator<Item = &mut Layer> + '_ {
        let held = self.held_by_group(scale);
        self.layers.iter_mut().filter(move |l| l.visible && l.selectable && l.in_scale_range(scale) && !held.contains(&l.id))
    }

    /// Rows of each selectable, visible layer inside a rectangle in the camera's coordinates.
    pub fn hits_in(&mut self, e: &Extent, scale: f64) -> Vec<(u32, Vec<usize>)> {
        let (flat, key) = (self.flat(), self.projection.code());
        let mut out = Vec::new();
        for l in self.selectable_layers(scale) {
            // Only features drawn: the definition query and the display filters let them through.
            let rows = l.drawn_rows(scale);
            let id = l.id;
            let hits: Vec<usize> = l
                .projected_in(&flat, &key)
                .iter()
                .enumerate()
                .filter(|(r, g)| rows.get(*r) == Some(&true) && g.as_ref().is_some_and(|g| g.intersects_extent(e)))
                .map(|(r, _)| r)
                .collect();
            if !hits.is_empty() {
                out.push((id, hits));
            }
        }
        out
    }
}

/// The reference scales a layer takes (1:N), as [`Layer::reference_scale`].
pub const REFERENCE_SCALES: (f64, f64) = (1.0, 1e9);

/// The most a reference scale shrinks or grows a layer's sizes ([`Layer::size_factor`]): zoomed
/// far in, its lines would otherwise be drawn thousands of pixels wide.
pub const SIZE_FACTORS: (f64, f64) = (0.01, 32.0);

/// A [`Layer::no_fill`] this version doesn't know (a later variant, another type, or edited by
/// hand) reads as off, rather than failing the whole project.
fn de_no_fill<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<NoFill>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Read {
        Known(NoFill),
        Other(serde::de::IgnoredAny),
    }
    Ok(match Option::<Read>::deserialize(d)? {
        Some(Read::Known(how)) => Some(how),
        Some(Read::Other(_)) | None => None,
    })
}

/// Reads a layer's reference scale as `layer.referenceScale` would leave it: one out of
/// [`REFERENCE_SCALES`] (or not a number) is none. A project file is hostile input.
fn de_reference_scale<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
    let v: Option<f64> = Option::deserialize(d)?;
    Ok(v.filter(|r| (REFERENCE_SCALES.0..=REFERENCE_SCALES.1).contains(r)))
}

impl Map {
    /// Offers each persisted address and local path once, before any output mutation.
    pub fn slots(&self) -> Vec<Slot<'_>> {
        let Map {
            name: _,
            layers,
            service_groups,
            groups: _,
            basemap: _,
            tiles,
            camera: _,
            bookmarks: _,
            graticule: _,
            projection: _,
            popup_mode: _,
            label_display: _,
            label_lock: _,
            label_lock_line_anchors: _,
            editing: _,
            swipe: _,
            view,
            thumbnail: _,
            animation: _,
            time: _,
            body: _,
            next_layer_id: _,
            instance_id: _,
        } = self;
        let mut out = Vec::new();
        for (group, g) in service_groups.iter().enumerate() {
            let ServiceGroup { id: _, name: _, url, kind: _, anonymous: _, expanded: _, children } = g;
            out.push(Slot { at: SlotPos::Group { group, child: None }, value: url, address: true, local_path: false });
            for (child, c) in children.iter().enumerate() {
                let ServiceChild { layer: _, collection: _, source_url } = c;
                out.push(Slot { at: SlotPos::Group { group, child: Some(child) }, value: source_url, address: true, local_path: false });
            }
        }
        for (tile, t) in tiles.iter().enumerate() {
            out.extend(t.output_slots().into_iter().map(|(role, value, local_path)| Slot {
                at: SlotPos::Tile { tile, role },
                value,
                address: true,
                local_path,
            }));
        }
        let view::ViewSettings {
            effects: _,
            color_vision: _,
            wireframe: _,
            clip_distance: _,
            terrain,
            fast: _,
            navigator: _,
            first_person: _,
        } = view;
        if let Some(view::Terrain { source, exaggeration: _ }) = terrain {
            match source {
                view::TerrainSource::Tiles { url, encoding: _ } => {
                    out.push(Slot { at: SlotPos::Terrain, value: url, address: true, local_path: false })
                }
                view::TerrainSource::Layer(_) => {}
            }
        }
        for (layer, l) in layers.iter().enumerate() {
            out.extend(l.output_slots().into_iter().map(|(role, value, address, local_path)| Slot {
                at: SlotPos::Layer { layer, role },
                value,
                address,
                local_path,
            }));
        }
        out
    }

    /// Resolves only the recorded location; never rediscovers or reclassifies a written value.
    pub fn slot_mut(&mut self, at: &SlotPos) -> Option<&mut String> {
        match at {
            SlotPos::Group { group, child } => {
                let g = self.service_groups.get_mut(*group)?;
                match child {
                    Some(c) => Some(&mut g.children.get_mut(*c)?.source_url),
                    None => Some(&mut g.url),
                }
            }
            SlotPos::Tile { tile, role } => self.tiles.get_mut(*tile)?.output_slot_mut(*role),
            SlotPos::Terrain => match &mut self.view.terrain.as_mut()?.source {
                view::TerrainSource::Tiles { url, encoding: _ } => Some(url),
                view::TerrainSource::Layer(_) => None,
            },
            SlotPos::Layer { layer, role } => self.layers.get_mut(*layer)?.output_slot_mut(*role),
        }
    }
}

impl Layer {
    /// Classifies each whole owning field once; equal addresses in different fields stay distinct.
    pub fn output_slots(&self) -> Vec<(LayerRole, &str, bool, bool)> {
        let Layer {
            id: _,
            name: _,
            visible: _,
            dataset,
            renderer: _,
            labels: _,
            transparency: _,
            min_scale: _,
            max_scale: _,
            definition_query: _,
            time: _,
            selection: _,
            expanded: _,
            selectable: _,
            graphics: _,
            blend: _,
            mask: _,
            display_filters: _,
            aggregation: _,
            extrusion: _,
            line_strokes: _,
            fill_layers: _,
            no_fill: _,
            stroke_levels: _,
            marker_rotation: _,
            symbol_by: _,
            reference_scale: _,
            priority: _,
            design: _,
            raster,
            source,
            postgis,
            gpkg,
            connection: _,
            anonymous: _,
            instance_id: _,
            projected: _,
            display: _,
            display_rejected: _,
            display_levels: _,
            projected_extents: _,
            projected_vertices: _,
            thinned: _,
            projected_rev: _,
            projected_for: _,
            view_memory: _,
            geometry_kinds: _,
            geometry_rev: _,
            replacement_rev: _,
            diagram: _,
        } = self;
        let mut out = Vec::new();
        let rustgis_data::Dataset {
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
            out.push((LayerRole::Dataset, v.as_str(), true, paths::is_local(v)));
        }
        if let Some(s) = source {
            out.extend(s.output_slots());
        }
        if let Some(r) = raster {
            out.extend(r.output_slots());
        }
        if let Some(g) = gpkg {
            out.push((LayerRole::GpkgPath, g.path.as_str(), false, paths::is_local(&g.path)));
        }
        if let Some(pg) = postgis {
            out.push((LayerRole::PostgisEditDatabase, pg.origin.database.as_str(), true, false));
        }
        out
    }

    /// Allocation-free resolution, valid while the layer's structure is held unchanged.
    pub fn output_slot_mut(&mut self, role: LayerRole) -> Option<&mut String> {
        match role {
            LayerRole::Dataset => self.dataset.source.as_mut(),
            LayerRole::Streamed
            | LayerRole::GdalPath
            | LayerRole::DuckDbDatabase
            | LayerRole::PostgisDatabase
            | LayerRole::MysqlDatabase
            | LayerRole::SqlServerDatabase => self.source.as_mut()?.output_slot_mut(role),
            LayerRole::Raster | LayerRole::ZarrStore => self.raster.as_mut()?.output_slot_mut(role),
            LayerRole::GpkgPath => Some(&mut self.gpkg.as_mut()?.path),
            LayerRole::PostgisEditDatabase => Some(&mut self.postgis.as_mut()?.origin.database),
        }
    }
}

impl Map {
    /// Bridge identity is independent of the address value; duplicate ids remain ambiguous.
    pub fn slot_identity(&self, at: &SlotPos) -> Option<SlotAt> {
        match at {
            SlotPos::Group { group, child } => {
                let g = self.service_groups.get(*group)?;
                Some(SlotAt::Group {
                    id: g.id,
                    child: match child {
                        Some(i) => Some(g.children.get(*i)?.layer),
                        None => None,
                    },
                })
            }
            SlotPos::Tile { tile, role } => Some(SlotAt::Tile { id: self.tiles.get(*tile)?.id.clone(), role: *role }),
            SlotPos::Terrain => Some(SlotAt::Terrain),
            SlotPos::Layer { layer, role } => Some(SlotAt::Layer { id: self.layers.get(*layer)?.id, role: *role }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_data::{Field, FieldType, Value};
    use rustgis_geom::lonlat_to_merc;

    fn points() -> Dataset {
        let mut d = Dataset::new("pts", GeometryType::Point, vec![Field::new("n", FieldType::Integer)]);
        d.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![Value::Int(1)]);
        d.push(Some(Geometry::Point(Coord::new(10.0, 10.0))), vec![Value::Int(2)]);
        d
    }

    #[test]
    fn a_marker_rotation_is_kept_and_a_crafted_one_reads_as_none() {
        let mut l =
            Layer::new(1, Dataset::new("p", GeometryType::Point, vec![rustgis_data::Field::new("a", rustgis_data::FieldType::Double)]));
        l.dataset.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![rustgis_data::Value::Float(450.0)]);
        // None isn't written: an older RustGIS reads the layer as before.
        assert!(serde_json::to_value(&l).unwrap().get("marker_rotation").is_none());
        l.marker_rotation = Some(symbology::MarkerRotation { field: "a".into(), counterclockwise: true });
        let v = serde_json::to_value(&l).unwrap();
        let back: Layer = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(back.marker_rotation, l.marker_rotation);
        assert_eq!(back.marker_turn(0), -90.0, "450° counterclockwise");
        for junk in [
            serde_json::json!(5),
            serde_json::json!({"field": 3}),
            serde_json::json!({"field": ""}),
            serde_json::json!({"field": "x".repeat(10_000)}),
        ] {
            let mut v = v.clone();
            v["marker_rotation"] = junk.clone();
            let read: Layer = serde_json::from_value(v).unwrap();
            assert!(read.marker_rotation.is_none(), "{junk}");
        }
        // A field the data lacks turns nothing.
        l.marker_rotation = Some(symbology::MarkerRotation { field: "gone".into(), counterclockwise: false });
        assert_eq!(l.marker_turn(0), 0.0);
    }

    #[test]
    fn a_diagram_is_kept_and_a_crafted_one_reads_as_none() {
        let mut l = Layer::new(
            1,
            Dataset::new(
                "p",
                GeometryType::Polygon,
                vec![Field::new("male", FieldType::Integer), Field::new("female", FieldType::Integer)],
            ),
        );
        l.dataset.push(
            Some(Geometry::Polygon(vec![vec![vec![
                Coord::new(0.0, 0.0),
                Coord::new(1.0, 0.0),
                Coord::new(1.0, 1.0),
                Coord::new(0.0, 1.0),
                Coord::new(0.0, 0.0),
            ]]])),
            vec![Value::Int(52_000), Value::Int(48_000)],
        );
        // None isn't written: an older RustGIS reads the layer as before.
        assert!(serde_json::to_value(&l).unwrap().get("diagram").is_none());
        l.diagram = Some(diagram::DiagramSettings {
            kind: diagram::DiagramKind::Pie,
            categories: vec![
                diagram::DiagramCategory { field: "male".into(), colour: diagram::CATEGORY_COLORS[0] },
                diagram::DiagramCategory { field: "female".into(), colour: diagram::CATEGORY_COLORS[1] },
            ],
            size: diagram::DiagramSize::Scaled { min_size: 6.0, max_size: 40.0, min: 45_000.0, max: 100_000.0 },
            start_angle: 0.0,
            background: None,
            stroke: diagram::DiagramSettings::default_stroke(),
            stroke_width: 0.75,
            labels: diagram::DiagramLabel::Percent,
        });
        let v = serde_json::to_value(&l).unwrap();
        let back: Layer = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(back.diagram, l.diagram);
        // Its values read from the row it draws on.
        let d = l.diagram.as_ref().unwrap();
        let (values, total) = d.values(&l.dataset, 0).unwrap();
        assert_eq!(total, 100_000.0);
        assert_eq!(values.len(), 2);
        for junk in [
            serde_json::json!("nonsense"),
            serde_json::json!({"kind": "hexagon"}),
            serde_json::json!({"categories": []}),
            serde_json::json!(17),
        ] {
            let mut v = v.clone();
            v["diagram"] = junk.clone();
            let read: Layer = serde_json::from_value(v).unwrap();
            assert!(read.diagram.is_none(), "{junk}");
        }
    }

    #[test]
    fn hostile_line_strokes_in_a_project_are_brought_into_range() {
        // A crafted project file: too many strokes, and widths and offsets far out of range.
        let mut l = Layer::new(1, Dataset::new("lines", GeometryType::Polyline, vec![]));
        l.line_strokes = vec![symbology::LineStroke { width: Some(1e30), offset: -1e30, ..Default::default() }; 5000];
        let text = serde_json::to_string(&l).unwrap();
        let back: Layer = serde_json::from_str(&text).unwrap();
        assert_eq!(back.line_strokes.len(), symbology::MAX_STROKES);
        assert!(
            back.line_strokes.iter().all(|st| st.width == Some(symbology::MAX_STROKE_WIDTH) && st.offset == -symbology::MAX_STROKE_OFFSET)
        );
    }

    #[test]
    fn dash_patterns_are_kept_and_hostile_ones_brought_into_range() {
        use symbology::{Dash, MAX_DASH_STEP, MAX_DASH_STEPS};
        // Built: in range, not all 0, at most MAX_DASH_STEPS.
        assert_eq!(Dash::new(&[4.0, 2.0]).map(|d| d.steps().to_vec()), Some(vec![4.0, 2.0]));
        assert_eq!(Dash::new(&[3.0]).map(|d| (d.even_steps(), d.period())), Some((vec![3.0, 3.0], 6.0)), "odd: twice over");
        for bad in [&[][..], &[0.0, 0.0], &[-1.0, 2.0], &[f32::NAN, 1.0], &[MAX_DASH_STEP + 1.0], &[1.0; MAX_DASH_STEPS + 1]] {
            assert!(Dash::new(bad).is_none(), "{bad:?}");
        }
        // Saved as a plain list and read back.
        let mut l = Layer::new(1, Dataset::new("lines", GeometryType::Polyline, vec![]));
        l.line_strokes = vec![symbology::LineStroke { dash: Dash::new(&[4.0, 2.0, 1.0, 2.0]), ..Default::default() }];
        let text = serde_json::to_string(&l).unwrap();
        assert!(text.contains(r#""dash":[4.0,2.0,1.0,2.0]"#), "{text}");
        assert_eq!(serde_json::from_str::<Layer>(&text).unwrap().line_strokes, l.line_strokes);
        // A crafted project's: steps clamped, too many cut, and one without length solid.
        let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
        let read = |v: &serde_json::Value| serde_json::from_value::<Layer>(v.clone()).unwrap().line_strokes[0].dash;
        v["line_strokes"][0]["dash"] = serde_json::json!([1e30, -5, 2]);
        assert_eq!(read(&v).map(|d| d.steps().to_vec()), Some(vec![MAX_DASH_STEP, 0.0, 2.0]));
        // Too large for an f32 (infinite once read): the longest step, as a large one is.
        v["line_strokes"][0]["dash"] = serde_json::json!([1e40, 2]);
        assert_eq!(read(&v).map(|d| d.steps().to_vec()), Some(vec![MAX_DASH_STEP, 2.0]));
        v["line_strokes"][0]["dash"] = serde_json::json!(vec![1.0; 5000]);
        assert_eq!(read(&v).map(|d| d.steps().len()), Some(MAX_DASH_STEPS));
        for solid in [serde_json::json!([0, 0]), serde_json::json!([]), serde_json::json!([-3])] {
            v["line_strokes"][0]["dash"] = solid.clone();
            assert_eq!(read(&v), None, "{solid}");
            // Read as a stroke alone too, so no empty pattern is ever made.
            let st = serde_json::from_value::<symbology::LineStroke>(v["line_strokes"][0].clone()).unwrap();
            assert_eq!(st.dash, None, "{solid}");
        }
    }

    #[test]
    fn markers_along_lines_are_kept_and_hostile_ones_brought_into_range() {
        use symbology::{LineMark, LineMarkers, MAX_LINE_MARKER_SIZE, MAX_MARKER_INTERVAL, MIN_MARKER_INTERVAL, Placements};
        let mut l = Layer::new(1, Dataset::new("lines", GeometryType::Polyline, vec![]));
        let m = LineMarkers {
            mark: LineMark::Marker(Marker::Triangle),
            size: 6.0,
            at: Placements::FIRST.with(Placements::LAST),
            interval: 20.0,
            offset_along: -2.0,
            rotate: false,
            average_angle: 15.0,
            angle: 45.0,
            outline: Some(Rgba::rgb(0, 0, 0)),
        };
        l.line_strokes = vec![
            symbology::LineStroke { markers: Some(m), ..Default::default() },
            symbology::LineStroke { markers: Some(LineMarkers { mark: LineMark::Hash, ..LineMarkers::default() }), ..Default::default() },
        ];
        // Saved with the places as codes, and read back as they were.
        let text = serde_json::to_string(&l).unwrap();
        assert!(text.contains(r#""at":["first","last"]"#) && text.contains(r#""mark":"hash""#), "{text}");
        assert_eq!(serde_json::from_str::<Layer>(&text).unwrap().line_strokes, l.line_strokes);
        // A stroke without markers is written as before.
        let plain = serde_json::to_string(&symbology::LineStroke::default()).unwrap();
        assert!(!plain.contains("markers"), "{plain}");

        // A crafted project's: sizes and intervals clamped, angles within a turn, places it doesn't
        // know left out (none at all: every interval).
        let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
        let read = |v: &serde_json::Value| serde_json::from_value::<Layer>(v.clone()).unwrap().line_strokes[0].markers.unwrap();
        let mk = &mut v["line_strokes"][0]["markers"];
        mk["size"] = serde_json::json!(1e30);
        mk["interval"] = serde_json::json!(0);
        mk["offset_along"] = serde_json::json!(-1e40);
        mk["average_angle"] = serde_json::json!(-3);
        mk["angle"] = serde_json::json!(-90);
        mk["at"] = serde_json::json!(["last", "sideways"]);
        let got = read(&v);
        assert_eq!((got.size, got.interval, got.offset_along), (MAX_LINE_MARKER_SIZE, MIN_MARKER_INTERVAL, -MAX_MARKER_INTERVAL));
        assert_eq!((got.average_angle, got.angle, got.at), (0.0, 270.0, Placements::LAST));
        v["line_strokes"][0]["markers"]["at"] = serde_json::json!(["sideways"]);
        assert_eq!(read(&v).at, Placements::INTERVAL);
        v["line_strokes"][0]["markers"]["at"] = serde_json::json!(vec!["first"; 5000]);
        assert_eq!(read(&v).at, Placements::FIRST);
        // Only what's given: the rest is the default.
        v["line_strokes"][0]["markers"] = serde_json::json!({});
        assert_eq!(read(&v), LineMarkers::default());
    }

    #[test]
    fn a_reference_scale_scales_sizes_with_the_map_and_hostile_ones_are_dropped() {
        let mut l = Layer::new(1, Dataset::new("lines", GeometryType::Polyline, vec![]));
        assert_eq!(l.size_factor(10_000.0), 1.0, "none: sizes stay as set");
        l.reference_scale = Some(25_000.0);
        assert_eq!(l.size_factor(25_000.0), 1.0);
        assert_eq!(l.size_factor(12_500.0), 2.0, "zoomed in twice: twice the size");
        assert_eq!(l.size_factor(100_000.0), 0.25);
        assert_eq!(l.size_factor(1.0), SIZE_FACTORS.1 as f32, "zoomed far in: kept in bounds");
        assert_eq!(l.size_factor(1e12), SIZE_FACTORS.0 as f32);
        for bad in [0.0, -5.0, f64::NAN, f64::INFINITY] {
            assert_eq!(l.size_factor(bad), 1.0, "scale {bad}");
        }
        // Saved and read back; a crafted project's out of range ones are none.
        let text = serde_json::to_string(&l).unwrap();
        assert_eq!(serde_json::from_str::<Layer>(&text).unwrap().reference_scale, Some(25_000.0));
        let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
        for bad in [serde_json::json!(0.0), serde_json::json!(-1.0), serde_json::json!(1e300), serde_json::Value::Null] {
            v["reference_scale"] = bad.clone();
            assert_eq!(serde_json::from_value::<Layer>(v.clone()).unwrap().reference_scale, None, "{bad}");
        }
        // Without a reference scale nothing is written.
        l.reference_scale = None;
        assert!(!serde_json::to_string(&l).unwrap().contains("reference_scale"));
    }

    #[test]
    fn no_fill_is_saved_only_when_on_and_an_unknown_one_reads_as_off() {
        let mut l = Layer::new(1, Dataset::new("parcels", GeometryType::Polygon, vec![]));
        assert!(!serde_json::to_string(&l).unwrap().contains("no_fill"));
        for how in [NoFill::Override, NoFill::Symbols] {
            l.no_fill = Some(how);
            let text = serde_json::to_string(&l).unwrap();
            assert_eq!(serde_json::from_str::<Layer>(&text).unwrap().no_fill, Some(how));
        }
        let mut v = serde_json::to_value(&l).unwrap();
        for unknown in [
            serde_json::json!("Later"),
            serde_json::json!(1),
            serde_json::json!(true),
            serde_json::json!({"Override": 1}),
            serde_json::Value::Null,
        ] {
            v["no_fill"] = unknown.clone();
            assert_eq!(serde_json::from_value::<Layer>(v.clone()).unwrap().no_fill, None, "{unknown}");
        }
    }

    #[test]
    fn hostile_class_strokes_in_a_project_are_brought_into_range() {
        // The same for a class's own strokes, Unique Values' (and <all other values>') and
        // Graduated Colors'; a class listed with none is dropped.
        let wild = vec![symbology::LineStroke { width: Some(f32::MAX), offset: 1e30, ..Default::default() }; 5000];
        let tame = |v: &[symbology::LineStroke]| {
            v.len() == symbology::MAX_STROKES
                && v.iter().all(|st| st.width == Some(symbology::MAX_STROKE_WIDTH) && st.offset == symbology::MAX_STROKE_OFFSET)
        };
        let s = Symbol::default_for(GeometryType::Polyline, 0);
        let mut l = Layer::new(1, Dataset::new("lines", GeometryType::Polyline, vec![]));
        l.renderer = Renderer::Unique {
            field: "k".into(),
            classes: vec![("a".into(), s), ("b".into(), s)],
            other: s,
            ramp: Ramp::Ember,
            class_labels: Default::default(),
            other_label: None,
            class_strokes: [("a".to_string(), wild.clone()), ("b".to_string(), Vec::new())].into_iter().collect(),
            other_strokes: wild.clone(),
            class_fills: Default::default(),
            other_fills: Vec::new(),
            expression: None,
        };
        let back: Layer = serde_json::from_str(&serde_json::to_string(&l).unwrap()).unwrap();
        let Renderer::Unique { class_strokes, other_strokes, .. } = &back.renderer else { panic!("unique renderer expected") };
        assert!(tame(&class_strokes["a"]) && !class_strokes.contains_key("b") && tame(other_strokes));
        l.renderer = Renderer::Graduated {
            field: "k".into(),
            breaks: vec![1.0],
            symbols: vec![s],
            ramp: Ramp::Ember,
            method: ClassMethod::NaturalBreaks,
            min: 0.0,
            classes: 1,
            class_labels: Default::default(),
            class_strokes: [(0, wild)].into_iter().collect(),
            class_fills: Default::default(),
        };
        let back: Layer = serde_json::from_str(&serde_json::to_string(&l).unwrap()).unwrap();
        let Renderer::Graduated { class_strokes, .. } = &back.renderer else { panic!("graduated renderer expected") };
        assert!(tame(&class_strokes[&0]));
        // A project from before per-class strokes reads with none.
        let old = r#"{"Unique":{"field":"k","classes":[],"other":{"fill":[0,0,0,255],"stroke":[0,0,0,255],"stroke_width":1,"size":0,"marker":"Circle"},"ramp":"Ember"}}"#;
        let r: Renderer = serde_json::from_str(old).unwrap();
        assert!(!r.has_class_strokes());
    }

    /// A layer of one long line along the equator with `n` vertices.
    fn long_line(n: usize) -> Layer {
        let mut d = Dataset::new("line", GeometryType::Polyline, vec![]);
        let run = (0..n).map(|i| Coord::new(-170.0 + 340.0 * i as f64 / n as f64, 0.0)).collect();
        d.push(Some(Geometry::Line(vec![run])), vec![]);
        Layer::new(1, d)
    }

    #[test]
    fn ids_stay_unique_when_a_project_reaches_the_largest_id() {
        let mut m = Map::new("m");
        m.layers.push(Layer::new(u32::MAX, points()));
        let group = ServiceGroup {
            id: 0,
            name: "g".into(),
            url: String::new(),
            kind: String::new(),
            anonymous: false,
            expanded: true,
            children: vec![],
        };
        let a = m.add_dataset(points());
        let b = m.add_service_group(group);
        let c = m.add_dataset(points());
        let ids = [u32::MAX, a, b, c];
        assert!(ids.iter().all(|&id| id != 0));
        assert_eq!(ids.iter().collect::<std::collections::HashSet<_>>().len(), ids.len(), "{ids:?}");
    }

    #[test]
    fn a_map_takes_the_caches_a_copy_of_it_filled() {
        let mut m = Map::new("m");
        let id = m.add_dataset(points());
        let mut copy = m.clone();
        copy.layer_mut(id).unwrap().projected();
        m.adopt_caches(&copy);
        assert!(m.layer(id).unwrap().is_cached("mercator"));
        // Not once the geometry changed since.
        let mut m = Map::new("m");
        let id = m.add_dataset(points());
        let mut copy = m.clone();
        copy.layer_mut(id).unwrap().projected();
        m.layer_mut(id).unwrap().touch_geometry();
        m.adopt_caches(&copy);
        assert!(!m.layer(id).unwrap().is_cached("mercator"));
    }

    #[test]
    fn display_projection_is_lazy_shared_bounded_and_invalidated() {
        let flat = projection::Flat::Mercator;
        let mut l = long_line(lod::THIN_ABOVE + 1000);
        let original = l.dataset.features.clone();
        assert!(l.prepare_display(&flat, 12_000.0, false));
        assert!(l.projected_cache().is_empty(), "display must not project discarded vertices");
        let (g, extents) = l.display_cache().unwrap();
        assert!(lod::vertices(g[0].as_ref().unwrap()) < (lod::THIN_ABOVE + 1000) / 40);
        let expected_extent = flat.project(original[0].geometry.as_ref().unwrap()).extent();
        assert_eq!(extents, &[expected_extent]);
        let before = l.view_cache_bytes();
        assert!(before > rustgis_data::source::memory::features_bytes(&original));
        let mut copy = l.clone();
        assert!(copy.prepare_display(&flat, 12_000.0, false));
        assert_eq!(copy.display_cache().unwrap().0.as_ptr(), l.display_cache().unwrap().0.as_ptr());
        let mut receiver = long_line(1);
        receiver.dataset = l.dataset.clone();
        receiver.geometry_rev = l.geometry_rev;
        assert!(receiver.adopt_cache(&copy));
        assert_eq!(receiver.display_cache().unwrap().0.as_ptr(), l.display_cache().unwrap().0.as_ptr());
        // Requesting exact positions remains exact and drops the optional display copy.
        assert_eq!(copy.projected().len(), 1);
        assert!(copy.display_cache().is_none());
        assert_eq!(lod::vertices(copy.projected_cache()[0].as_ref().unwrap()), lod::THIN_ABOVE + 1000);
        l.touch_geometry();
        assert!(l.display_cache().is_none());
        assert!(l.prepare_display(&flat, 24_000.0, false));
        l.clear_cache();
        assert!(l.display_cache().is_none());
        assert_eq!(l.view_cache_bytes(), rustgis_data::source::memory::features_bytes(&original));
        assert!(lod::display(&original, 0, 12, 1, false).is_none());
        assert!(!long_line(100).prepare_display(&flat, 12_000.0, false));
        assert_eq!(l.dataset.features, original);
    }

    /// A worker's display-only snapshot must preserve exact positions filled later by the UI.
    #[test]
    fn adopting_display_preserves_newer_exact_cache_and_its_budget() {
        let flat = projection::Flat::Mercator;
        for tight in [false, true] {
            let mut receiver = long_line(lod::THIN_ABOVE + 1000);
            let mut worker = receiver.clone();
            assert!(worker.prepare_display(&flat, 12_000.0, false));
            receiver.thinned_in(&flat, "mercator", 24_000.0);
            let exact = receiver.projected.clone();
            let extents = receiver.projected_extents.clone();
            let thinned = receiver.thinned.clone();
            let base = receiver.view_memory.base(&receiver.dataset.features, &receiver.projected, &receiver.projected_extents);
            if tight {
                let mut source = source::SourceLayer::default();
                source.byte_budget = base;
                worker.source = Some(source.clone());
                receiver.source = Some(source);
            }
            assert!(receiver.adopt_cache(&worker));
            assert!(receiver.is_cached("mercator"), "display-only adoption must keep exact positions");
            assert!(std::sync::Arc::ptr_eq(&exact, &receiver.projected));
            assert!(std::sync::Arc::ptr_eq(&extents, &receiver.projected_extents));
            if tight {
                assert!(receiver.display_cache().is_none());
                assert!(receiver.display_levels.is_empty());
                assert!(receiver.thinned.iter().all(|(_, c)| c.is_none()));
                assert!(receiver.view_cache_bytes() <= base);
            } else {
                assert!(std::sync::Arc::ptr_eq(&thinned, &receiver.thinned));
                assert_eq!(receiver.display_cache().unwrap().0.as_ptr(), worker.display_cache().unwrap().0.as_ptr());
            }
            receiver.projected();
            assert!(std::sync::Arc::ptr_eq(&exact, &receiver.projected), "the next exact request must reuse the allocation");
        }
    }

    #[test]
    fn streamed_display_uses_whole_layer_eligibility_and_respects_remaining_budget() {
        let flat = projection::Flat::Mercator;
        let mut chunk = long_line(100);
        assert!(!chunk.prepare_display(&flat, 1e6, false));
        // The complete source is detailed even though this chunk alone is small.
        assert!(chunk.prepare_display(&flat, 1e6, true));
        assert!(chunk.projected_cache().is_empty());
        let (g, extents) = chunk.display_cache().unwrap();
        assert!(lod::vertices(g[0].as_ref().unwrap()) < 100);
        assert_eq!(extents[0], flat.project(chunk.dataset.features[0].geometry.as_ref().unwrap()).extent());
        assert!(!chunk.prepare_display(&flat, 1e6, false), "chunk eligibility must not leak into ordinary drawing");
        assert!(chunk.prepare_display(&flat, 1e6, true));
        chunk.clear_cache();
        let mut source = source::SourceLayer::default();
        source.byte_budget = 1;
        chunk.source = Some(source);
        assert!(!chunk.prepare_display(&flat, 1e6, true));
        assert!(chunk.display_cache().is_none());
        // Budget exhaustion falls back to the original, fully projected geometry.
        let full = flat.project(chunk.dataset.features[0].geometry.as_ref().unwrap());
        assert_eq!(chunk.thinned_in(&flat, "mercator", 1e6), &[Some(full)]);
    }

    /// #1456: a view holding more features than it reads keeps its limit of the definition
    /// query's matches, so a match the unfiltered sample leaves out still draws.
    #[test]
    fn a_streamed_view_reads_its_definition_querys_matches() {
        let mut ds = Dataset::new("sites", GeometryType::Point, vec![Field::new("n", FieldType::Integer)]);
        for i in 0..5 {
            ds.push(Some(Geometry::Point(Coord::new(f64::from(i), 0.0))), vec![Value::Int(i64::from(i))]);
        }
        let bytes = std::sync::Arc::new(rustgis_data::source::fgb::write(&ds).unwrap());
        let handle = rustgis_data::source::open_shared("sites.fgb", bytes.clone(), None).unwrap();
        let mut src = source::SourceLayer::from_bytes("sites.fgb", bytes, None, handle);
        src.limit = 2;
        let mut l = Layer::new_streamed(1, src);
        let view = Extent::new(-1.0, -1.0, 5.0, 1.0);
        let shown = |l: &Layer| -> Vec<Value> {
            l.visible_rows()
                .iter()
                .zip(l.dataset.features.iter())
                .filter(|(v, _)| **v)
                .filter_map(|(_, f)| f.attrs.first().cloned())
                .collect()
        };
        l.sync_source(&view, true);
        let sampled = shown(&l);
        assert_eq!(sampled.len(), 2, "the view reads a sample of two");
        let left_out = (0..5).find(|n| !sampled.contains(&Value::Int(*n))).unwrap();
        l.definition_query = Some(format!("n = {left_out}"));
        assert!(shown(&l).is_empty(), "filtering the sample loses the match");
        l.sync_source(&view, true);
        assert_eq!(shown(&l), [Value::Int(left_out)]);
        // More matches than the view reads: a sample of them, up to its limit, counted as matches.
        l.definition_query = Some(format!("n <> {left_out}"));
        l.sync_source(&view, true);
        assert_eq!((shown(&l).len(), l.dataset.features.len()), (2, 2));
        let src = l.source.as_ref().unwrap();
        assert_eq!((src.sampled(), src.sampled_matches()), (Some((2, 4)), true));
        // A row number is a row's place in the view: the view is read as without a query. The
        // words in a text are only text.
        l.definition_query = Some("@row_number = 1".into());
        l.sync_source(&view, true);
        let src = l.source.as_ref().unwrap();
        assert_eq!((src.sampled(), src.sampled_matches()), (Some((2, 5)), false));
        l.definition_query = Some(format!("n = {left_out} or 'x' = '@row_number'"));
        l.sync_source(&view, true);
        assert_eq!(shown(&l), [Value::Int(left_out)]);
        // However it's quoted or nested, a row number is found in the parsed query.
        l.definition_query = Some(format!("!n! = {left_out} and (@row_number >= 1 or 'it''s' = '')"));
        l.sync_source(&view, true);
        assert!(!l.source.as_ref().unwrap().sampled_matches());
        // A query that doesn't parse leaves the view as read without one, as for loaded layers.
        l.definition_query = Some("n = = 1".into());
        l.sync_source(&view, true);
        assert_eq!(shown(&l), sampled);
    }

    #[test]
    fn geometry_only_lod_does_not_charge_for_unused_extents() {
        let l = long_line(100);
        let copy = lod::project_budgeted(&l.dataset.features, 20, usize::MAX).unwrap();
        let budget = lod::cache_bytes(&copy);
        assert_eq!(lod::project_budgeted(&l.dataset.features, 20, budget).unwrap(), copy);
        assert!(lod::display(&l.dataset.features, 0, 20, budget, true).is_none());
    }

    /// Tilted bands and zoom reversals reuse bounded display levels shared by snapshots.
    #[test]
    fn display_levels_are_reused_across_bands_and_bounded_together() {
        let flat = projection::Flat::Mercator;
        let mut l = long_line(lod::THIN_ABOVE + 1000);
        let tolerances = [12_000.0, 24_000.0, 48_000.0];
        let before = lod::DISPLAY_BUILDS.get();
        let mut pointers = Vec::new();
        for tol in tolerances {
            assert!(l.prepare_display(&flat, tol, false));
            pointers.push(l.display_cache().unwrap().0.as_ptr());
        }
        let mut copy = l.clone();
        for _ in 0..3 {
            for (tol, pointer) in tolerances.into_iter().zip(&pointers) {
                assert!(copy.prepare_display(&flat, tol, false));
                assert_eq!(copy.display_cache().unwrap().0.as_ptr(), *pointer);
            }
        }
        assert_eq!(lod::DISPLAY_BUILDS.get(), before + 3);
        assert!(copy.projected_cache().is_empty());
        let available = copy.display_levels.iter().map(|d| d.bytes).min().unwrap();
        let base = copy.view_memory.base(&copy.dataset.features, &copy.projected, &copy.projected_extents);
        let mut source = source::SourceLayer::default();
        source.byte_budget = base + available;
        copy.source = Some(source);
        assert!(copy.prepare_display(&flat, 48_000.0, false));
        assert!(copy.display_levels.iter().map(|d| d.bytes).sum::<usize>() <= available);
        assert_eq!(copy.display_levels.len(), 1);
        copy.touch_geometry();
        assert!(copy.prepare_display(&flat, 48_000.0, false));
        assert_eq!(lod::DISPLAY_BUILDS.get(), before + 4);
        copy.clear_cache();
        assert!(copy.display_levels.is_empty());
    }

    /// Unchanged budgets leave snapshot LOD arrays shared; only eviction needs a private array.
    #[test]
    fn trimming_lod_preserves_snapshot_sharing_when_nothing_is_evicted() {
        let flat = projection::Flat::Mercator;
        let mut l = long_line(lod::THIN_ABOVE + 1000);
        l.thinned_in(&flat, "mercator", 12_000.0);
        let mut copy = l.clone();
        copy.trim_thinned_levels(lod::CACHE_BYTES);
        assert!(std::sync::Arc::ptr_eq(&l.thinned, &copy.thinned));
        copy.trim_thinned_levels(0);
        assert!(!std::sync::Arc::ptr_eq(&l.thinned, &copy.thinned));
        assert!(l.thinned.iter().any(|(_, c)| c.is_some()));
        assert!(copy.thinned.iter().all(|(_, c)| c.is_none()));
    }

    /// Display and exact-consumer LOD levels coexist within one source budget, including shrinkage.
    #[test]
    fn display_and_exact_lod_share_one_budget() {
        let flat = projection::Flat::Mercator;
        let mut l = long_line(lod::THIN_ABOVE + 1000);
        assert!(l.prepare_display(&flat, 12_000.0, false));
        let display_bytes = l.display_levels[0].bytes;
        l.projected();
        assert_eq!(l.display_levels.len(), 1);
        let base = l.view_memory.base(&l.dataset.features, &l.projected, &l.projected_extents);
        let copy = lod::project_budgeted(&l.dataset.features, lod::level(24_000.0).unwrap(), usize::MAX).unwrap();
        let extra = lod::cache_bytes(&copy);
        let mut source = source::SourceLayer::default();
        source.byte_budget = base + display_bytes + extra;
        l.source = Some(source);
        l.thinned_in(&flat, "mercator", 24_000.0);
        assert_eq!(l.display_levels.len(), 1);
        assert!(l.thinned.iter().any(|(_, c)| c.is_some()));
        assert!(l.view_cache_bytes() <= base + display_bytes + extra);
        l.source.as_mut().unwrap().byte_budget = base + display_bytes;
        l.thinned_in(&flat, "mercator", 24_000.0);
        assert_eq!(l.display_levels.len(), 1);
        assert!(l.thinned.iter().all(|(_, c)| c.is_none()));
        assert!(l.view_cache_bytes() <= base + display_bytes);
        l.source.as_mut().unwrap().byte_budget = base;
        l.thinned_in(&flat, "mercator", f64::NAN);
        assert!(l.display_levels.is_empty());
        assert!(l.thinned.iter().all(|(_, c)| c.is_none()));
        assert!(l.view_cache_bytes() <= base);
    }

    /// Invalid requests hide the active display without discarding reusable Mercator levels.
    #[test]
    fn invalid_display_requests_clear_the_active_cache() {
        let flat = projection::Flat::Mercator;
        let mut l = long_line(lod::THIN_ABOVE + 1000);
        for tol in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(l.prepare_display(&flat, 12_000.0, false));
            assert!(l.display_cache().is_some());
            assert!(!l.prepare_display(&flat, tol, false));
            assert!(l.display_cache().is_none());
            assert_eq!(l.display_levels.len(), 1, "invalid requests keep the reusable level");
        }
        assert!(l.prepare_display(&flat, 12_000.0, false));
        let custom = projection::Flat::of(&Projection::Custom("EPSG:32637".into()));
        assert!(!l.prepare_display(&custom, 12_000.0, false));
        assert!(l.display_cache().is_none());
    }

    /// Alternating failed zoom levels must not repeatedly walk all source vertices.
    #[test]
    fn several_rejected_display_levels_are_memoized() {
        let flat = projection::Flat::Mercator;
        let mut l = long_line(lod::THIN_ABOVE + 1000);
        let before = lod::DISPLAY_BUILDS.get();
        for _ in 0..3 {
            for tol in [0.001, 0.002, 0.004] {
                assert!(!l.prepare_display(&flat, tol, false));
            }
        }
        assert_eq!(lod::DISPLAY_BUILDS.get(), before + 3);
        assert_eq!(l.display_rejected.len(), lod::LEVELS_KEPT);
        l.touch_geometry();
        assert!(!l.prepare_display(&flat, 0.001, false));
        assert_eq!(lod::DISPLAY_BUILDS.get(), before + 4);
    }

    #[test]
    fn rejected_display_levels_are_not_rebuilt_on_unchanged_frames() {
        let flat = projection::Flat::Mercator;
        let mut l = long_line(lod::THIN_ABOVE + 1000);
        assert!(!l.prepare_display(&flat, 0.001, false));
        let built = lod::DISPLAY_BUILDS.get();
        l.thinned_in(&flat, "mercator", 0.001);
        assert!(!l.prepare_display(&flat, 0.001, false));
        assert_eq!(lod::DISPLAY_BUILDS.get(), built, "thinned_in builds geometry only");
        l.thinned_in(&flat, "mercator", 0.001);
        assert!(!l.prepare_display(&flat, 0.001, false));
        assert_eq!(lod::DISPLAY_BUILDS.get(), built);
        l.touch_geometry();
        assert!(!l.prepare_display(&flat, 0.001, false));
        assert_eq!(lod::DISPLAY_BUILDS.get(), built + 1, "a changed geometry must retry");
    }

    #[test]
    fn a_detailed_layer_draws_from_a_thinned_copy() {
        let flat = projection::Flat::Mercator;
        let mut l = long_line(lod::THIN_ABOVE + 1000);
        let full = lod::vertices(l.projected()[0].as_ref().unwrap());
        // Seen whole (about 40 km a pixel), one vertex every 8 km (the level under 12 km) is enough.
        let thinned = lod::vertices(l.thinned_in(&flat, "mercator", 12_000.0)[0].as_ref().unwrap());
        assert!(thinned < full / 40, "{thinned} of {full} vertices drawn");
        assert_eq!(lod::vertices(l.thinned_cache(12_000.0)[0].as_ref().unwrap()), thinned);
        // Zoomed in past its vertex spacing, every vertex draws.
        assert_eq!(lod::vertices(l.thinned_in(&flat, "mercator", 0.001)[0].as_ref().unwrap()), full);
        // Its extents are cached row for row, and a clone shares the caches.
        assert_eq!(l.projected_extents().len(), 1);
        let copy = l.clone();
        assert_eq!(copy.projected_cache().as_ptr(), l.projected_cache().as_ptr());
        assert_eq!(copy.thinned_cache(12_000.0).as_ptr(), l.thinned_cache(12_000.0).as_ptr());
        // A change of geometry drops the thinned copies with the projection.
        l.touch_geometry();
        l.projected();
        assert_eq!(l.thinned_cache(12_000.0).as_ptr(), l.projected_cache().as_ptr());
    }

    #[test]
    fn a_level_saving_little_draws_in_full() {
        // Zoomed in past the vertex spacing, thinning drops nothing: no copy is kept.
        let flat = projection::Flat::Mercator;
        let mut l = long_line(lod::THIN_ABOVE + 1000);
        l.projected();
        assert_eq!(l.thinned_in(&flat, "mercator", 1.0).as_ptr(), l.projected_cache().as_ptr());
        // Nor for points, however many.
        let mut d = Dataset::new("pts", GeometryType::Point, vec![]);
        let pts = (0..lod::THIN_ABOVE + 10).map(|i| Coord::new(i as f64 * 1e-6, 0.0)).collect();
        d.push(Some(Geometry::MultiPoint(pts)), vec![]);
        let mut p = Layer::new(2, d);
        p.projected();
        assert_eq!(p.thinned_in(&flat, "mercator", 1e6).as_ptr(), p.projected_cache().as_ptr());
    }

    #[test]
    fn a_small_layer_always_draws_in_full() {
        let flat = projection::Flat::Mercator;
        let mut l = long_line(1000);
        let full = l.projected().to_vec();
        assert_eq!(l.thinned_in(&flat, "mercator", 1e6), &full[..]);
    }

    #[test]
    fn layer_order_and_hit() {
        let mut m = Map::new("Map");
        let poly = Dataset::new("poly", GeometryType::Polygon, vec![]);
        m.add_dataset(poly);
        let id = m.add_dataset(points());
        assert_eq!(m.layers[0].id, id, "points go on top of polygons");
        let p = lonlat_to_merc(Coord::new(10.0, 10.0));
        assert_eq!(m.hit(p, 1000.0, 1e6, |_| true), Some((id, 1)));
        let e = Extent::from_points(lonlat_to_merc(Coord::new(-1.0, -1.0)), lonlat_to_merc(Coord::new(1.0, 1.0)));
        assert_eq!(m.hits_in(&e, 1e6), vec![(id, vec![0])]);
        m.layers[0].definition_query = Some("n = 2".into());
        assert_eq!(m.hits_in(&e, 1e6), vec![]);
    }

    #[test]
    fn hits_at_finds_every_overlapping_feature() {
        let mut m = Map::new("Map");
        let mut d = points();
        d.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![Value::Int(3)]);
        let below = m.add_dataset(d);
        let above = m.add_dataset(points());
        let p = lonlat_to_merc(Coord::new(0.0, 0.0));
        // Top layer first; in a layer, the feature drawn last (on top) first.
        assert_eq!(m.hits_at(p, 1000.0, 1e6, |_| true, false, 100), vec![(above, 0), (below, 2), (below, 0)]);
        assert_eq!(m.hits_at(p, 1000.0, 1e6, |_| true, false, 2), vec![(above, 0), (below, 2)]);
        assert_eq!(m.hits_at(p, 1000.0, 1e6, |l| l.id == below, false, 100), vec![(below, 2), (below, 0)]);
        // Only the top layer with a hit.
        assert_eq!(m.hits_at(p, 1000.0, 1e6, |_| true, true, 100), vec![(above, 0)]);
        assert_eq!(m.hits_at(p, 1000.0, 1e6, |l| l.id == below, true, 100), vec![(below, 2), (below, 0)]);
        // A higher depth priority draws on top, whatever the order.
        if let Some(l) = m.layers.iter_mut().find(|l| l.id == below) {
            l.priority = 1;
        }
        assert_eq!(m.hits_at(p, 1000.0, 1e6, |_| true, true, 100), vec![(below, 2), (below, 0)]);
        assert_eq!(m.hits_at(p, 1000.0, 1e6, |_| true, false, 100), vec![(below, 2), (below, 0), (above, 0)]);
        if let Some(l) = m.layers.iter_mut().find(|l| l.id == below) {
            l.priority = 0;
        }
        // Features the definition query hides aren't hit.
        if let Some(l) = m.layers.iter_mut().find(|l| l.id == below) {
            l.definition_query = Some("n = 1".into());
        }
        assert_eq!(m.hits_at(p, 1000.0, 1e6, |l| l.id == below, false, 100), vec![(below, 0)]);
    }

    #[test]
    fn hits_at_skips_features_display_filters_hide() {
        let mut m = Map::new("Map");
        let mut d = points();
        d.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![Value::Int(3)]);
        let id = m.add_dataset(d);
        let p = lonlat_to_merc(Coord::new(0.0, 0.0));
        let filter = |min_scale| effects::DisplayFilter { name: "big".into(), min_scale, max_scale: None, where_: "n = 3".into() };
        m.layers[0].display_filters = vec![filter(None)];
        assert_eq!(m.layers[0].drawn_rows(1e6), vec![false, false, true]);
        assert_eq!(m.hits_at(p, 1000.0, 1e6, |_| true, false, 100), vec![(id, 2)]);
        // A filter that doesn't apply at this scale hides nothing.
        m.layers[0].display_filters = vec![filter(Some(1e5))];
        assert_eq!(m.hits_at(p, 1000.0, 1e6, |_| true, false, 100), vec![(id, 2), (id, 0)]);
    }

    #[test]
    fn select_hits_skip_hidden_features_and_follow_draw_order() {
        let mut m = Map::new("Map");
        let mut d = points();
        d.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![Value::Int(3)]);
        let below = m.add_dataset(d);
        let above = m.add_dataset(points());
        let p = lonlat_to_merc(Coord::new(0.0, 0.0));
        let e = Extent::from_points(lonlat_to_merc(Coord::new(-1.0, -1.0)), lonlat_to_merc(Coord::new(1.0, 1.0)));
        let all = |_: &Layer| true;
        assert_eq!(m.hit(p, 1000.0, 1e6, all), Some((above, 0)));
        // A higher depth priority draws on top, whatever the order (#243).
        m.layers.iter_mut().filter(|l| l.id == below).for_each(|l| l.priority = 1);
        assert_eq!(m.hit(p, 1000.0, 1e6, all), Some((below, 2)));
        m.layers.iter_mut().filter(|l| l.id == below).for_each(|l| l.priority = 0);
        // Features a display filter hides are neither clicked nor boxed (#243).
        let only = |w: &str| vec![effects::DisplayFilter { name: "f".into(), min_scale: None, max_scale: None, where_: w.into() }];
        m.layers.iter_mut().filter(|l| l.id == above).for_each(|l| l.display_filters = only("n = 2"));
        m.layers.iter_mut().filter(|l| l.id == below).for_each(|l| l.display_filters = only("n = 3"));
        assert_eq!(m.hit(p, 1000.0, 1e6, all), Some((below, 2)));
        assert_eq!(m.hits_in(&e, 1e6), vec![(below, vec![2])]);
        // A layer the click doesn't take doesn't hide the ones below it.
        m.layers.iter_mut().for_each(|l| l.display_filters.clear());
        assert_eq!(m.hit(p, 1000.0, 1e6, |l| l.id != above), Some((below, 2)));
    }

    #[test]
    fn shown_extent_follows_the_definition_query() {
        let mut m = Map::new("Map");
        m.add_dataset(points());
        let l = &mut m.layers[0];
        assert_eq!(l.shown_extent(), Extent::new(0.0, 0.0, 10.0, 10.0));
        l.definition_query = Some("n = 2".into());
        assert_eq!(l.shown_extent(), Extent::new(10.0, 10.0, 10.0, 10.0));
        l.definition_query = Some("n = 3".into());
        assert!(l.shown_extent().is_empty());
        // An invalid query hides nothing.
        l.definition_query = Some("n = = 2".into());
        assert_eq!(l.shown_extent(), Extent::new(0.0, 0.0, 10.0, 10.0));
        // The map's shown extent (Full Extent) follows it too; its full extent doesn't.
        m.layers[0].definition_query = Some("n = 2".into());
        assert_eq!(m.shown_extent(), Extent::new(10.0, 10.0, 10.0, 10.0));
        assert_eq!(m.full_extent(), Extent::new(0.0, 0.0, 10.0, 10.0));
    }

    #[test]
    fn the_shown_legend_is_what_the_layer_draws() {
        let mut m = Map::new("Map");
        m.add_dataset(points());
        let l = &mut m.layers[0];
        // Classes for n = 1, 2 and 9: 9 is no feature's value.
        let s = |c| Symbol::default_for(GeometryType::Point, c);
        l.renderer = Renderer::Unique {
            field: "n".into(),
            classes: vec![("1".into(), s(0)), ("2".into(), s(1)), ("9".into(), s(2))],
            other: s(3),
            ramp: Ramp::Ember,
            class_labels: Default::default(),
            other_label: None,
            class_strokes: Default::default(),
            other_strokes: Vec::new(),
            class_fills: Default::default(),
            other_fills: Vec::new(),
            expression: None,
        };
        let labels = |l: &Layer, scale| l.shown_legend(scale).map(|v| v.into_iter().map(|e| e.label).collect::<Vec<_>>());
        assert_eq!(labels(l, 1e6), Some(vec!["1".to_string(), "2".to_string()]), "no 9, no <all other values>");
        // The definition query and display filters narrow it.
        l.definition_query = Some("n = 2".into());
        assert_eq!(labels(l, 1e6), Some(vec!["2".to_string()]));
        l.definition_query = None;
        l.display_filters = vec![effects::DisplayFilter { name: "f".into(), min_scale: None, max_scale: None, where_: "n = 1".into() }];
        assert_eq!(labels(l, 1e6), Some(vec!["1".to_string()]));
        // Nothing drawn: no legend at all.
        l.display_filters.clear();
        l.definition_query = Some("n = 3".into());
        assert_eq!(labels(l, 1e6), None);
        l.definition_query = None;
        l.min_scale = Some(1e5);
        assert_eq!(labels(l, 1e6), None, "out of its scale range");
        assert!(labels(l, 5e4).is_some());
        // The full legend is still there for "all classes".
        assert_eq!(l.renderer.legend().len(), 4);
        // An empty layer, with no query or filter to evaluate, draws nothing either.
        l.min_scale = None;
        l.dataset.features.clear();
        assert_eq!(labels(l, 1e6), None);
    }
}
