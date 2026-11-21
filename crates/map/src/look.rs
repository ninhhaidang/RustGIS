//! What a frame draws a map and each of its layers from, kept to tell whether a picture drawn
//! earlier still shows them (#1057). A renderer keeps a [`LayerLook`] for each layer it drew and
//! a [`MapLook`] for the rest of the map, and draws again only from the first layer whose look
//! no longer matches. Neither look records where a layer is in the drawing order: the renderer
//! compares them place by place, each with its layer's id.
//!
//! A look is compared field by field, with every field of [`Layer`] and [`Map`] named, so a field
//! added later has to be placed: drawn (compared) or not (left out with a reason). Rows are
//! compared by their allocation ([`rustgis_data::Features::allocation_key`]), which any edit
//! changes and which doesn't keep them: a look holds no copy of a layer's data.

use std::sync::Weak;

use rustgis_data::{Dataset, Feature};

use crate::raster::RasterLook;
use crate::{Layer, Map};

/// How a layer has changed since its [`LayerLook`] was taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// It draws as it did.
    Same,
    /// Only its transparency differs: a picture of the layer alone, drawn opaque, still shows it.
    Transparency,
    /// It draws differently.
    Drawn,
}

/// What a layer was drawn from: its settings, without its rows or caches, and its rows'
/// allocation (see the module).
#[derive(Clone, Debug)]
pub struct LayerLook {
    /// The layer, its rows, raster and caches taken out.
    layer: Layer,
    rows: Weak<Vec<Feature>>,
    raster: Option<RasterLook>,
}

/// `layer` without its rows, raster, caches and what else a look leaves out (see the module),
/// copied field by field: a whole copy taken first and pared down would copy, in every frame, what
/// grows with the data, such as an edit archive's whole features for every edit.
fn bare(layer: &Layer) -> Layer {
    let Layer {
        id,
        name,
        visible,
        dataset,
        renderer,
        labels,
        transparency,
        min_scale,
        max_scale,
        definition_query,
        time,
        selection,
        expanded,
        selectable,
        graphics,
        blend,
        mask,
        display_filters,
        aggregation,
        extrusion,
        line_strokes,
        fill_layers,
        no_fill,
        hidden_fills,
        stroke_levels,
        marker_rotation,
        diagram,
        symbol_by,
        size_by,
        reference_scale,
        priority,
        design,
        // Compared by its own look.
        raster: _,
        source,
        // Saving back to a database or GeoPackage keeps per-row versions, which aren't drawn.
        postgis: _,
        gpkg: _,
        synced: _,
        connection,
        anonymous,
        instance_id,
        replacement_rev,
        // Caches, emptied below.
        projected,
        display,
        display_rejected: _,
        display_levels,
        projected_extents,
        projected_vertices,
        thinned,
        projected_rev,
        projected_for,
        projected_crs,
        lonlat,
        unplaced: _,
        flat_unplaced: _,
        extent_cache: _,
        drawn: _,
        view_memory: _,
        geometry_kinds: _,
        geometry_rev,
    } = layer;
    let crate::design::Design { domains, subtypes, rules, contingent, archive, templates } = design;
    let mut kept = Layer {
        id: *id,
        name: name.clone(),
        visible: *visible,
        // Its rows are shared, so the copy costs a pointer; dropped below.
        dataset: dataset.clone(),
        renderer: renderer.clone(),
        labels: labels.clone(),
        transparency: *transparency,
        min_scale: *min_scale,
        max_scale: *max_scale,
        definition_query: definition_query.clone(),
        // A time binding keeps the rows its dates were read from: only what it draws by.
        time: time.as_ref().map(|t| {
            let mut bare = crate::time::TimeBinding::new(t.field.clone());
            bare.window = t.window;
            bare
        }),
        selection: selection.clone(),
        expanded: *expanded,
        selectable: *selectable,
        graphics: *graphics,
        blend: *blend,
        mask: mask.clone(),
        display_filters: display_filters.clone(),
        aggregation: *aggregation,
        extrusion: extrusion.clone(),
        line_strokes: line_strokes.clone(),
        fill_layers: fill_layers.clone(),
        no_fill: *no_fill,
        hidden_fills: hidden_fills.clone(),
        stroke_levels: *stroke_levels,
        marker_rotation: marker_rotation.clone(),
        diagram: diagram.clone(),
        symbol_by: symbol_by.clone(),
        size_by: size_by.clone(),
        reference_scale: *reference_scale,
        priority: *priority,
        design: crate::design::Design {
            domains: domains.clone(),
            subtypes: subtypes.clone(),
            rules: rules.clone(),
            contingent: contingent.clone(),
            // Whether edits are archived, not the whole features from before and after each.
            archive: archive.as_ref().map(|_| Vec::new()),
            templates: templates.clone(),
        },
        raster: None,
        source: source.clone(),
        postgis: None,
        gpkg: None,
        synced: None,
        connection: connection.clone(),
        anonymous: *anonymous,
        instance_id: *instance_id,
        replacement_rev: *replacement_rev,
        projected: projected.clone(),
        display: display.clone(),
        display_rejected: Vec::new(),
        display_levels: display_levels.clone(),
        projected_extents: projected_extents.clone(),
        projected_vertices: *projected_vertices,
        thinned: thinned.clone(),
        projected_rev: *projected_rev,
        projected_for: projected_for.clone(),
        projected_crs: *projected_crs,
        lonlat: lonlat.clone(),
        unplaced: Default::default(),
        flat_unplaced: Default::default(),
        extent_cache: Default::default(),
        drawn: Default::default(),
        view_memory: Default::default(),
        geometry_kinds: Default::default(),
        geometry_rev: *geometry_rev,
    };
    kept.dataset.features = Default::default();
    kept.clear_cache();
    kept
}

impl LayerLook {
    /// The look of `layer`; `None` for one whose drawing reads more than the layer holds, which no
    /// look can vouch for: a source read in place (its file is read again for each frame) or a
    /// raster without a look ([`crate::RasterLayer::look`]: read in place, still being read, or
    /// lit by shadows worked out in the background).
    pub fn of(layer: &Layer) -> Option<LayerLook> {
        if layer.source.is_some() {
            return None;
        }
        let raster = match &layer.raster {
            Some(r) => Some(r.look()?),
            None => None,
        };
        let rows = layer.dataset.features.allocation_key();
        Some(LayerLook { layer: bare(layer), rows, raster })
    }

    /// How the layer `now` was taken of differs from the one this look was taken of.
    pub fn change(&self, now: &LayerLook) -> Change {
        let raster = match (&self.raster, &now.raster) {
            (Some(a), Some(b)) => a.same(b),
            (a, b) => a.is_none() && b.is_none(),
        };
        if !raster || !Weak::ptr_eq(&self.rows, &now.rows) {
            return Change::Drawn;
        }
        let layer = &now.layer;
        let Layer {
            id,
            name,
            visible,
            dataset,
            renderer,
            labels,
            transparency,
            min_scale,
            max_scale,
            definition_query,
            time,
            selection,
            // The Layers pane's, not the map's.
            expanded: _,
            selectable,
            graphics,
            blend,
            mask,
            display_filters,
            aggregation,
            extrusion,
            line_strokes,
            fill_layers,
            no_fill,
            hidden_fills,
            stroke_levels,
            marker_rotation,
            diagram,
            symbol_by,
            size_by,
            reference_scale,
            priority,
            design,
            // Compared above, and `of` turns down a layer with a source.
            raster: _,
            source: _,
            // Saving back to a database or GeoPackage: what is drawn is the rows.
            postgis: _,
            gpkg: _,
            synced: _,
            connection: _,
            anonymous: _,
            instance_id,
            replacement_rev,
            // Caches worked out from the rows, which are compared themselves.
            projected: _,
            display: _,
            display_rejected: _,
            display_levels: _,
            projected_extents: _,
            projected_vertices: _,
            thinned: _,
            projected_rev: _,
            projected_for: _,
            projected_crs: _,
            lonlat: _,
            // What the last drawing counted and whether it drew the layer; neither is the look.
            unplaced: _,
            flat_unplaced: _,
            extent_cache: _,
            drawn: _,
            view_memory: _,
            geometry_kinds: _,
            geometry_rev,
        } = &self.layer;
        let drawn = *id == layer.id
            && *name == layer.name
            && *visible == layer.visible
            && same_dataset(dataset, &layer.dataset)
            && *renderer == layer.renderer
            && *labels == layer.labels
            && *min_scale == layer.min_scale
            && *max_scale == layer.max_scale
            && *definition_query == layer.definition_query
            && same_time(time.as_ref(), layer.time.as_ref())
            && *selection == layer.selection
            && *selectable == layer.selectable
            && *graphics == layer.graphics
            && *blend == layer.blend
            && *mask == layer.mask
            && *display_filters == layer.display_filters
            && *aggregation == layer.aggregation
            && *extrusion == layer.extrusion
            && *line_strokes == layer.line_strokes
            && *fill_layers == layer.fill_layers
            && *no_fill == layer.no_fill
            && *hidden_fills == layer.hidden_fills
            && *stroke_levels == layer.stroke_levels
            && *marker_rotation == layer.marker_rotation
            && *diagram == layer.diagram
            && *symbol_by == layer.symbol_by
            && *size_by == layer.size_by
            && *reference_scale == layer.reference_scale
            && *priority == layer.priority
            && *design == layer.design
            && *instance_id == layer.instance_id
            && *replacement_rev == layer.replacement_rev
            && *geometry_rev == layer.geometry_rev;
        if !drawn {
            Change::Drawn
        } else if transparency.to_bits() != layer.transparency.to_bits() {
            Change::Transparency
        } else {
            Change::Same
        }
    }

    /// The layer's id.
    pub fn id(&self) -> u32 {
        self.layer.id
    }
}

/// Whether two time bindings show the same rows: the same field through the same window (what
/// they keep beside, worked out from those and the rows, aside).
fn same_time(a: Option<&crate::time::TimeBinding>, b: Option<&crate::time::TimeBinding>) -> bool {
    a.map(|t| (&t.field, t.window)) == b.map(|t| (&t.field, t.window))
}

/// Whether two datasets hold what a frame draws alike, their rows aside (compared by allocation).
fn same_dataset(a: &Dataset, b: &Dataset) -> bool {
    let Dataset { name, geometry_type, fields, features: _, source, crs, crs_incomplete, zm_dropped, windowed, held_in, shown_in } = a;
    *name == b.name
        && *geometry_type == b.geometry_type
        && *fields == b.fields
        && *source == b.source
        && *crs == b.crs
        && *crs_incomplete == b.crs_incomplete
        && *zm_dropped == b.zm_dropped
        && *windowed == b.windowed
        && *held_in == b.held_in
        // Labels' lines, terrain water and block walls read its places as its map shows them.
        && *shown_in == b.shown_in
}

/// Whether two layer groups apply alike to their layers: whether the Layers pane shows a group
/// open doesn't change the map.
fn same_group(a: &crate::LayerGroup, b: &crate::LayerGroup) -> bool {
    let crate::LayerGroup { id, name, visible, expanded: _, transparency, min_scale, max_scale, children, parent } = a;
    *id == b.id
        && *name == b.name
        && *visible == b.visible
        && transparency.to_bits() == b.transparency.to_bits()
        && *min_scale == b.min_scale
        && *max_scale == b.max_scale
        && *children == b.children
        && *parent == b.parent
}

/// What a frame draws from a map apart from its layers: its basemap, tile layers, groups,
/// projection, view settings and the rest (see the module).
#[derive(Clone, Debug)]
pub struct MapLook {
    /// The map without its layers.
    map: Map,
}

impl MapLook {
    /// The look of `map`, its layers left out (each has its own [`LayerLook`]).
    pub fn of(map: &Map) -> MapLook {
        let Map {
            name,
            offline_area_id: _,
            layers: _,
            service_groups: _,
            groups,
            basemap,
            paper,
            tiles,
            camera,
            // Not compared (see `same`), so not kept.
            bookmarks: _,
            graticule,
            graticule_type,
            graticule_crs,
            projection,
            popup_mode,
            label_display,
            label_lock,
            label_lock_line_anchors,
            editing: _,
            swipe,
            view,
            thumbnail: _,
            animation: _,
            time,
            body,
            next_layer_id,
            instance_id,
        } = map;
        MapLook {
            map: Map {
                name: name.clone(),
                offline_area_id: None,
                layers: Vec::new(),
                service_groups: Vec::new(),
                groups: groups.clone(),
                basemap: *basemap,
                paper: *paper,
                tiles: tiles.clone(),
                camera: *camera,
                bookmarks: Vec::new(),
                graticule: *graticule,
                graticule_type: *graticule_type,
                graticule_crs: graticule_crs.clone(),
                projection: projection.clone(),
                popup_mode: *popup_mode,
                label_display: *label_display,
                label_lock: label_lock.clone(),
                label_lock_line_anchors: label_lock_line_anchors.clone(),
                editing: Default::default(),
                swipe: *swipe,
                view: view.clone(),
                thumbnail: None,
                animation: Vec::new(),
                time: time.clone(),
                body: body.clone(),
                next_layer_id: *next_layer_id,
                instance_id: *instance_id,
            },
        }
    }

    /// Whether the map `now` was taken of, its layers aside, draws as the one this look was
    /// taken of did.
    pub fn same(&self, now: &MapLook) -> bool {
        let map = &now.map;
        let Map {
            // Graphics text can show the map's name.
            name,
            offline_area_id: _,
            layers: _,
            // A service's layers are drawn as layers; the groups only list them.
            service_groups: _,
            groups,
            basemap,
            paper,
            tiles,
            camera,
            bookmarks: _,
            graticule,
            graticule_type,
            graticule_crs,
            projection,
            popup_mode: _,
            label_display,
            label_lock,
            label_lock_line_anchors,
            // How sketches snap; nothing on the map's picture.
            editing: _,
            swipe,
            view,
            thumbnail: _,
            animation: _,
            time,
            body,
            next_layer_id: _,
            instance_id,
        } = &self.map;
        *name == map.name
            && groups.len() == map.groups.len()
            && groups.iter().zip(&map.groups).all(|(a, b)| same_group(a, b))
            && *basemap == map.basemap
            && *paper == map.paper
            && *tiles == map.tiles
            && *camera == map.camera
            && *graticule == map.graticule
            && *graticule_type == map.graticule_type
            && *graticule_crs == map.graticule_crs
            && *projection == map.projection
            && *label_display == map.label_display
            && *label_lock == map.label_lock
            && *label_lock_line_anchors == map.label_lock_line_anchors
            && *swipe == map.swipe
            && *view == map.view
            && *time == map.time
            && *body == map.body
            && *instance_id == map.instance_id
    }
}

#[cfg(test)]
mod tests {
    use rustgis_data::{Coord, Field, FieldType, Geometry, GeometryType, Value};

    use super::*;
    use crate::symbology::Renderer;
    use crate::{Bookmark, Rgba};

    fn layer() -> Layer {
        let mut d = Dataset::new("towns", GeometryType::Point, vec![Field::new("pop", FieldType::Integer)]);
        d.push(Some(Geometry::Point(Coord::new(1.0, 2.0))), vec![Value::Int(5)]);
        d.push(Some(Geometry::Point(Coord::new(3.0, 4.0))), vec![Value::Int(6)]);
        Layer::new(1, d)
    }

    fn change(edit: impl FnOnce(&mut Layer)) -> Change {
        let mut l = layer();
        let Some(before) = LayerLook::of(&l) else { return Change::Drawn };
        edit(&mut l);
        LayerLook::of(&l).map_or(Change::Drawn, |now| before.change(&now))
    }

    #[test]
    fn each_drawn_edit_changes_the_look() {
        assert_eq!(change(|_| {}), Change::Same);
        assert_eq!(change(|l| l.visible = false), Change::Drawn);
        assert_eq!(change(|l| l.definition_query = Some("pop > 5".into())), Change::Drawn);
        assert_eq!(change(|l| l.selection = vec![1]), Change::Drawn);
        assert_eq!(change(|l| l.labels.enabled = !l.labels.enabled), Change::Drawn);
        assert_eq!(change(|l| l.min_scale = Some(1000.0)), Change::Drawn);
        assert_eq!(change(|l| l.renderer = Renderer::Single(crate::Symbol { fill: Rgba::rgb(1, 2, 3), ..l.symbol(0) })), Change::Drawn);
        // A row's value or geometry, however small the edit.
        assert_eq!(
            change(|l| if let Some(f) = l.dataset.features.get_mut(0) {
                f.attrs = vec![Value::Int(7)]
            }),
            Change::Drawn
        );
        assert_eq!(change(|l| l.dataset.push(None, vec![Value::Int(8)])), Change::Drawn);
        assert_eq!(change(|l| l.touch_geometry()), Change::Drawn);
    }

    #[test]
    fn transparency_alone_is_told_apart() {
        assert_eq!(change(|l| l.transparency = 0.5), Change::Transparency);
        // With anything else, the layer is drawn again.
        assert_eq!(
            change(|l| {
                l.transparency = 0.5;
                l.visible = false;
            }),
            Change::Drawn
        );
    }

    #[test]
    fn what_the_map_does_not_draw_leaves_the_look() {
        assert_eq!(change(|l| l.expanded = !l.expanded), Change::Same);
        assert_eq!(change(|l| l.connection = "saved".into()), Change::Same);
        // A copy shares its rows, as a frame's snapshot of the map does.
        let l = layer();
        let copy = l.clone();
        assert!(LayerLook::of(&l).zip(LayerLook::of(&copy)).is_some_and(|(a, b)| a.change(&b) == Change::Same));
    }

    #[test]
    fn a_raster_in_memory_has_a_look() {
        use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster};
        let gt = GeoTransform { origin_x: 10.0, origin_y: 50.0, pixel_width: 0.5, pixel_height: -0.5 };
        let Ok(r) = Raster::new("dem", 2, 1, vec![vec![1.0, 2.0]], gt, Crs::LonLat, None, ColorHint::Gray, "Float32") else {
            panic!("a raster")
        };
        let mut l = layer();
        l.raster = Some(crate::RasterLayer::from_path(r.clone(), "dem.tif"));
        let look = |l: &Layer| LayerLook::of(l).expect("a look");
        let before = look(&l);
        assert_eq!(before.change(&look(&l.clone())), Change::Same);
        if let Some(rl) = &mut l.raster {
            rl.style.gamma = 2.0;
        }
        assert_eq!(before.change(&look(&l)), Change::Drawn, "its style");
        let before = look(&l);
        if let Some(rl) = &mut l.raster {
            rl.data = Some(std::sync::Arc::new(r));
        }
        assert_eq!(before.change(&look(&l)), Change::Drawn, "its pixels");
        if let Some(rl) = &mut l.raster {
            rl.in_place = true;
        }
        assert!(LayerLook::of(&l).is_none(), "read in place");
    }

    #[test]
    fn a_look_holds_no_rows() {
        let mut l = layer();
        let look = LayerLook::of(&l);
        // The first edit after taking it changes the rows where they are, copying none.
        let ((), copied) = rustgis_data::before_rows_copy(Box::new(|| {}), || l.dataset.push(None, vec![Value::Int(8)]));
        assert!(!copied, "the look kept the rows");
        drop(look);
        // As a copy of the rows would.
        let held = l.dataset.features.clone();
        let ((), copied) = rustgis_data::before_rows_copy(Box::new(|| {}), || l.dataset.push(None, vec![Value::Int(9)]));
        assert!(copied);
        drop(held);
    }

    #[test]
    fn a_look_keeps_no_archived_edits() {
        let mut l = layer();
        let edit = l.dataset.features.first().cloned();
        l.design.archive = Some(vec![crate::design::ArchiveEntry { time: 0.0, id: 1, before: edit.clone(), after: edit }]);
        let look = LayerLook::of(&l).expect("a look");
        assert_eq!(look.layer.design.archive, Some(Vec::new()));
    }

    #[test]
    fn a_layer_drawn_from_more_than_it_holds_has_no_look() {
        let mut l = layer();
        l.source = Some(Default::default());
        assert!(LayerLook::of(&l).is_none());
    }

    #[test]
    fn the_map_look_follows_what_is_drawn() {
        let mut map = Map::new("m");
        let before = MapLook::of(&map);
        map.bookmarks.push(Bookmark { name: "home".into(), camera: map.camera, description: String::new() });
        map.add_layer(layer());
        assert!(before.same(&MapLook::of(&map)), "bookmarks and layers aren't part of the map's own look");
        map.groups.push(crate::LayerGroup {
            id: 1,
            name: "g".into(),
            visible: true,
            expanded: true,
            transparency: 0.0,
            min_scale: None,
            max_scale: None,
            children: Vec::new(),
            parent: None,
        });
        let grouped = MapLook::of(&map);
        if let Some(g) = map.groups.first_mut() {
            g.expanded = false;
        }
        assert!(grouped.same(&MapLook::of(&map)), "a group shown closed in the Layers pane draws the same");
        if let Some(g) = map.groups.first_mut() {
            g.visible = false;
        }
        assert!(!grouped.same(&MapLook::of(&map)));
        map.graticule = !map.graticule;
        assert!(!before.same(&MapLook::of(&map)));
        // The blank basemap's paper colour (#2532) is the map's background.
        let white = MapLook::of(&map);
        map.paper = Some(Rgba([241, 234, 223, 255]));
        assert!(!white.same(&MapLook::of(&map)), "a new paper colour draws the map again");
    }
}
