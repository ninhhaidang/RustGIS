//! Data sources: favourites, recently added ones, saved ones and the library (see
//! [`crate::sources`]). Lists only; adding a source is its own command's (`layer.addData`,
//! `service.add`, `layer.addDatabase`…), as `sources.list` says for each kind.

use serde_json::{Value, json};

use super::ps;
use crate::sources::{KINDS, Source, library};
use crate::{CmdResult, CommandSpec as C, Session};

pub fn commands() -> Vec<C> {
    vec![
        C::new("sources.list", "List Data Sources", "Home › Add › Data Sources", list).roles(&[]).effects(crate::policy::Effects::NONE).view(),
        C::new("sources.save", "Save Data Source", "Home › Add › Data Sources", |s, p| keep(s, p, "saved")).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files)).view().params(
            r#"{"kind": "service"|"database"|"stac"|"file"|"url"|"tiles"|"zarr"|"s3", "address": "its path, web address or connection (never with a password)", "name"?: text, "extra"?: {"type"?: service type, "layer"?: text}}"#,
        ),
        C::new("sources.favourite", "Favourite Data Source", "Home › Add › Data Sources", favourite).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files))
            .view()
            .params(r#"{"kind": as sources.save, "address": text, "name"?: text, "extra"?: object, "on"?: bool (false: no longer a favourite)}"#),
        C::new("sources.remove", "Remove Data Source", "Home › Add › Data Sources", remove).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files))
            .view()
            .params(r#"{"list": "saved"|"favourites"|"recent", "kind": text, "address": text}"#),
    ]
}

/// How a source of each kind is added.
fn adds_with(kind: &str) -> &'static str {
    match kind {
        "file" => "layer.addData {path}",
        "url" => "layer.addUrl {url}",
        "tiles" => "map.addTiles {url}",
        "service" => "service.layers {url}, then service.add {url, layer}",
        "database" => "info.databaseTables {database}, then layer.addDatabase {database, layer?}",
        "zarr" => "raster.zarrInfo, then layer.addZarr {url or path, variable}",
        "stac" => "stac.collections {url} and stac.search {url, collections}",
        "sample" => "layer.addSample {name}",
        "s3" => "s3.list, then layer.addUrl {url: s3://…}",
        _ => "",
    }
}

fn shown(s: &Source) -> Value {
    let mut v = serde_json::to_value(s).unwrap_or(Value::Null);
    v["addWith"] = json!(adds_with(&s.kind));
    v
}

fn list(s: &mut Session, _p: &Value) -> CmdResult {
    let src = &s.sources;
    Ok(json!({
        "favourites": src.favourites.iter().map(shown).collect::<Vec<_>>(),
        "recent": src.recent.iter().map(shown).collect::<Vec<_>>(),
        "saved": src.saved.iter().map(shown).collect::<Vec<_>>(),
        "library": library().iter().map(|(group, s)| { let mut v = shown(s); v["group"] = json!(group); v }).collect::<Vec<_>>(),
    }))
}

fn source(p: &Value) -> Result<Source, String> {
    let kind = ps(p, "kind").ok_or_else(|| format!("kind is required ({})", KINDS.join(", ")))?;
    let address = ps(p, "address").ok_or("address is required")?;
    Ok(Source {
        kind: kind.trim().to_string(),
        name: ps(p, "name").unwrap_or_default().to_string(),
        address: address.to_string(),
        extra: p.get("extra").cloned().unwrap_or(Value::Null),
    })
}

fn keep(s: &mut Session, p: &Value, list: &str) -> CmdResult {
    let src = source(p)?;
    let kept = s.sources.keep(list, src)?;
    Ok(shown(&kept))
}

fn favourite(s: &mut Session, p: &Value) -> CmdResult {
    if p.get("on").and_then(Value::as_bool) == Some(false) {
        let src = crate::sources::clean(source(p)?)?;
        let gone = s.sources.forget("favourites", &src.kind, &src.address)?;
        return Ok(json!({"removed": gone}));
    }
    keep(s, p, "favourites")
}

fn remove(s: &mut Session, p: &Value) -> CmdResult {
    let list = ps(p, "list").ok_or("list is required (saved, favourites or recent)")?;
    let src = crate::sources::clean(source(p)?)?;
    let gone = s.sources.forget(list, &src.kind, &src.address)?;
    if !gone {
        return Err(format!("no {} source at {} in {list}", src.kind, src.address));
    }
    Ok(json!({"removed": true}))
}
