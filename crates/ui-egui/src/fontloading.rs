//! Native fallback-font downloads, bounded and kept in the app cache for offline restarts.
//! Reuses the normal internet-only transport; the UI owns activation, failure notices and retries.

use crate::{Inbox, Services};
use rustgis_engine::render::fonts::{MAX_FONT_BYTES, ScriptFont, draws_script, script_font_url};

/// Installs the native counterpart of the browser's font service. Custom host services take
/// precedence. Work, including cache reads and validation, runs on a worker; no frame waits.
pub(crate) fn install(services: &mut Services, ctx: &egui::Context) {
    if services.fetch_font.is_some() {
        return;
    }
    let inbox = services.font_inbox.get_or_insert_with(Inbox::default).clone();
    let wake = ctx.clone();
    services.fetch_font = Some(Box::new(move |url| {
        let Some(script) = ScriptFont::ALL.into_iter().find(|s| script_font_url(*s) == Some(url)) else { return };
        let (url, inbox, wake) = (url.to_string(), inbox.clone(), wake.clone());
        let failed = (url.clone(), inbox.clone(), wake.clone());
        if std::thread::Builder::new()
            .name("fallback font".into())
            .spawn(move || {
                let bytes = read_or_download(script, &url).unwrap_or_else(|e| {
                    log::warn!("fallback font: {e}");
                    Vec::new()
                });
                inbox.lock().unwrap_or_else(|e| e.into_inner()).push((url, bytes));
                wake.request_repaint();
            })
            .is_err()
        {
            failed.1.lock().unwrap_or_else(|e| e.into_inner()).push((failed.0, Vec::new()));
            failed.2.request_repaint();
        }
    }));
}

/// Reads at most the font budget plus a sentinel, rejecting an unusable cache entry before
/// trying the network. An interrupted cache write cannot become the next launch's font.
fn read_or_download(script: ScriptFont, url: &str) -> Result<Vec<u8>, String> {
    use std::io::Write;
    let file = url.rsplit('/').next().ok_or("font URL has no file name")?;
    let path = rustgis_engine::data::dirs::cache_dir().map(|dir| dir.join("fonts").join(file));
    if let Some(bytes) = path.as_deref().and_then(|p| read_cached(p, script))
        && crate::valid_script_download(&bytes, script)
    {
        return Ok(bytes);
    }
    let bytes =
        rustgis_engine::render::rustgis_tiles::cache::download_public_within(url, MAX_FONT_BYTES, std::time::Duration::from_secs(60))?;
    if !crate::valid_script_download(&bytes, script) {
        return Err("downloaded font cannot draw its script".into());
    }
    if let Some(path) = path
        && let Some(dir) = path.parent()
        && std::fs::create_dir_all(dir).is_ok()
    {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let temporary = dir.join(format!(".{file}.{}.{id}.part", std::process::id()));
        if let Ok(mut output) = std::fs::OpenOptions::new().write(true).create_new(true).open(&temporary) {
            let stored = output.write_all(&bytes).and_then(|_| output.sync_all());
            drop(output);
            if stored.is_ok() {
                let _ = std::fs::rename(&temporary, &path);
            }
            let _ = std::fs::remove_file(&temporary);
        }
    }
    Ok(bytes)
}

/// A cache entry is accepted only within the byte budget and with readable script outlines.
fn read_cached(path: &std::path::Path, script: ScriptFont) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?.take(MAX_FONT_BYTES as u64 + 1);
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    (bytes.len() <= MAX_FONT_BYTES && draws_script(&bytes, 0, script)).then_some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_bounded_script_font_can_be_used_from_the_offline_cache() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("font.ttf");
        assert!(read_cached(&path, ScriptFont::Bengali).is_none());
        let bytes = include_bytes!("../../render/testdata/test-script-only.otf");
        std::fs::write(&path, bytes).unwrap();
        assert_eq!(read_cached(&path, ScriptFont::Bengali).unwrap(), bytes);
        assert!(read_cached(&path, ScriptFont::Arabic).is_none(), "wrong-script/mixed-run coverage");
        std::fs::write(&path, b"interrupted font download").unwrap();
        assert!(read_cached(&path, ScriptFont::Bengali).is_none());
        std::fs::File::create(&path).unwrap().set_len(MAX_FONT_BYTES as u64 + 1).unwrap();
        assert!(read_cached(&path, ScriptFont::Bengali).is_none(), "oversized cache entry");
    }
}
