//! `connection.browse`: the datasets a saved connection offers, one level at a time, each with
//! the command that adds it.
//!
//! The app reads over the network in the background, so browsing is a probe/answer pair like
//! `connection.test`: `probe` names the address a level reads, the app fetches it and asks again
//! with the `answer`. Without an answer, the command line and the assistant read it themselves.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use std::borrow::Cow;

use super::super::{b, ps};
use crate::connections::{Connection, DESKTOP_ONLY};
use crate::render::rustgis_tiles::{address, s3, stac};
use crate::{CmdResult, Session};

/// The most children of a non-database level; database levels use the provider's catalog.
const MAX_CHILDREN: usize = 500;
/// The items of a STAC collection that are listed.
const ITEMS: usize = 20;
/// The longest node a caller may give.
const MAX_NODE: usize = 4096;

/// What a node of a connection stands for.
enum Level {
    Database,
    DatabaseSchema { schema: String },
    Service { url: String, kind: Option<String> },
    Catalogue,
    StacRoot { api: String },
    StacCollection { api: String, id: String },
    StacCatalog { url: String, origin: address::Origin },
    StacItem { url: String, origin: address::Origin },
    S3Root,
    S3Prefix { bucket: String, prefix: String },
}

/// The types a `service:<type>:<url>` node names (`auto` leaves the type to be guessed).
const SERVICE_TYPES: [&str; 7] = ["auto", "wms", "wmts", "wfs", "wcs", "ogcapi", "arcgis"];

/// The level a connection's `node` names (its top level when empty).
fn level(c: &Connection, node: &str, observed: Option<&address::Origin>) -> Result<Level, String> {
    let kind = c.kind.as_str();
    let not_a_node = || format!("'{node}' isn't a node of {}", c.name);
    if node.is_empty() {
        return Ok(match kind {
            "postgres" | "mysql" | "sqlserver" => {
                if cfg!(target_arch = "wasm32") {
                    return Err(DESKTOP_ONLY.into());
                }
                Level::Database
            }
            "service" => Level::Service { url: c.address.clone(), kind: None },
            "wms" | "wmts" | "wfs" | "wcs" | "ogcapi" | "arcgis" => Level::Service { url: c.address.clone(), kind: Some(c.kind.clone()) },
            "csw" => Level::Catalogue,
            "stac" => Level::StacRoot { api: stac::api(Some(&c.address))? },
            "s3" => Level::S3Root,
            _ => return Err(format!("unknown kind '{}'", c.kind)),
        });
    }
    if let Some(rest) = node.strip_prefix("service:") {
        if !matches!(kind, "service" | "wms" | "wmts" | "wfs" | "wcs" | "ogcapi" | "arcgis" | "csw") {
            return Err(not_a_node());
        }
        let (t, url) = rest.split_once(':').filter(|(t, _)| SERVICE_TYPES.contains(t)).ok_or_else(not_a_node)?;
        // Judged as a document of the connection would be: a link may not lead into the local network.
        address::check(url, Some(&c.address))?;
        return Ok(Level::Service { url: url.to_string(), kind: (t != "auto").then(|| t.to_string()) });
    }
    if let Some(schema) = node.strip_prefix("schema:") {
        if !matches!(kind, "postgres" | "mysql" | "sqlserver") || schema.is_empty() {
            return Err(not_a_node());
        }
        let schema = URL_SAFE_NO_PAD
            .decode(schema)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .filter(|name| !name.is_empty())
            .ok_or_else(not_a_node)?;
        return Ok(Level::DatabaseSchema { schema });
    }
    if let Some(id) = node.strip_prefix("collection:") {
        if kind != "stac" || id.is_empty() {
            return Err(not_a_node());
        }
        let api = stac::api(Some(&c.address))?;
        if stac::catalog::is_static(&api) {
            return Err(not_a_node());
        }
        return Ok(Level::StacCollection { api, id: id.to_string() });
    }
    if let Some(url) = node.strip_prefix("catalog:") {
        if kind != "stac" {
            return Err(not_a_node());
        }
        let origin = observed.cloned().unwrap_or_else(|| address::Origin::named(&c.address));
        origin.check(url)?;
        return Ok(Level::StacCatalog { url: url.to_string(), origin });
    }
    if let Some(url) = node.strip_prefix("item:") {
        // A static catalog's item: its own file, read when opened.
        if kind != "stac" {
            return Err(not_a_node());
        }
        let origin = observed.cloned().unwrap_or_else(|| address::Origin::named(&c.address));
        origin.check(url)?;
        return Ok(Level::StacItem { url: url.to_string(), origin });
    }
    if let Some(rest) = node.strip_prefix("s3:") {
        let (bucket, prefix) = rest
            .split_once('/')
            .filter(|(bucket, prefix)| kind == "s3" && s3::valid_bucket(bucket) && prefix.len() <= 1024)
            .ok_or_else(not_a_node)?;
        return Ok(Level::S3Prefix { bucket: bucket.to_string(), prefix: prefix.to_string() });
    }
    Err(not_a_node())
}

/// The session's S3 connection for an S3 level, logging in with what needs no typing (a profile,
/// the environment, anonymously, or the keys saved on this computer: browsing is the user action
/// that may ask the system to unlock its store).
/// (A probe logs in too, as `connection.test` does: the address it names, and the signature the
/// fetch needs, depend on the connection.)
fn s3_login(s: &mut Session, c: &Connection, level: &Level) -> Result<Option<s3::Connection>, String> {
    if !matches!(level, Level::S3Root | Level::S3Prefix { .. }) {
        return Ok(None);
    }
    super::ensure_login(s, c)?;
    let conn = s3::connection(&c.name).ok_or("log in first: connection.login {id, accessKeyId, secretAccessKey}")?;
    if let Level::S3Prefix { bucket, .. } = level {
        super::grant(s, c, &conn, bucket);
    }
    Ok(Some(conn))
}

/// The address a level reads (`None`: nothing is fetched, or the database driver reads it).
fn fetch_url(s: &Session, c: &Connection, level: &Level, conn: Option<&s3::Connection>) -> Result<Option<String>, String> {
    Ok(match level {
        Level::Database | Level::DatabaseSchema { .. } => None,
        Level::Service { url, kind } => Some(rustgis_data::ogc::capabilities_url(url, kind.as_deref())),
        Level::Catalogue => Some(super::super::service::records_request(s, &json!({"url": c.address}))?),
        Level::StacRoot { api } => Some(if stac::catalog::is_static(api) { api.clone() } else { stac::collections_url(api) }),
        Level::StacCollection { api, id } => {
            Some(stac::search_url(api, &stac::Search { collections: vec![id.clone()], limit: ITEMS, ..stac::Search::default() })?)
        }
        Level::StacCatalog { url, .. } | Level::StacItem { url, .. } => Some(url.clone()),
        Level::S3Root => match (super::first_bucket(c), conn) {
            (Some(_), _) => None,
            (None, Some(conn)) if conn.credentials.is_some() => Some(super::through(s, c, conn, s3::buckets_url(conn))?),
            _ => return Err("name a bucket under Buckets to browse an anonymous S3 connection".into()),
        },
        Level::S3Prefix { bucket, prefix } => {
            let url = s3::list_url(bucket, prefix, None, conn);
            Some(match conn {
                Some(conn) => super::through(s, c, conn, url)?,
                None => url,
            })
        }
    })
}

/// What a level reads, fetched here (waiting for it).
#[cfg(not(target_arch = "wasm32"))]
fn download(url: &str, ceiling: address::Reach) -> Result<(String, address::Origin), String> {
    use crate::render::rustgis_tiles::cache;
    let fetched = cache::download_blocking_with_origin_within(url, cache::MAX_DOWNLOAD, cache::BLOCKING_TIMEOUT, &[], ceiling)?;
    let text = String::from_utf8(fetched.bytes).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
    Ok((text, fetched.origin))
}

#[cfg(target_arch = "wasm32")]
fn download(_: &str, _: address::Reach) -> Result<(String, address::Origin), String> {
    Err("in the web app, expand the connection under Data Sources › Connections".into())
}

/// The children of a level, and what is known of the rest.
#[derive(Default)]
struct Found {
    children: Vec<Value>,
    truncated: bool,
    matched: Option<u64>,
}

impl Found {
    /// Keeps the first [`MAX_CHILDREN`] of a list (collected with one more, to know it was cut).
    fn new(mut children: Vec<Value>) -> Found {
        let truncated = children.len() > MAX_CHILDREN;
        children.truncate(MAX_CHILDREN);
        Found { children, truncated, matched: None }
    }
}

/// A node that fits what [`browse`] accepts.
fn node_ok(node: &str) -> bool {
    node.len() <= MAX_NODE && !node.chars().any(char::is_control)
}

pub(super) fn browse(s: &mut Session, p: &Value) -> CmdResult {
    let c = super::found(s, p)?.clone();
    let node = ps(p, "node").unwrap_or("");
    if !node_ok(node) {
        return Err("node is a child's node, as connection.browse lists it".into());
    }
    // The app's answers come from a fetch the cache already limits; a caller's may be any size.
    let documents = p.get("documents").and_then(Value::as_array).into_iter().flatten().filter_map(|d| ps(d, "text")).map(str::len);
    if ps(p, "answer").map(str::len).into_iter().chain(documents).sum::<usize>() > super::super::service::MAX_GROUP_BYTES {
        return Err("answer and documents are too large to read".into());
    }
    let observed = node_origin(s, &c, node);
    let level = level(&c, node, observed.as_ref())?;
    let ceiling = match &level {
        Level::StacCatalog { origin, .. } | Level::StacItem { origin, .. } => origin.reach(),
        _ => address::Reach::Local,
    };
    let conn = s3_login(s, &c, &level)?;
    let url = fetch_url(s, &c, &level, conn.as_ref())?;
    let url = match url {
        Some(url) if crate::service_logins::is_service(&c) => Some(crate::service_logins::request(s, Some(&c.id), &url)?),
        other => other,
    };
    if ceiling == address::Reach::Public
        && let Some(url) = &url
    {
        crate::render::rustgis_tiles::cache::restrict_to_public(url);
    }
    let mut out = json!({"connection": c.id, "node": node});
    if b(p, "probe").unwrap_or(false) {
        out["fetch"] = json!(url);
        if ceiling == address::Reach::Public {
            out["fromDocument"] = json!(true);
        }
        return Ok(out);
    }
    let found = match &level {
        Level::Database => database_schemas(s, &c)?,
        Level::DatabaseSchema { schema } => database_schema(s, &c, schema)?,
        Level::Service { url, kind } => service(s, p, &mut out, &c, url, kind.as_deref())?,
        Level::S3Root if url.is_none() => {
            buckets(c.options.get("buckets").map(String::as_str).unwrap_or("").split(',').map(str::to_string))
        }
        _ => {
            let at = url.as_deref().unwrap_or("");
            let (text, origin): (Cow<str>, address::Origin) = match ps(p, "answer") {
                Some(a) => {
                    let origin = match s.browse_response.take() {
                        Some((id, supplied_node, origin)) if id == c.id && supplied_node == node => origin,
                        _ => address::Origin::named(at).narrow_reach(address::Reach::Public),
                    };
                    (Cow::Borrowed(a), origin)
                }
                None => {
                    let (text, origin) = download(at, ceiling)?;
                    (Cow::Owned(text), origin)
                }
            };
            let bytes = text.as_bytes();
            let found = match &level {
                Level::Catalogue => catalogue(s, &c, &text, &mut out)?,
                Level::StacRoot { api } if stac::catalog::is_static(api) => catalogs(s, bytes, &origin)?,
                Level::StacRoot { .. } => collections(bytes)?,
                Level::StacCatalog { .. } => catalogs(s, bytes, &origin)?,
                Level::StacItem { .. } => item_file(s, bytes, &origin)?,
                Level::StacCollection { .. } => items(s, bytes, &origin)?,
                Level::S3Root => buckets(s3::parse_buckets(bytes)?.into_iter()),
                Level::S3Prefix { bucket, prefix } => {
                    let binding = conn.as_ref().and_then(|conn| super::saved_binding(s, &c, conn));
                    objects(bytes, bucket, prefix, binding.as_deref())?
                }
                Level::Database | Level::DatabaseSchema { .. } | Level::Service { .. } => Found::default(),
            };
            if matches!(&level, Level::StacCatalog { .. }) || matches!(&level, Level::StacRoot { api } if stac::catalog::is_static(api)) {
                remember_node_origins(s, &c, &found.children, &origin, node.is_empty())?;
            }
            found
        }
    };
    let mut children = found.children;
    for child in &mut children {
        public_addresses(child);
    }
    if crate::service_logins::is_service(&c) {
        for child in &mut children {
            name_connection(child, &c.id, 0);
        }
    }
    out["children"] = json!(children);
    if found.truncated {
        out["truncated"] = json!(true);
    }
    if let Some(m) = found.matched {
        out["matched"] = json!(m);
    }
    Ok(out)
}

impl Session {
    /// Completes a native UI transport response without accepting provenance through JSON.
    /// A fresh probe checks the exact request and current project before one command consumes it.
    pub fn browse_from_origin(&mut self, params: &Value, requested: &str, opened: u64, origin: address::Origin) -> CmdResult {
        if !self.policy_user() {
            return Err("only native UI dispatch supplies a fetched browse origin".into());
        }
        if self.opened != opened {
            return Err("the project changed while the connection was being read; browse again".into());
        }
        let connection = super::found(self, params)?.clone();
        let node = ps(params, "node").unwrap_or("");
        let probe = self.run("connection.browse", &json!({"id":connection.id,"node":node,"probe":true}))?;
        if probe.get("fetch").and_then(Value::as_str) != Some(requested) {
            return Err("the connection changed while it was being read; browse again".into());
        }
        let ceiling =
            if probe.get("fromDocument").and_then(Value::as_bool) == Some(true) { address::Reach::Public } else { address::Reach::Local };
        let previous = self.browse_response.replace((connection.id, node.to_string(), origin.narrow_reach(ceiling)));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.run("connection.browse", params)));
        self.browse_response = previous;
        match result {
            Ok(result) => result,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }
}

/// A node's provenance is runtime-only and belongs to this project and connection definition.
fn node_origin(s: &Session, c: &Connection, node: &str) -> Option<address::Origin> {
    if s.browse_origins.0 != s.opened {
        return None;
    }
    let root = s.browse_origins.1.get(&(c.id.clone(), c.address.clone(), String::new()));
    let origin = match s.browse_origins.1.get(&(c.id.clone(), c.address.clone(), node.to_string())) {
        Some(origin) => origin.clone(),
        None => root.map(|_| address::Origin::named(&c.address))?,
    };
    Some(match root {
        Some(root) => origin.narrow_reach(root.reach()),
        None => origin,
    })
}

/// Retains only nodes actually admitted by a fetched or conservatively named document origin.
/// Neither reach nor provenance is reconstructed from caller-supplied JSON fields.
fn remember_node_origins(s: &mut Session, c: &Connection, children: &[Value], origin: &address::Origin, root: bool) -> Result<(), String> {
    if c.address.len() > MAX_NODE || origin.url().len() > MAX_NODE {
        return Err(format!("catalog source addresses exceed the {MAX_NODE}-byte provenance limit"));
    }
    if s.browse_origins.0 != s.opened {
        s.browse_origins = (s.opened, Default::default());
    }
    let mut keys = Vec::new();
    if root {
        keys.push((c.id.clone(), c.address.clone(), String::new()));
    }
    for child in children {
        if let Some(node) = ps(child, "node") {
            let mut public = json!(node);
            public_addresses(&mut public);
            if let Some(node) = public.as_str() {
                keys.push((c.id.clone(), c.address.clone(), node.to_string()));
            }
        }
    }
    let added = keys.iter().filter(|key| !s.browse_origins.1.contains_key(*key)).count();
    if s.browse_origins.1.len().saturating_add(added) > 16_384 {
        return Err("too many catalog nodes are open in this session; reopen the project to browse more".into());
    }
    for key in keys {
        s.browse_origins.1.insert(key, origin.clone());
    }
    Ok(())
}

/// Browse candidates persist public addresses plus a connection id, not a live capability.
fn public_addresses(value: &mut Value) {
    match value {
        Value::String(text) => {
            if let Some((prefix, tail)) = text.split_once(rustgis_data::login_address::PREFIX) {
                let wrapped = format!("{}{tail}", rustgis_data::login_address::PREFIX);
                if let Ok(Some(bound)) = rustgis_data::login_address::parse(&wrapped) {
                    *text = format!("{prefix}{}", bound.url);
                }
            }
        }
        Value::Array(values) => values.iter_mut().for_each(public_addresses),
        Value::Object(values) => values.values_mut().for_each(public_addresses),
        _ => {}
    }
}

/// Names the connection in the add commands a listed child (and its children) offers, so the
/// layers they add record it and are read with its login.
fn name_connection(child: &mut Value, id: &str, depth: usize) {
    if depth > 4 {
        return;
    }
    let command = child.get("add").and_then(|a| a.get("command")).and_then(Value::as_str).unwrap_or("");
    if crate::service_logins::ADD_COMMANDS.contains(&command)
        && let Some(params) = child.get_mut("add").and_then(|a| a.get_mut("params")).and_then(Value::as_object_mut)
    {
        params.insert("connection".into(), json!(id));
    }
    if let Some(list) = child.get_mut("children").and_then(Value::as_array_mut) {
        for c in list {
            name_connection(c, id, depth + 1);
        }
    }
}

/// A live database's schemas, each browsed separately so one large schema cannot hide later ones.
fn database_schemas(s: &mut Session, c: &Connection) -> Result<Found, String> {
    let tables = super::connection_use::tables(s, c, None)?;
    Ok(schemas(&tables))
}

/// The names of every schema represented by the database's geometry tables.
fn schemas(tables: &[Value]) -> Found {
    let mut names = std::collections::BTreeSet::new();
    for table in tables {
        if let Some(schema) = table.get("schema").and_then(Value::as_str) {
            names.insert(schema);
        }
    }
    Found {
        children: names
            .into_iter()
            .map(|schema| {
                let mut node = String::with_capacity(7 + schema.len().div_ceil(3) * 4);
                node.push_str("schema:");
                URL_SAFE_NO_PAD.encode_string(schema, &mut node);
                json!({"key": node, "node": node, "name": schema, "what": "schema"})
            })
            .collect(),
        ..Found::default()
    }
}

/// A live database schema's geometry tables.
fn database_schema(s: &mut Session, c: &Connection, schema: &str) -> Result<Found, String> {
    let tables = super::connection_use::tables(s, c, None)?;
    Ok(schema_tables(&tables, schema, &c.address))
}

/// Every geometry table in the opened schema; lazy expansion must not hide tables after a cap.
fn schema_tables(tables: &[Value], schema: &str, address: &str) -> Found {
    let text = |t: &Value, k: &str| t.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let mut children = Vec::new();
    for t in tables.iter().filter(|t| t.get("schema").and_then(Value::as_str) == Some(schema)) {
        let label = text(t, "label");
        let name = text(t, "name");
        let shown = if label.contains(':') { format!("{name}:{}", text(t, "column")) } else { name };
        children.push(json!({
            "key": label,
            "name": shown,
            "what": "table",
            "detail": text(t, "kind"),
            "add": {"command": "layer.addDatabase", "params": {"database": address, "layer": label}},
            "options": true,
        }));
    }
    Found { children, ..Found::default() }
}

/// A service's layers (capabilities read here, or the app's `answer` and `documents`).
fn service(s: &mut Session, p: &Value, out: &mut Value, c: &Connection, url: &str, kind: Option<&str>) -> Result<Found, String> {
    let mut q = json!({"url": crate::service_logins::request(s, Some(&c.id), url)?, "connection": c.id});
    if let Some(k) = kind {
        q["type"] = json!(k);
    }
    if let Some(a) = ps(p, "answer") {
        q["text"] = json!(a);
        if let Some(d) = p.get("documents") {
            q["documents"] = d.clone();
        }
    }
    let r = super::super::service::layers(s, &q)?;
    out["service"] = json!({"url": url, "type": kind.unwrap_or("auto")});
    if let Some(n) = r.get("notes").filter(|n| n.as_array().is_some_and(|a| !a.is_empty())) {
        out["notes"] = n.clone();
    }
    if let Some(next) = r.get("next").filter(|n| !n.is_null()) {
        out["next"] = next.clone();
    }
    // The dialog imports an OGC API's collections together: a features layer isn't added alone there.
    let grouped = r.get("kind").and_then(Value::as_str).is_some_and(|k| k.starts_with("OGC API"));
    let text = |l: &Value, k: &str| l.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let children = r
        .get("layers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(MAX_CHILDREN + 1)
        .map(|l| {
            let (name, title) = (text(l, "name"), text(l, "title"));
            let offer = l.get("offer").cloned().unwrap_or(Value::Null);
            let offer_type = offer.get("type").and_then(Value::as_str).unwrap_or("");
            if offer_type == "service" {
                let node = format!("service:arcgis:{}", text(&offer, "url"));
                if !node_ok(&node) {
                    return Value::Null;
                }
                let what = if name.ends_with('/') { "folder" } else { "service" };
                return json!({"key": node, "node": node, "name": title.trim_end_matches('/'), "what": what});
            }
            let what = match offer_type {
                "vectorTiles" => "vectorTiles",
                "features" => "features",
                "coverage" => "coverage",
                "style" => "style",
                _ => "map",
            };
            let source = text(l, "source_url");
            let mut params = json!({"url": url, "layer": name, "source_url": source});
            if let Some(k) = kind {
                params["type"] = json!(k);
            }
            let mut child = json!({
                "key": format!("{source}#{name}"),
                "name": if title.is_empty() { name.as_str() } else { title.as_str() },
                "what": what,
                "detail": name,
                "layer": l,
                "options": true,
            });
            if !(grouped && what == "features") {
                child["add"] = json!({"command": "service.add", "params": params});
            }
            child
        })
        .collect::<Vec<Value>>();
    // A folder or service whose address is too long to follow as a node is left out, and said so.
    let listed = children.len();
    let children: Vec<Value> = children.into_iter().filter(|c| !c.is_null()).collect();
    let left_out = children.len() < listed;
    let mut found = Found::new(children);
    found.truncated |= left_out;
    Ok(found)
}

/// A catalogue's records, each with the services and files it links to.
fn catalogue(s: &mut Session, c: &Connection, text: &str, out: &mut Value) -> Result<Found, String> {
    let url = crate::service_logins::request(s, Some(&c.id), &c.address)?;
    let r = super::super::service::catalog_search(s, &json!({"url": url, "connection": c.id, "text": text}))?;
    if let Some(n) = r.get("notes") {
        out["notes"] = n.clone();
    }
    let str_of = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let mut dropped = false;
    let children = r
        .get("records")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(MAX_CHILDREN + 1)
        .enumerate()
        .map(|(i, rec)| {
            let id = str_of(rec, "id");
            let title = str_of(rec, "title");
            let links: Vec<Value> = rec
                .get("links")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|l| {
                    let (url, kind, name) = (str_of(l, "url"), str_of(l, "kind"), str_of(l, "name"));
                    match kind.as_str() {
                        "wms" | "wmts" | "wfs" | "wcs" | "ogcapi" | "arcgis" => {
                            let node = format!("service:{kind}:{url}");
                            if !node_ok(&node) {
                                dropped = true;
                                return None;
                            }
                            Some(json!({"key": node, "node": node, "name": if name.is_empty() { url.as_str() } else { name.as_str() }, "what": "service", "detail": kind}))
                        }
                        "download" => {
                            let last = url.split(['?', '#']).next().unwrap_or("").trim_end_matches('/').rsplit('/').next().unwrap_or("");
                            let shown: String = if name.is_empty() { last } else { name.as_str() }.chars().take(200).collect();
                            let named: String = title.chars().take(200).collect();
                            Some(json!({
                                "key": url,
                                "name": shown,
                                "what": "download",
                                "add": {"command": "layer.addUrl", "params": {"url": url, "name": named}},
                            }))
                        }
                        _ => None,
                    }
                })
                .collect();
            let summary: String = str_of(rec, "abstract").chars().take(200).collect();
            json!({"key": format!("record:{}", if id.is_empty() { i.to_string() } else { id }), "name": title, "what": "record", "detail": summary, "children": links})
        })
        .collect();
    let mut found = Found::new(children);
    found.matched = r.get("matched").and_then(Value::as_u64);
    found.truncated |= dropped || r.get("next").is_some_and(|n| !n.is_null());
    Ok(found)
}

/// A STAC API's collections.
fn collections(bytes: &[u8]) -> Result<Found, String> {
    let list = stac::parse_collections(bytes)?;
    Ok(Found::new(
        list.into_iter()
            .take(MAX_CHILDREN + 1)
            .map(|c| {
                let node = format!("collection:{}", c.id);
                json!({"key": node, "node": node, "name": if c.title.is_empty() { c.id.as_str() } else { c.title.as_str() }, "what": "collection", "detail": c.id, "options": true})
            })
            .collect(),
    ))
}

/// What a static catalog's document lists below it: its catalogs and collections (`child` links)
/// and its items (`item` links), each a node read when it is opened (an item's file holds its
/// assets).
fn catalogs(s: &mut Session, bytes: &[u8], origin: &address::Origin) -> Result<Found, String> {
    let mut dropped = 0usize;
    let mut omitted = 0usize;
    let mut skipped = 0usize;
    let mut refused = 0usize;
    let mut children = Vec::new();
    for (prefix, what, list) in [
        ("catalog", "catalog", stac::catalog::parse_children_noted(bytes, origin)?),
        ("item", "item", stac::catalog::parse_item_links_noted(bytes, origin)?),
    ] {
        refused += list.refused;
        omitted += list.omitted;
        skipped += list.skipped;
        for c in list.links {
            let node = format!("{prefix}:{}", c.url);
            if node_ok(&node) {
                children.push(json!({"key": node, "node": node, "name": c.title, "what": what}));
            } else {
                dropped += 1;
            }
        }
    }
    if refused > 0 {
        s.notify(format!("{refused} catalog links were refused by the document's network reach"), false);
    }
    if skipped > 0 {
        s.notify(format!("{skipped} catalog links were left out because their addresses are missing, invalid or not HTTP(S)"), false);
    }
    omitted += children.len().saturating_sub(MAX_CHILDREN);
    if omitted > 0 {
        s.notify(format!("{omitted} catalog links were left out because the listing limit was reached"), false);
    }
    if dropped > 0 {
        s.notify(
            format!("{dropped} catalog links were left out because their node addresses are too long or contain control characters"),
            false,
        );
    }
    let mut found = Found::new(children);
    found.truncated |= dropped > 0 || refused > 0 || omitted > 0 || skipped > 0;
    Ok(found)
}

#[derive(Default)]
struct AssetListing {
    children: Vec<Value>,
    skipped: usize,
}

/// The usable assets, retaining a count of unsupported or refused entries.
fn assets_of(item: &stac::Item) -> AssetListing {
    let mut listing = AssetListing::default();
    for a in item.assets.iter().take(MAX_CHILDREN) {
        let Ok((key, href)) = item.asset_href(Some(&a.key)) else {
            listing.skipped += 1;
            continue;
        };
        let mut params = json!({"asset": key, "name": format!("{} {key}", item.id)});
        match &item.url {
            Some(u) => params["url"] = json!(u),
            None => params["item"] = stac::item_json_with(item, false),
        }
        listing.children.push(json!({
            "key": format!("{}#{key}", item.url.as_deref().unwrap_or(&item.id)),
            "name": if a.title.is_empty() { key.as_str() } else { a.title.as_str() },
            "what": "asset",
            "detail": a.media_type,
            "href": href,
            "fromDocument": item.origin_reach == address::Reach::Public,
            "add": {"command": "stac.addAsset", "params": params},
        }));
    }
    listing
}

/// One counted notice per parsing-loss kind for this document, with a redacted source address.
fn listing_notices(s: &mut Session, origin: &address::Origin, losses: stac::ParseLosses) {
    let source = crate::sources::address_in_message(origin.url());
    for (count, kind, reason) in [
        (losses.skipped_items, "STAC items", "they were invalid"),
        (losses.omitted_items, "STAC items", "the response limit was reached"),
        (losses.skipped_assets, "STAC assets", "their addresses were missing, invalid, unsupported or refused"),
        (losses.omitted_assets, "STAC assets", "the response limit was reached"),
    ] {
        if count > 0 {
            s.notify(format!("{source}: {count} {kind} were left out because {reason}"), false);
        }
    }
}

/// A static catalog's item (its own file): its assets.
fn item_file(s: &mut Session, bytes: &[u8], origin: &address::Origin) -> Result<Found, String> {
    let stac::ParsedItem { item, mut losses } = stac::parse_item_noted(bytes, origin)?;
    let listing = assets_of(&item);
    losses.skipped_assets += listing.skipped;
    listing_notices(s, origin, losses);
    let mut found = Found::new(listing.children);
    found.truncated |= !losses.is_empty();
    Ok(found)
}

/// The items of a collection's search, each with the assets that can be added.
fn items(s: &mut Session, bytes: &[u8], origin: &address::Origin) -> Result<Found, String> {
    let stac::ItemListing { items: list, next, next_refused, matched, mut losses } = stac::parse_items_noted(bytes, origin)?;
    let children = list
        .iter()
        .take(MAX_CHILDREN)
        .map(|parsed| {
            let item = &parsed.item;
            let listing = assets_of(item);
            losses.skipped_assets += listing.skipped;
            let mut child = json!({"key": format!("item:{}", item.url.as_deref().unwrap_or(&item.id)), "name": item.id, "what": "item", "detail": item.datetime, "children": listing.children});
            if listing.skipped > 0 || !parsed.losses.is_empty() { child["truncated"] = json!(true); }
            child
        })
        .collect();
    listing_notices(s, origin, losses);
    if next_refused {
        let source = crate::sources::address_in_message(origin.url());
        s.notify(format!("{source}: 1 next-page link was refused by the document's network reach"), false);
    }
    let truncated = next.is_some() || next_refused || matched.is_some_and(|m| m > list.len() as u64) || !losses.is_empty();
    Ok(Found { children, truncated, matched })
}

/// Buckets to browse, each a node.
fn buckets(names: impl Iterator<Item = String>) -> Found {
    Found::new(
        names
            .map(|n| n.trim().to_string())
            .filter(|n| s3::valid_bucket(n))
            .take(MAX_CHILDREN + 1)
            .map(|n| {
                let node = format!("s3:{n}/");
                json!({"key": node, "node": node, "name": n, "what": "bucket"})
            })
            .collect(),
    )
}

/// One level of a bucket: its folders (each a node) and its files. Through a saved connection's
/// login (`binding`), a file's add command carries an address that names it, so the layer is read
/// with that connection's keys and no other's; its key and name stay the plain object's.
fn objects(bytes: &[u8], bucket: &str, prefix: &str, binding: Option<&str>) -> Result<Found, String> {
    let list = s3::parse_list(bytes)?;
    let last = |k: &str| k.trim_end_matches('/').rsplit('/').next().unwrap_or("").to_string();
    let folders = list.prefixes.iter().map(|p| {
        let node = format!("s3:{bucket}/{p}");
        json!({"key": node, "node": node, "name": format!("{}/", last(p)), "what": "prefix"})
    });
    let mut dropped = false;
    let files = list.objects.iter().filter(|o| o.key != prefix && !o.key.ends_with('/')).filter_map(|o| {
        let url = format!("s3://{bucket}/{}", o.key);
        let add = match binding {
            Some(b) => match s3::bound_url(b, &url) {
                Ok(bound) => bound,
                Err(_) => {
                    dropped = true;
                    return None;
                }
            },
            None => url.clone(),
        };
        Some(json!({
            "key": url,
            "name": last(&o.key),
            "what": "object",
            "detail": o.size.to_string(),
            "add": {"command": "layer.addUrl", "params": {"url": add}},
        }))
    });
    let mut found = Found::new(folders.chain(files).take(MAX_CHILDREN + 1).collect());
    found.truncated |= dropped || list.next.is_some();
    Ok(found)
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    #[test]
    fn failed_saved_login_addresses_mark_the_object_listing_incomplete() {
        let xml = b"<ListBucketResult><Contents><Key>object.fgb</Key><Size>42</Size></Contents></ListBucketResult>";
        let result = super::objects(xml, "data", "", Some("not-a-live-binding")).unwrap();
        assert!(result.truncated);
        assert!(result.children.is_empty());
    }

    fn table(schema: &str, name: &str) -> Value {
        json!({"schema": schema, "name": name, "label": format!("{schema}.{name}"), "kind": "MultiPolygon"})
    }

    #[test]
    fn every_schema_and_its_tables_are_reachable_past_the_old_cap() {
        let mut tables: Vec<Value> = (1..=832).map(|i| table("public", &format!("t_{i:04}"))).collect();
        tables.extend(["addr", "edges", "zip"].map(|name| table("tiger", name)));

        let root = super::schemas(&tables);
        assert_eq!(root.children.len(), 2);
        assert_eq!(root.children[0]["name"], "public");
        assert_eq!(root.children[1]["name"], "tiger");
        for child in &root.children {
            let node = child["node"].as_str().unwrap();
            let parsed = super::level(
                &crate::connections::Connection { kind: "postgres".into(), name: "gis".into(), ..Default::default() },
                node,
                None,
            )
            .unwrap();
            assert!(matches!(parsed, super::Level::DatabaseSchema { .. }));
            assert!(
                super::level(
                    &crate::connections::Connection { kind: "service".into(), name: "gis".into(), ..Default::default() },
                    node,
                    None
                )
                .is_err()
            );
        }

        let public = super::schema_tables(&tables, "public", "postgres:dbname=gis");
        assert_eq!(public.children.len(), 832);
        assert!(!public.truncated);
        assert_eq!(public.matched, None);
        assert_eq!(public.children[0]["name"], "t_0001");
        assert_eq!(public.children.last().unwrap()["add"]["params"]["layer"], "public.t_0832");
    }

    #[test]
    fn schema_nodes_round_trip_unusual_identifiers() {
        for name in ["line\nbreak", "tab\tname", "quote\"\\:name", "réseau\u{85}", "日本語"] {
            let tables = [table(name, "roads"), table("other", "parks")];
            let root = super::schemas(&tables);
            let child = root.children.iter().find(|child| child["name"] == name).unwrap();
            let node = child["node"].as_str().unwrap();
            assert!(super::node_ok(node), "generated schema node must pass command validation: {node:?}");
            for kind in ["postgres", "mysql", "sqlserver"] {
                let connection = crate::connections::Connection { kind: kind.into(), ..Default::default() };
                let super::Level::DatabaseSchema { schema } = super::level(&connection, node, None).unwrap() else {
                    panic!("schema node must select a database schema");
                };
                assert_eq!(schema, name);
                let found = super::schema_tables(&tables, &schema, "database");
                assert_eq!(found.children.len(), 1);
                assert_eq!(found.children[0]["add"]["params"]["layer"], format!("{name}.roads"));
            }
        }
    }

    #[test]
    fn invalid_encoded_schema_nodes_are_rejected() {
        let connection = crate::connections::Connection { kind: "postgres".into(), ..Default::default() };
        for node in ["schema:", "schema:!", "schema:_w", "schema:YQ=="] {
            assert!(super::level(&connection, node, None).is_err(), "{node}");
        }
    }

    #[test]
    fn opening_a_short_schema_lists_every_table_without_a_truncation_note() {
        let tables = vec![table("tiger", "addr"), table("tiger", "edges"), table("public", "roads")];
        let tiger = super::schema_tables(&tables, "tiger", "postgres:dbname=gis");
        assert_eq!(tiger.children.len(), 2);
        assert!(!tiger.truncated);
        assert_eq!(tiger.matched, None);
    }
    #[test]
    fn catalog_browsing_reports_refused_children_and_items_and_keeps_clean_input_quiet() {
        use crate::render::rustgis_tiles::address::Origin;
        let mut session = crate::Session::new(crate::Project::default());
        let from = "https://catalog.corp.example/catalog.json";
        for (local, lossy) in [(false, false), (false, true), (true, true)] {
            let mut links = vec![json!({"rel":"child","href":"https://public.example/child.json"})];
            if lossy {
                links.extend([
                    json!({"rel":"child","href":"http://10.0.0.5/catalog.json"}),
                    json!({"rel":"item","href":"http://10.0.0.5/item.json"}),
                    json!({"rel":"item","href":"http://10.0.0.5/item.json"}),
                ]);
            }
            let origin = if local { Origin::connected(from, "10.0.0.5".parse().unwrap()).unwrap() } else { Origin::named(from) };
            let text = json!({"type":"Catalog","id":"test","links":links}).to_string();
            let mark = session.notice_mark();
            let found = super::catalogs(&mut session, text.as_bytes(), &origin).unwrap();
            let refused = lossy && !local;
            assert_eq!(found.truncated, refused);
            assert_eq!(found.children.len(), if lossy && local { 3 } else { 1 });
            let notices = session.notices_since(mark);
            assert_eq!(notices.len(), usize::from(refused));
            if refused {
                assert!(notices[0].text.contains("2 catalog links were refused"));
            }
        }
    }
    #[test]
    fn partial_catalog_browsing_keeps_usable_nodes_and_reports_both_listing_limits() {
        use crate::render::rustgis_tiles::address::Origin;
        let mut session = crate::Session::new(crate::Project::default());
        let origin = Origin::named("https://public.example/catalog.json");
        for count in [super::MAX_CHILDREN, super::MAX_CHILDREN + 1, 2501] {
            let mut links: Vec<Value> =
                (0..count).map(|i| json!({"rel":"child","href":format!("https://public.example/{i}.json")})).collect();
            links.extend([links[0].clone(), links[0].clone()]);
            let text = json!({"type":"Catalog","id":"test","links":links}).to_string();
            let mark = session.notice_mark();
            let found = super::catalogs(&mut session, text.as_bytes(), &origin).unwrap();
            let omitted = count - super::MAX_CHILDREN;
            assert_eq!(found.children.len(), super::MAX_CHILDREN);
            assert_eq!(found.truncated, omitted > 0);
            let notices = session.notices_since(mark);
            assert_eq!(notices.len(), usize::from(omitted > 0));
            if omitted > 0 {
                assert!(notices[0].text.contains(&format!("{omitted} catalog links were left out")));
            }
        }
    }
    #[test]
    fn admitted_intranet_nodes_keep_their_origin_and_cannot_be_granted_by_json() {
        use crate::render::rustgis_tiles::address::Origin;
        let mut session = crate::Session::new(crate::Project::default());
        let saved = session.run("connection.new", &json!({"kind":"stac","address":"https://catalog.corp.example/catalog.json"})).unwrap();
        let connection = session.connections.connections.iter().find(|c| c.id == saved["id"].as_str().unwrap()).unwrap().clone();
        let body = json!({"type":"Catalog","id":"test","links":[
            {"rel":"child","href":"http://10.0.0.5/catalog.json"},
            {"rel":"item","href":"http://10.0.0.5/item.json"}
        ]})
        .to_string();
        let fake = json!({"id":connection.id,"node":"catalog:http://10.0.0.5/catalog.json","probe":true,"origin":{"reach":"Local"},"fromDocument":false});
        assert!(super::browse(&mut session, &fake).is_err());
        let origin = Origin::connected(&connection.address, "10.0.0.5".parse().unwrap()).unwrap();
        let found = super::catalogs(&mut session, body.as_bytes(), &origin).unwrap();
        super::remember_node_origins(&mut session, &connection, &found.children, &origin, true).unwrap();
        for child in &found.children {
            let node = child["node"].as_str().unwrap();
            let probe = super::browse(&mut session, &json!({"id":connection.id,"node":node,"probe":true})).unwrap();
            assert_eq!(probe["fetch"], node.split_once(':').unwrap().1);
            let mut changed = connection.clone();
            changed.address = "https://another.example/catalog.json".into();
            assert!(super::node_origin(&session, &changed, node).is_none());
        }
        // A later public root narrows older private node grants even when it no longer lists them.
        let public = Origin::connected(&connection.address, "93.184.216.34".parse().unwrap()).unwrap();
        super::remember_node_origins(&mut session, &connection, &[], &public, true).unwrap();
        assert!(super::browse(&mut session, &fake).is_err());
        super::remember_node_origins(&mut session, &connection, &found.children, &origin, true).unwrap();
        session.opened += 1;
        assert!(super::browse(&mut session, &fake).is_err(), "another project cannot inherit a node's local allowance");
    }

    #[test]
    fn invalid_catalog_children_are_counted_in_connection_notices() {
        use crate::render::rustgis_tiles::address::Origin;
        let mut session = crate::Session::new(crate::Project::default());
        let origin = Origin::named("https://public.example/catalog.json");
        let body = json!({"type":"Catalog","id":"test","links":[
            {"rel":"child","href":"ftp://files.example/catalog.json"}, {"rel":"item","href":""}
        ]})
        .to_string();
        let mark = session.notice_mark();
        let found = super::catalogs(&mut session, body.as_bytes(), &origin).unwrap();
        assert!(found.children.is_empty());
        assert!(found.truncated);
        let notices = session.notices_since(mark);
        assert_eq!(notices.len(), 1);
        assert!(notices[0].text.contains("2 catalog links were left out because their addresses"));
    }

    #[test]
    fn ui_browse_origin_is_scoped_to_the_current_request_and_project() {
        use crate::render::rustgis_tiles::address::Origin;
        let mut session = crate::Session::new(crate::Project::default());
        let requested = "https://ui-origin.corp.example/catalog.json";
        let saved = session.run("connection.new", &json!({"kind":"stac","address":requested})).unwrap();
        let id = saved["id"].as_str().unwrap();
        let body = json!({"type":"Catalog","id":"test","links":[{"rel":"child","href":"http://10.0.0.7/catalog.json"}]}).to_string();
        let params = json!({"id":id,"answer":body,"origin":{"reach":"Local"}});
        let raw = session.run("connection.browse", &params).unwrap();
        assert!(raw["children"].as_array().unwrap().is_empty());
        let origin = Origin::connected(requested, "10.0.0.7".parse().unwrap()).unwrap();
        let opened = session.opened;
        let found = session.browse_from_origin(&params, requested, opened, origin.clone()).unwrap();
        assert_eq!(found["children"][0]["node"], "catalog:http://10.0.0.7/catalog.json");
        assert!(session.browse_response.is_none());
        assert!(session.browse_from_origin(&params, "https://changed.example/catalog.json", opened, origin.clone()).is_err());
        session.opened += 1;
        assert!(session.browse_from_origin(&params, requested, opened, origin.clone()).is_err());
        let opened = session.opened;
        // Observed Public redirects narrow a locally named entry point as well.
        let public = Origin::connected("https://public.example/catalog.json", "93.184.216.34".parse().unwrap()).unwrap();
        let found = session.browse_from_origin(&params, requested, opened, public).unwrap();
        assert!(found["children"].as_array().unwrap().is_empty());
        assert!(session.browse_response.is_none());
    }

    #[test]
    fn listed_assets_carry_the_observed_items_public_download_ceiling() {
        use crate::render::rustgis_tiles::address::Origin;
        let requested = "https://catalog.corp.example/item.json";
        let mut session = crate::Session::new(crate::Project::default());
        for (peer, href, public) in
            [("10.0.0.5", "http://10.0.0.5/scene.geojson", false), ("93.184.216.34", "https://public.example/scene.geojson", true)]
        {
            let body = json!({"type":"Feature","id":"scene","geometry":null,"properties":{},
                "assets":{"data":{"href":href,"roles":["data"]}},"links":[]})
            .to_string();
            let origin = Origin::connected(requested, peer.parse().unwrap()).unwrap();
            let found = super::item_file(&mut session, body.as_bytes(), &origin).unwrap();
            assert_eq!(found.children[0]["href"], href);
            assert_eq!(found.children[0]["fromDocument"], public);
        }
    }

    #[test]
    fn collection_search_assets_keep_the_responses_observed_origin() {
        let mut session = crate::Session::new(crate::Project::default());
        let requested = "https://search.corp.example/search";
        let body = json!({"type":"FeatureCollection","features":[{
            "type":"Feature","id":"scene","geometry":null,"properties":{},
            "assets":{"data":{"href":"http://10.0.0.5/scene.geojson","roles":["data"]}},"links":[]
        }]})
        .to_string();
        let local = super::address::Origin::connected(requested, "10.0.0.5".parse().unwrap()).unwrap();
        let found = super::items(&mut session, body.as_bytes(), &local).unwrap();
        assert_eq!(found.children[0]["children"][0]["href"], "http://10.0.0.5/scene.geojson");
        assert_eq!(found.children[0]["children"][0]["fromDocument"], false);
        let public = super::address::Origin::connected(requested, "93.184.216.34".parse().unwrap()).unwrap();
        let found = super::items(&mut session, body.as_bytes(), &public).unwrap();
        assert!(found.children[0]["children"].as_array().unwrap().is_empty());
    }

    #[test]
    fn unusable_assets_are_counted_once_per_listing_and_clean_assets_stay_quiet() {
        let mut session = crate::Session::new(crate::Project::default());
        let origin = super::address::Origin::named("https://public.example/item.json");
        for bad in [None, Some("http://10.0.0.5/private.geojson"), Some("ftp://files.example/scene.geojson")] {
            let mut item = json!({"type":"Feature","id":"scene","geometry":null,"properties":{},"links":[],
                "assets":{"data":{"href":"https://public.example/scene.geojson","roles":["data"]}}});
            if let Some(href) = bad {
                item["assets"]["bad"] = json!({"href":href,"roles":["data"]});
            }
            let mark = session.notice_mark();
            let found = super::item_file(&mut session, item.to_string().as_bytes(), &origin).unwrap();
            assert_eq!(found.children.len(), 1);
            assert_eq!(found.truncated, bad.is_some());
            let notices = session.notices_since(mark);
            assert_eq!(notices.len(), usize::from(bad.is_some()));
            if bad.is_some() {
                assert!(notices[0].text.contains("1 STAC assets were left out"));
                assert!(notices[0].text.contains("public.example/item.json"));
            }
            let mut second = item.clone();
            second["id"] = json!("scene2");
            let body = json!({"type":"FeatureCollection","features":[item,second]}).to_string();
            let mark = session.notice_mark();
            let found = super::items(&mut session, body.as_bytes(), &origin).unwrap();
            assert_eq!(found.truncated, bad.is_some());
            for child in &found.children {
                assert_eq!(child["children"].as_array().unwrap().len(), 1);
                assert_eq!(child.get("truncated").and_then(Value::as_bool).unwrap_or(false), bad.is_some());
            }
            let notices = session.notices_since(mark);
            assert_eq!(notices.len(), usize::from(bad.is_some()));
            if bad.is_some() {
                assert!(notices[0].text.contains("2 STAC assets were left out"));
            }
        }
    }

    #[test]
    fn browse_reports_parser_caps_and_invalid_entries_and_clean_lists_stay_quiet() {
        let mut session = crate::Session::new(crate::Project::default());
        let origin = super::address::Origin::named("https://public.example/search");
        let item = |id: usize| json!({"type":"Feature","id":id.to_string(),"properties":{},"assets":{}});
        for (count, omitted) in [(3, 0), (201, 1)] {
            let body = json!({"features":(0..count).map(item).collect::<Vec<_>>()}).to_string();
            let mark = session.notice_mark();
            let found = super::items(&mut session, body.as_bytes(), &origin).unwrap();
            assert_eq!(found.children.len(), count.min(200));
            assert_eq!(found.truncated, omitted > 0);
            let notices = session.notices_since(mark);
            assert_eq!(notices.len(), usize::from(omitted > 0));
            if omitted > 0 {
                assert!(notices[0].text.contains("1 STAC items were left out because the response limit"));
            }
        }
        let body = json!({"features":[item(0),{"type":"Feature"}]}).to_string();
        let mark = session.notice_mark();
        let found = super::items(&mut session, body.as_bytes(), &origin).unwrap();
        assert_eq!(found.children.len(), 1);
        assert!(found.truncated);
        assert!(session.notices_since(mark)[0].text.contains("1 STAC items were left out because they were invalid"));

        let mut feature = item(0);
        for i in 0..101 {
            feature["assets"][format!("a{i:03}")] = json!({"href":format!("https://public.example/{i}.geojson")});
        }
        let mark = session.notice_mark();
        let found = super::item_file(&mut session, feature.to_string().as_bytes(), &origin).unwrap();
        assert_eq!(found.children.len(), 100);
        assert!(found.truncated);
        let notices = session.notices_since(mark);
        assert_eq!(notices.len(), 1);
        assert!(notices[0].text.contains("1 STAC assets were left out because the response limit"));

        let body = json!({"features":[item(0)],"links":[{"rel":"next","href":"http://10.0.0.5/next"}]}).to_string();
        let mark = session.notice_mark();
        assert!(super::items(&mut session, body.as_bytes(), &origin).unwrap().truncated);
        assert!(session.notices_since(mark)[0].text.contains("1 next-page link was refused"));
    }

    #[test]
    fn public_catalog_nodes_keep_a_public_transport_ceiling_after_a_local_entrypoint() {
        use crate::render::rustgis_tiles::address::{Origin, Reach};
        let mut session = crate::Session::new(crate::Project::default());
        let saved = session.run("connection.new", &json!({"kind":"stac","address":"http://localhost/catalog.json"})).unwrap();
        let connection = session.connections.connections.iter().find(|c| c.id == saved["id"].as_str().unwrap()).unwrap().clone();
        let url = "https://public-node-ceiling.example/child.json";
        let body = json!({"type":"Catalog","id":"test","links":[{"rel":"child","href":url}]}).to_string();
        let origin = Origin::connected("https://public.example/final.json", "93.184.216.34".parse().unwrap()).unwrap();
        let found = super::catalogs(&mut session, body.as_bytes(), &origin).unwrap();
        super::remember_node_origins(&mut session, &connection, &found.children, &origin, true).unwrap();
        let probe = super::browse(&mut session, &json!({"id":connection.id,"node":found.children[0]["node"],"probe":true})).unwrap();
        assert!(
            super::browse(&mut session, &json!({"id":connection.id,"node":"catalog:http://10.0.0.8/private.json","probe":true})).is_err()
        );
        let unlisted = super::browse(
            &mut session,
            &json!({"id":connection.id,"node":"catalog:https://unlisted-public-node.example/catalog.json","probe":true}),
        )
        .unwrap();
        assert_eq!(unlisted["fromDocument"], true);
        assert_eq!(probe["fromDocument"], true);
        assert_eq!(crate::render::rustgis_tiles::cache::ceiling_for(url), Reach::Public);
    }
}
