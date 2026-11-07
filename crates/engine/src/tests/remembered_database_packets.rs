use super::*;
use rustgis_data::secrets::{MemorySecrets, SecretStore};
use std::io::{Read, Write};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc,
};

struct Server {
    address: String,
    packets: mpsc::Receiver<Vec<u8>>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Server {
    fn new() -> Self {
        Self::start(false)
    }

    /// When authentication succeeds, fail the first catalogue query instead of the password.
    fn start(authenticate: bool) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let (send, packets) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                let (mut stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(2));
                        continue;
                    }
                    Err(_) => break,
                };
                // macOS accepts can inherit the listener's nonblocking flag.
                stream.set_nonblocking(false).unwrap();
                stream.set_read_timeout(Some(std::time::Duration::from_secs(3))).unwrap();
                let mut size = [0; 4];
                if stream.read_exact(&mut size).is_err() {
                    continue;
                }
                let size = u32::from_be_bytes(size);
                if !(4..=65536).contains(&size) {
                    continue;
                }
                if stream.read_exact(&mut vec![0; (size - 4) as usize]).is_err() {
                    continue;
                }
                if stream.write_all(b"R\0\0\0\x08\0\0\0\x03").is_err() {
                    continue;
                }
                let mut tag = [0];
                let mut size = [0; 4];
                if stream.read_exact(&mut tag).is_err() || tag[0] != b'p' || stream.read_exact(&mut size).is_err() {
                    continue;
                }
                let size = u32::from_be_bytes(size);
                if !(4..=65536).contains(&size) {
                    continue;
                }
                let mut password = vec![0; (size - 4) as usize];
                if stream.read_exact(&mut password).is_err() {
                    continue;
                }
                let echoed = String::from_utf8_lossy(&password).trim_end_matches('\0').to_string();
                send.send(password).unwrap();
                if authenticate {
                    // AuthenticationOk and ReadyForQuery: the client's session setup is a
                    // simple Query containing three SET statements.
                    if stream.write_all(b"R\0\0\0\x08\0\0\0\0Z\0\0\0\x05I").is_err() {
                        continue;
                    }
                    let mut tag = [0];
                    let mut size = [0; 4];
                    if stream.read_exact(&mut tag).is_err() || tag[0] != b'Q' || stream.read_exact(&mut size).is_err() {
                        continue;
                    }
                    let size = u32::from_be_bytes(size);
                    if !(4..=65536).contains(&size) || stream.read_exact(&mut vec![0; (size - 4) as usize]).is_err() {
                        continue;
                    }
                    for _ in 0..3 {
                        stream.write_all(b"C\0\0\0\x08SET\0").unwrap();
                    }
                    stream.write_all(b"Z\0\0\0\x05I").unwrap();
                    // The catalogue lookup uses an extended query. Consume through Sync so
                    // the server's non-login error occurs after successful authentication.
                    loop {
                        let mut size = [0; 4];
                        if stream.read_exact(&mut tag).is_err() || stream.read_exact(&mut size).is_err() {
                            break;
                        }
                        let size = u32::from_be_bytes(size);
                        if !(4..=65536).contains(&size) || stream.read_exact(&mut vec![0; (size - 4) as usize]).is_err() {
                            break;
                        }
                        if tag[0] == b'S' {
                            break;
                        }
                    }
                }
                let body = if authenticate {
                    format!("SERROR\0VERROR\0C42P01\0Mfixture listing failed {echoed}\0\0")
                } else {
                    format!("SFATAL\0VFATAL\0C28P01\0Mrefused {echoed}\0\0")
                };
                let _ = stream.write_all(b"E");
                let _ = stream.write_all(&((body.len() + 4) as u32).to_be_bytes());
                let _ = stream.write_all(body.as_bytes());
                if authenticate {
                    let _ = stream.write_all(b"Z\0\0\0\x05I");
                    // A catalogue error is nonfatal: keep the connection alive until the
                    // client closes it, rather than racing its error response with TCP EOF.
                    let mut terminate = [0; 5];
                    let _ = stream.read_exact(&mut terminate);
                }
            }
        });
        Self {
            address: format!("postgres:host=127.0.0.1 port={port} user=packet_user dbname=gis sslmode=disable"),
            packets,
            stop,
            worker: Some(worker),
        }
    }

    #[track_caller]
    fn take(&self) -> Vec<u8> {
        self.packets.recv_timeout(std::time::Duration::from_secs(3)).unwrap()
    }
    fn empty(&self) {
        assert!(self.packets.try_recv().is_err(), "unexpected password packet");
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}
fn app(store: Arc<MemorySecrets>, prefs: &str) -> Session {
    let mut s = Session::new(Project::default());
    s.database_logins.set_store(Some(store));
    s.connections = crate::connections::Connections::from_json(prefs);
    s
}
fn project(address: &str) -> String {
    project_layers(address, 2)
}

/// A project whose first `n` layers read `address`.
fn project_layers(address: &str, n: usize) -> String {
    let mut p = world().project;
    for layer in p.map_mut().layers.iter_mut().take(n) {
        layer.source = Some(serde_json::from_value(json!({"postgis":{"database":address,"table":"roads","family":"Lines"}})).unwrap());
    }
    serde_json::to_string(&p).unwrap()
}
fn remember(s: &mut Session, name: &str, address: &str, password: &str) -> String {
    let id =
        s.run("connection.new", &json!({"kind":"postgres","name":name,"address":address})).unwrap()["id"].as_str().unwrap().to_string();
    assert_eq!(s.run("connection.login", &json!({"id":id,"password":password,"remember":true})).unwrap()["passwordSaved"], true);
    id
}

/// A store that counts its loads, so a test can say what a login asked of the keychain.
struct Counting {
    inner: Arc<MemorySecrets>,
    loads: AtomicUsize,
}
impl Counting {
    fn over(inner: Arc<MemorySecrets>) -> Arc<Self> {
        Arc::new(Self { inner, loads: AtomicUsize::new(0) })
    }
    fn loads(&self) -> usize {
        self.loads.load(Ordering::SeqCst)
    }
}
impl SecretStore for Counting {
    fn load(&self, account: &str) -> Result<Option<rustgis_data::secrets::Secret>, rustgis_data::secrets::SecretError> {
        self.loads.fetch_add(1, Ordering::SeqCst);
        self.inner.load(account)
    }
    fn save(&self, account: &str, value: &rustgis_data::secrets::Secret) -> Result<(), rustgis_data::secrets::SecretError> {
        self.inner.save(account, value)
    }
    fn delete(&self, account: &str) -> Result<(), rustgis_data::secrets::SecretError> {
        self.inner.delete(account)
    }
}

/// A session over any store, holding the saved connections `prefs` names, as the command line and
/// Python open one with `--use-saved-secrets` / `use_saved_secrets`.
fn session_with(store: Arc<dyn SecretStore>, prefs: &str) -> Session {
    let mut s = Session::new(Project::default());
    let prefs = serde_json::Map::from_iter([(crate::connections::PREF.to_string(), Value::String(prefs.to_string()))]);
    crate::prefs::use_saved_secrets(&mut s, &prefs, store);
    s
}

/// Saves a connection of any database kind with its password, as `remember` does for PostgreSQL.
fn remember_kind(s: &mut Session, kind: &str, address: &str, password: &str) -> String {
    let id = s.run("connection.new", &json!({"kind":kind,"name":kind,"address":address})).unwrap()["id"].as_str().unwrap().to_string();
    assert_eq!(s.run("connection.login", &json!({"id":id,"password":password,"remember":true})).unwrap()["passwordSaved"], true);
    id
}

/// A project whose first layer reads `address` through `origin` (`postgis`, `mysql`, `sqlserver`).
fn database_project(origin: &str, address: &str) -> String {
    let mut p = world().project;
    for layer in p.map_mut().layers.iter_mut().take(1) {
        layer.source = Some(serde_json::from_value(json!({(origin):{"database":address,"table":"roads","family":"Lines"}})).unwrap());
    }
    serde_json::to_string(&p).unwrap()
}

/// The three database kinds: the kind, the layer origin that names it, and an address nothing
/// listens at.
const KINDS: [(&str, &str, &str); 3] = [
    ("postgres", "postgis", "postgres:host=127.0.0.1 port=1 user=kinds dbname=gis sslmode=disable"),
    ("mysql", "mysql", "mysql://kinds@127.0.0.1:1/gis?ssl-mode=DISABLED"),
    ("sqlserver", "sqlserver", "mssql://kinds@127.0.0.1:1/gis?encrypt=false"),
];

// Driver passwords are process-global. Isolate packet scenarios from unrelated full-suite
// project resets (and ambient-password precedence tests) without mutating the parent's environment.
fn isolated(name: &str) -> bool {
    isolated_with(name, &[])
}

/// The same isolation, with extra environment for the child (an ambient password, say).
fn isolated_with(name: &str, extra: &[(&str, &str)]) -> bool {
    if std::env::var("RUSTGIS_PACKET_SCENARIO").as_deref() == Ok(name) {
        return false;
    }
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", &format!("tests::remembered_database_packets::{name}"), "--test-threads=1"])
        .env("RUSTGIS_PACKET_SCENARIO", name)
        .env_remove("PGPASSWORD")
        .env_remove("PGPASSFILE")
        .env_remove("MYSQL_PWD")
        .env_remove("SQLCMDPASSWORD");
    for (key, value) in extra {
        command.env(key, value);
    }
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut summaries = stdout.lines().filter(|line| line.starts_with("test result:"));
    let ran_one = summaries.next().is_some_and(|line| line.starts_with("test result: ok. 1 passed; 0 failed; 0 ignored;"))
        && summaries.next().is_none();
    assert!(
        output.status.success() && ran_one,
        "isolated database scenario failed or did not run exactly one test:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    true
}
#[test]
fn remembered_database_unique_packet_refusal_and_replacement() {
    if isolated("remembered_database_unique_packet_refusal_and_replacement") {
        return;
    }
    let server = Server::new();
    let store = Arc::new(MemorySecrets::new());
    let mut first = app(store.clone(), "{}");
    let id = remember(&mut first, "One", &server.address, "saved-packet-sentinel");
    let prefs = first.connections.to_json();
    let mut s = app(store.clone(), &prefs);
    let text = project(&server.address);
    s.run("project.openText", &json!({"text":text})).unwrap();
    assert!(s.map().layers.iter().take(2).all(|l| l.source.as_ref().is_some_and(|src| src.login)), "{:?}", s.notices);
    assert_eq!(server.take(), b"saved-packet-sentinel\0");
    server.empty();
    assert!(s.map().layers.iter().take(2).all(|l| l.source.as_ref().unwrap().login));
    assert_eq!(s.run("connection.list", &Value::Null).unwrap()["loginRequests"][0]["reason"], "password");
    assert!(!s.notices.iter().any(|n| n.text.contains("saved-packet-sentinel")));
    assert!(s.map().layers.iter().take(2).all(|l| !l.source.as_ref().unwrap().error.as_ref().unwrap().contains("saved-packet-sentinel")));
    let saved = store.load(&format!("connection/{id}")).unwrap();
    s.run("project.openText", &json!({"text":text})).unwrap();
    server.empty();
    s.run("connection.login", &json!({"id":id,"password":"unchecked-new"})).unwrap();
    // One attempt per database: the refused password is not sent to the group's other layers.
    assert_eq!(server.take(), b"unchecked-new\0");
    server.empty();
    assert_eq!(store.load(&format!("connection/{id}")).unwrap(), saved);
    assert!(s.database_logins.is_rejected(&id));
    // A refused password is not "available for this session", and the store keeps the old value.
    let refused = s.run("connection.login", &json!({"id":id,"password":"refused-with-remember","remember":true})).unwrap();
    assert_eq!(refused["passwordSaved"], false, "{refused}");
    assert!(refused.get("warning").is_none(), "a refused password must not be called available: {refused}");
    assert_eq!(server.take(), b"refused-with-remember\0");
    server.empty();
    assert_eq!(store.load(&format!("connection/{id}")).unwrap(), saved);
    assert!(s.database_logins.is_rejected(&id));
    s.run("project.new", &Value::Null).unwrap();
    assert_eq!(s.run("connection.login", &json!({"id":id,"password":"checked-new","remember":true})).unwrap()["passwordSaved"], true);
    assert!(!s.database_logins.is_rejected(&id));
}
#[test]
fn remembered_database_ambiguity_requires_exact_id_packet() {
    if isolated("remembered_database_ambiguity_requires_exact_id_packet") {
        return;
    }
    let server = Server::new();
    let store = Arc::new(MemorySecrets::new());
    let mut first = app(store.clone(), "{}");
    let one = remember(&mut first, "One", &server.address, "first-sentinel");
    let two = remember(&mut first, "Two", &server.address, "second-sentinel");
    let mut s = app(store.clone(), &first.connections.to_json());
    let text = project(&server.address);
    s.run("project.openText", &json!({"text":text})).unwrap();
    server.empty();
    let listing = s.run("connection.list", &Value::Null).unwrap();
    assert_eq!(listing["loginRequests"][0]["reason"], "choose");
    assert_eq!(listing["loginRequests"][0]["connections"], json!([one, two]));
    assert!(s.connections.for_layer(&s.map().layers[0]).is_none());
    s.run("connection.login", &json!({"id":two})).unwrap();
    assert_eq!(server.take(), b"second-sentinel\0");
    server.empty();
    // Fresh sessions still see ambiguity; no prior automatic activation authorizes a choice.
    let mut fresh = app(store.clone(), &first.connections.to_json());
    fresh.run("project.openText", &json!({"text":text})).unwrap();
    server.empty();
    assert_eq!(fresh.run("connection.list", &Value::Null).unwrap()["loginRequests"][0]["reason"], "choose");
    for id in [&one, &two] {
        fresh.run("connection.forgetPassword", &json!({"id":id})).unwrap();
    }
    let mut forgotten = app(store, &fresh.connections.to_json());
    forgotten.run("project.openText", &json!({"text":text})).unwrap();
    server.empty();
}

#[test]
fn remembered_database_exact_address_and_explicit_precedence_packets() {
    if isolated("remembered_database_exact_address_and_explicit_precedence_packets") {
        return;
    }
    let server = Server::new();
    let store = Arc::new(MemorySecrets::new());
    let mut seed = app(store.clone(), "{}");
    let id = remember(&mut seed, "Exact", &server.address, "exact-saved-sentinel");
    let prefs = seed.connections.to_json();
    let port = server.address.split_whitespace().find_map(|s| s.strip_prefix("port=")).unwrap();
    for address in [
        format!("postgresql://packet_user@127.0.0.1:{port}/gis?sslmode=disable"),
        server.address.replace("user=packet_user dbname=gis", "dbname=gis user=packet_user"),
        server.address.replace("user=packet_user", "user=other"),
    ] {
        let mut s = app(store.clone(), &prefs);
        s.run("project.openText", &json!({"text":project(&address)})).unwrap();
        server.empty();
    }
    let mut explicit = app(store, &prefs);
    let (_, key) = crate::connections::database_key(&server.address).unwrap();
    rustgis_data::source::duckdb::remember_password(&key, "explicit-sentinel");
    explicit.run("project.openText", &json!({"text":project(&server.address)})).unwrap();
    assert_eq!(server.take(), b"explicit-sentinel\0");
    assert_eq!(server.take(), b"explicit-sentinel\0");
    server.empty();
    // An explicit login without a password must not replace the address's own password with the
    // saved one either: the address's wins, as it does when the project opens.
    explicit.run("connection.login", &json!({"id":id})).unwrap();
    assert_eq!(server.take(), b"explicit-sentinel\0");

    server.empty();
}

/// With the saved-secrets opt-in, a saved connection's password from the environment
/// (`PGPASSWORD`) beats the keychain's, for an explicit command and for opening a project, and the
/// keychain is never asked; the desktop (no opt-in) keeps sending the saved one. A password given
/// to a command, or typed for the address, beats the environment; the environment reaches no
/// address the saved connection doesn't name exactly, nor a connection kept without a password.
/// The ambient value is set in a child process: it is process-global, and the rest of the suite
/// must not see it.
#[test]
fn remembered_database_environment_password_wins_over_saved_for_postgres() {
    let name = "remembered_database_environment_password_wins_over_saved_for_postgres";
    if isolated_with(name, &[("PGPASSWORD", "environment-sentinel")]) {
        return;
    }
    let server = Server::new();
    let seeded = Arc::new(MemorySecrets::new());
    let mut seed = app(seeded.clone(), "{}");
    let id = remember(&mut seed, "Env", &server.address, "saved-sentinel");
    let prefs = seed.connections.to_json();
    // Seeding activated the typed password for the driver; the sessions below start without it.
    rustgis_data::source::duckdb::forget_connection_passwords();
    // The desktop (a store without the opt-in) never reads the environment: the saved one is sent.
    let mut desktop = app(seeded.clone(), &prefs);
    assert!(desktop.run("connection.test", &json!({"id":id})).is_err());
    assert_eq!(server.take(), b"saved-sentinel\0");
    server.empty();
    rustgis_data::source::duckdb::forget_connection_passwords();
    let store = Counting::over(seeded);
    let (_, key) = crate::connections::database_key(&server.address).unwrap();

    // The environment's password is for the saved address only.
    let mut s = session_with(store.clone(), &prefs);
    s.run("project.openText", &json!({"text":project(&server.address.replace("user=packet_user", "user=other"))})).unwrap();
    server.empty();
    let mut unsaved = session_with(store.clone(), "{}");
    unsaved.run("connection.new", &json!({"kind":"postgres","name":"Plain","address":server.address})).unwrap();
    unsaved.run("project.openText", &json!({"text":project(&server.address)})).unwrap();
    server.empty();
    assert_eq!(store.loads(), 0);

    // An explicit command: the environment's password is sent, not the saved one, and the
    // keychain isn't read.
    let mut s = session_with(store.clone(), &prefs);
    assert!(s.run("connection.test", &json!({"id":id})).is_err(), "the refusing server must fail the listing");
    assert_eq!(server.take(), b"environment-sentinel\0");
    server.empty();
    assert_eq!(store.loads(), 0);

    // Opening a project: the same.
    let mut s = session_with(store.clone(), &prefs);
    s.run("project.openText", &json!({"text":project(&server.address)})).unwrap();
    assert_eq!(server.take(), b"environment-sentinel\0");
    server.empty();
    assert_eq!(store.loads(), 0);
    // A refused environment password isn't sent again, nor replaced by the saved one unasked.
    assert!(s.database_logins.is_rejected(&id));
    assert_eq!(s.run("connection.login", &json!({"id":id})).unwrap_err(), "password is required");
    server.empty();
    assert_eq!(store.loads(), 0);
    // A password given to the command wins over the environment's.
    s.run("connection.login", &json!({"id":id,"password":"typed-sentinel"})).unwrap();
    assert_eq!(server.take(), b"typed-sentinel\0");
    server.empty();

    // A password typed for the address wins over the environment's as well.
    rustgis_data::source::duckdb::remember_password(&key, "address-sentinel");
    let mut s = session_with(store.clone(), &prefs);
    assert!(s.run("connection.test", &json!({"id":id})).is_err());
    assert_eq!(server.take(), b"address-sentinel\0");
    server.empty();
    assert_eq!(store.loads(), 0);
}

/// Every database kind takes the saved connection's password from the environment of its own
/// client (`PGPASSWORD`, `MYSQL_PWD`, `SQLCMDPASSWORD`) before the keychain, for a login and for a
/// project, and then never asks the keychain, which here fails if it is asked.
#[test]
fn remembered_database_environment_password_wins_for_every_database_kind() {
    let name = "remembered_database_environment_password_wins_for_every_database_kind";
    if isolated_with(name, &[("PGPASSWORD", "env-pg"), ("MYSQL_PWD", "env-my"), ("SQLCMDPASSWORD", "env-ms")]) {
        return;
    }
    for (kind, origin, address) in KINDS {
        let expected = match kind {
            "postgres" => "env-pg",
            "mysql" => "env-my",
            _ => "env-ms",
        };
        let (_, key) = crate::connections::database_key(address).unwrap();
        let mut seed = app(Arc::new(MemorySecrets::new()), "{}");
        remember_kind(&mut seed, kind, address, &format!("saved-{kind}"));
        let prefs = seed.connections.to_json();
        rustgis_data::source::duckdb::forget_connection_passwords();
        let unreadable = Arc::new(MemorySecrets::failing(rustgis_data::secrets::SecretError::Unavailable("no keychain here".into())));
        let store = Counting::over(unreadable);

        let mut s = session_with(store.clone(), &prefs);
        let listed = s.run("connection.list", &Value::Null).unwrap();
        let id = listed["connections"][0]["id"].as_str().unwrap().to_string();
        let shown = s.run("connection.login", &json!({"id":id})).unwrap_or_else(|e| panic!("{kind}: {e}"));
        assert_eq!(shown["loggedIn"], true, "{kind}");
        assert_eq!(s.database_logins.known_password(&id).unwrap().expose(), expected, "{kind}");
        assert!(rustgis_data::source::duckdb::has_connection_password(&key, &id), "{kind}");
        assert_eq!(store.loads(), 0, "{kind}: a login the environment satisfies doesn't ask the keychain");

        rustgis_data::source::duckdb::forget_connection_passwords();
        let mut s = session_with(store.clone(), &prefs);
        s.run("project.openText", &json!({"text":database_project(origin, address)})).unwrap();
        assert_eq!(s.database_logins.known_password(&id).unwrap().expose(), expected, "{kind}");
        assert!(rustgis_data::source::duckdb::has_connection_password(&key, &id), "{kind}");
        assert!(s.database_logins.activations().contains(&(key.clone(), id.clone())), "{kind}");
        assert_eq!(store.loads(), 0, "{kind}: opening a project the environment satisfies doesn't ask the keychain");
        assert!(!s.notices.iter().any(|n| n.text.contains(expected)), "{kind}");

        // A password given to the command is the one used, and the environment's isn't.
        let shown = s.run("connection.login", &json!({"id":id,"password":format!("typed-{kind}")})).unwrap();
        assert_eq!(shown["loggedIn"], true, "{kind}");
        assert_eq!(s.database_logins.known_password(&id).unwrap().expose(), format!("typed-{kind}"), "{kind}");
        assert_eq!(store.loads(), 0, "{kind}");
        rustgis_data::source::duckdb::forget_connection_passwords();
    }
}

/// Without an environment password the keychain's serves, read once, for every database kind: the
/// environment's precedence takes nothing from the saved passwords.
#[test]
fn remembered_database_saved_password_serves_when_the_environment_has_none() {
    let name = "remembered_database_saved_password_serves_when_the_environment_has_none";
    if isolated(name) {
        return;
    }
    for (kind, origin, address) in KINDS {
        let (_, key) = crate::connections::database_key(address).unwrap();
        let seeded = Arc::new(MemorySecrets::new());
        let mut seed = app(seeded.clone(), "{}");
        let id = remember_kind(&mut seed, kind, address, &format!("saved-{kind}"));
        let prefs = seed.connections.to_json();
        rustgis_data::source::duckdb::forget_connection_passwords();
        let store = Counting::over(seeded);

        let mut s = session_with(store.clone(), &prefs);
        assert_eq!(s.run("connection.login", &json!({"id":id})).unwrap()["loggedIn"], true, "{kind}");
        assert_eq!(s.database_logins.known_password(&id).unwrap().expose(), format!("saved-{kind}"), "{kind}");
        assert_eq!(store.loads(), 1, "{kind}");

        rustgis_data::source::duckdb::forget_connection_passwords();
        let mut s = session_with(store.clone(), &prefs);
        s.run("project.openText", &json!({"text":database_project(origin, address)})).unwrap();
        assert_eq!(s.database_logins.known_password(&id).unwrap().expose(), format!("saved-{kind}"), "{kind}");
        assert!(rustgis_data::source::duckdb::has_connection_password(&key, &id), "{kind}");
        assert_eq!(store.loads(), 2, "{kind}");
        rustgis_data::source::duckdb::forget_connection_passwords();
    }
}

#[test]
fn remembered_database_forget_preserves_active_owner_and_explicit_input() {
    if isolated("remembered_database_forget_preserves_active_owner_and_explicit_input") {
        return;
    }
    use rustgis_data::source::{duckdb, postgis};
    let server = Server::new();
    let store = Arc::new(MemorySecrets::new());
    let mut s = app(store.clone(), "{}");
    let a = remember(&mut s, "A", &server.address, "owner-a");
    let b = remember(&mut s, "B", &server.address, "owner-b");
    let (_, key) = crate::connections::database_key(&server.address).unwrap();
    s.run("connection.forgetPassword", &json!({"id":a})).unwrap();
    assert_eq!(s.database_logins.selection("postgres", &key), Some(b.as_str()));
    assert!(postgis::connect(&key, false).is_err());
    assert_eq!(server.take(), b"owner-b\0");
    assert_eq!(s.database_logins.session_password(&a).unwrap().expose(), "owner-a");
    assert!(store.load(&format!("connection/{a}")).unwrap().is_none());

    duckdb::remember_password(&key, "address-typed");
    s.run("connection.forgetPassword", &json!({"id":a})).unwrap();
    assert!(duckdb::has_explicit_password(&key));
    assert!(postgis::connect(&key, false).is_err());
    assert_eq!(server.take(), b"address-typed\0");

    s.run("connection.login", &json!({"id":b,"password":"owner-b"})).unwrap();
    // A loaded/active cache value can differ from the ID's latest explicit session value.
    // Forget must restore the session value, not preserve the old owned slot by accident.
    s.database_logins.set_session_password(&b, "session-b-new");
    s.run("connection.forgetPassword", &json!({"id":b})).unwrap();
    assert!(postgis::connect(&key, false).is_err());
    assert_eq!(server.take(), b"session-b-new\0");
    assert_eq!(s.database_logins.selection("postgres", &key), Some(b.as_str()));
    server.empty();
}

#[test]
fn remembered_database_forget_saved_only_clears_its_activation() {
    if isolated("remembered_database_forget_saved_only_clears_its_activation") {
        return;
    }
    let server = Server::new();
    let store = Arc::new(MemorySecrets::new());
    let mut seed = app(store.clone(), "{}");
    let id = remember(&mut seed, "Saved", &server.address, "saved-only");
    let mut s = app(store, &seed.connections.to_json());
    let (_, key) = crate::connections::database_key(&server.address).unwrap();
    s.run("connection.login", &json!({"id":id})).unwrap();
    assert!(!s.database_logins.has_session_password(&id));
    s.run("connection.forgetPassword", &json!({"id":id})).unwrap();
    assert!(!rustgis_data::source::duckdb::has_password(&key));
    assert!(!s.database_logins.activations().iter().any(|(_, active)| active == &id));
    server.empty();
}

#[test]
fn remembered_database_test_refusal_restores_prior_session_and_active_password() {
    if isolated("remembered_database_test_refusal_restores_prior_session_and_active_password") {
        return;
    }
    use rustgis_data::source::{duckdb, postgis};
    let server = Server::new();
    let store = Arc::new(MemorySecrets::new());
    let mut s = app(store.clone(), "{}");
    let a = remember(&mut s, "A", &server.address, "prior-a");
    let b = remember(&mut s, "B", &server.address, "prior-b");
    let (_, key) = crate::connections::database_key(&server.address).unwrap();
    let saved = store.load(&format!("connection/{a}")).unwrap();
    for (active, expected) in [(Some(b.as_str()), "prior-b"), (Some(a.as_str()), "prior-a"), (None, "prior-address")] {
        if let Some(id) = active {
            s.run("connection.login", &json!({"id":id,"password":expected})).unwrap();
        } else {
            duckdb::remember_password(&key, expected);
        }
        let selection = s.database_logins.selection("postgres", &key).map(str::to_string);
        let activations = s.database_logins.activations();
        let error = s.run("connection.test", &json!({"id":a,"password":"test-refused","remember":true})).unwrap_err();
        assert!(!error.contains("test-refused"));
        assert_eq!(server.take(), b"test-refused\0");
        assert_eq!(s.database_logins.session_password(&a).unwrap().expose(), "prior-a");
        assert_eq!(s.database_logins.selection("postgres", &key), selection.as_deref());
        assert_eq!(s.database_logins.activations(), activations);
        assert_eq!(duckdb::has_explicit_password(&key), active.is_none());
        assert!(postgis::connect(&key, false).is_err());
        let mut packet = expected.as_bytes().to_vec();
        packet.push(0);
        assert_eq!(server.take(), packet);
        assert_eq!(store.load(&format!("connection/{a}")).unwrap(), saved);
        assert!(!s.database_logins.is_rejected(&a));
    }
    duckdb::set_connection_password(&key, &a, Some("cleanup-owned"));
    duckdb::forget_connection_password(&a);
    let mut fresh = app(Arc::new(MemorySecrets::new()), "{}");
    fresh.run("connection.new", &json!({"kind":"postgres","name":"Empty","address":server.address})).unwrap();
    assert!(fresh.run("connection.test", &json!({"id":"c1","password":"test-refused"})).is_err());
    assert_eq!(server.take(), b"test-refused\0");
    assert!(!fresh.database_logins.has_session_password("c1"));
    assert!(fresh.database_logins.selection("postgres", &key).is_none());
    assert!(!duckdb::has_password(&key));
    server.empty();
}

#[test]
fn remembered_database_test_listing_failure_keeps_authenticated_session() {
    if isolated("remembered_database_test_listing_failure_keeps_authenticated_session") {
        return;
    }
    let server = Server::start(true);
    let store = Arc::new(MemorySecrets::new());
    let mut s = app(store.clone(), "{}");
    let a = remember(&mut s, "A", &server.address, "prior-a");
    let b = remember(&mut s, "B", &server.address, "prior-b");
    let error = s.run("connection.test", &json!({"id":a,"password":"accepted-new","remember":true})).unwrap_err();
    assert!(error.contains("fixture listing failed"), "{error}");
    assert!(!error.contains("accepted-new"));
    assert_eq!(server.take(), b"accepted-new\0");
    let (_, key) = crate::connections::database_key(&server.address).unwrap();
    assert_eq!(s.database_logins.session_password(&a).unwrap().expose(), "accepted-new");
    assert_eq!(s.database_logins.selection("postgres", &key), Some(a.as_str()));
    assert!(s.database_logins.activations().contains(&(key.clone(), a.clone())));
    assert!(s.database_logins.requests(s.opened).next().is_none());
    let envelope: Value = serde_json::from_str(store.load(&format!("connection/{a}")).unwrap().unwrap().expose()).unwrap();
    assert_eq!(envelope["password"], "prior-a");
    assert_eq!(s.database_logins.session_password(&b).unwrap().expose(), "prior-b");
    server.empty();
}

#[test]
fn remembered_database_incremental_change_resolves_only_its_database() {
    if isolated("remembered_database_incremental_change_resolves_only_its_database") {
        return;
    }
    let server = Server::start(true);
    let store = Arc::new(MemorySecrets::new());
    let mut s = app(store, "{}");
    let id = remember(&mut s, "Waiting", &server.address, "waiting-sentinel");
    // One layer waits for this database. Opening the project connects once, and the catalogue
    // read fails (not a refusal), so the group stays unopened for a later resolution.
    s.run("project.openText", &json!({"text":project_layers(&server.address, 1)})).unwrap();
    assert_eq!(server.take(), b"waiting-sentinel\0");
    // A change to another connection must not connect to this database again.
    s.run(
        "connection.new",
        &json!({"kind":"postgres","name":"Far","address":"postgres:host=127.0.0.1 port=1 user=far dbname=gis sslmode=disable"}),
    )
    .unwrap();
    server.empty();
    // A change to this database's own definition still resolves its layers.
    let same = s.run("connection.new", &json!({"kind":"postgres","name":"Same","address":server.address})).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(server.take(), b"waiting-sentinel\0");
    s.run("connection.remove", &json!({"id":same})).unwrap();
    assert_eq!(server.take(), b"waiting-sentinel\0");
    server.empty();
    assert!(s.connections.find(&id).is_some());
}

#[test]
fn remembered_database_refused_login_does_not_keep_the_typed_password() {
    if isolated("remembered_database_refused_login_does_not_keep_the_typed_password") {
        return;
    }
    let server = Server::new();
    let store = Arc::new(MemorySecrets::new());
    let mut seed = app(store.clone(), "{}");
    let id = remember(&mut seed, "Refused", &server.address, "saved-a");
    let mut s = app(store.clone(), &seed.connections.to_json());
    // The saved password is refused, so the layer waits and the login below reaches the server.
    s.run("project.openText", &json!({"text":project_layers(&server.address, 1)})).unwrap();
    assert_eq!(server.take(), b"saved-a\0");
    assert!(s.database_logins.is_rejected(&id));
    let (_, key) = crate::connections::database_key(&server.address).unwrap();
    let selection = s.database_logins.selection("postgres", &key).map(str::to_string);
    let activations = s.database_logins.activations();
    // A password typed for the login is sent, but a refusal does not make it this session's.
    let result = s.run("connection.login", &json!({"id":id,"password":"typed-wrong"})).unwrap();
    assert_eq!(result["failed"], 1, "{result}");
    assert_eq!(server.take(), b"typed-wrong\0");
    assert!(!s.database_logins.has_session_password(&id));
    assert!(!rustgis_data::source::duckdb::has_password(&key));
    assert_eq!(s.database_logins.selection("postgres", &key), selection.as_deref());
    assert_eq!(s.database_logins.activations(), activations);
    assert!(store.load(&format!("connection/{id}")).unwrap().is_some());
    server.empty();
}

#[test]
fn remembered_database_passwordless_login_keeps_another_connection_active() {
    if isolated("remembered_database_passwordless_login_keeps_another_connection_active") {
        return;
    }
    use rustgis_data::source::duckdb;
    let server = Server::new();
    let store = Arc::new(MemorySecrets::new());
    let mut s = app(store, "{}");
    let a = remember(&mut s, "A", &server.address, "owner-a");
    let b = s.run("connection.new", &json!({"kind":"postgres","name":"B","address":server.address})).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (_, key) = crate::connections::database_key(&server.address).unwrap();
    s.run("connection.login", &json!({"id":a})).unwrap();
    assert!(duckdb::has_connection_password(&key, &a));
    // A login for a connection with no password of its own fails without taking the address's
    // activation away from the connection that is using it.
    let error = s.run("connection.login", &json!({"id":b})).unwrap_err();
    assert!(error.contains("password is required"), "{error}");
    assert!(duckdb::has_connection_password(&key, &a));
    assert!(s.database_logins.activations().contains(&(key.clone(), a.clone())));
    server.empty();
}

#[test]
fn remembered_database_refused_login_restores_previous_driver_owner() {
    if isolated("remembered_database_refused_login_restores_previous_driver_owner") {
        return;
    }
    use rustgis_data::source::{duckdb, postgis};
    let server = Server::new();
    let store = Arc::new(MemorySecrets::new());
    let mut s = app(store.clone(), "{}");
    let a = remember(&mut s, "A", &server.address, "prior-a");
    let b = remember(&mut s, "B", &server.address, "prior-b");
    // Stage two waiting layers without letting a project open change the active credential.
    s.project = serde_json::from_str(&project(&server.address)).unwrap();
    for layer in s.map_mut().layers.iter_mut().take(2) {
        layer.source.as_mut().unwrap().login = true;
    }
    let (_, key) = crate::connections::database_key(&server.address).unwrap();
    let saved = store.load(&format!("connection/{a}")).unwrap();
    for (owner, expected) in [(Some(b.as_str()), "prior-b"), (Some(a.as_str()), "prior-a"), (None, "prior-address")] {
        if let Some(owner) = owner {
            duckdb::set_connection_password(&key, owner, Some(expected));
            s.database_logins.select("postgres", &key, owner);
            s.database_logins.note_activation(&key, owner);
        } else {
            duckdb::remember_password(&key, expected);
        }
        let selection = s.database_logins.selection("postgres", &key).map(str::to_string);
        let activations = s.database_logins.activations();
        let result = s.run("connection.login", &json!({"id":a,"password":"login-refused","remember":true})).unwrap();
        assert_eq!(result["failed"], 2);
        assert_eq!(result["passwordSaved"], false);
        assert_eq!(server.take(), b"login-refused\0");
        server.empty();
        assert_eq!(s.database_logins.session_password(&a).unwrap().expose(), "prior-a");
        assert_eq!(s.database_logins.selection("postgres", &key), selection.as_deref());
        assert_eq!(s.database_logins.activations(), activations);
        assert_eq!(duckdb::has_explicit_password(&key), owner.is_none());
        if let Some(owner) = owner {
            assert!(duckdb::has_connection_password(&key, owner));
        }
        let listing = s.run("connection.list", &Value::Null).unwrap();
        assert_eq!(listing["loginRequests"][0]["reason"], "password");
        assert_eq!(listing["loginRequests"][0]["connections"], json!([a]));
        assert!(postgis::connect(&key, false).is_err());
        let mut packet = expected.as_bytes().to_vec();
        packet.push(0);
        assert_eq!(server.take(), packet);
        assert_eq!(store.load(&format!("connection/{a}")).unwrap(), saved);
    }
    server.empty();
}

#[test]
fn remembered_database_cached_session_refusal_evicts_reused_password() {
    if isolated("remembered_database_cached_session_refusal_evicts_reused_password") {
        return;
    }
    use rustgis_data::source::duckdb;
    let server = Server::new();
    for command in ["connection.login", "connection.test", "connection.browse", "project.openText"] {
        let store = Arc::new(MemorySecrets::new());
        let mut s = app(store.clone(), "{}");
        let id = s.run("connection.new", &json!({"kind":"postgres","name":"Session","address":server.address})).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        s.run("connection.login", &json!({"id":id,"password":"cached-refused"})).unwrap();
        let text = project(&server.address);
        s.project = serde_json::from_str(&text).unwrap();
        for layer in s.map_mut().layers.iter_mut().take(2) {
            layer.source.as_mut().unwrap().login = true;
        }
        let params = if command == "project.openText" { json!({"text":text}) } else { json!({"id":id}) };
        let result = s.run(command, &params);
        match command {
            "connection.login" => assert_eq!(result.unwrap()["failed"], 2),
            "project.openText" => assert!(result.is_ok()),
            _ => assert!(!result.unwrap_err().contains("cached-refused")),
        }
        assert_eq!(server.take(), b"cached-refused\0", "{command}");
        server.empty();
        let (_, key) = crate::connections::database_key(&server.address).unwrap();
        assert!(!s.database_logins.has_session_password(&id), "{command}");
        assert!(!duckdb::has_password(&key), "{command}");
        assert!(!s.database_logins.activations().iter().any(|(active, _)| active == &key), "{command}");
        assert!(!s.database_logins.is_rejected(&id), "a session refusal must not reject the saved credential");
        assert!(store.load(&format!("connection/{id}")).unwrap().is_none());
        let listing = s.run("connection.list", &Value::Null).unwrap();
        assert_eq!(listing["connections"][0]["loggedIn"], false);
        assert_eq!(listing["loginRequests"][0]["reason"], "password");
        assert_eq!(listing["loginRequests"][0]["connections"], json!([id]));
        // A second password-free operation must not put the refused password on the wire again.
        let _ = s.run(command, &params);
        server.empty();
    }
}

#[test]
fn remembered_database_cached_session_listing_failure_keeps_password() {
    if isolated("remembered_database_cached_session_listing_failure_keeps_password") {
        return;
    }
    use rustgis_data::source::duckdb;
    let server = Server::start(true);
    for command in ["connection.login", "connection.test", "connection.browse", "project.openText"] {
        let mut s = app(Arc::new(MemorySecrets::new()), "{}");
        let id = s.run("connection.new", &json!({"kind":"postgres","name":"Session","address":server.address})).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        s.run("connection.login", &json!({"id":id,"password":"cached-accepted"})).unwrap();
        let text = project_layers(&server.address, 1);
        s.project = serde_json::from_str(&text).unwrap();
        s.map_mut().layers[0].source.as_mut().unwrap().login = true;
        let params = if command == "project.openText" { json!({"text":text}) } else { json!({"id":id}) };
        let result = s.run(command, &params);
        match command {
            "connection.login" => assert_eq!(result.unwrap()["failed"], 1),
            "project.openText" => assert!(result.is_ok()),
            _ => assert!(result.unwrap_err().contains("fixture listing failed")),
        }
        assert_eq!(server.take(), b"cached-accepted\0", "{command}");
        server.empty();
        let (_, key) = crate::connections::database_key(&server.address).unwrap();
        assert_eq!(s.database_logins.session_password(&id).unwrap().expose(), "cached-accepted", "{command}");
        assert!(duckdb::has_connection_password(&key, &id), "{command}");
        assert!(s.database_logins.activations().contains(&(key, id.clone())), "{command}");
        assert!(!s.database_logins.is_rejected(&id));
        assert!(s.database_logins.requests(s.opened).next().is_none(), "{command}");
    }
}

#[test]
fn remembered_database_session_refusal_suppresses_only_matching_saved_value() {
    if isolated("remembered_database_session_refusal_suppresses_only_matching_saved_value") {
        return;
    }
    use rustgis_data::source::duckdb;
    let server = Server::new();
    for same in [true, false] {
        for command in ["connection.login", "connection.test", "connection.browse", "project.openText"] {
            let store = Arc::new(MemorySecrets::new());
            let mut s = app(store.clone(), "{}");
            let id = remember(&mut s, "Remembered", &server.address, "stored-value");
            let attempted = if same { "stored-value" } else { "session-other" };
            if !same {
                s.run("connection.login", &json!({"id":id,"password":attempted})).unwrap();
            }
            let saved = store.load(&format!("connection/{id}")).unwrap();
            let text = project(&server.address);
            s.project = serde_json::from_str(&text).unwrap();
            for layer in s.map_mut().layers.iter_mut().take(2) {
                layer.source.as_mut().unwrap().login = true;
            }
            // Passwordless login invalidates the saved cache before using the session value;
            // the other entry points exercise the already-cached saved copy.
            let params = if command == "project.openText" { json!({"text":text}) } else { json!({"id":id}) };
            let result = s.run(command, &params);
            match command {
                "connection.login" => assert_eq!(result.unwrap()["failed"], 2),
                "project.openText" => assert!(result.is_ok()),
                _ => assert!(!result.unwrap_err().contains(attempted)),
            }
            let mut packet = attempted.as_bytes().to_vec();
            packet.push(0);
            assert_eq!(server.take(), packet, "{command}, same={same}");
            server.empty();
            let (_, key) = crate::connections::database_key(&server.address).unwrap();
            assert!(!s.database_logins.has_session_password(&id));
            assert!(!duckdb::has_password(&key));
            assert_eq!(s.database_logins.is_rejected(&id), same, "{command}");
            assert!(s.connections.find(&id).unwrap().remembered);
            assert_eq!(store.load(&format!("connection/{id}")).unwrap(), saved);
            // Automatic resolution must not retry the same refused bytes via the saved copy.
            s.run("project.openText", &json!({"text":text})).unwrap();
            if !same {
                // A different saved value was not refused by the first attempt and remains eligible.
                assert_eq!(server.take(), b"stored-value\0", "{command}");
            }
            server.empty();
            assert_eq!(s.run("connection.list", &Value::Null).unwrap()["loginRequests"][0]["reason"], "password");
            assert_eq!(store.load(&format!("connection/{id}")).unwrap(), saved);
        }
    }
}

/// A peer cannot use a typed or saved password to select a different database table.
#[test]
fn peer_database_replacement_requires_the_exact_previously_shared_source() {
    if isolated("peer_database_replacement_requires_the_exact_previously_shared_source") {
        return;
    }
    for saved in [false, true] {
        for changed in [true, false] {
            let server = Server::start(true);
            let mut s = app(Arc::new(MemorySecrets::new()), "{}");
            let id = if saved {
                remember(&mut s, "Peer", &server.address, "RGSYNTH_peer_password")
            } else {
                let id = s.run("connection.new", &json!({"kind":"postgres", "address":server.address})).unwrap()["id"]
                    .as_str()
                    .unwrap()
                    .to_string();
                let (_, key) = crate::connections::database_key(&server.address).unwrap();
                rustgis_data::source::duckdb::remember_password(&key, "RGSYNTH_peer_password");
                id
            };
            // Install the approved source without starting its reader before the exchange.
            s.project = serde_json::from_str(&project_layers(&server.address, 1)).unwrap();
            let (text, _) = s.peer_text(crate::Peer::Bridge).unwrap();
            let mut incoming: Value = serde_json::from_str(&text).unwrap();
            if changed {
                incoming["maps"][0]["layers"][0]["source"]["postgis"]["table"] = json!("unapproved");
            } else {
                incoming["maps"][0]["layers"][0]["visible"] = json!(false);
            }
            let report = s.open_peer_text(crate::Peer::Bridge, &incoming.to_string()).unwrap();
            if changed {
                server.empty();
                assert!(report.notices().iter().any(|n| n.contains("reconnect")));
                let source = s.map_mut().layers[0].source.as_mut().unwrap();
                assert!(source.open().is_err(), "a later automatic open must not adopt a password either");
                server.empty();
                let (again, _) = s.peer_text(crate::Peer::Bridge).unwrap();
                let report = s.open_peer_text(crate::Peer::Bridge, &again).unwrap();
                assert!(report.notices().iter().any(|n| n.contains("reconnect")), "another peer round trip cannot approve the source");
                server.empty();
                s.run("connection.login", &json!({"id":id})).unwrap();
                assert_eq!(server.take(), b"RGSYNTH_peer_password\0", "explicit reconnect remains available");
            } else {
                assert_eq!(server.take(), b"RGSYNTH_peer_password\0");
                assert!(report.notices().is_empty(), "unchanged readers keep their login: {report:?}");
            }
        }
    }
}
