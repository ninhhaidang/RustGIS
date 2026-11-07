//! S3 connections and bucket listings (`s3://` addresses are read wherever data is added by
//! address: see `rustgis_tiles::s3`).

use serde_json::{Value, json};

use super::ps;
use crate::render::rustgis_tiles::s3::{self, Connection, Credentials};
use crate::{CmdResult, CommandSpec as C, Session};

pub fn commands() -> Vec<C> {
    vec![
        C::new("s3.connect", "Connect to S3", "Home › Add › S3", connect).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Reserved)).params(
            r#"{"name"?: text (default "default"), "endpoint"?: "https://… of an S3-compatible service (MinIO, Cloudflare R2, Wasabi…); AWS when absent", "region"?: text (default us-east-1), "buckets"?: [names] (the buckets it serves; others are read anonymously unless browsed through it with s3.list), "accessKeyId"?: text, "secretAccessKey"?: text, "sessionToken"?: text, "profile"?: "an AWS profile in ~/.aws/credentials and config (desktop)", "anonymous"?: bool (no credentials), "environment"?: bool (AWS_ACCESS_KEY_ID… from the environment, desktop)} — without keys, a profile or environment, the connection is anonymous; credentials are kept for the session only"#,
        )
        .view(),
        C::new("s3.disconnect", "Disconnect From S3", "Home › Add › S3", disconnect).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Reserved)).view().params(r#"{"name": text}"#),
        C::new("s3.connections", "S3 Connections", "Home › Add › S3", |_s, _p| {
            Ok(json!({"connections": s3::connections().iter().map(Connection::public).collect::<Vec<_>>()}))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .view(),
        C::new("s3.list", "List S3 Bucket", "Home › Add › S3", list).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network)).view().params(
            r#"{"url"?: "s3://bucket/prefix/", "bucket"?: text, "prefix"?: text, "connection"?: "a connection's name: its buckets when no bucket is given; a bucket listed through it is served by it from then on", "limit"?: number (default 1000, at most 10000)}"#,
        ),
    ]
}

/// The credentials a connection is made with: given, from a profile or the environment (desktop,
/// only when asked for by name), or none (anonymous). The profile's endpoint comes along.
fn credentials(p: &Value) -> Result<(Option<Credentials>, Option<String>, Option<String>, &'static str), String> {
    let text = |k: &str| ps(p, k).map(str::trim).filter(|v| !v.is_empty()).map(str::to_string);
    if super::b(p, "anonymous").unwrap_or(false) {
        return Ok((None, None, None, "anonymous"));
    }
    if let (Some(access_key_id), Some(secret_access_key)) = (text("accessKeyId"), text("secretAccessKey")) {
        return Ok((Some(Credentials { access_key_id, secret_access_key, session_token: text("sessionToken") }), None, None, "keys"));
    }
    if text("accessKeyId").is_some() || text("secretAccessKey").is_some() {
        return Err("give both accessKeyId and secretAccessKey".into());
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        if let Some(name) = text("profile") {
            let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(std::path::PathBuf::from);
            let file = |var: &str, default: &str| {
                std::env::var_os(var).map(std::path::PathBuf::from).or_else(|| home.as_ref().map(|h| h.join(".aws").join(default)))
            };
            // Small text files; anything else is ignored.
            let read = |path: Option<std::path::PathBuf>| {
                path.filter(|p| std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.len() < 1 << 20))
                    .and_then(|p| std::fs::read_to_string(p).ok())
                    .unwrap_or_default()
            };
            let pr =
                s3::profile(&read(file("AWS_SHARED_CREDENTIALS_FILE", "credentials")), &read(file("AWS_CONFIG_FILE", "config")), &name);
            return match pr.credentials {
                Some(c) => Ok((Some(c), pr.region, pr.endpoint, "profile")),
                None => Err(format!("the AWS profile '{name}' has no access key (in ~/.aws/credentials or ~/.aws/config)")),
            };
        }
        // The environment's keys only when asked for: never for a blank form.
        if super::b(p, "environment").unwrap_or(false) {
            let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
            let (Some(access_key_id), Some(secret_access_key)) = (env("AWS_ACCESS_KEY_ID"), env("AWS_SECRET_ACCESS_KEY")) else {
                return Err("AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY aren't set".into());
            };
            let region = env("AWS_REGION").or_else(|| env("AWS_DEFAULT_REGION"));
            return Ok((
                Some(Credentials { access_key_id, secret_access_key, session_token: env("AWS_SESSION_TOKEN") }),
                region,
                None,
                "environment",
            ));
        }
    }
    #[cfg(target_arch = "wasm32")]
    if text("profile").is_some() || super::b(p, "environment").unwrap_or(false) {
        return Err("AWS profiles and the environment are read by the desktop app; give accessKeyId and secretAccessKey".into());
    }
    Ok((None, None, None, "anonymous"))
}

pub(super) fn connect(_s: &mut Session, p: &Value) -> CmdResult {
    install(p, None)
}

/// The login of the saved connection `id`, made live under this session's runtime token
/// `binding` in one step (see `rustgis_tiles::s3::connect_saved`): it signs only the addresses
/// that name the binding, never one found by its bucket or host. Takes the same parameters as
/// `s3.connect`.
pub(super) fn connect_saved(_s: &mut Session, p: &Value, binding: &str, id: &str) -> CmdResult {
    install(p, Some((binding, id)))
}

fn install(p: &Value, saved: Option<(&str, &str)>) -> CmdResult {
    let name = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).unwrap_or("default").to_string();
    let endpoint = ps(p, "endpoint").map(str::trim).filter(|e| !e.is_empty()).map(str::to_string);
    let (creds, region, profile_endpoint, source) = credentials(p)?;
    // Credentials found on this computer (a profile's or the environment's) go only to AWS or to
    // the endpoint the same profile names: a command can't send them to another server.
    if matches!(source, "profile" | "environment")
        && let Some(e) = &endpoint
        && profile_endpoint.as_deref().map(|pe| pe.trim_end_matches('/')) != Some(e.trim_end_matches('/'))
    {
        return Err(format!(
            "credentials from an AWS profile or the environment are used only with AWS or the profile's own endpoint_url, not {e}; give accessKeyId and secretAccessKey for it"
        ));
    }
    let buckets: Vec<String> = match p.get("buckets") {
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        Some(Value::String(s)) => s.split(',').map(str::to_string).collect(),
        _ => Vec::new(),
    };
    let c = Connection {
        name,
        endpoint: endpoint.or(profile_endpoint),
        // A blank region leaves the profile's or the environment's.
        region: ps(p, "region").map(str::trim).filter(|r| !r.is_empty()).map(str::to_string).or(region).unwrap_or_default(),
        buckets,
        credentials: creds,
    };
    match saved {
        Some((binding, id)) => s3::connect_saved(c.clone(), binding, id)?,
        None => s3::connect(c.clone())?,
    }
    let c = s3::connection(&c.name).unwrap_or(c);
    let mut out = c.public();
    out["credentials"] = json!(source);
    Ok(out)
}

fn disconnect(_s: &mut Session, p: &Value) -> CmdResult {
    let name = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).ok_or("name is required")?;
    if !s3::disconnect(name) {
        return Err(format!("no S3 connection named '{name}'"));
    }
    Ok(Value::Null)
}

/// The bucket and prefix a listing names.
fn target(p: &Value) -> Result<(Option<String>, String), String> {
    if let Some(u) = ps(p, "url").map(str::trim).filter(|u| !u.is_empty()) {
        let (bucket, prefix) = s3::parse(u)?;
        return Ok((Some(bucket), prefix));
    }
    let bucket = ps(p, "bucket").map(str::trim).filter(|b| !b.is_empty()).map(str::to_string);
    if let Some(b) = &bucket
        && !s3::valid_bucket(b)
    {
        return Err(format!("'{b}' isn't a bucket name"));
    }
    let prefix = ps(p, "prefix").unwrap_or("").to_string();
    if prefix.len() > 1024 || prefix.chars().any(char::is_control) {
        return Err("the prefix isn't one S3 takes".into());
    }
    Ok((bucket, prefix))
}

pub(super) fn list(_s: &mut Session, p: &Value) -> CmdResult {
    let (bucket, prefix) = target(p)?;
    let named = match ps(p, "connection").map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => Some(s3::connection(n).ok_or_else(|| format!("no S3 connection named '{n}'"))?),
        None => None,
    };
    let limit = super::f(p, "limit").map(|l| l.clamp(1.0, s3::MAX_LIST as f64) as usize).unwrap_or(1000);
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (bucket, prefix, named, limit);
        Err("in the web app, browse buckets with Home › Add › Add Data › S3…".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let fetch = |url: &str| crate::render::rustgis_tiles::cache::download_blocking(url);
        // A saved connection's login signs only the addresses that name it.
        let through = |c: &s3::Connection, url: String| match s3::binding_of(&c.name) {
            Some(binding) => s3::bound_url(&binding, &url),
            None => Ok(url),
        };
        let Some(bucket) = bucket else {
            // A connection's buckets (ListBuckets needs its credentials).
            let c = named
                .or_else(|| s3::connections().into_iter().find(|c| c.credentials.is_some()))
                .ok_or("give a bucket (or an s3:// address), or connect with credentials first to list your buckets")?;
            if c.credentials.is_none() {
                return Err(format!("'{}' is anonymous: listing its buckets needs credentials; give a bucket", c.name));
            }
            let buckets = s3::parse_buckets(&fetch(&through(&c, s3::buckets_url(&c))?)?)?;
            return Ok(json!({"connection": c.name, "buckets": buckets}));
        };
        // A bucket listed through a connection is served by it from then on (its objects are read
        // with its keys); otherwise the bucket's own connection, if any, or anonymously.
        if let Some(c) = &named {
            match s3::binding_of(&c.name) {
                Some(binding) => s3::grant_saved(&binding, &bucket)?,
                None => s3::grant(&c.name, &bucket),
            }
        }
        let conn = named.or_else(|| s3::for_bucket(&bucket));
        let (mut prefixes, mut objects, mut token, mut pages) = (Vec::new(), Vec::new(), None::<String>, 0);
        loop {
            let url = s3::list_url(&bucket, &prefix, token.as_deref(), conn.as_ref());
            let url = match &conn {
                Some(c) => through(c, url)?,
                None => url,
            };
            let page = s3::parse_list(&fetch(&url)?)?;
            prefixes.extend(page.prefixes);
            objects.extend(page.objects);
            pages += 1;
            token = page.next;
            if token.is_none() || pages >= 10 || prefixes.len() + objects.len() >= limit {
                break;
            }
        }
        let more = token.is_some() || prefixes.len() + objects.len() > limit;
        prefixes.truncate(limit);
        objects.truncate(limit.saturating_sub(prefixes.len()));
        // What a saved connection lists is added through it: its objects' addresses name it.
        let address = |key: &str| {
            let url = format!("s3://{bucket}/{key}");
            match conn.as_ref().map(|c| through(c, url.clone())) {
                Some(Ok(bound)) => bound,
                _ => url,
            }
        };
        Ok(json!({
            "bucket": bucket,
            "prefix": prefix,
            "prefixes": prefixes,
            "objects": objects.iter().map(|o| json!({"key": o.key, "size": o.size, "modified": o.modified, "url": address(&o.key)})).collect::<Vec<_>>(),
            "truncated": more,
        }))
    }
}
