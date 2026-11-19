//! Esri `.sde` connection files: what ArcGIS Pro saves for an enterprise geodatabase. The file is
//! an undocumented binary container, so the few properties we need are scanned for by name rather
//! than parsed. Some files carry them as `KEY=VALUE;` text (ASCII and/or UTF-16LE); others are a
//! persisted ArcObjects property set, each key a UTF-16LE string followed by its value (a VARIANT,
//! with binary between). Without a single `KEY=` pair the file is read the second way: each key
//! found as a string of its own, its value the next printable run of text after it (the layout of
//! the files ArcGIS Pro 3 writes: the fixtures in `testdata/sde`). Only the server, database,
//! user and version come out, enough to prefill a SQL Server connection; the encrypted password
//! is never looked up, so it can't be shown or logged. Pure text, so the browser build has it
//! too. The file is hostile input: its size is bounded by [`MAX_BYTES`] and every scan by a value's
//! length.

/// Largest `.sde` file read.
pub const MAX_BYTES: usize = 1 << 20;

/// Why a `.sde` file or the connection it describes can't be used. `Display` is a sentence for the
/// person, not a log line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SdeError {
    /// Over [`MAX_BYTES`].
    TooLarge,
    /// The file names no server.
    NoServer,
    /// Not a SQL Server geodatabase; carries the platform for people ("a geodatabase on PostgreSQL").
    OtherPlatform(String),
    /// Anything else (an unusable server, port or similar).
    Other(String),
}

impl std::fmt::Display for SdeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge => write!(f, "that file is too large to be a connection file (over {} MB)", MAX_BYTES >> 20),
            Self::NoServer => f.write_str("that connection file doesn't name a server; enter the server by hand"),
            Self::OtherPlatform(phrase) => write!(f, "this is {phrase}; only SQL Server geodatabases can be opened for now"),
            Self::Other(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for SdeError {}

/// Longest value kept; a longer one is junk, not a server or a user.
const MAX_VALUE: usize = 1024;

/// What a `.sde` file says about where to connect and as whom (never the password).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SdeConnection {
    /// `sqlserver`, `postgresql`, `oracle` or another, in lower case: `DBCLIENT`, else the
    /// `sde:<platform>:` prefix of `INSTANCE`. Empty when the file doesn't say.
    pub platform: String,
    /// As [`pick_server`] picks it.
    pub server: String,
    /// `DATABASE`; empty when the file doesn't name one.
    pub database: String,
    /// `USER`; `None` with operating-system authentication.
    pub user: Option<String>,
    /// `AUTHENTICATION_MODE=OSA`: sign in as the Windows user, with no user name or password.
    pub os_auth: bool,
    /// `VERSION`, the geodatabase version the connection opens.
    pub version: Option<String>,
}

impl SdeConnection {
    /// The version the file names when it isn't Default (`sde.DEFAULT`, `dbo.DEFAULT`, or
    /// `DEFAULT` under any owner): RustGIS reads Default, so the caller says so by this name.
    pub fn named_version(&self) -> Option<&str> {
        let version = self.version.as_deref().map(str::trim).filter(|v| !v.is_empty())?;
        let name = version.rsplit_once('.').map_or(version, |(_, name)| name);
        (!name.eq_ignore_ascii_case("DEFAULT")).then_some(version)
    }
}

/// The properties read; never a password's.
const KEYS: [&str; 8] =
    ["SERVER", "INSTANCE", "DBCLIENT", "DB_CONNECTION_PROPERTIES", "DATABASE", "USER", "AUTHENTICATION_MODE", "VERSION"];

/// The names of the secrets a value must never run into or be.
const SECRETS: [&str; 2] = ["ENCRYPTED_PASSWORD", "PASSWORD"];

/// The connection a `.sde` file describes. An error is a sentence for the person, not a log line.
pub fn parse(bytes: &[u8]) -> Result<SdeConnection, SdeError> {
    if bytes.len() > MAX_BYTES {
        return Err(SdeError::TooLarge);
    }
    let texts = Texts::new(bytes);
    // A file with a counted value is a property set, whatever `KEY=` text its values hold.
    // Otherwise pairs win, and a file without any is scanned as a property set.
    // `lost` marks a key whose value was there and couldn't be kept ([`kept`]).
    let mut lost = [false; KEYS.len()];
    let mut read = |properties: bool| -> Vec<Option<String>> {
        let one = |(key, lost): (&&str, &mut bool)| if properties { texts.property(key, lost) } else { texts.value(key, lost) };
        KEYS.iter().zip(lost.iter_mut()).map(one).collect()
    };
    let counted = KEYS.iter().any(|k| texts.counted(k));
    let mut values = read(counted);
    if !counted && values.iter().all(Option::is_none) {
        values = read(true);
    }
    // Reading on without it would be silent: no database opens the login's default one, and no
    // version shows Default.
    if let Some(key) = KEYS.iter().zip(&values).zip(lost).find_map(|((key, value), lost)| (lost && value.is_none()).then_some(key)) {
        return Err(SdeError::Other(format!(
            "that connection file's {key} is too long to read, holds a character a value can't have, or holds the word PASSWORD, which is never read; enter the connection by hand"
        )));
    }
    let value = |key: &str| KEYS.iter().position(|k| *k == key).and_then(|i| values.get(i)).cloned().flatten();
    let instance = value("INSTANCE");
    let instance_platform = instance.as_deref().and_then(|i| split_instance(i).0);
    let platform = value("DBCLIENT").or(instance_platform).map(|p| p.to_ascii_lowercase()).unwrap_or_default();
    let server = pick_server(value("SERVER").as_deref(), value("DB_CONNECTION_PROPERTIES").as_deref(), instance.as_deref());
    let Some(server) = server else {
        return Err(SdeError::NoServer);
    };
    let os_auth = value("AUTHENTICATION_MODE").is_some_and(|m| m.eq_ignore_ascii_case("OSA"));
    Ok(SdeConnection {
        platform,
        server,
        database: value("DATABASE").unwrap_or_default(),
        user: if os_auth { None } else { value("USER") },
        os_auth,
        version: value("VERSION"),
    })
}

/// The server a connection names, from its `SERVER`, `DB_CONNECTION_PROPERTIES` and `INSTANCE`
/// properties: `DB_CONNECTION_PROPERTIES`, else `INSTANCE` after its `sde:<platform>:` prefix,
/// else `SERVER`, else an `INSTANCE` without the prefix. Pro often saves the bare machine name as
/// `SERVER` while the others carry the instance or port (`gis-db\GIS`, `gis-db,1434`). Blank
/// values don't count.
pub fn pick_server(server: Option<&str>, properties: Option<&str>, instance: Option<&str>) -> Option<String> {
    let clean = |s: Option<&str>| s.map(str::trim).filter(|s| !s.is_empty()).map(String::from);
    let (prefixed, bare) = match instance.map(str::trim) {
        Some(i) if i.get(..4).is_some_and(|p| p.eq_ignore_ascii_case("sde:")) => (split_instance(i).1, None),
        other => (None, clean(other)),
    };
    clean(properties).or(prefixed).or_else(|| clean(server)).or(bare)
}

/// The SQL Server address for a connection: `mssql://<user>@<host>[:port][/<database>][?instance=<i>]`.
/// `host\instance` gives `?instance=`, `host,port` gives `:port`. Any other platform is refused,
/// saying which it is.
pub fn mssql_address(c: &SdeConnection) -> Result<String, SdeError> {
    if c.platform != "sqlserver" {
        return Err(SdeError::OtherPlatform(platform_phrase(&c.platform)));
    }
    let (rest, port) = match c.server.trim().rsplit_once(',') {
        Some((rest, port)) => match port.trim().parse::<u16>() {
            Ok(p) if p > 0 => (rest, Some(p)),
            _ => return Err(SdeError::Other(format!("the server's port `{}` isn't a port number", port.trim()))),
        },
        None => (c.server.trim(), None),
    };
    let (host, instance) = rest.split_once('\\').map_or((rest, None), |(h, i)| (h, Some(i)));
    let host = host.trim();
    if host.is_empty() || !host.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_')) {
        return Err(SdeError::Other("the server's name isn't one a connection address can carry; enter the server by hand".into()));
    }
    let mut address = String::from("mssql://");
    if let Some(user) = c.user.as_deref().filter(|u| !u.is_empty() && !c.os_auth) {
        address.push_str(&percent_encode(user));
        address.push('@');
    }
    address.push_str(host);
    if let Some(port) = port {
        address.push_str(&format!(":{port}"));
    }
    if !c.database.is_empty() {
        address.push('/');
        address.push_str(&percent_encode(&c.database));
    }
    if let Some(instance) = instance.map(str::trim).filter(|i| !i.is_empty()) {
        address.push_str("?instance=");
        address.push_str(&percent_encode(instance));
    }
    Ok(address)
}

/// A platform for people: "a geodatabase on PostgreSQL" (worded so no article depends on the
/// platform's name).
fn platform_phrase(platform: &str) -> String {
    match platform {
        "postgresql" => "a geodatabase on PostgreSQL".into(),
        "oracle" => "a geodatabase on Oracle".into(),
        "" => "a geodatabase of an unknown kind".into(),
        other => format!("a geodatabase on `{other}`"),
    }
}

/// `sde:<platform>:<server>` as (platform, server); a value without the prefix is all server.
fn split_instance(instance: &str) -> (Option<String>, Option<String>) {
    let mut parts = instance.splitn(3, ':');
    match (parts.next(), parts.next(), parts.next()) {
        // `sde:sqlserver` alone names the platform and no server.
        (Some(sde), Some(platform), server) if sde.eq_ignore_ascii_case("sde") => {
            let some = |s: &str| Some(s.trim().to_string()).filter(|s| !s.is_empty());
            (some(platform), server.and_then(some))
        }
        _ => (None, Some(instance.to_string())),
    }
}

/// Text with everything but unreserved characters as `%XX` (so `DOMAIN\user` is `DOMAIN%5Cuser`).
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The file's bytes read as text every way it may hold the properties: ASCII, and UTF-16LE at
/// both byte alignments. Anything that isn't text becomes NUL, which ends a value.
struct Texts(Vec<Text>);

/// One reading of the file's bytes.
struct Text {
    chars: Vec<char>,
    /// Where a UTF-16 unit was half a surrogate pair and became NUL, in order. A counted length
    /// is a number, not text, so [`counted`] has to tell such a half from a zero.
    unpaired: Vec<usize>,
}

impl Texts {
    fn new(bytes: &[u8]) -> Self {
        let ascii = Text { chars: bytes.iter().map(|&b| if b.is_ascii() { b as char } else { '\0' }).collect(), unpaired: Vec::new() };
        let utf16 = |skip: usize| {
            let units = bytes.get(skip..).unwrap_or_default().as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c));
            let mut unpaired = Vec::new();
            let chars = char::decode_utf16(units)
                .enumerate()
                .map(|(i, r)| {
                    r.unwrap_or_else(|_| {
                        unpaired.push(i);
                        '\0'
                    })
                })
                .collect();
            Text { chars, unpaired }
        };
        Self(vec![ascii, utf16(0), utf16(1)])
    }

    /// The first non-empty value of `key` (in any case) after `KEY=` and before the next `;`,
    /// control character or secret's name. The key starts a pair: at the start of the text or
    /// after a character that can't be part of another key.
    fn value(&self, key: &str, lost: &mut bool) -> Option<String> {
        self.0.iter().find_map(|text| text_value(&text.chars, key, lost))
    }

    /// The value of `key` in a property set: the key as a UTF-16LE string of its own, then the
    /// first printable run within [`GAP`] characters after it (a one-character run, which may be
    /// a VARIANT's length, only when no longer one follows). Never asked for a secret.
    fn property(&self, key: &str, lost: &mut bool) -> Option<String> {
        self.0.get(1..)?.iter().find_map(|text| property_value(text, key, false, lost))
    }

    /// Whether `key` has a value in the counted layout ArcGIS Pro writes ([`counted`]).
    fn counted(&self, key: &str) -> bool {
        self.0.get(1..).is_some_and(|texts| texts.iter().any(|text| property_value(text, key, true, &mut false).is_some()))
    }
}

/// How far past a property-set key its value may start: the VARIANT's type and length between.
const GAP: usize = 64;

/// A character a property-set value is made of: ASCII letters, digits and punctuation.
fn printable(c: char) -> bool {
    c.is_ascii_alphanumeric() || c.is_ascii_punctuation()
}

/// The value of `key` in a property set; with `counted_only`, only one in the counted layout.
/// `lost` is set when a value is there and can't be kept.
fn property_value(text: &Text, key: &str, counted_only: bool, lost: &mut bool) -> Option<String> {
    let (whole, text) = (text, text.chars.as_slice());
    let key: Vec<char> = key.chars().collect();
    (0..text.len()).find_map(|at| {
        let alone_before = at == 0 || text.get(at - 1).is_some_and(|c| !(c.is_ascii_alphanumeric() || *c == '_'));
        let named = text.get(at..at + key.len()).is_some_and(|w| w.iter().zip(&key).all(|(a, b)| a.eq_ignore_ascii_case(b)));
        // A string of its own ends where the key does.
        let alone_after = text.get(at + key.len()).is_some_and(|c| *c == '\0' || c.is_control());
        if !alone_before || !named || !alone_after {
            return None;
        }
        let start = at + key.len();
        // A counted value is the value, even when it is another key's name.
        match counted(whole, start) {
            Counted::Value(value) => return kept(value.iter().collect(), lost),
            // Not scanned for a printable run: that would read its first word as the value.
            Counted::Lost => {
                *lost = true;
                return None;
            }
            Counted::No => {}
        }
        if counted_only {
            return None;
        }
        let (mut i, mut short) = (start, None);
        while i < start + GAP {
            let rest = text.get(i..)?;
            if !rest.first().copied().is_some_and(printable) {
                i += 1;
                continue;
            }
            // Look no further than a value can be long, so a key costs O(MAX_VALUE), not O(file).
            let run = rest.iter().take(MAX_VALUE + 1).take_while(|c| printable(**c)).count();
            if run > MAX_VALUE {
                *lost = true;
                return None;
            }
            let found = rest.get(..run)?;
            if run >= 2 {
                // The next key's name ends this one's value: a one-character run before it is the value.
                return property_text(found, lost).or_else(|| short.and_then(|run| property_text(run, lost)));
            }
            short.get_or_insert(found);
            i += run;
        }
        short.and_then(|run| property_text(run, lost))
    })
}

/// What follows a key, read as the counted layout ([`counted`]).
enum Counted<'a> {
    /// The value, whole.
    Value(&'a [char]),
    /// A counted value that can't be kept: longer than [`MAX_VALUE`], or holding a control
    /// character.
    Lost,
    /// Another layout, which is scanned for a printable run instead.
    No,
}

/// The value ArcGIS Pro writes after a key: the key's NUL, a string VARIANT's type (8), the
/// value's length in bytes as 32 bits (its NUL included) and the text, which keeps its spaces
/// and any letters.
fn counted(text: &Text, start: usize) -> Counted<'_> {
    let (unpaired, text) = (&text.unpaired, text.chars.as_slice());
    let Some(head) = text.get(start..start + 4) else { return Counted::No };
    if (head[0], head[1]) != ('\0', '\u{8}') {
        return Counted::No;
    }
    // The length's two halves were read as UTF-16 units. A half that was a surrogate, alone
    // (read as NUL) or paired (read as one character outside the basic plane), is 0xD800 or
    // more, so the length is far over the limit.
    let surrogate = |at: usize| unpaired.binary_search(&at).is_ok();
    let (low, high) = (head[2] as u64, head[3] as u64);
    let bytes = low | high << 16;
    if surrogate(start + 2) || surrogate(start + 3) || low > 0xFFFF || high > 0xFFFF || bytes / 2 > MAX_VALUE as u64 + 1 {
        return Counted::Lost;
    }
    if bytes < 2 || !bytes.is_multiple_of(2) {
        return Counted::No;
    }
    let want = (bytes / 2 - 1) as usize;
    // The length counts UTF-16 units, and a character outside the basic plane is two of them.
    let Some(rest) = text.get(start + 4..) else { return Counted::No };
    let (mut units, mut chars) = (0, 0);
    while units < want {
        let Some(c) = rest.get(chars) else { return Counted::No };
        units += c.len_utf16();
        chars += 1;
    }
    // A length that doesn't end at the value's NUL isn't this layout's.
    let Some(value) = rest.get(..chars).filter(|_| units == want && rest.get(chars) == Some(&'\0')) else { return Counted::No };
    if value.iter().any(|c| c.is_control()) { Counted::Lost } else { Counted::Value(value) }
}

/// A property-set value as kept: not another key's or a secret's name, and as [`kept`] has it.
fn property_text(run: &[char], lost: &mut bool) -> Option<String> {
    let value: String = run.iter().collect();
    let upper = value.to_ascii_uppercase();
    if KEYS.iter().chain(SECRETS.iter()).any(|k| *k == upper) {
        return None;
    }
    kept(value, lost)
}

/// A value as kept: trimmed, not blank, and cut before a secret's pair it runs into
/// (`editor PASSWORD=…`, a missing `;`). A value that holds a secret's name any other way can't
/// be told from the secret: it isn't kept, and `lost` says so.
fn kept(mut value: String, lost: &mut bool) -> Option<String> {
    let upper = value.to_ascii_uppercase();
    if let Some(at) = SECRETS.iter().filter_map(|k| upper.find(k)).min() {
        let rest = upper.get(at..).unwrap_or_default();
        if !SECRETS.iter().any(|k| rest.strip_prefix(k).is_some_and(|after| after.trim_start().starts_with('='))) {
            *lost = true;
            return None;
        }
        value.truncate(at);
    }
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// Whether `at` lies in a secret's value: the pair it is in (back to the last `;` or control
/// character) starts with a secret's name. A pair too long to find the start of counts as one.
fn in_secret(text: &[char], at: usize) -> bool {
    let back = MAX_VALUE + 32;
    let from = at.saturating_sub(back);
    let before = text.get(from..at).unwrap_or_default();
    let start = match before.iter().rposition(|&c| c == ';' || c.is_control()) {
        Some(i) => i + 1,
        None if from == 0 => 0,
        None => return true,
    };
    let pair: String = before.get(start..).unwrap_or_default().iter().collect::<String>().trim_start().to_ascii_uppercase();
    SECRETS.iter().any(|k| pair.strip_prefix(k).is_some_and(|rest| rest.starts_with('=')))
}

fn text_value(text: &[char], key: &str, lost: &mut bool) -> Option<String> {
    let key: Vec<char> = key.chars().collect();
    (0..text.len()).find_map(|at| {
        let starts_pair = at == 0 || text.get(at - 1).is_some_and(|c| !(c.is_ascii_alphanumeric() || *c == '_'));
        let named = text.get(at..at + key.len()).is_some_and(|w| w.iter().zip(&key).all(|(a, b)| a.eq_ignore_ascii_case(b)));
        if !starts_pair || !named || text.get(at + key.len()) != Some(&'=') || in_secret(text, at) {
            return None;
        }
        let rest = text.get(at + key.len() + 1..)?;
        // Look no further than a value can be long, so a pair start costs O(MAX_VALUE), not O(file).
        let end = match rest.iter().take(MAX_VALUE + 1).position(|&c| c == ';' || c.is_control()) {
            Some(end) => end,
            None if rest.len() <= MAX_VALUE => rest.len(),
            None => {
                *lost = true;
                return None;
            }
        };
        kept(rest.get(..end)?.iter().collect(), lost)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ASCII: &[u8] = b"\x00\x01SERVER=gis-db;INSTANCE=sde:sqlserver:gis-db;DBCLIENT=sqlserver;DB_CONNECTION_PROPERTIES=gis-db;DATABASE=utility;USER=editor;ENCRYPTED_PASSWORD=00ff;VERSION=sde.DEFAULT;AUTHENTICATION_MODE=DBMS;\x00";

    fn utf16(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    fn connection(server: &str, user: Option<&str>, os_auth: bool) -> SdeConnection {
        SdeConnection {
            platform: "sqlserver".into(),
            server: server.into(),
            database: "utility".into(),
            user: user.map(String::from),
            os_auth,
            version: None,
        }
    }

    fn assert_ascii_result(c: &SdeConnection) {
        assert_eq!(c.platform, "sqlserver");
        assert_eq!(c.server, "gis-db");
        assert_eq!(c.database, "utility");
        assert_eq!(c.user.as_deref(), Some("editor"));
        assert!(!c.os_auth);
        assert_eq!(c.version.as_deref(), Some("sde.DEFAULT"));
    }

    #[test]
    fn ascii_pairs_parse() {
        assert_ascii_result(&parse(ASCII).unwrap());
    }

    #[test]
    fn utf16_pairs_parse() {
        let text = String::from_utf8_lossy(ASCII.get(2..ASCII.len() - 1).unwrap()).into_owned();
        for pad in [0, 1] {
            let mut bytes = vec![0xFF; 7 + pad];
            bytes.extend(utf16(&text));
            bytes.extend([0x13, 0x37, 0x00, 0xFF]);
            assert_ascii_result(&parse(&bytes).unwrap());
        }
    }

    #[test]
    fn os_auth_has_no_user() {
        let c = parse(b"SERVER=gis-db;DATABASE=utility;USER=ignored;AUTHENTICATION_MODE=OSA;").unwrap();
        assert!(c.os_auth);
        assert_eq!(c.user, None);
        let c = parse(b"SERVER=gis-db;DATABASE=utility;AUTHENTICATION_MODE=osa;").unwrap();
        assert!(c.os_auth && c.user.is_none());
    }

    #[test]
    fn instance_prefix_gives_platform_and_server() {
        let c = parse(b"INSTANCE=sde:postgresql:pg-host,5432;DATABASE=d").unwrap();
        assert_eq!(c.platform, "postgresql");
        assert_eq!(c.server, "pg-host,5432");
        assert_eq!(c.database, "d");
    }

    #[test]
    fn server_falls_back_to_connection_properties() {
        let c = parse(b"DB_CONNECTION_PROPERTIES=props-host;INSTANCE=sde:sqlserver:inst-host;DATABASE=d").unwrap();
        assert_eq!(c.server, "props-host");
        assert_eq!(c.platform, "sqlserver");
    }

    #[test]
    fn the_server_prefers_the_connection_properties_and_instance() {
        let server = |bytes: &[u8]| parse(bytes).unwrap().server;
        assert_eq!(server(br"SERVER=gis-db;DB_CONNECTION_PROPERTIES=gis-db\GIS;DATABASE=d"), r"gis-db\GIS");
        assert_eq!(server(b"SERVER=gis-db;INSTANCE=sde:sqlserver:gis-db,1434;DATABASE=d"), "gis-db,1434");
        assert_eq!(server(b"SERVER=gis-db;DATABASE=d"), "gis-db");
        assert_eq!(pick_server(Some("gis-db"), Some(r"gis-db\GIS"), Some("sde:sqlserver:gis-db,1434")).as_deref(), Some(r"gis-db\GIS"));
        assert_eq!(pick_server(Some("gis-db"), Some(" "), Some("sde:sqlserver:gis-db,1434")).as_deref(), Some("gis-db,1434"));
        assert_eq!(pick_server(Some("gis-db"), None, Some("5151")).as_deref(), Some("gis-db"), "a bare instance is the last resort");
        assert_eq!(pick_server(None, None, Some("5151")).as_deref(), Some("5151"));
        assert_eq!(pick_server(Some(""), None, None), None);
        // A prefix without a server part names the platform and no server.
        let c = parse(b"SERVER=gis-db;INSTANCE=sde:sqlserver;DATABASE=d").unwrap();
        assert_eq!((c.server.as_str(), c.platform.as_str()), ("gis-db", "sqlserver"));
        assert_eq!(pick_server(Some("gis-db"), None, Some("sde:sqlserver:")).as_deref(), Some("gis-db"));
        assert_eq!(parse(b"INSTANCE=sde:sqlserver;DATABASE=d").unwrap_err(), SdeError::NoServer);
    }

    #[test]
    fn a_value_that_cannot_be_kept_is_an_error_not_a_blank() {
        let refused = |bytes: &[u8], key: &str| {
            let err = parse(bytes).err().unwrap_or_else(|| panic!("{key} was dropped without a word"));
            assert!(err.to_string().contains(key), "{err}");
            assert!(!err.to_string().to_uppercase().contains("VAULT"), "{err}");
        };
        // Too long to be a name: without it the connection would open the login's default database.
        refused(format!("SERVER=gis-db;DATABASE={};USER=u;", "d".repeat(MAX_VALUE + 1)).as_bytes(), "DATABASE");
        // A name holding a secret's name can't be told from a value that ran into the secret.
        refused(b"SERVER=gis-db;DATABASE=PasswordVault;USER=u;", "DATABASE");
        refused(b"SERVER=gis-db;DATABASE=d;VERSION=sde.password_vault;", "VERSION");
        refused(&property_set(&[("SERVER", "gis-db"), ("DATABASE", "PasswordVault")], 0), "DATABASE");
        // A blank value is a blank, and a value that runs into a secret's pair is cut before it.
        assert_eq!(parse(b"SERVER=gis-db;DATABASE=;USER=u;").unwrap().database, "");
        assert_eq!(parse(b"SERVER=gis-db;DATABASE=d PASSWORD=hunter2;").unwrap().database, "d");
    }

    /// A property-set-like buffer: each key a UTF-16LE string ended by NUL, binary, then its value.
    fn property_set(pairs: &[(&str, &str)], pad: usize) -> Vec<u8> {
        let mut bytes = vec![0xD0, 0xCF, 0x11, 0xE0][..pad.min(4)].to_vec();
        for (key, value) in pairs {
            bytes.extend(utf16(key));
            bytes.extend([0, 0, 0x08, 0x00, 0x16, 0x00, 0x00, 0x00]);
            bytes.extend(utf16(value));
            bytes.extend([0, 0, 0x03, 0x00, 0x01, 0x00]);
        }
        bytes
    }

    #[test]
    fn a_property_set_parses() {
        let pairs = [
            ("SERVER", r"gis-db\GIS"),
            ("INSTANCE", r"sde:sqlserver:gis-db\GIS"),
            ("DBCLIENT", "sqlserver"),
            ("DATABASE", "utility"),
            ("USER", "editor"),
            ("ENCRYPTED_PASSWORD", "00ff"),
            ("AUTHENTICATION_MODE", "DBMS"),
            ("VERSION", "sde.DEFAULT"),
        ];
        for pad in [0, 1] {
            let c = parse(&property_set(&pairs, pad)).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(
                (c.platform.as_str(), c.server.as_str(), c.database.as_str(), c.user.as_deref()),
                ("sqlserver", r"gis-db\GIS", "utility", Some("editor"))
            );
            assert_eq!(c.version.as_deref(), Some("sde.DEFAULT"));
            let shown = format!("{c:?}");
            assert!(!shown.contains("00ff") && !shown.to_uppercase().contains("PASSWORD"), "{shown}");
        }
        // only the essentials, in the ruling's order
        let c = parse(&property_set(
            &[("SERVER", r"gis-db\GIS"), ("DATABASE", "utility"), ("USER", "editor"), ("ENCRYPTED_PASSWORD", "00ff")],
            0,
        ))
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!((c.server.as_str(), c.database.as_str(), c.user.as_deref()), (r"gis-db\GIS", "utility", Some("editor")));
        assert!(!format!("{c:?}").contains("00ff"));
    }

    #[test]
    fn a_property_set_fails_closed() {
        // a key with no value before the next key gives nothing, not the next key's name
        let c = parse(&property_set(&[("USER", ""), ("SERVER", "gis-db")], 0)).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(c.user, None);
        // no server, or a password only: refused
        assert_eq!(parse(&property_set(&[("DATABASE", "utility"), ("ENCRYPTED_PASSWORD", "00ff")], 0)).unwrap_err(), SdeError::NoServer);
        // a key inside a longer word isn't the key
        assert_eq!(parse(&property_set(&[("MYSERVER", "gis-db")], 0)).unwrap_err(), SdeError::NoServer);
        // a value longer than a value can be is junk, and said to be
        let long = "x".repeat(MAX_VALUE + 1);
        let err = parse(&property_set(&[("SERVER", &long)], 0)).unwrap_err();
        assert!(matches!(&err, SdeError::Other(why) if why.contains("SERVER is too long")), "{err}");
        // pairs win: a file with `KEY=` text isn't read as a property set
        let mut bytes = property_set(&[("SERVER", "set-host")], 0);
        bytes.extend(b"\x00DATABASE=d;");
        assert_eq!(parse(&bytes).unwrap_err(), SdeError::NoServer);
    }

    /// A property set as ArcGIS Pro writes it: each key and value a counted UTF-16 string.
    fn counted_set(pairs: &[(&str, &str)]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for (key, value) in pairs {
            let (k, v) = (utf16(key), utf16(value));
            bytes.extend((k.len() as u32 + 2).to_le_bytes());
            bytes.extend(k);
            bytes.extend([0, 0, 0x08, 0x00]);
            bytes.extend((v.len() as u32 + 2).to_le_bytes());
            bytes.extend(v);
            bytes.extend([0, 0]);
        }
        bytes
    }

    #[test]
    fn a_counted_value_keeps_its_spaces_and_letters() {
        let pairs = [
            ("SERVER", "gis-db"),
            ("DBCLIENT", "sqlserver"),
            ("DATABASE", "City GIS"),
            ("USER", "Jos\u{e9} M"),
            ("VERSION", "editor.Design 2"),
            ("ENCRYPTED_PASSWORD", "00ff"),
        ];
        let c = parse(&counted_set(&pairs)).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!((c.server.as_str(), c.database.as_str()), ("gis-db", "City GIS"));
        assert_eq!(c.user.as_deref(), Some("Jos\u{e9} M"));
        assert_eq!(c.version.as_deref(), Some("editor.Design 2"));
        assert!(!format!("{c:?}").contains("00ff"));
        // an empty counted value is no value, not the next key's
        let c = parse(&counted_set(&[("USER", ""), ("SERVER", "gis-db")])).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!((c.user, c.server.as_str()), (None, "gis-db"));
    }

    #[test]
    fn text_inside_a_counted_value_does_not_pick_the_layout() {
        let pairs = [("SERVER", "gis-db"), ("DBCLIENT", "sqlserver"), ("DATABASE", "City USER=editor")];
        let c = parse(&counted_set(&pairs)).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!((c.server.as_str(), c.database.as_str(), c.user), ("gis-db", "City USER=editor", None));
    }

    #[test]
    fn a_counted_value_keeps_letters_outside_the_basic_plane() {
        // One character, two UTF-16 units: the stored length counts units.
        let pairs = [("SERVER", "gis-db"), ("DBCLIENT", "sqlserver"), ("DATABASE", "City \u{20000} USER=editor")];
        let c = parse(&counted_set(&pairs)).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!((c.server.as_str(), c.database.as_str(), c.user), ("gis-db", "City \u{20000} USER=editor", None));
    }

    #[test]
    fn a_counted_value_is_kept_whole_or_not_at_all() {
        // A counted value that is another key's name is still the value.
        let c = parse(&counted_set(&[("SERVER", "gis-db"), ("DBCLIENT", "sqlserver"), ("DATABASE", "VERSION")]))
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(c.database, "VERSION");
        // One too long to keep is an error, not its first word.
        let long = format!("City {}", "x".repeat(MAX_VALUE));
        let err =
            parse(&counted_set(&[("SERVER", "gis-db"), ("DATABASE", &long)])).err().unwrap_or_else(|| panic!("read as another database"));
        assert!(matches!(&err, SdeError::Other(why) if why.contains("DATABASE is too long")), "{err}");
        // So is one whose length needs the count's high half: 65,536 bytes with its NUL.
        let long = format!("City {}", "x".repeat(32_762));
        let err =
            parse(&counted_set(&[("SERVER", "gis-db"), ("DATABASE", &long)])).err().unwrap_or_else(|| panic!("read as another database"));
        assert!(matches!(&err, SdeError::Other(why) if why.contains("DATABASE is too long")), "{err}");
        // And one whose length isn't text at all: 55,296 bytes is 0xD800, half a surrogate pair,
        // in the low half and then in the high half.
        let long = format!("City {}", "x".repeat(27_642));
        let err =
            parse(&counted_set(&[("SERVER", "gis-db"), ("DATABASE", &long)])).err().unwrap_or_else(|| panic!("read as another database"));
        assert!(matches!(&err, SdeError::Other(why) if why.contains("DATABASE is too long")), "{err}");
        let mut high = counted_set(&[("SERVER", "gis-db"), ("DATABASE", "City GIS")]);
        let at = high.len() - utf16("City GIS").len() - 2 - 2;
        high[at..at + 2].copy_from_slice(&[0x00, 0xD8]);
        let err = parse(&high).err().unwrap_or_else(|| panic!("read with half its length"));
        assert!(matches!(&err, SdeError::Other(why) if why.contains("DATABASE is too long")), "{err}");
        // And one holding a control character: its first word isn't the value either.
        let err = parse(&counted_set(&[("SERVER", "gis-db"), ("DATABASE", "City\tGIS")]))
            .err()
            .unwrap_or_else(|| panic!("read as another database"));
        assert!(matches!(&err, SdeError::Other(why) if why.contains("DATABASE is too long")), "{err}");
        // A good counted value beside them still reads whole.
        let c = parse(&counted_set(&[("SERVER", "gis-db"), ("DATABASE", "City GIS")])).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(c.database, "City GIS");
    }

    #[test]
    fn a_one_character_value_survives_the_next_key() {
        let c = parse(&property_set(&[("SERVER", "h"), ("DATABASE", "utility")], 0)).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!((c.server.as_str(), c.database.as_str()), ("h", "utility"));
    }

    #[test]
    fn a_key_inside_a_password_is_not_a_key() {
        let c = parse(b"SERVER=gis-db;DBCLIENT=sqlserver;PASSWORD=USER=hunter2;").unwrap();
        assert_eq!(c.user, None);
        assert!(!format!("{c:?}").contains("hunter2"));
        let c = parse(b"SERVER=gis-db;ENCRYPTED_PASSWORD=ab DATABASE=cd;").unwrap();
        assert_eq!(c.database, "");
        // a pair of its own after the password still reads
        let c = parse(b"SERVER=gis-db;PASSWORD=a=b;USER=editor;").unwrap();
        assert_eq!(c.user.as_deref(), Some("editor"));
    }

    #[test]
    fn repeated_property_keys_are_cheap() {
        let unit: Vec<u8> = [utf16("SERVER"), vec![0, 0], utf16(&"x".repeat(MAX_VALUE + 1)), vec![0, 0]].concat();
        let bytes: Vec<u8> = unit.iter().copied().cycle().take(MAX_BYTES).collect();
        let started = std::time::Instant::now();
        let parsed = parse(&bytes);
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "{:?}", started.elapsed());
        // the cut-off unit at the end may give a short junk server; never a long one
        assert!(parsed.map_or(true, |c| c.server.chars().count() <= MAX_VALUE));
    }

    #[test]
    fn keys_match_only_at_a_boundary() {
        let c = parse(b"OS_USER=other;SERVER=gis-db;DATABASE=d;USER=editor;").unwrap();
        assert_eq!(c.user.as_deref(), Some("editor"));
        let c = parse(b"SERVER=gis-db;DATABASE=d;Server_Extra=x;user=low;").unwrap();
        assert_eq!(c.user.as_deref(), Some("low"));
    }

    #[test]
    fn no_server_is_refused() {
        let err = parse(b"DATABASE=d;USER=u").unwrap_err();
        assert_eq!(err, SdeError::NoServer);
        assert!(err.to_string().contains("enter the server"), "{err}");
        assert_eq!(parse(b"").unwrap_err(), SdeError::NoServer);
    }

    #[test]
    fn too_large_is_refused() {
        assert_eq!(parse(&vec![b'x'; MAX_BYTES + 1]).unwrap_err(), SdeError::TooLarge);
        assert_eq!(parse(&vec![b'x'; MAX_BYTES]).unwrap_err(), SdeError::NoServer); // right size, no server
    }

    #[test]
    fn secrets_never_come_out() {
        let shown = format!("{:?}", parse(ASCII).unwrap());
        assert!(!shown.contains("00ff") && !shown.to_uppercase().contains("PASSWORD"), "{shown}");
        // a value that runs into a secret (a missing `;`) stops before it
        let c = parse(b"SERVER=gis-db;DATABASE=d;USER=editor ENCRYPTED_PASSWORD=00ff;").unwrap();
        let shown = format!("{c:?}");
        assert!(!shown.contains("00ff") && !shown.to_uppercase().contains("PASSWORD"), "{shown}");
        let c = parse(b"SERVER=gis-db;PASSWORD=hunter2;DATABASE=d;").unwrap();
        assert!(!format!("{c:?}").contains("hunter2"));
    }

    #[test]
    fn repeated_keys_without_a_terminator_are_cheap() {
        // Every `SERVER=` here starts a pair and none ends, in a file just under the size cap: the
        // scan from each must stop after a value's length, not run to the end of the file.
        let bytes = "SERVER=".repeat(MAX_BYTES / 7);
        assert!(bytes.len() <= MAX_BYTES);
        let started = std::time::Instant::now();
        let parsed = parse(bytes.as_bytes());
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "{:?}", started.elapsed());
        // the pairs near the end have short tails, so a bounded junk server may come out; never a long one
        assert!(parsed.map_or(true, |c| c.server.chars().count() <= MAX_VALUE));
    }

    /// The connection files in `testdata/sde`, as ArcGIS Pro 3 wrote them for servers that don't
    /// exist (`*.example.invalid`).
    const FIXTURES: [(&str, &[u8]); 3] = [
        ("sqlserver-os.sde", include_bytes!("../../testdata/sde/sqlserver-os.sde")),
        ("sqlserver-dbauth-nosave.sde", include_bytes!("../../testdata/sde/sqlserver-dbauth-nosave.sde")),
        ("postgres-os.sde", include_bytes!("../../testdata/sde/postgres-os.sde")),
    ];

    #[test]
    fn files_arcgis_pro_wrote_parse() {
        let file = |name: &str| FIXTURES.iter().find(|(n, _)| *n == name).map(|(_, b)| parse(b).unwrap()).unwrap();
        let os = file("sqlserver-os.sde");
        assert_eq!(
            (os.platform.as_str(), os.server.as_str(), os.database.as_str(), os.user.as_deref(), os.os_auth),
            ("sqlserver", "gisdb.example.invalid", "gis", None, true)
        );
        assert_eq!(mssql_address(&os).unwrap(), "mssql://gisdb.example.invalid/gis");
        // A database login saved without its user and password: neither is there to read.
        let db = file("sqlserver-dbauth-nosave.sde");
        assert_eq!((db.server.as_str(), db.user.as_deref(), db.os_auth), ("gisdb.example.invalid", None, false));
        let other = mssql_address(&file("postgres-os.sde"));
        assert!(matches!(&other, Err(SdeError::OtherPlatform(p)) if p.contains("PostgreSQL")), "{other:?}");
    }

    #[test]
    fn no_fixture_holds_a_password() {
        for (name, bytes) in FIXTURES {
            let wide: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
            let text = format!("{}{}", String::from_utf8_lossy(bytes), String::from_utf16_lossy(&wide)).to_ascii_uppercase();
            assert!(!text.contains("PASSWORD"), "{name} was saved with a password");
        }
    }

    #[test]
    fn a_version_other_than_default_is_named() {
        let with = |version: Option<&str>| SdeConnection {
            platform: "sqlserver".into(),
            server: "gis-db".into(),
            database: "utility".into(),
            user: None,
            os_auth: true,
            version: version.map(String::from),
        };
        for default in [None, Some(""), Some("sde.DEFAULT"), Some("dbo.DEFAULT"), Some("DBO.default"), Some("DEFAULT")] {
            assert_eq!(with(default).named_version(), None, "{default:?}");
        }
        assert_eq!(with(Some("editor.Design 2")).named_version(), Some("editor.Design 2"));
        assert_eq!(with(Some(" sde.DEFAULTS ")).named_version(), Some("sde.DEFAULTS"));
    }

    #[test]
    fn addresses_compose() {
        let a = |c: &SdeConnection| mssql_address(c).unwrap();
        assert_eq!(a(&connection(r"gis-db\GIS", Some("editor"), false)), "mssql://editor@gis-db/utility?instance=GIS");
        assert_eq!(a(&connection("gis-db,1434", Some("editor"), false)), "mssql://editor@gis-db:1434/utility");
        assert_eq!(a(&connection("gis-db", None, true)), "mssql://gis-db/utility");
        assert_eq!(a(&connection("gis-db", Some(r"CORP\ana"), false)), "mssql://CORP%5Cana@gis-db/utility");
        let mut c = connection("gis-db", Some("editor"), false);
        c.database = String::new();
        assert_eq!(a(&c), "mssql://editor@gis-db");
        c.platform = "postgresql".into();
        let err = mssql_address(&c).unwrap_err();
        assert_eq!(err, SdeError::OtherPlatform("a geodatabase on PostgreSQL".into()));
        assert!(err.to_string().contains("this is a geodatabase on PostgreSQL"), "{err}");
        c.platform = "oracle".into();
        assert_eq!(mssql_address(&c).unwrap_err(), SdeError::OtherPlatform("a geodatabase on Oracle".into()));
    }

    #[test]
    fn hostile_servers_are_refused() {
        for server in ["", r"\GIS", "h/x", "h?x", "h@x", "h,notaport", "h,0", "h,70000", "h x,1"] {
            assert!(matches!(mssql_address(&connection(server, Some("u"), false)), Err(SdeError::Other(_))), "{server:?}");
        }
    }
}
