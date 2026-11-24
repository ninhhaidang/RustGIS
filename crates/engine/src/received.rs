//! What the addresses a file names may reach, by where the file came from.
//!
//! A project, map file, layer file, template or package the user opens from their own disk is
//! theirs: what it names reaches as far as the user's own addresses do. One that arrived from the
//! internet (opened by its web address, a GeoLibre project downloaded, or file text the Assistant,
//! MCP or a plugin supplied, which says nothing trustworthy about where it was read) is a document
//! read from the internet, and what it names reaches only the internet, as what any other such
//! document names does ([`cache::restrict_to_public`]). Either way, the addresses it names on this
//! computer or the local network are said as it opens.

use crate::Session;
use crate::render::rustgis_tiles::{address, address::Reach, cache, logins};
use serde_json::Value;
use std::collections::HashSet;

/// How many addresses a notice names; the count covers them all.
const NAMED: usize = 3;

impl Session {
    /// The widest reach what a file being read names may have (`p`: the command's parameters).
    /// A file downloaded by its web address is read within the reach it was read at
    /// ([`Session::receiving`]); file text or data supplied by anything but the user (the
    /// Assistant, MCP, a plugin) within the internet; a file read from a path, or given by the user,
    /// within this computer and the local network, as before. That includes a path an attached
    /// agent opens, even one it wrote: what the file names reaches no further than the agent's own
    /// approved commands do already (its `layer.addUrl` of a local server reads from there).
    pub(crate) fn file_reach(&self, p: &Value) -> Reach {
        let supplied = ["text", "data"].iter().any(|k| p.get(k).is_some_and(Value::is_string));
        let given = if supplied && !self.policy_user() { Reach::Public } else { Reach::Local };
        self.received.map_or(given, |read| given.min(read.min(Reach::Local)))
    }

    /// Reads the files the commands run next open as downloaded from an address reached at
    /// `reach` (the app's open by web address: `Reach::Public` when the download came from the
    /// internet), until it is called again with `None`. Returns what it replaced, for the caller
    /// to put back.
    pub fn receiving(&mut self, reach: Option<Reach>) -> Option<Reach> {
        std::mem::replace(&mut self.received, reach)
    }
}

/// What a file names on this computer or the local network, gathered as it is read
/// ([`hold`]), for [`Named::say`].
#[derive(Debug, Default)]
#[must_use = "say what the file names"]
pub(crate) struct Named {
    /// The distinct addresses named there that are read from there.
    read: Few,
    /// The distinct addresses named there that are held to the internet: all of them when the file
    /// arrived from there, else those something received from there named first.
    held: Few,
    /// Whether the file arrived from the internet.
    received: bool,
    /// Every distinct address counted, so each counts once however often the file names it.
    seen: HashSet<String>,
}

/// How many addresses there are, and the first few, for a notice.
#[derive(Debug, Default)]
struct Few {
    first: Vec<String>,
    count: usize,
}

impl Few {
    fn add(&mut self, a: &str) {
        if self.first.len() < NAMED {
            self.first.push(a.to_string());
        }
        self.count += 1;
    }

    /// The first few as a notice shows them, and how many more.
    fn shown(&self) -> String {
        let mut shown: Vec<String> = self.first.iter().map(|a| crate::sources::address_in_message(a)).collect();
        if self.count > NAMED {
            shown.push(format!("and {} more", self.count - NAMED));
        }
        shown.join(", ")
    }
}

/// What an address a file names is fetched from.
enum Fetched {
    /// This web address, whatever wraps it: a tile template (`pmtiles:`, `cog:`), an anonymous
    /// read, a saved S3 connection or a login.
    Web(String),
    /// An S3 address whose destination can't be told: held all the same, though not counted.
    Unknown,
    /// Not the web (a file, a database).
    Not,
}

fn fetched_from(a: &str) -> Fetched {
    use crate::render::rustgis_tiles::s3;
    let archive = crate::sources::template_archive(a).map_or(a, |(_, archive)| archive);
    let (plain, _) = s3::split_anonymous(archive);
    let web = match s3::bound_destination(plain) {
        Ok(Some(destination)) => destination,
        Ok(None) if s3::is_s3(plain) => match s3::https_url(plain) {
            Ok(web) => web,
            Err(_) => return Fetched::Unknown,
        },
        Ok(None) => logins::inner(plain).to_string(),
        Err(_) => return Fetched::Unknown,
    };
    if rustgis_map::tiles::is_web(&web) { Fetched::Web(web) } else { Fetched::Not }
}

/// Holds every web address a file names (`addresses`, its whole list as login bindings are cleared
/// from it) within `reach`, before anything it names is read: within the internet, each is
/// recorded so ([`cache::restrict_to_public`]), with a Shapefile's parts, so it is fetched only
/// from there, however its name resolves; an address inside a tile template or a wrapper is held
/// by what it is fetched from. Within the local network, nothing is recorded, and the addresses
/// reach as before. Returns what the file names on this computer or the local network.
pub(crate) fn hold(addresses: impl IntoIterator<Item = impl AsRef<str>>, reach: Reach) -> Named {
    let received = reach == Reach::Public;
    let mut named = Named { received, ..Named::default() };
    // Recorded together: the cache forgets their earlier reads in one pass, not one per address.
    let mut held = Vec::new();
    for a in addresses {
        let a = a.as_ref();
        let web = match fetched_from(a) {
            Fetched::Web(web) => Some(web),
            Fetched::Unknown => None,
            Fetched::Not => continue,
        };
        if received {
            held.push(a.to_string());
            held.extend(rustgis_data::io::shapefile_part_urls(a).unwrap_or_default().into_iter().map(|(_, part)| part));
        }
        // Only what can be read there: a forbidden address (link-local, a cloud's metadata) never is.
        if web.is_some_and(|web| address::reach(&web) == Reach::Local) && named.seen.insert(a.to_string()) {
            if received || cache::ceiling_for(a) == Reach::Public {
                named.held.add(a);
            } else {
                named.read.add(a);
            }
        }
    }
    cache::restrict_all_to_public(held.iter().map(String::as_str));
    named
}

impl Named {
    /// Says, once, what `what` (a project, a map file, a layer file) names on this computer or the
    /// local network, with how many and the first few: held to the internet, or read from there as
    /// the user's own, and which of those stay held because something received from the internet
    /// named them first. (Filling the record of held addresses is said after the command, as any
    /// way of filling it is: `Session::say_public_only_full`.)
    pub(crate) fn say(self, s: &mut Session, what: &str) {
        let (read, held) = (self.read.count, self.held.count);
        let kept = "stay held to the internet, as something received from the internet named them first, and aren't read";
        let said = match (self.received, read, held) {
            (_, 0, 0) => None,
            (true, _, _) => Some(format!(
                "{what} came from the internet, so the {held} address(es) it names on this computer or the local network are held to the internet and aren't read: {}",
                self.held.shown()
            )),
            (false, _, 0) => {
                Some(format!("{what} reads from {read} address(es) on this computer or the local network: {}", self.read.shown()))
            }
            (false, 0, _) => {
                Some(format!("{what} names {held} address(es) on this computer or the local network that {kept}: {}", self.held.shown()))
            }
            (false, _, _) => Some(format!(
                "{what} reads from {read} address(es) on this computer or the local network: {}. {held} more it names there {kept}: {}",
                self.read.shown(),
                self.held.shown()
            )),
        };
        if let Some(said) = said {
            s.notify(said, false);
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a test module gated on more than `test`, which clippy's allow-unwrap-in-tests does not see"
)]
mod tests {
    use super::*;
    use crate::Project;
    use crate::policy::{AgentAccess, Call, Effects, Outcome, Revocation, Scope};
    use serde_json::json;

    /// Authority that is never revoked, for running a command as an agent would.
    struct Live;
    impl Revocation for Live {
        fn allows(&self, _: Effects) -> bool {
            true
        }
    }

    /// Runs `id` as an attached agent (MCP) with full access does, approved. (The Assistant
    /// refuses to run these documents at all.)
    fn as_agent(s: &mut Session, id: &str, p: &Value) -> crate::CmdResult {
        let scope = Scope::agent(AgentAccess::Full, false, "mcp", None, std::sync::Arc::new(Live));
        let call = Call::command(id, p.clone());
        match s.review(&scope, &call) {
            Outcome::NeedsApproval(permit) => s.run_permitted(scope, permit.allow(), &call),
            Outcome::Allowed => s.run_scoped(scope, &call),
            Outcome::Refused(e) => panic!("{id} refused: {e}"),
        }
    }

    /// A project naming a tile server on this computer, a layer's source on the local network and
    /// a tile server on the internet (hosts unique to `tag`, so tests running together don't share
    /// a record), with those three addresses.
    fn project(tag: &str) -> (Session, [String; 3]) {
        let local = format!("http://localhost:18080/{tag}/{{z}}/{{x}}/{{y}}.png");
        let lan = format!("http://192.168.1.10/{tag}/roads.fgb");
        let public = format!("https://{tag}.example/tiles/{{z}}/{{x}}/{{y}}.png");
        let mut s = Session::new(Project::default());
        s.project.name = tag.into();
        s.run("map.addTiles", &json!({"url": local, "name": "Mine"})).unwrap();
        s.run("map.addTiles", &json!({"url": public, "name": "Theirs"})).unwrap();
        let roads = r#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{"n":1},"geometry":{"type":"Point","coordinates":[8.5,47.4]}}]}"#;
        s.run("layer.addDataText", &json!({"fileName": "roads.geojson", "text": roads})).unwrap();
        let id = s.map().layers.last().map(|l| l.id).unwrap();
        s.map_mut().layer_mut(id).unwrap().dataset.source = Some(lan.clone());
        (s, [local, lan, public])
    }

    /// The reach each address has now, after the record of them is put back as a new process has
    /// it (the user's own: [`cache::allow_named`]) before `open` runs.
    fn reach_after(addresses: &[String; 3], open: impl FnOnce()) -> [Reach; 3] {
        for a in addresses {
            cache::allow_named(a);
        }
        open();
        [0, 1, 2].map(|i| cache::ceiling_for(&addresses[i]))
    }

    /// The notices that say what a file names on this computer or the local network.
    fn said(s: &Session) -> Vec<String> {
        s.notices.iter().filter(|n| n.text.contains("on this computer or the local network")).map(|n| n.text.clone()).collect()
    }

    /// A project opened from the user's own disk, or given by the user, reads its servers on this
    /// computer and the local network as before, and says so once, with how many and which. One
    /// that names none says nothing.
    #[test]
    fn a_project_from_your_own_disk_keeps_its_local_servers_and_names_them() {
        let (mut s, addresses) = project("own-disk-reach");
        let (text, _) = s.project.saved_text(None).unwrap();
        let dir = std::env::temp_dir().join(format!("rustgis-own-disk-reach-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("own.rgis");
        std::fs::write(&path, &text).unwrap();
        let mut opened = Session::new(Project::default());
        let reach = reach_after(&addresses, || {
            opened.run("project.open", &json!({"path": path.display().to_string()})).unwrap();
        });
        assert_eq!(reach, [Reach::Local; 3], "opened from disk");
        let notices = said(&opened);
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(notices[0].starts_with("the project 'own' reads from 2 address(es)"), "{notices:?}");
        assert!(notices[0].contains("localhost:18080") && notices[0].contains("192.168.1.10"), "{notices:?}");
        let mut given = Session::new(Project::default());
        let reach = reach_after(&addresses, || {
            given.run("project.openText", &json!({"text": text})).unwrap();
        });
        assert_eq!(reach, [Reach::Local; 3], "given by the user");
        assert_eq!(said(&given).len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
        // Nothing on this computer or the local network: nothing said.
        let mut plain = Session::new(Project::default());
        plain.run("map.addTiles", &json!({"url": "https://own-disk-plain.example/{z}/{x}/{y}.png", "name": "Theirs"})).unwrap();
        let (text, _) = plain.project.saved_text(None).unwrap();
        let mut opened = Session::new(Project::default());
        opened.run("project.openText", &json!({"text": text})).unwrap();
        assert_eq!(said(&opened), Vec::<String>::new());
    }

    /// A project received from the internet (downloaded from there, or given as text by an
    /// attached agent) reaches only the internet: every address it names is held there, before any is
    /// read, and the ones it names on this computer or the local network are said, once. One
    /// downloaded from a server on the local network reads from there as the user's own.
    #[test]
    fn a_project_received_from_the_internet_reaches_only_the_internet() {
        let (mut s, addresses) = project("received-reach");
        let (text, _) = s.project.saved_text(None).unwrap();
        let mut downloaded = Session::new(Project::default());
        let reach = reach_after(&addresses, || {
            let was = downloaded.receiving(Some(Reach::Public));
            downloaded.run("project.openText", &json!({"text": text, "fileName": "shared.rgis"})).unwrap();
            downloaded.receiving(was);
        });
        assert_eq!(reach, [Reach::Public; 3], "downloaded from the internet");
        let notices = said(&downloaded);
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(notices[0].starts_with("the project 'shared' came from the internet, so the 2 address(es)"), "{notices:?}");
        assert!(notices[0].contains("held to the internet"), "{notices:?}");
        assert_eq!(downloaded.received, None, "put back");
        let mut supplied = Session::new(Project::default());
        let reach = reach_after(&addresses, || {
            as_agent(&mut supplied, "project.openText", &json!({"text": text})).unwrap();
        });
        assert_eq!(reach, [Reach::Public; 3], "given by an agent");
        let mut from_lan = Session::new(Project::default());
        let reach = reach_after(&addresses, || {
            from_lan.receiving(Some(Reach::Local));
            from_lan.run("project.openText", &json!({"text": text})).unwrap();
        });
        assert_eq!(reach, [Reach::Local; 3], "downloaded from the local network");
        assert!(said(&from_lan)[0].contains("reads from 2 address(es)"));
    }

    /// A map file, a layer file and a template given as text by an attached agent hold what they
    /// name to the internet too; the same files given by the user don't.
    #[test]
    fn map_layer_and_template_files_an_agent_gives_reach_only_the_internet() {
        let (mut s, addresses) = project("item-reach");
        let (template, _) = s.project.saved_text(None).unwrap();
        let map = String::from_utf8(crate::project::map_file_bytes(s.map()).unwrap()).unwrap();
        let layer = String::from_utf8(crate::project::layer_file_bytes(s.map().layers.last().unwrap()).unwrap()).unwrap();
        for (id, text, held) in [
            ("map.import", &map, [true, true, true]),
            ("layer.addLayerFile", &layer, [false, true, false]),
            ("project.fromTemplate", &template, [true, true, true]),
        ] {
            let p = json!({"text": text});
            let mut by_user = Session::new(Project::default());
            assert_eq!(reach_after(&addresses, || drop(by_user.run(id, &p).unwrap())), [Reach::Local; 3], "{id} by the user");
            assert_eq!(said(&by_user).len(), 1, "{id}");
            let mut supplied = Session::new(Project::default());
            let reach = reach_after(&addresses, || drop(as_agent(&mut supplied, id, &p).unwrap()));
            assert_eq!(reach, held.map(|h| if h { Reach::Public } else { Reach::Local }), "{id} by an agent");
            let notices = said(&supplied);
            assert_eq!(notices.len(), 1, "{id}: {notices:?}");
            assert!(notices[0].contains("came from the internet"), "{id}: {notices:?}");
        }
    }

    /// A GeoLibre project an attached agent gives holds its background and layers to the internet
    /// before any is read; the same project given by the user doesn't.
    #[test]
    fn a_geolibre_project_an_agent_gives_reaches_only_the_internet() {
        let tiles = "http://localhost:18082/geolibre-reach/{z}/{x}/{y}.png".to_string();
        let style = "http://localhost:18083/geolibre-reach/style.json".to_string();
        let public = "https://geolibre-reach.example/{z}/{x}/{y}.png".to_string();
        let text = json!({
            "version": "0.2.0",
            "name": "Received",
            "mapView": {},
            "basemapStyleUrl": style,
            "layers": [
                {"id": "a", "name": "Mine", "type": "xyz", "source": {"tiles": [tiles]}},
                {"id": "b", "name": "Theirs", "type": "xyz", "source": {"tiles": [public]}},
            ],
        })
        .to_string();
        let addresses = [tiles, style, public];
        let p = json!({"text": text});
        let mut by_user = Session::new(Project::default());
        assert_eq!(reach_after(&addresses, || drop(by_user.run("project.importGeolibre", &p))), [Reach::Local; 3]);
        assert!(said(&by_user).first().is_some_and(|n| n.contains("reads from 2 address(es)")), "{:?}", said(&by_user));
        let mut supplied = Session::new(Project::default());
        let reach = reach_after(&addresses, || drop(as_agent(&mut supplied, "project.importGeolibre", &p)));
        assert_eq!(reach, [Reach::Public; 3]);
        let notices = said(&supplied);
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(notices[0].starts_with("the GeoLibre project 'Received' came from the internet, so the 2 address(es)"), "{notices:?}");
    }

    /// A GeoLibre project downloaded from the internet whose layer the app downloads later (as
    /// the desktop app and the browser do) asks for it as an address a document read from there
    /// named, so adding it when it lands keeps the hold; the user's own asks as before.
    #[test]
    fn a_received_geolibre_layer_downloaded_later_stays_held() {
        let data = "https://geolibre-later.example/roads.geojson".to_string();
        let text = json!({
            "version": "0.2.0",
            "name": "Later",
            "mapView": {},
            "basemapStyleUrl": "",
            "layers": [{"id": "a", "name": "Roads", "type": "geojson", "source": {"url": data}}],
        })
        .to_string();
        let p = json!({"text": text});
        for received in [Some(Reach::Public), None] {
            let mut s = Session::new(Project::default());
            let reach = reach_after(&[data.clone(), data.clone(), data.clone()], || {
                s.receiving(received);
                s.run_leaving_downloads("project.importGeolibre", &p).unwrap();
                s.receiving(None);
            });
            let asked: Vec<&Value> = s.url_requests.iter().map(|r| r.params()).collect();
            assert_eq!(asked.len(), 1, "{asked:?}");
            let from_document = asked[0].get("fromDocument").and_then(Value::as_bool) == Some(true);
            assert_eq!((reach[0], from_document), if received.is_some() { (Reach::Public, true) } else { (Reach::Local, false) });
        }
    }

    /// An address inside a tile template or a wrapper is held by what it is fetched from: a
    /// received project's PMTiles archives on the local network, one read anonymously, are held
    /// there and said.
    #[test]
    fn a_received_projects_wrapped_archives_are_held_too() {
        use crate::render::rustgis_tiles::{pmtiles, s3};
        use rustgis_map::tiles::{TileKind, TileLayer};
        let plain = "http://192.168.1.5:8080/wrapped-reach/a.pmtiles".to_string();
        let anonymous = "http://192.168.1.6/wrapped-reach/b.pmtiles".to_string();
        let mut s = Session::new(Project::default());
        s.project.name = "wrapped".into();
        s.map_mut().tiles.push(TileLayer::new("A", TileKind::PmTiles, pmtiles::template(&plain)));
        s.map_mut().tiles.push(TileLayer::new("B", TileKind::PmTiles, pmtiles::template(&s3::anonymous(&anonymous))));
        // Never read from anywhere, so not counted.
        s.map_mut().tiles.push(TileLayer::new("C", TileKind::Raster, "http://169.254.169.254/wrapped-reach/{z}/{x}/{y}.png"));
        let (text, _) = s.project.saved_text(None).unwrap();
        let mut downloaded = Session::new(Project::default());
        cache::allow_named(&plain);
        cache::allow_named(&anonymous);
        downloaded.receiving(Some(Reach::Public));
        downloaded.run("project.openText", &json!({"text": text})).unwrap();
        downloaded.receiving(None);
        assert_eq!([&plain, &anonymous].map(|a| cache::ceiling_for(a)), [Reach::Public; 2]);
        let notices = said(&downloaded);
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(notices[0].starts_with("the project 'wrapped' came from the internet, so the 2 address(es)"), "{notices:?}");
    }

    /// A project of the user's own, opened after a received one named some of its servers, says
    /// which stay held to the internet rather than that it reads from them; the rest it reads.
    #[test]
    fn your_own_project_says_which_addresses_a_received_one_left_held() {
        let (mut s, addresses) = project("held-before");
        let (received, _) = s.project.saved_text(None).unwrap();
        let mine = "http://localhost:18081/held-before/{z}/{x}/{y}.png";
        s.run("map.addTiles", &json!({"url": mine, "name": "Also mine"})).unwrap();
        let (own, _) = s.project.saved_text(None).unwrap();
        // As a new process has the record.
        for a in addresses.iter().map(String::as_str).chain([mine]) {
            cache::allow_named(a);
        }
        let mut downloaded = Session::new(Project::default());
        downloaded.receiving(Some(Reach::Public));
        downloaded.run("project.openText", &json!({"text": received})).unwrap();
        downloaded.receiving(None);
        // The user's own project next, in the same process.
        let mut opened = Session::new(Project::default());
        opened.run("project.openText", &json!({"text": own})).unwrap();
        assert_eq!([0, 1].map(|i| cache::ceiling_for(&addresses[i])), [Reach::Public; 2]);
        assert_eq!(cache::ceiling_for(mine), Reach::Local);
        let notices = said(&opened);
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(
            notices[0].starts_with("the project 'held-before' reads from 1 address(es) on this computer or the local network: ")
                && notices[0].contains("localhost:18081")
                && notices[0].contains(". 2 more it names there stay held to the internet"),
            "{notices:?}"
        );
    }

    /// A received project reads nothing it names before holding it: its raster read in place
    /// from a server on this computer is never asked for.
    #[test]
    fn a_received_project_reads_nothing_before_it_is_held() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let scene = format!("http://{}/read-order/scene.tif", listener.local_addr().unwrap());
        let mut s = Session::new(Project::default());
        s.map_mut().add_raster("Scene".to_string(), rustgis_map::raster::RasterLayer::unread_in_place(scene.clone()));
        let (text, _) = s.project.saved_text(None).unwrap();
        cache::allow_named(&scene);
        let mut downloaded = Session::new(Project::default());
        downloaded.receiving(Some(Reach::Public));
        downloaded.run("project.openText", &json!({"text": text})).unwrap();
        downloaded.receiving(None);
        downloaded.wait_in_place_reads();
        assert_eq!(cache::ceiling_for(&scene), Reach::Public);
        assert_eq!(
            listener.accept().map(|_| ()).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock,
            "the raster was read before the hold"
        );
    }

    /// A style file and a field package an attached agent gives hold what they name to the
    /// internet too, before reading them reads anything they name.
    #[test]
    fn style_files_and_field_packages_an_agent_gives_are_held_too() {
        use base64::Engine;
        let (mut s, addresses) = project("style-reach");
        let (text, _) = s.project.saved_text(None).unwrap();
        let mut styled = Session::new(Project::default());
        let reach = reach_after(&addresses, || drop(as_agent(&mut styled, "style.import", &json!({"text": text}))));
        assert_eq!(reach, [Reach::Public; 3], "style.import");
        let notices = said(&styled);
        assert!(notices.first().is_some_and(|n| n.starts_with("the style file came from the internet")), "{notices:?}");
        let package = rustgis_data::zip::write(&[("back.rgis".into(), text.into_bytes())]).unwrap();
        let data = base64::engine::general_purpose::STANDARD.encode(package);
        let mut home = Session::new(Project::default());
        let reach = reach_after(&addresses, || drop(as_agent(&mut home, "offline.bringBack", &json!({"data": data}))));
        assert_eq!(reach, [Reach::Public; 3], "offline.bringBack");
    }

    /// A GeoLibre project the app downloads is read within where its download connected: from a
    /// server on the local network, as the user's own, though its name doesn't look local; and
    /// when the app doesn't say where, within the internet.
    #[test]
    fn a_downloaded_geolibre_project_reaches_as_far_as_where_it_was_read() {
        let tiles = "http://localhost:18084/download-reach/{z}/{x}/{y}.png";
        let text = json!({
            "version": "0.2.0",
            "name": "Downloaded",
            "mapView": {},
            "basemapStyleUrl": "",
            "layers": [{"id": "a", "name": "Mine", "type": "xyz", "source": {"tiles": [tiles]}}],
        })
        .to_string();
        for (read_at, reach) in [(Some(Reach::Local), Reach::Local), (None, Reach::Public)] {
            cache::allow_named(tiles);
            let mut s = Session::new(Project::default());
            // As the browser's `project.importGeolibre {"url"}` asks the app for it.
            s.request_import_url(&json!({"url": "http://nas/download-reach.json", "geolibreProject": true})).unwrap();
            let request = s.take_url_requests().pop().unwrap();
            let id = s.download_started_for(&request).unwrap();
            if let Some(read_at) = read_at {
                s.download_read_at(id, read_at);
            }
            assert!(s.download_landed(id, text.clone().into_bytes()));
            let mut params = request.params().clone();
            params["id"] = json!(id);
            s.complete_download(id, &params).unwrap();
            assert_eq!(cache::ceiling_for(tiles), reach, "read at {read_at:?}");
        }
    }
}
