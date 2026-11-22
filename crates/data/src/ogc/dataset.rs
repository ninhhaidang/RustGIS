//! Dataset tile discovery: advertised Web Mercator templates establish which style sources
//! are safe to draw. Fetching and the total document budget belong to the caller.

use super::{
    Offer, Service, ServiceLayer, TilesStep, discovery_link, representation_link, resolve, resolve_tiles, tile_data_type,
    web_mercator_tileset,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

const MAX_STYLES: usize = 16;
const MAX_PARTS: usize = 200;
const MAX_ADDRESS: usize = 8192;

/// A bounded dataset discovery plan. No stylesheet is offered until every source has been
/// matched against an advertised Web Mercator tile template, including TileJSON sources.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DatasetDiscovery {
    pending: VecDeque<Document>,
    templates: Vec<(String, bool)>,
    unnamed: Vec<String>,
    styled: BTreeSet<String>,
    sources: BTreeMap<String, Vec<String>>,
    omissions: BTreeMap<&'static str, usize>,
}

#[derive(Clone, Debug, PartialEq)]
struct Document {
    parent: String,
    url: String,
    kind: Kind,
}

#[derive(Clone, Debug, PartialEq)]
enum Kind {
    Tiles(Tileset),
    Styles,
    Style { id: String, title: String },
    Source(Box<Candidate>),
}

/// Metadata already advertised by the selected list entry survives its metadata link.
#[derive(Clone, Debug, PartialEq)]
struct Tileset {
    vector: bool,
    metadata: bool,
    names: Vec<String>,
    title: String,
    schemes: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
struct Candidate {
    layer: ServiceLayer,
    remaining: VecDeque<(String, bool)>,
    templates: BTreeSet<String>,
}

/// A short or registered OGC link relation, without accepting unrelated suffixes.
pub(super) fn relation(link: &Value, name: &str) -> bool {
    link.get("rel").and_then(Value::as_str).is_some_and(|r| r == name || r == format!("http://www.opengis.net/def/rel/ogc/1.0/{name}"))
}

/// Linked metadata may repeat the validated list entry's definition link, whose server
/// path need not name the grid. A new scheme link must establish its own supported grid.
fn inherits_grid(from: &str, value: &Value, context: &Tileset) -> bool {
    context.metadata
        && !["tileMatrixSetURI", "tileMatrixSetId", "tileMatrixSet"].iter().any(|k| value.get(*k).is_some())
        && value.get("links").and_then(Value::as_array).into_iter().flatten().filter(|link| relation(link, "tiling-scheme")).all(|link| {
            link.get("href")
                .and_then(Value::as_str)
                .filter(|href| href.len() <= MAX_ADDRESS)
                .and_then(|href| resolve(from, href))
                .is_some_and(|url| context.schemes.contains(&url))
        })
}

/// Keeps an advertised destination authoritative and counts settings omitted on this hop.
fn advertised(from: &str, href: &str, withheld: &mut u32) -> Option<String> {
    if href.len() > MAX_ADDRESS {
        return None;
    }
    let to = resolve(from, href)?;
    if to.len() > MAX_ADDRESS {
        return None;
    }
    let inherited = crate::address::inherit_query(
        crate::login_address::inner(from),
        crate::login_address::inner(&to),
        crate::address::Relation::Advertised,
        &["f"],
    );
    *withheld = withheld.saturating_add(u32::from(inherited.withheld || inherited.url != crate::login_address::inner(&to)));
    Some(to)
}

/// Constructs an offer whose tile template has already been read, so adding it needs no
/// second tileset fetch and cannot fall back to a guessed template.
fn layer(name: String, title: String, offer: Offer) -> ServiceLayer {
    ServiceLayer { name, title, offer, bbox: None, tilesets: None, time: None, feature_info: None, legends: Vec::new(), zooms: None }
}

impl DatasetDiscovery {
    /// Accumulates one notice per omission kind across all discovery documents.
    fn omitted(&mut self, service: &mut Service, count: usize, reason: &'static str) {
        if count > 0 {
            *self.omissions.entry(reason).or_default() += count;
            service.incomplete = true;
        }
    }

    /// A snapshot of counted omissions, including partial listings and failed traversals.
    pub fn notes(&self) -> Vec<String> {
        self.omissions.iter().map(|(reason, count)| format!("{count} {reason}")).collect()
    }

    /// Adds counts for known work abandoned by a terminal discovery error or budget. A document
    /// queued twice (a shared list and metadata pair) or left unread by the failure itself is
    /// counted per queue entry. Waiting for supplied bytes is not terminal; callers use `notes`
    /// for that snapshot.
    pub fn stopped_notes(&self) -> Vec<String> {
        let mut notes = self.notes();
        let mut left = BTreeMap::<&str, usize>::new();
        for doc in &self.pending {
            let (kind, count) = match &doc.kind {
                Kind::Tiles(_) => ("tileset documents", 1),
                Kind::Styles => ("style catalogues", 1),
                Kind::Style { .. } => ("stylesheets", 1),
                Kind::Source(candidate) => ("TileJSON source checks", candidate.remaining.len()),
            };
            *left.entry(kind).or_default() += count;
        }
        notes.extend(left.into_iter().map(|(kind, count)| format!("{count} {kind} were left out: dataset discovery stopped")));
        notes
    }

    /// Counts dataset catalogues that the existing collections traversal does not visit.
    /// Map metadata keeps its legacy resolver; vector tiles and styles need the dataset plan.
    pub(super) fn collections_notes(mut self) -> Vec<String> {
        let skipped = self.pending.iter().filter(|doc| !matches!(&doc.kind, Kind::Tiles(t) if !t.vector)).count();
        if skipped > 0 {
            *self.omissions.entry("dataset tileset and style catalogues were left out: the landing also lists collections").or_default() +=
                skipped;
        }
        self.notes()
    }

    /// Starts only from advertised dataset tileset links. Styles alone do not establish a
    /// projection, and a collection id is never used as a dataset's vector layer name.
    pub(super) fn landing(from: &str, links: &[Value], withheld: &mut u32) -> Option<Self> {
        let mut out = Self::default();
        let mut advertised_tiles = false;
        for (rel, vector) in [("tilesets-vector", true), ("tilesets-map", false)] {
            if !links.iter().any(|link| relation(link, rel)) {
                continue;
            }
            advertised_tiles = true;
            if let Some((url, lost)) = discovery_link(from, links, |l| relation(l, rel)) {
                *withheld = withheld.saturating_add(u32::from(lost));
                if url.len() > MAX_ADDRESS {
                    *out.omissions.entry("tileset documents were left out: addresses exceed 8192 bytes").or_default() += 1;
                    continue;
                }
                out.pending.push_back(Document {
                    parent: from.into(),
                    url,
                    kind: Kind::Tiles(Tileset { vector, metadata: false, names: Vec::new(), title: String::new(), schemes: Vec::new() }),
                });
            } else {
                let reason = if links
                    .iter()
                    .any(|l| relation(l, rel) && l.get("href").and_then(Value::as_str).is_some_and(|h| h.len() > MAX_ADDRESS))
                {
                    "tileset documents were left out: addresses exceed 8192 bytes"
                } else {
                    "tileset documents were left out: no usable JSON link"
                };
                *out.omissions.entry(reason).or_default() += 1;
            }
        }
        if !advertised_tiles {
            return None;
        }
        if out.pending.is_empty() {
            return Some(out);
        }
        if let Some((url, lost)) = discovery_link(from, links, |l| relation(l, "styles")) {
            *withheld = withheld.saturating_add(u32::from(lost));
            if url.len() <= MAX_ADDRESS {
                out.pending.push_back(Document { parent: from.into(), url, kind: Kind::Styles });
            } else {
                out.omissions.insert("style catalogues were left out: addresses exceed 8192 bytes", 1);
            }
        } else if links.iter().any(|link| relation(link, "styles")) {
            out.omissions.insert("style catalogues were left out: no usable JSON link", 1);
        }
        Some(out)
    }

    /// The next document and its actual referrer, for address checks and login inheritance.
    pub fn next(&self) -> Option<(&str, &str)> {
        self.pending.front().map(|d| (d.parent.as_str(), d.url.as_str()))
    }

    /// The map metadata address after representation and length checks. The collections
    /// discovery path keeps its existing resolver, but must not guess from a rejected link.
    pub(super) fn map_url(&self) -> Option<&str> {
        self.pending.iter().find(|d| matches!(&d.kind, Kind::Tiles(t) if !t.vector && !t.metadata)).map(|d| d.url.as_str())
    }

    /// Keeps the existing `service.add` shortcut for a caller that has already resolved a map
    /// template. The engine still checks that explicit template against the service address.
    pub fn explicit_map(&self, title: &str, template: &str) -> Option<ServiceLayer> {
        let document = self.map_url()?;
        let title = if title.is_empty() { "tiles" } else { title };
        let mut offer = layer("map".into(), format!("{title} (map tiles)"), Offer::Tiles { template: template.into() });
        offer.tilesets = Some(document.to_string());
        Some(offer)
    }

    /// Consumes the next advertised document. Callers fetch it or replay supplied bytes through
    /// this same path; invalid candidate styles are omitted without losing valid tile offers.
    /// `allowed` applies the caller's address policy to a template and its supplying document.
    pub fn read(&mut self, body: &[u8], allowed: impl Fn(&str, &str) -> bool) -> Result<Service, String> {
        let doc = self.pending.pop_front().ok_or("no dataset discovery document is pending")?;
        let mut service = Service { kind: "OGC API Tiles".into(), incomplete: !self.omissions.is_empty(), ..Service::default() };
        let mut contexts = Vec::new();
        if let Kind::Tiles(context) = &doc.kind {
            contexts.push(context.clone());
            // Map and vector offers may share a list or metadata document. Retain both
            // contexts in one read without crossing the parent's address-policy boundary.
            let shared = self.pending.iter().position(|next| {
                next.url == doc.url
                    && next.parent == doc.parent
                    && matches!(&next.kind, Kind::Tiles(other) if other.metadata == context.metadata && other.vector != context.vector)
            });
            if let Some(at) = shared
                && let Some(Document { kind: Kind::Tiles(other), .. }) = self.pending.remove(at)
            {
                contexts.push(other);
            }
        }
        match serde_json::from_slice::<Value>(body) {
            Ok(value) => match doc.kind.clone() {
                Kind::Tiles(_) => self.tiles(&doc, &value, &contexts, &allowed, &mut service),
                Kind::Styles => self.styles(&doc, &value, &mut service),
                Kind::Style { id, title } => self.style(&doc, &value, id, title, &mut service),
                Kind::Source(candidate) => {
                    let templates = Self::source_templates(&doc.url, &value)
                        .filter(|templates| self.matches(templates, true) || self.matches(templates, false))
                        .unwrap_or_default();
                    for template in &templates {
                        advertised(&doc.url, template, &mut service.withheld_addresses);
                    }
                    self.sources.insert(doc.url.clone(), templates);
                    self.advance(*candidate, &mut service);
                }
            },
            Err(_) => {
                if matches!(doc.kind, Kind::Source(_)) {
                    self.sources.insert(doc.url.clone(), Vec::new());
                }
                self.omitted(&mut service, 1, "dataset documents were left out: invalid JSON");
            }
        }
        self.finish(&mut service);
        Ok(service)
    }

    /// Omits a document refused by the caller's address policy without losing independent
    /// offers. A refused TileJSON source stays unusable if another style references it.
    pub fn reject_address(&mut self) -> Service {
        self.reject("dataset documents were left out: disallowed addresses")
    }

    /// Omits an already visited document without ending independent discovery. Multiple
    /// offers may advertise the same address; the caller retains its ordinary loop checks.
    pub fn reject_repeated(&mut self) -> Service {
        self.reject("dataset documents were left out: repeated addresses")
    }

    /// Consumes one unusable candidate and prevents its source from being reused as valid.
    fn reject(&mut self, reason: &'static str) -> Service {
        if let Some(doc) = self.pending.pop_front()
            && matches!(doc.kind, Kind::Source(_))
        {
            self.sources.insert(doc.url, Vec::new());
        }
        let mut service = Service { kind: "OGC API Tiles".into(), ..Service::default() };
        self.omitted(&mut service, 1, reason);
        self.finish(&mut service);
        service
    }

    /// Reports unnamed vector tilesets once all candidate styles have been considered.
    fn finish(&mut self, service: &mut Service) {
        if self.pending.is_empty() {
            let missing = self
                .unnamed
                .iter()
                .filter(|t| !self.styled.iter().any(|known| crate::login_address::inner(known) == crate::login_address::inner(t)))
                .count();
            self.omitted(
                service,
                missing,
                "vector tilesets were left out: neither named vector-layer metadata nor a supported style supplies their layer names",
            );
        }
    }

    /// Reads the advertised template using the existing OGC resolver. A list may lead to one
    /// metadata document; unknown projections and entries beyond the bound are counted. A
    /// shared map/vector list checks each entry against both advertised contexts before omitting it.
    fn tiles(&mut self, doc: &Document, value: &Value, contexts: &[Tileset], allowed: &impl Fn(&str, &str) -> bool, service: &mut Service) {
        if let Some(sets) = value.get("tilesets").and_then(Value::as_array) {
            let mut skipped = sets.len().saturating_sub(MAX_PARTS);
            for set in sets.iter().take(MAX_PARTS) {
                let mut accepted = false;
                if web_mercator_tileset(set) {
                    for context in contexts.iter().filter(|c| tile_data_type(set, c.vector)) {
                        accepted = true;
                        self.tile(doc, set, context, true, allowed, service);
                    }
                }
                if !accepted {
                    skipped += 1;
                }
            }
            self.omitted(
                service,
                skipped,
                "tilesets were left out: unsupported tile matrix set or data type, or past the 200-tileset limit",
            );
        } else {
            let mut accepted = false;
            for context in contexts {
                if (inherits_grid(&doc.url, value, context) || web_mercator_tileset(value)) && tile_data_type(value, context.vector) {
                    accepted = true;
                    self.tile(doc, value, context, false, allowed, service);
                }
            }
            if !accepted {
                self.omitted(service, 1, "tilesets were left out: unsupported or undeclared tile matrix set, or unsupported data type");
            }
        }
    }

    /// Resolves one selected tileset; a list entry can advertise its template inline or link
    /// to metadata. A metadata document cannot start another unbounded tileset traversal.
    fn tile(
        &mut self,
        doc: &Document,
        value: &Value,
        context: &Tileset,
        listed: bool,
        allowed: &impl Fn(&str, &str) -> bool,
        service: &mut Service,
    ) {
        let vector = context.vector;
        let metadata = context.metadata;
        let mut names = context.names.clone();
        let named = value.get("vector_layers").or_else(|| value.get("layers")).and_then(Value::as_array);
        if let Some(named) = named {
            names = named
                .iter()
                .take(MAX_PARTS)
                .filter_map(|v| v.get("id").and_then(Value::as_str))
                .filter(|id| !id.is_empty() && id.len() <= 2000)
                .map(str::to_string)
                .collect();
            if vector {
                self.omitted(
                    service,
                    named.len().saturating_sub(names.len()),
                    "vector layer names were left out: missing or too long ids, or past the 200-layer limit",
                );
            }
        }
        let title =
            value.get("title").and_then(Value::as_str).filter(|t| !t.is_empty() && t.len() <= 2000).unwrap_or(&context.title).to_string();
        let wrapped;
        let document = if listed {
            wrapped = serde_json::json!({"tilesets":[value]});
            &wrapped
        } else {
            value
        };
        let body = match serde_json::to_vec(document) {
            Ok(body) => body,
            Err(_) => {
                self.omitted(service, 1, "dataset tileset documents were left out: metadata could not be encoded");
                return;
            }
        };
        let Some(resolved) = resolve_tiles(&doc.url, &doc.url, &body, vector) else {
            self.omitted(service, 1, "dataset tileset documents were left out: no usable Web Mercator tileset");
            return;
        };
        service.withheld_addresses = service.withheld_addresses.saturating_add(resolved.withheld_addresses);
        match resolved.step {
            TilesStep::Next(url) if !metadata && url.len() <= MAX_ADDRESS => {
                // Keep advertised order, with every tileset ahead of the style catalogue.
                let at = self.pending.iter().position(|d| !matches!(d.kind, Kind::Tiles(_))).unwrap_or(self.pending.len());
                let schemes = value
                    .get("links")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .take(MAX_PARTS)
                    .filter(|link| relation(link, "tiling-scheme"))
                    .filter_map(|link| link.get("href").and_then(Value::as_str))
                    .filter(|href| href.len() <= MAX_ADDRESS)
                    .filter_map(|href| resolve(&doc.url, href))
                    .collect();
                self.pending.insert(
                    at,
                    Document { parent: doc.url.clone(), url, kind: Kind::Tiles(Tileset { vector, metadata: true, names, title, schemes }) },
                );
            }
            TilesStep::Next(_) => self.omitted(
                service,
                1,
                "tilesets were left out: metadata links lead past two documents or their addresses exceed 8192 bytes",
            ),
            TilesStep::Template(template) => {
                if template.len() > 2 * MAX_ADDRESS {
                    self.omitted(service, 1, "tilesets were left out: template addresses exceed 16384 bytes");
                    return;
                }
                // Only templates accepted from their own document can validate later styles.
                if !allowed(&template, &doc.url) {
                    self.omitted(service, 1, "tilesets were left out: disallowed template addresses");
                    return;
                }
                self.templates.push((template.clone(), vector));
                let title = if title.is_empty() { if vector { "Vector tiles" } else { "Map tiles" }.into() } else { title };
                if vector && names.is_empty() {
                    self.unnamed.push(template);
                } else {
                    let offer = if vector { Offer::VectorTiles { template, layers: names } } else { Offer::Tiles { template } };
                    let count = self.templates.iter().filter(|(_, kind)| *kind == vector).count();
                    let kind = if vector { "vector" } else { "map" };
                    let name = if count == 1 { kind.into() } else { format!("{kind}-{count}") };
                    service.layers.push(layer(name, title, offer));
                }
            }
        }
    }

    /// Queues at most sixteen explicitly encoded MapLibre styles, never inferring projection
    /// from a catalogue id or title. Its stylesheet is checked after tile metadata is read.
    fn styles(&mut self, doc: &Document, value: &Value, service: &mut Service) {
        let Some(styles) = value.get("styles").and_then(Value::as_array) else {
            self.omitted(service, 1, "style catalogues were left out: no styles array");
            return;
        };
        let mut skipped = styles.len().saturating_sub(MAX_STYLES);
        for style in styles.iter().take(MAX_STYLES) {
            let id = style.get("id").and_then(Value::as_str).filter(|id| !id.is_empty() && id.len() <= 2000);
            let links = style.get("links").and_then(Value::as_array);
            let url = links.and_then(|links| {
                representation_link(
                    &doc.url,
                    links,
                    |l| relation(l, "stylesheet") && l.get("type").is_some(),
                    "application/vnd.mapbox.style+json",
                )
            });
            if let (Some(id), Some((url, withheld))) = (id, url.filter(|(url, _)| url.len() <= MAX_ADDRESS)) {
                service.withheld_addresses = service.withheld_addresses.saturating_add(u32::from(withheld));
                let title = style.get("title").and_then(Value::as_str).filter(|t| t.len() <= 2000).unwrap_or(id).into();
                self.pending.push_back(Document { parent: doc.url.clone(), url, kind: Kind::Style { id: id.into(), title } });
            } else {
                skipped += 1;
            }
        }
        self.omitted(service, skipped, "styles were left out: no usable MapLibre stylesheet, or past the 16-style discovery limit");
    }

    /// Copies only validated templates, to record which unnamed tilesets a style can draw.
    fn source_templates(from: &str, source: &Value) -> Option<Vec<String>> {
        if source.get("scheme").and_then(Value::as_str).is_some_and(|s| s != "xyz") {
            return None;
        }
        let tiles = source.get("tiles")?.as_array()?;
        if tiles.is_empty() || tiles.len() > MAX_PARTS {
            return None;
        }
        tiles
            .iter()
            .map(|t| {
                let t = t.as_str()?;
                (t.len() <= 2 * MAX_ADDRESS).then(|| resolve(from, t)).flatten().filter(|to| to.len() <= 2 * MAX_ADDRESS)
            })
            .collect()
    }

    /// Projection matching compares destination addresses, independently of runtime login
    /// bindings. Each request still keeps its own referrer and binding at the fetch boundary.
    fn matches(&self, templates: &[String], vector: bool) -> bool {
        !templates.is_empty()
            && templates.iter().all(|template| {
                self.templates
                    .iter()
                    .any(|(known, kind)| *kind == vector && crate::login_address::inner(known) == crate::login_address::inner(template))
            })
    }

    /// Rejects a style whose sources or source-layer references cannot be drawn. External
    /// TileJSON sources are read before the style becomes addable, under the same budget.
    fn style(&mut self, doc: &Document, value: &Value, id: String, title: String, service: &mut Service) {
        let valid = || -> Option<Candidate> {
            if value.get("version").and_then(Value::as_u64) != Some(8) {
                return None;
            }
            let sources = value.get("sources")?.as_object()?;
            let layers = value.get("layers")?.as_array()?;
            if sources.is_empty() || sources.len() > MAX_PARTS || layers.is_empty() || layers.len() > 2000 {
                return None;
            }
            let mut candidate = Candidate {
                layer: layer(id, title, Offer::Style { url: doc.url.clone() }),
                remaining: VecDeque::new(),
                templates: BTreeSet::new(),
            };
            for source in sources.values() {
                let vector = match source.get("type")?.as_str()? {
                    "vector" => true,
                    "raster" => false,
                    _ => return None,
                };
                if source.get("scheme").and_then(Value::as_str).is_some_and(|s| s != "xyz") {
                    return None;
                }
                if source.get("url").is_some() && source.get("tiles").is_some() {
                    return None;
                }
                if let Some(url) = source.get("url").and_then(Value::as_str) {
                    candidate.remaining.push_back((advertised(&doc.url, url, &mut service.withheld_addresses)?, vector));
                } else {
                    let templates = Self::source_templates(&doc.url, source)?;
                    for template in &templates {
                        advertised(&doc.url, template, &mut service.withheld_addresses);
                    }
                    if !self.matches(&templates, vector) {
                        return None;
                    }
                    candidate.templates.extend(templates);
                }
            }
            for l in layers {
                if l.get("type").and_then(Value::as_str) == Some("background") {
                    continue;
                }
                let source = sources.get(l.get("source")?.as_str()?)?;
                if source.get("type").and_then(Value::as_str) == Some("vector")
                    && l.get("source-layer").and_then(Value::as_str).is_none_or(str::is_empty)
                {
                    return None;
                }
            }
            Some(candidate)
        };
        if let Some(candidate) = valid() {
            self.advance(candidate, service);
        } else {
            self.omitted(service, 1, "styles were left out: sources or layer references do not match advertised Web Mercator tiles");
        }
    }

    /// Reuses already checked TileJSON sources; a shared source is fetched only once.
    fn advance(&mut self, mut candidate: Candidate, service: &mut Service) {
        while let Some((url, vector)) = candidate.remaining.front() {
            match self.sources.get(url) {
                Some(templates) if self.matches(templates, *vector) => {
                    candidate.templates.extend(templates.iter().cloned());
                    candidate.remaining.pop_front();
                }
                Some(_) => {
                    self.omitted(service, 1, "styles were left out: a TileJSON source does not match advertised Web Mercator tiles");
                    return;
                }
                None => {
                    let parent = match &candidate.layer.offer {
                        Offer::Style { url } => url.clone(),
                        _ => return,
                    };
                    self.pending.push_front(Document { parent, url: url.clone(), kind: Kind::Source(Box::new(candidate)) });
                    return;
                }
            }
        }
        self.styled.extend(candidate.templates);
        service.layers.push(candidate.layer);
    }
}
