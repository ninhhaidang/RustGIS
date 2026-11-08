//! Project exchange with a peer is an import, never an explicit saved-file reopen. A framing
//! page receives no credentials and cannot borrow the process's identical-text restore memory.

use crate::{
    Project, Session,
    durable::{self, Report},
};
use durable::{At, Durable, FieldMut, Kept, Leaf, Owner};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Position<'a> {
    Layer { id: u32, role: rustgis_map::LayerRole },
    Tile { id: Cow<'a, str>, role: rustgis_map::TileRole },
    Group { id: u32, child: Option<u32> },
    Terrain,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum SlotKey<'a> {
    Map { index: usize, name: Cow<'a, str>, at: Position<'a> },
    Area { index: usize, at: Position<'a> },
    Table(u32),
    Video { presentation: usize, page: usize },
}

impl SlotKey<'_> {
    /// Only retained, budgeted identities own copies; raw censuses borrow hostile names.
    fn owned(&self) -> SlotKey<'static> {
        let position = |at: &Position<'_>| match at {
            Position::Layer { id, role } => Position::Layer { id: *id, role: *role },
            Position::Tile { id, role } => Position::Tile { id: Cow::Owned(id.to_string()), role: *role },
            Position::Group { id, child } => Position::Group { id: *id, child: *child },
            Position::Terrain => Position::Terrain,
        };
        match self {
            Self::Map { index, name, at } => SlotKey::Map { index: *index, name: Cow::Owned(name.to_string()), at: position(at) },
            Self::Area { index, at } => SlotKey::Area { index: *index, at: position(at) },
            Self::Table(id) => SlotKey::Table(*id),
            Self::Video { presentation, page } => SlotKey::Video { presentation: *presentation, page: *page },
        }
    }
}

struct Slot<'a> {
    key: SlotKey<'a>,
    at: At,
    value: &'a str,
    layer: Option<&'a rustgis_map::Layer>,
}
struct Entry {
    key: SlotKey<'static>,
    choice: Option<String>,
    public: String,
    full: String,
    ambiguous: bool,
}
pub(crate) struct Exchange {
    generation: u64,
    entries: Vec<Entry>,
}
const MAX_ENTRIES: usize = 4096;
const MAX_BYTES: usize = 1 << 20;

/// Enumerates slot identities before any normalization. Duplicate identities are retained in the
/// census: they must never be made unique by dropping or renumbering a hostile duplicate.
fn slots(p: &Project) -> Vec<Slot<'_>> {
    let mut out = Vec::new();
    for (i, m) in p.maps.iter().enumerate() {
        for slot in m.slots().into_iter().filter(|s| s.address) {
            if let Some(at) = position(m, &slot.at) {
                out.push(Slot {
                    key: SlotKey::Map { index: i, name: Cow::Borrowed(&m.name), at },
                    at: At { owners: vec![Owner::Map(i)], leaf: Leaf::Map(slot.at.clone()) },
                    value: slot.value,
                    layer: match slot.at {
                        rustgis_map::SlotPos::Layer { layer, .. } => m.layers.get(layer),
                        _ => None,
                    },
                });
            }
        }
    }
    for (i, area) in p.offline_areas.iter().enumerate() {
        for slot in area.data.slots().into_iter().filter(|s| s.address) {
            if let Some(at) = position(&area.data, &slot.at) {
                out.push(Slot {
                    key: SlotKey::Area { index: i, at },
                    at: At { owners: vec![Owner::Area(i)], leaf: Leaf::Map(slot.at.clone()) },
                    value: slot.value,
                    layer: match slot.at {
                        rustgis_map::SlotPos::Layer { layer, .. } => area.data.layers.get(layer),
                        _ => None,
                    },
                });
            }
        }
    }
    for (i, table) in p.tables.iter().enumerate() {
        if let Some(value) = &table.source {
            out.push(Slot {
                key: SlotKey::Table(table.id),
                at: At { owners: vec![Owner::Table(i)], leaf: Leaf::Table },
                value,
                layer: None,
            });
        }
    }
    for (i, pres) in p.presentations.iter().enumerate() {
        for (page, p) in pres.pages.iter().enumerate() {
            if let rustgis_map::presentation::PageKind::Video { source, .. } = &p.kind {
                out.push(Slot {
                    key: SlotKey::Video { presentation: i, page },
                    at: At { owners: vec![Owner::Presentation(i)], leaf: Leaf::Video(page) },
                    value: source,
                    layer: None,
                });
            }
        }
    }
    out
}

/// Resolves raw owner identity without copying untrusted map or tile names.
fn position<'a>(map: &'a rustgis_map::Map, at: &rustgis_map::SlotPos) -> Option<Position<'a>> {
    use rustgis_map::SlotPos;
    Some(match at {
        SlotPos::Layer { layer, role } => Position::Layer { id: map.layers.get(*layer)?.id, role: *role },
        SlotPos::Tile { tile, role } => Position::Tile { id: Cow::Borrowed(&map.tiles.get(*tile)?.id), role: *role },
        SlotPos::Group { group, child } => {
            let group = map.service_groups.get(*group)?;
            Position::Group {
                id: group.id,
                child: match child {
                    Some(i) => Some(group.children.get(*i)?.layer),
                    None => None,
                },
            }
        }
        SlotPos::Terrain => Position::Terrain,
    })
}

/// Read selection, not drawing style or cached rows. The last exchange, live source and raw
/// incoming source must agree before normalization or a full address can be retained.
fn source_choice(layer: Option<&rustgis_map::Layer>) -> Result<Option<String>, ()> {
    let Some(layer) = layer.filter(|l| l.source.is_some() || l.raster.is_some() || l.postgis.is_some()) else { return Ok(None) };
    if layer.output_slots().iter().any(|(_, value, _, _)| value.len() > MAX_BYTES)
        || layer.source.as_ref().and_then(|s| s.embedded.as_ref()).is_some_and(|b| b.len() > MAX_BYTES)
        || layer.raster.as_ref().and_then(|r| r.embedded.as_ref()).is_some_and(|b| b.len() > MAX_BYTES)
    {
        return Err(());
    }
    let public = |s: &str| match durable::public_address(s) {
        Kept::Unchanged | Kept::Withheld(_) => s.to_string(),
        Kept::Changed(s) => s,
    };
    let source = layer.source.as_ref().map(|s| {
        (
            s.path.as_deref().map(public),
            &s.file_name,
            &s.table,
            &s.shapefile_crs,
            s.embedded.as_deref(),
            s.gdal.as_ref().map(|g| (public(&g.path), &g.layer, g.family, &g.crs)),
            s.duckdb.as_ref().map(|d| (&d.sql, d.database.as_deref().map(public), &d.table, &d.crs, d.family)),
            s.postgis.as_ref().map(|d| (public(&d.database), &d.table, d.family, &d.crs)),
            s.mysql.as_ref().map(|d| (public(&d.database), &d.table, d.family, &d.crs)),
            s.sqlserver.as_ref().map(|d| (public(&d.database), &d.table, d.family, &d.crs)),
        )
    });
    let raster = layer.raster.as_ref().map(|r| {
        (
            r.path.as_deref().map(public),
            r.in_place,
            &r.file_name,
            r.embedded.as_deref(),
            &r.subdataset,
            &r.crs,
            r.placement,
            r.time_zarr.as_ref().map(|z| (public(&z.store), &z.variable, &z.index, &z.dim, z.offset, &z.dates, &z.crs)),
        )
    });
    let edit = layer.postgis.as_ref().map(|p| {
        (public(&p.origin.database), &p.origin.table, p.origin.family, &p.origin.crs, &p.key, p.srid, p.writable, &p.condition, p.limit)
    });
    struct Bounded(Vec<u8>);
    impl std::io::Write for Bounded {
        /// Stops at the retention budget before allocating another chunk.
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.0.len().saturating_add(bytes.len()) > MAX_BYTES {
                return Err(std::io::Error::other("peer source identity exceeds retention limit"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        /// The bounded buffer has no pending I/O.
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut out = Bounded(Vec::new());
    serde_json::to_writer(&mut out, &(source, raster, edit)).map_err(|_| ())?;
    String::from_utf8(out.0).map(Some).map_err(|_| ())
}

/// A wrapper's binding belongs to S3 connection resolution, never exchange retention.
fn wrapped(value: &str) -> bool {
    let value = crate::sources::template_archive(value).map_or(value, |(_, v)| v);
    value.trim_start().get(..11).is_some_and(|v| v.eq_ignore_ascii_case("rustgis-s3:"))
}

/// Charges both forms and the complete identity payload, bounded before copying either form.
fn key_bytes(key: &SlotKey<'_>) -> usize {
    let extra = match key {
        SlotKey::Map { name, at, .. } => name.len().saturating_add(match at {
            Position::Tile { id, .. } => id.len(),
            _ => 0,
        }),
        SlotKey::Area { at: Position::Tile { id, .. }, .. } => id.len(),
        _ => 0,
    };
    std::mem::size_of::<SlotKey<'_>>().saturating_add(extra)
}

impl Exchange {
    /// Recorded only after output rollback. Every entry is session-local and replaced per push.
    fn record(p: &Project, generation: u64) -> (Self, u32) {
        let mut entries: Vec<Entry> = Vec::new();
        let mut forms: HashMap<String, (usize, bool)> = HashMap::new();
        let mut recording = true;
        let mut held = 0usize;
        let mut omitted = 0u32;
        for slot in slots(p) {
            if wrapped(slot.value) || slot.layer.and_then(|l| l.source.as_ref()).is_some_and(|s| s.reconnect_required()) {
                continue;
            }
            // A clean database address may still use a typed password held by its driver.
            let database = slot
                .layer
                .and_then(|l| l.source.as_ref())
                .is_some_and(|s| s.postgis.is_some() || s.mysql.is_some() || s.sqlserver.is_some() || s.duckdb.is_some());
            let public = match durable::public_address(slot.value) {
                Kept::Unchanged if database => slot.value.to_string(),
                Kept::Unchanged => continue,
                Kept::Changed(s) => s,
                Kept::Withheld(prefix) => prefix.into(),
            };
            // Even an account omitted by the cap cannot make an earlier recorded public form
            // unambiguous. After the first overflow, no later slot is recorded.
            if let Some((first, ambiguous)) = forms.get_mut(&public) {
                *ambiguous |= entries.get(*first).is_none_or(|e| e.full != slot.value);
            }
            let Ok(choice) = source_choice(slot.layer) else {
                omitted = omitted.saturating_add(1);
                continue;
            };
            let bytes = slot
                .value
                .len()
                .saturating_add(public.len())
                .saturating_add(key_bytes(&slot.key))
                .saturating_add(choice.as_ref().map_or(0, String::len));
            if !recording || entries.len() >= MAX_ENTRIES || held.saturating_add(bytes) > MAX_BYTES {
                recording = false;
                omitted = omitted.saturating_add(1);
                continue;
            }
            held += bytes;
            forms.entry(public.clone()).or_insert((entries.len(), false));
            entries.push(Entry { key: slot.key.owned(), choice, public, full: slot.value.into(), ambiguous: false });
        }
        for entry in &mut entries {
            entry.ambiguous = forms.get(&entry.public).is_none_or(|(_, ambiguous)| *ambiguous);
        }
        (Self { generation, entries }, omitted)
    }

    /// The pre-normalization census must contain exactly one byte-equal public slot.
    fn incoming(&self, p: &Project) -> HashSet<SlotKey<'static>> {
        let census = census(p);
        self.entries
            .iter()
            .filter(|e| {
                census.get(&e.key).is_some_and(|(n, value, layer)| {
                    *n == 1 && *value == e.public && source_choice(*layer).ok().as_ref() == Some(&e.choice)
                })
            })
            .map(|e| e.key.clone())
            .collect()
    }

    /// Checks last exchange, both incoming censuses, unique account and the exact current live
    /// value in one synchronous replacement. Public URL or incoming id alone grants nothing.
    fn retain(&self, live: &Project, incoming: &mut Project, raw: &HashSet<SlotKey<'static>>) {
        let current = census(live);
        let normalized = census(incoming);
        let mut records: HashMap<SlotKey<'_>, (usize, &Entry)> = HashMap::new();
        let mut accounts: HashMap<&str, Option<&str>> = HashMap::new();
        for e in &self.entries {
            records.entry(e.key.clone()).or_insert((0, e)).0 += 1;
            let account = accounts.entry(&e.public).or_insert(Some(&e.full));
            if *account != Some(e.full.as_str()) {
                *account = None;
            }
        }
        let edits: Vec<_> = slots(incoming)
            .into_iter()
            .filter_map(|slot| {
                let (count, e) = records.get(&slot.key)?;
                let unique = |census: &Census<'_>, expected: &str| {
                    census.get(&slot.key).is_some_and(|(n, value, layer)| {
                        *n == 1 && *value == expected && source_choice(*layer).ok().as_ref() == Some(&e.choice)
                    })
                };
                (!e.ambiguous
                    && raw.contains(&e.key)
                    && !current
                        .get(&slot.key)
                        .is_some_and(|(_, _, layer)| layer.and_then(|l| l.source.as_ref()).is_some_and(|s| s.reconnect_required()))
                    && *count == 1
                    && accounts.get(e.public.as_str()) == Some(&Some(e.full.as_str()))
                    && unique(&current, &e.full)
                    && unique(&normalized, &e.public))
                .then(|| (slot.at, e.full.clone()))
            })
            .collect();
        drop(normalized);
        for (at, full) in edits {
            if let Some(FieldMut::Text(slot)) = incoming.field_mut(&at.owners, &at.leaf) {
                *slot = full;
            }
            reopen(incoming, &at);
        }
    }
}

/// A retained address must reopen its own reader, which normalization previously opened with
/// the public form. Only the exact retained owner/role is touched; S3 wrappers are excluded.
fn reopen(p: &mut Project, at: &At) {
    let Some(owner) = at.owners.first() else { return };
    let map = match owner {
        Owner::Map(i) => p.maps.get_mut(*i),
        Owner::Area(i) => p.offline_areas.get_mut(*i).map(|a| &mut a.data),
        _ => None,
    };
    let Some(map) = map else { return };
    let Leaf::Map(rustgis_map::SlotPos::Layer { layer, role }) = &at.leaf else { return };
    let Some(layer) = map.layers.get_mut(*layer) else { return };
    match role {
        rustgis_map::LayerRole::Streamed
        | rustgis_map::LayerRole::PostgisDatabase
        | rustgis_map::LayerRole::MysqlDatabase
        | rustgis_map::LayerRole::SqlServerDatabase
        | rustgis_map::LayerRole::DuckDbDatabase
        | rustgis_map::LayerRole::GdalPath => {
            if let Some(source) = &mut layer.source {
                source.allow_reconnect();
                if let Err(e) = source.reopen() {
                    source.error = Some(rustgis_data::diagnostics::Redacted::new(&e).into_string());
                }
            }
        }
        rustgis_map::LayerRole::Raster => {
            if let Some(raster) = &mut layer.raster
                && let Err(e) = raster.reload()
            {
                raster.error = Some(rustgis_data::diagnostics::Redacted::new(&e).into_string());
            }
        }
        _ => {}
    }
}

/// Values borrow the project, but identities include all duplicates; no normalization runs here.
type Census<'a> = HashMap<SlotKey<'a>, (usize, &'a str, Option<&'a rustgis_map::Layer>)>;

/// Counts raw duplicates while borrowing all untrusted identity text.
fn census(p: &Project) -> Census<'_> {
    let mut out = HashMap::new();
    for slot in slots(p) {
        let entry = out.entry(slot.key).or_insert((0usize, slot.value, slot.layer));
        entry.0 = entry.0.saturating_add(1);
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Peer {
    Bridge,
    Embedder,
}

impl Session {
    /// Writes a credential-free project for a peer, without evicting explicit-save restoration.
    /// Pending downloads require a structural draft; settled projects use the live journal.
    pub fn peer_text(&mut self, peer: Peer) -> Result<(String, Report), String> {
        let runtime = self.project.bound_address_losses(true);
        let (text, mut report) = if self.project.ensure_imports_complete().is_err() {
            let mut draft = self.project.clone();
            draft.omit_pending_imports();
            durable::output_owned(draft, text)
        } else {
            durable::output(&mut self.project, None, text)
        }?;
        report.addresses = report.addresses.saturating_add(u32::try_from(runtime).unwrap_or(u32::MAX));
        self.exchange = if peer == Peer::Bridge {
            let (record, omitted) = Exchange::record(&self.project, self.opened);
            report.not_retained = omitted;
            Some(record)
        } else {
            None
        };
        Ok((text, report))
    }

    /// Replaces a project synchronously with peer/import semantics. No command runs with User
    /// authority. Reset advances generation, clears item trust, history, pending work and active
    /// connections; old approvals and continuations must bind to that generation.
    pub fn open_peer_text(&mut self, peer: Peer, text: &str) -> Result<Report, String> {
        self.with_scope(crate::policy::Scope::peer(), |s| s.open_peer_text_scoped(peer, text))?
    }
    /// The existing raw census/reset path runs synchronously under peer provenance.
    fn open_peer_text_scoped(&mut self, peer: Peer, text: &str) -> Result<Report, String> {
        let mut p = Project::from_peer_raw(text)?;
        let exchange = self.exchange.as_ref().filter(|e| peer == Peer::Bridge && e.generation == self.opened);
        let raw = exchange.map(|e| e.incoming(&p));
        // Normalization may open a database with a typed process-local password. Keep every
        // incoming database source closed until last/live/raw/normalized selection agrees.
        for map in p.maps.iter_mut().chain(p.offline_areas.iter_mut().map(|a| &mut a.data)) {
            for source in map.layers.iter_mut().filter_map(|l| l.source.as_mut()) {
                if source.postgis.is_some() || source.mysql.is_some() || source.sqlserver.is_some() || source.duckdb.is_some() {
                    source.require_reconnect();
                }
            }
        }
        crate::cmd::connections::clear_active();
        let layout_notices = p.normalize_opened();
        p.read_notices.extend(layout_notices.into_iter().map(|n| (crate::project::ReadPart::Layouts, n)));
        let mut report = Report::default();

        if let (Some(exchange), Some(raw)) = (exchange, raw) {
            exchange.retain(&self.project, &mut p, &raw);
        }
        report.reconnect = p
            .maps
            .iter()
            .chain(p.offline_areas.iter().map(|a| &a.data))
            .flat_map(|m| &m.layers)
            .filter(|l| l.source.as_ref().is_some_and(|s| s.reconnect_required()))
            .count()
            .try_into()
            .unwrap_or(u32::MAX);
        let incoming = census(&p);
        report.lost = slots(&self.project)
            .into_iter()
            .filter(|slot| {
                !matches!(durable::public_address(slot.value), Kept::Unchanged)
                    && !incoming.get(&slot.key).is_some_and(|(count, value, _)| *count == 1 && *value == slot.value)
            })
            .count()
            .try_into()
            .unwrap_or(u32::MAX);
        drop(incoming);
        report.executable_changes = durable::changed_executables(&mut self.project, &mut p)?;
        crate::cmd::project::reset_to(self, p);
        self.path = None;
        crate::cmd::connections::resolve_project(self);
        crate::cmd::connections::note_waiting(self);
        self.revision = self.revision.wrapping_add(1);
        if peer == Peer::Bridge {
            let (exchange, omitted) = Exchange::record(&self.project, self.opened);
            self.exchange = Some(exchange);
            report.not_retained = omitted;
        }
        Ok(report)
    }
}

/// A peer may be the web app, which opens at most [`BYTES_FLOOR`](crate::project::BYTES_FLOOR)
/// whatever this computer saves: the text is never built past that. No destination is written on
/// a formatting failure.
fn text(p: &Project) -> Result<String, String> {
    let max = crate::project::BYTES_FLOOR;
    crate::project::json_within(p, max)?
        .ok_or_else(|| format!("the project is too large to exchange (maximum {} MiB, what the web app opens)", max >> 20))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Table/family/CRS are part of a retained reader, even before normalization opens it.
    #[test]
    fn bridge_census_binds_the_full_source_choice_before_normalization() {
        let mut p = Project::default();
        let id = p.map_mut().add_dataset(rustgis_data::Dataset::new("Database", rustgis_data::GeometryType::None, vec![]));
        let mut source = rustgis_map::source::SourceLayer::default();
        source.postgis = Some(rustgis_map::source::PostgisOrigin {
            database: "postgresql://reader:RGSYNTH_peer_choice@db.invalid/gis".into(),
            table: "approved".into(),
            family: None,
            crs: None,
        });
        p.map_mut().layer_mut(id).unwrap().source = Some(source);
        let mut s = Session::new(p);
        let (public, _) = s.peer_text(Peer::Bridge).unwrap();
        let exchange = s.exchange.as_ref().unwrap();
        for change in ["style", "table", "family", "crs"] {
            let mut p = Project::from_peer_raw(&public).unwrap();
            let layer = &mut p.maps[0].layers[0];
            let pg = layer.source.as_mut().unwrap().postgis.as_mut().unwrap();
            match change {
                "table" => pg.table = "different".into(),
                "family" => pg.family = Some(rustgis_data::source::Family::Points),
                "crs" => pg.crs = Some("EPSG:4326".into()),
                _ => layer.visible = false,
            }
            assert_eq!(exchange.incoming(&p).len(), usize::from(change == "style"), "{change}");
        }
        // Reading a raster in place changes its source selection, not just its drawing style.
        let mut p = Project::default();
        let mut raster = rustgis_map::raster::RasterLayer::unread_in_place("https://choice.invalid/scene.tif?token=RGSYNTH_raster_choice");
        raster.in_place = false;
        p.map_mut().add_raster("Scene", raster);
        let mut s = Session::new(p);
        let (public, _) = s.peer_text(Peer::Bridge).unwrap();
        let exchange = s.exchange.as_ref().unwrap();
        let mut incoming = Project::from_peer_raw(&public).unwrap();
        assert_eq!(exchange.incoming(&incoming).len(), 1);
        incoming.maps[0].layers[0].raster.as_mut().unwrap().in_place = true;
        assert!(exchange.incoming(&incoming).is_empty(), "a changed raster read mode cannot retain credentials");
    }
}
