//! Reads the cookies of the person's other browsers (Chrome, Brave, Arc, Edge,
//! Firefox) so the browser pane can import them. Only reads: no UI, no CEF.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::net::IpAddr;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use aes::Aes128;
use anyhow::{Context, Result};
use cbc::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};

/// Seconds between 1601-01-01 (the Chromium and CEF time base) and 1970-01-01.
const EPOCH_1601_TO_1970_SECS: i64 = 11_644_473_600;
/// Chromium prefixes the plaintext with SHA-256(host_key) from meta version 24.
const HOST_HASH_VERSION: i64 = 24;
/// Firefox changed `expiry` from seconds to milliseconds; no second count
/// reaches this before the year 5000.
const FIREFOX_MILLISECONDS_FROM: i64 = 100_000_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Keychain {
    service: &'static str,
    account: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Engine {
    Chromium(Keychain),
    Firefox,
}

struct ChromiumBrowser {
    id: &'static str,
    label: &'static str,
    /// Under `~/Library/Application Support`.
    root: &'static str,
    keychain: Keychain,
}

const CHROMIUM_BROWSERS: [ChromiumBrowser; 4] = [
    ChromiumBrowser {
        id: "chrome",
        label: "Google Chrome",
        root: "Google/Chrome",
        keychain: Keychain {
            service: "Chrome Safe Storage",
            account: "Chrome",
        },
    },
    ChromiumBrowser {
        id: "brave",
        label: "Brave",
        root: "BraveSoftware/Brave-Browser",
        keychain: Keychain {
            service: "Brave Safe Storage",
            account: "Brave",
        },
    },
    ChromiumBrowser {
        id: "arc",
        label: "Arc",
        root: "Arc/User Data",
        keychain: Keychain {
            service: "Arc Safe Storage",
            account: "Arc",
        },
    },
    ChromiumBrowser {
        id: "edge",
        label: "Microsoft Edge",
        root: "Microsoft Edge",
        keychain: Keychain {
            service: "Microsoft Edge Safe Storage",
            account: "Microsoft Edge",
        },
    },
];

/// One browser profile with a cookie database.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    /// Stable across runs, e.g. `chrome:Default`.
    pub id: String,
    pub browser: String,
    /// The profile name as the browser shows it.
    pub profile: String,
    pub db: PathBuf,
    engine: Engine,
}

/// A registrable domain (eTLD+1) and how many live cookies it has.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Site {
    pub domain: String,
    pub count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SameSite {
    Unspecified,
    None,
    Lax,
    Strict,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Priority {
    Low,
    Medium,
    High,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    /// As stored; a leading dot marks a domain cookie.
    pub domain: String,
    pub path: String,
    pub secure: bool,
    pub http_only: bool,
    /// Microseconds since 1601-01-01 UTC (CEF's cef_basetime_t); None for a
    /// session cookie.
    pub expires: Option<i64>,
    pub same_site: SameSite,
    pub priority: Priority,
}

impl Cookie {
    /// The address the cookie is set for: a secure cookie needs https; a
    /// domain cookie's leading dot is not part of a host.
    pub fn url(&self) -> String {
        let scheme = if self.secure { "https" } else { "http" };
        let host = self.domain.trim_start_matches('.');
        let host = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]")
        } else {
            host.to_owned()
        };
        let path = if self.path.starts_with('/') {
            self.path.as_str()
        } else {
            "/"
        };
        format!("{scheme}://{host}{path}")
    }
}

impl fmt::Debug for Cookie {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cookie")
            .field("name", &self.name)
            .field("domain", &self.domain)
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// Every installed browser profile with a cookie database: Chrome, Brave,
/// Arc, Edge, Firefox in that order, the profile in use first.
pub fn sources() -> Vec<Source> {
    // Debug builds can read made-up profiles instead of the person's own.
    #[cfg(debug_assertions)]
    if let Some(root) = std::env::var_os("HERDR_COOKIE_IMPORT_ROOT") {
        return sources_in(Path::new(&root));
    }
    let Some(home) = std::env::home_dir() else {
        return Vec::new();
    };
    sources_in(&home.join("Library/Application Support"))
}

fn sources_json(sources: &[Source]) -> serde_json::Value {
    serde_json::json!(
        sources
            .iter()
            .map(|s| serde_json::json!({ "id": s.id, "browser": s.browser, "profile": s.profile }))
            .collect::<Vec<_>>()
    )
}

fn sites_json(sites: &[Site]) -> serde_json::Value {
    serde_json::json!(
        sites
            .iter()
            .map(|s| serde_json::json!({ "domain": s.domain, "count": s.count }))
            .collect::<Vec<_>>()
    )
}
/// The source `sources()` lists under `id`.
pub fn source(id: &str) -> Option<Source> {
    sources().into_iter().find(|s| s.id == id)
}

/// The subcommands the dialogs run on the app's own binary, so the person's
/// profiles are listed in a process of their own.
pub const VERBS: [&str; 2] = ["__cookie-sources", "__cookie-sites"];

/// `__cookie-sources`: every source as `[{id, browser, profile}]`.
/// `__cookie-sites <source id>`: its sites as `[{domain, count}]`. JSON on
/// stdout, or the reason on stderr and exit code 1. Never reads a cookie value.
pub fn cli(verb: &str, args: &[String]) -> i32 {
    let run = || -> Result<serde_json::Value> {
        if verb == VERBS[0] {
            return Ok(sources_json(&sources()));
        }
        let id = args.first().context("needs a source id")?;
        let source = source(id).with_context(|| format!("no cookie source {id:?}"))?;
        Ok(sites_json(&sites(&source)?))
    };
    match run() {
        Ok(json) => {
            println!("{json}");
            0
        }
        Err(error) => {
            eprintln!("{error:#}");
            1
        }
    }
}

fn sources_in(support: &Path) -> Vec<Source> {
    let mut sources = Vec::new();
    for browser in &CHROMIUM_BROWSERS {
        sources.extend(chromium_sources(support, browser));
    }
    sources.extend(firefox_sources(support));
    sources
}

/// Registrable domains with live cookies, most cookies first. Reads no values
/// and asks nothing of the keychain.
pub fn sites(source: &Source) -> Result<Vec<Site>> {
    let db = Db::open_copy(&source.db)?;
    let now = now_micros();
    let mut counts: HashMap<String, usize> = HashMap::new();
    match source.engine {
        Engine::Chromium(_) => {
            let mut statement = db.conn.prepare(
                "SELECT host_key, COUNT(*) FROM cookies \
                 WHERE (expires_utc = 0 OR expires_utc > ?1) AND top_frame_site_key = '' \
                 GROUP BY host_key",
            )?;
            let rows = statement.query_map([now], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?;
            for row in rows {
                let (host, count) = row?;
                *counts.entry(registrable_domain(&host)).or_default() += count as usize;
            }
        }
        Engine::Firefox => {
            let mut statement = db
                .conn
                .prepare("SELECT host, expiry FROM moz_cookies WHERE originAttributes = ''")?;
            let rows = statement.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?;
            for row in rows {
                let (host, expiry) = row?;
                if firefox_expires(expiry) > now {
                    *counts.entry(registrable_domain(&host)).or_default() += 1;
                }
            }
        }
    }
    let mut sites: Vec<Site> = counts
        .into_iter()
        .map(|(domain, count)| Site { domain, count })
        .collect();
    sites.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.domain.cmp(&b.domain)));
    Ok(sites)
}

/// Decrypted, unexpired cookies of the given registrable domains. For a
/// Chromium browser this reads its keychain entry, which makes macOS ask the
/// person for access.
pub fn read(source: &Source, domains: &[String]) -> Result<Vec<Cookie>> {
    read_with(source, domains, keychain_password)
}

/// The keychain would not give out a browser's cookie key; the reason is
/// fit to show the person.
#[derive(Debug)]
pub struct Locked(pub String);

impl fmt::Display for Locked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Locked {}

fn keychain_password(keychain: &Keychain) -> Result<Vec<u8>> {
    security_framework::passwords::get_generic_password(keychain.service, keychain.account)
        .map_err(|error| Locked(locked_reason(error.code(), error.message())).into())
}

fn locked_reason(code: i32, message: Option<String>) -> String {
    match code {
        // User canceled, authorization failed, user interaction not allowed.
        -128 | -25293 | -25308 => "access denied".to_owned(),
        -25300 => "its key is not in the keychain".to_owned(),
        _ => message.unwrap_or_else(|| format!("keychain error {code}")),
    }
}

/// The password is fetched at most once, when the first cookie needs it.
fn read_with(
    source: &Source,
    domains: &[String],
    password: impl FnOnce(&Keychain) -> Result<Vec<u8>>,
) -> Result<Vec<Cookie>> {
    let db = Db::open_copy(&source.db)?;
    let wanted: HashSet<&str> = domains.iter().map(String::as_str).collect();
    let now = now_micros();
    match source.engine {
        Engine::Chromium(keychain) => read_chromium(&db.conn, &wanted, now, &keychain, password),
        Engine::Firefox => read_firefox(&db.conn, &wanted, now),
    }
}

fn read_chromium(
    conn: &Connection,
    wanted: &HashSet<&str>,
    now: i64,
    keychain: &Keychain,
    password: impl FnOnce(&Keychain) -> Result<Vec<u8>>,
) -> Result<Vec<Cookie>> {
    let version = meta_version(conn)?;
    // Partitioned cookies (non-empty top_frame_site_key) are left out: CEF's
    // set_cookie has no partition, so they would leak into first-party use.
    let mut statement = conn.prepare(
        "SELECT host_key, name, value, encrypted_value, path, expires_utc, \
         is_secure, is_httponly, samesite, priority FROM cookies \
         WHERE (expires_utc = 0 OR expires_utc > ?1) AND top_frame_site_key = ''",
    )?;
    let rows = statement.query_map([now], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Vec<u8>>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, i64>(6)?,
            row.get::<_, i64>(7)?,
            row.get::<_, i64>(8)?,
            row.get::<_, i64>(9)?,
        ))
    })?;
    let mut password = Some(password);
    let mut key: Option<[u8; 16]> = None;
    let mut undecryptable = 0usize;
    let mut cookies = Vec::new();
    for row in rows {
        let (host, name, plain, encrypted, path, expires, secure, http_only, same_site, priority) =
            row?;
        if !wanted.contains(registrable_domain(&host).as_str()) {
            continue;
        }
        let value = if !plain.is_empty() {
            plain
        } else if encrypted.is_empty() {
            String::new()
        } else {
            if key.is_none() {
                let password = password.take().expect("the key is derived once");
                key = Some(derive_key(&password(keychain)?));
            }
            let key = key.as_ref().expect("derived above");
            match decrypt(key, &host, &encrypted, version) {
                Some(value) => value,
                None => {
                    undecryptable += 1;
                    continue;
                }
            }
        };
        cookies.push(Cookie {
            name,
            value,
            domain: host,
            path,
            secure: secure != 0,
            http_only: http_only != 0,
            expires: (expires != 0).then_some(expires),
            same_site: match same_site {
                0 => SameSite::None,
                1 => SameSite::Lax,
                2 => SameSite::Strict,
                _ => SameSite::Unspecified,
            },
            priority: match priority {
                0 => Priority::Low,
                2 => Priority::High,
                _ => Priority::Medium,
            },
        });
    }
    if undecryptable > 0 {
        tracing::warn!(undecryptable, "cookies skipped: could not be decrypted");
    }
    Ok(cookies)
}

fn read_firefox(conn: &Connection, wanted: &HashSet<&str>, now: i64) -> Result<Vec<Cookie>> {
    let mut statement = conn.prepare(
        "SELECT host, name, value, path, expiry, isSecure, isHttpOnly, sameSite \
         FROM moz_cookies WHERE originAttributes = ''",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, i64>(6)?,
            row.get::<_, i64>(7)?,
        ))
    })?;
    let mut cookies = Vec::new();
    for row in rows {
        let (host, name, value, path, expiry, secure, http_only, same_site) = row?;
        let expires = firefox_expires(expiry);
        if expires <= now || !wanted.contains(registrable_domain(&host).as_str()) {
            continue;
        }
        cookies.push(Cookie {
            name,
            value,
            domain: host,
            path,
            secure: secure != 0,
            http_only: http_only != 0,
            expires: Some(expires),
            // A cookie without the attribute is stored as 256 (SAMESITE_UNSET).
            same_site: match same_site {
                0 => SameSite::None,
                1 => SameSite::Lax,
                2 => SameSite::Strict,
                _ => SameSite::Unspecified,
            },
            priority: Priority::Medium,
        });
    }
    Ok(cookies)
}

fn meta_version(conn: &Connection) -> Result<i64> {
    let version: String = conn
        .query_row("SELECT value FROM meta WHERE key = 'version'", [], |row| {
            row.get(0)
        })
        .context("read the cookie database version")?;
    version
        .parse()
        .with_context(|| format!("cookie database version {version:?}"))
}

fn derive_key(password: &[u8]) -> [u8; 16] {
    let mut key = [0u8; 16];
    pbkdf2::pbkdf2_hmac::<sha1::Sha1>(password, b"saltysalt", 1003, &mut key);
    key
}

/// None when the value is not a `v10` AES-128-CBC blob, or a version 24+
/// blob does not start with the hash of its host.
fn decrypt(key: &[u8; 16], host: &str, blob: &[u8], version: i64) -> Option<String> {
    let mut buffer = blob.strip_prefix(b"v10")?.to_vec();
    let plain = cbc::Decryptor::<Aes128>::new_from_slices(key, &[b' '; 16])
        .ok()?
        .decrypt_padded_mut::<Pkcs7>(&mut buffer)
        .ok()?;
    let plain = if version >= HOST_HASH_VERSION {
        let (hash, rest) = plain.split_at_checked(32)?;
        if hash != Sha256::digest(host.as_bytes()).as_slice() {
            return None;
        }
        rest
    } else {
        plain
    };
    String::from_utf8(plain.to_vec()).ok()
}

/// Microseconds since 1601-01-01 UTC.
pub fn now_micros() -> i64 {
    let since_1970 = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_micros() as i64);
    since_1970 + EPOCH_1601_TO_1970_SECS * 1_000_000
}

fn firefox_expires(expiry: i64) -> i64 {
    let micros_since_1970 = if expiry > FIREFOX_MILLISECONDS_FROM {
        expiry.saturating_mul(1000)
    } else {
        expiry.saturating_mul(1_000_000)
    };
    micros_since_1970.saturating_add(EPOCH_1601_TO_1970_SECS * 1_000_000)
}

/// eTLD+1 of a cookie host; a host with none (an IP address, `localhost`)
/// stands for itself.
pub fn registrable_domain(host: &str) -> String {
    let host = host.trim_start_matches('.').to_ascii_lowercase();
    let address = host.trim_start_matches('[').trim_end_matches(']');
    if address.parse::<IpAddr>().is_ok() {
        return host;
    }
    match psl::domain_str(&host) {
        Some(domain) => domain.to_owned(),
        None => host,
    }
}

fn chromium_sources(support: &Path, browser: &ChromiumBrowser) -> Vec<Source> {
    let root = support.join(browser.root);
    let Ok(state) = fs::read_to_string(root.join("Local State")) else {
        return Vec::new();
    };
    let state: serde_json::Value = match serde_json::from_str(&state) {
        Ok(state) => state,
        Err(error) => {
            tracing::warn!(browser = browser.label, %error, "unreadable Local State");
            return Vec::new();
        }
    };
    let profile = &state["profile"];
    let Some(names) = profile["info_cache"].as_object() else {
        return Vec::new();
    };
    let mut dirs: Vec<&str> = Vec::new();
    let mut rest: Vec<&str> = names.keys().map(String::as_str).collect();
    rest.sort_by_key(|dir| natural_key(dir));
    let ordered = profile["last_used"]
        .as_str()
        .into_iter()
        .chain(
            profile["profiles_order"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|dir| dir.as_str()),
        )
        .chain(rest);
    for dir in ordered {
        if names.contains_key(dir) && !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs.into_iter()
        .filter_map(|dir| {
            let dir_path = root.join(dir);
            let db = [dir_path.join("Network/Cookies"), dir_path.join("Cookies")]
                .into_iter()
                .find(|db| db.is_file())?;
            Some(Source {
                id: format!("{}:{dir}", browser.id),
                browser: browser.label.to_owned(),
                profile: names[dir]["name"].as_str().unwrap_or(dir).to_owned(),
                db,
                engine: Engine::Chromium(browser.keychain),
            })
        })
        .collect()
}

/// "Profile 10" sorts after "Profile 2".
fn natural_key(dir: &str) -> (&str, u64) {
    let stem = dir.trim_end_matches(|c: char| c.is_ascii_digit());
    (stem, dir[stem.len()..].parse().unwrap_or(0))
}

fn firefox_sources(support: &Path) -> Vec<Source> {
    let root = support.join("Firefox");
    let Ok(ini) = fs::read_to_string(root.join("profiles.ini")) else {
        return Vec::new();
    };
    let sections = parse_ini(&ini);
    let in_use: Vec<&str> = sections
        .iter()
        .filter(|(name, _)| name.starts_with("Install"))
        .filter_map(|(_, keys)| keys.get("Default").map(String::as_str))
        .collect();
    let mut profiles: Vec<(u8, Source)> = sections
        .iter()
        .filter(|(name, _)| name.starts_with("Profile"))
        .filter_map(|(_, keys)| {
            let path = keys.get("Path")?;
            let dir = if keys.get("IsRelative").is_some_and(|v| v == "1") {
                root.join(path)
            } else {
                PathBuf::from(path)
            };
            let db = dir.join("cookies.sqlite");
            if !db.is_file() {
                return None;
            }
            let dir_name = dir.file_name()?.to_string_lossy().into_owned();
            let rank = if in_use.contains(&path.as_str()) {
                0
            } else if keys.get("Default").is_some_and(|v| v == "1") {
                1
            } else {
                2
            };
            Some((
                rank,
                Source {
                    id: format!("firefox:{dir_name}"),
                    browser: "Firefox".to_owned(),
                    profile: keys.get("Name").cloned().unwrap_or(dir_name),
                    db,
                    engine: Engine::Firefox,
                },
            ))
        })
        .collect();
    profiles.sort_by_key(|(rank, _)| *rank);
    profiles.into_iter().map(|(_, source)| source).collect()
}

/// Sections in file order with their `key=value` lines.
fn parse_ini(text: &str) -> Vec<(String, HashMap<String, String>)> {
    let mut sections: Vec<(String, HashMap<String, String>)> = Vec::new();
    for line in text.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            sections.push((name.to_owned(), HashMap::new()));
        } else if let Some((key, value)) = line.split_once('=')
            && let Some((_, keys)) = sections.last_mut()
        {
            keys.insert(key.trim().to_owned(), value.trim().to_owned());
        }
    }
    sections
}

/// A private directory that removes itself, also when a read fails.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Result<Self> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!(
            "herdr-cookies-{}-{nanos}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .with_context(|| format!("create {}", dir.display()))?;
        Ok(Self(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A read-only connection to a private copy of a browser's database. The
/// browser may hold a lock on the original or keep recent writes in its WAL.
struct Db {
    // Closes before the scratch directory goes.
    conn: Connection,
    _scratch: Scratch,
}

impl Db {
    fn open_copy(original: &Path) -> Result<Self> {
        let scratch = Scratch::new()?;
        let name = original.file_name().context("database path has no name")?;
        let copy = scratch.0.join(name);
        fs::copy(original, &copy).with_context(|| format!("copy {}", original.display()))?;
        for suffix in ["-wal", "-shm"] {
            let mut sibling_name = name.to_owned();
            sibling_name.push(suffix);
            let sibling = original.with_file_name(&sibling_name);
            match fs::copy(&sibling, scratch.0.join(&sibling_name)) {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).with_context(|| format!("copy {}", sibling.display()));
                }
            }
        }
        let conn = Connection::open_with_flags(&copy, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("open a copy of {}", original.display()))?;
        Ok(Self {
            conn,
            _scratch: scratch,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use cbc::cipher::BlockEncryptMut;

    use super::*;

    const PASSWORD: &[u8] = b"peanuts";
    const NEVER: i64 = 99_000_000_000_000_000;
    const LONG_AGO: i64 = 1_000_000_000_000_000;

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
            .collect()
    }

    fn encrypt(host: &str, value: &str, version: i64) -> Vec<u8> {
        let mut plain = Vec::new();
        if version >= HOST_HASH_VERSION {
            plain.extend_from_slice(&Sha256::digest(host.as_bytes()));
        }
        plain.extend_from_slice(value.as_bytes());
        let len = plain.len();
        plain.resize(len + 16, 0);
        let encrypted =
            cbc::Encryptor::<Aes128>::new_from_slices(&derive_key(PASSWORD), &[b' '; 16])
                .unwrap()
                .encrypt_padded_mut::<Pkcs7>(&mut plain, len)
                .unwrap()
                .to_vec();
        [b"v10".as_slice(), &encrypted].concat()
    }

    struct ChromiumRow {
        host: &'static str,
        name: &'static str,
        plain: &'static str,
        encrypted: Vec<u8>,
        expires: i64,
        samesite: i64,
        priority: i64,
        partition: &'static str,
    }

    fn row(host: &'static str, name: &'static str, version: i64, value: &str) -> ChromiumRow {
        ChromiumRow {
            host,
            name,
            plain: "",
            encrypted: encrypt(host, value, version),
            expires: NEVER,
            samesite: -1,
            priority: 1,
            partition: "",
        }
    }

    fn chromium_db(path: &Path, version: i64, rows: &[ChromiumRow]) -> Connection {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE meta(key LONGVARCHAR NOT NULL UNIQUE PRIMARY KEY, value LONGVARCHAR);
             CREATE TABLE cookies(host_key TEXT NOT NULL, name TEXT NOT NULL, value TEXT NOT NULL,
               encrypted_value BLOB NOT NULL, path TEXT NOT NULL, expires_utc INTEGER NOT NULL,
               is_secure INTEGER NOT NULL, is_httponly INTEGER NOT NULL,
               samesite INTEGER NOT NULL, priority INTEGER NOT NULL,
               top_frame_site_key TEXT NOT NULL);",
        )
        .unwrap();
        conn.execute("INSERT INTO meta VALUES('version', ?1)", [version])
            .unwrap();
        for r in rows {
            conn.execute(
                "INSERT INTO cookies VALUES(?1, ?2, ?3, ?4, '/p', ?5, 1, 1, ?6, ?7, ?8)",
                rusqlite::params![
                    r.host,
                    r.name,
                    r.plain,
                    r.encrypted,
                    r.expires,
                    r.samesite,
                    r.priority,
                    r.partition
                ],
            )
            .unwrap();
        }
        conn
    }

    fn chrome_source(db: PathBuf) -> Source {
        Source {
            id: "chrome:Default".into(),
            browser: "Google Chrome".into(),
            profile: "Test".into(),
            db,
            engine: Engine::Chromium(CHROMIUM_BROWSERS[0].keychain),
        }
    }

    fn firefox_db(path: &Path, rows: &[(&str, &str, i64, &str)]) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE moz_cookies(host TEXT, name TEXT, value TEXT, path TEXT, expiry INTEGER,
               isSecure INTEGER, isHttpOnly INTEGER, sameSite INTEGER, originAttributes TEXT);",
        )
        .unwrap();
        for (host, name, expiry, origin) in rows {
            conn.execute(
                "INSERT INTO moz_cookies VALUES(?1, ?2, 'v', '/', ?3, 0, 1, 256, ?4)",
                rusqlite::params![host, name, expiry, origin],
            )
            .unwrap();
        }
    }

    fn firefox_source(db: PathBuf) -> Source {
        Source {
            id: "firefox:x".into(),
            browser: "Firefox".into(),
            profile: "default".into(),
            db,
            engine: Engine::Firefox,
        }
    }

    fn domains(list: &[&str]) -> Vec<String> {
        list.iter().map(|d| d.to_string()).collect()
    }

    fn password_once(calls: &Cell<u32>) -> impl FnOnce(&Keychain) -> Result<Vec<u8>> + '_ {
        move |_| {
            calls.set(calls.get() + 1);
            Ok(PASSWORD.to_vec())
        }
    }

    #[test]
    fn decrypts_blobs_made_by_an_independent_implementation() {
        // PBKDF2-HMAC-SHA1(peanuts, saltysalt, 1003) and AES-128-CBC from
        // python hashlib and openssl.
        assert_eq!(
            derive_key(PASSWORD),
            hex("d9a09d499b4e1b7461f28e67972c6dbd").as_slice()
        );
        let v24 = [
            b"v10".as_slice(),
            &hex("bfd1db3abcef87f1fa5a40b21d11eaaa9a030da41e8699c59583ed42e94a5d1c02bd2c873e0a078d7b895014a916985f"),
        ]
        .concat();
        let key = derive_key(PASSWORD);
        assert_eq!(
            decrypt(&key, ".example.com", &v24, 24).as_deref(),
            Some("hello=world")
        );
        let v23 = [b"v10".as_slice(), &hex("a76837b87d69926b3386b9c6a8fd76da")].concat();
        assert_eq!(
            decrypt(&key, "example.org", &v23, 23).as_deref(),
            Some("plain-old")
        );
    }

    #[test]
    fn round_trips_and_checks_the_host_hash() {
        let key = derive_key(PASSWORD);
        let blob = encrypt(".a.com", "tok=1", 24);
        assert_eq!(decrypt(&key, ".a.com", &blob, 24).as_deref(), Some("tok=1"));
        assert_eq!(decrypt(&key, ".b.com", &blob, 24), None);
        // A prefix-less blob under version 24 does not match, a prefixed
        // blob under version 23 keeps its prefix bytes.
        assert_eq!(
            decrypt(&key, ".a.com", &encrypt(".a.com", "tok=1", 23), 24),
            None
        );
        assert_eq!(decrypt(&key, "", b"v11abcdefghijklmnop", 24), None);
        assert_eq!(decrypt(&derive_key(b"wrong"), ".a.com", &blob, 24), None);
    }

    #[test]
    fn groups_by_registrable_domain() {
        assert_eq!(registrable_domain(".google.com"), "google.com");
        assert_eq!(registrable_domain("accounts.google.com"), "google.com");
        assert_eq!(registrable_domain("a.b.example.co.uk"), "example.co.uk");
        assert_eq!(registrable_domain("foo.github.io"), "foo.github.io");
        assert_eq!(registrable_domain("Bar.GitHub.io"), "bar.github.io");
        assert_eq!(registrable_domain("localhost"), "localhost");
        assert_eq!(registrable_domain("127.0.0.1"), "127.0.0.1");
        assert_eq!(registrable_domain("[::1]"), "[::1]");
    }

    #[test]
    fn firefox_expiry_is_seconds_or_milliseconds() {
        let unix_2030_secs = 1_900_000_000;
        let micros_1601 = (unix_2030_secs + EPOCH_1601_TO_1970_SECS) * 1_000_000;
        assert_eq!(firefox_expires(unix_2030_secs), micros_1601);
        assert_eq!(firefox_expires(unix_2030_secs * 1000), micros_1601);
        assert_eq!(firefox_expires(i64::MAX), i64::MAX);
        assert!(firefox_expires(1) < now_micros());
    }

    #[test]
    fn chromium_sites_count_live_cookies_per_registrable_domain() {
        let scratch = Scratch::new().unwrap();
        let path = scratch.0.join("Cookies");
        let mut expired = row(".old.com", "e", 24, "x");
        expired.expires = LONG_AGO;
        let mut session = row("www.shop.io", "s", 24, "x");
        session.expires = 0;
        let mut partitioned = row(".google.com", "a", 24, "embedded");
        partitioned.partition = "https://other.com";
        let _conn = chromium_db(
            &path,
            24,
            &[
                row(".google.com", "a", 24, "1"),
                row("accounts.google.com", "b", 24, "2"),
                row("foo.github.io", "c", 24, "3"),
                expired,
                session,
                partitioned,
            ],
        );
        let sites = sites(&chrome_source(path)).unwrap();
        let sites: Vec<_> = sites.iter().map(|s| (s.domain.as_str(), s.count)).collect();
        assert_eq!(
            sites,
            [("google.com", 2), ("foo.github.io", 1), ("shop.io", 1)]
        );
    }

    #[test]
    fn chromium_read_decrypts_filters_and_maps_fields() {
        let scratch = Scratch::new().unwrap();
        let path = scratch.0.join("Cookies");
        let mut plain = row(".google.com", "plain", 24, "unused");
        plain.plain = "visible";
        plain.encrypted = Vec::new();
        plain.samesite = 2;
        plain.priority = 2;
        let mut session = row("accounts.google.com", "session", 24, "s3");
        session.expires = 0;
        session.samesite = 0;
        session.priority = 0;
        let mut expired = row(".google.com", "expired", 24, "x");
        expired.expires = LONG_AGO;
        let mut forged = row(".google.com", "forged", 24, "x");
        forged.encrypted = encrypt(".other.com", "x", 24);
        let mut lax = row(".google.com", "lax", 24, "l");
        lax.samesite = 1;
        let _conn = chromium_db(
            &path,
            24,
            &[
                plain,
                session,
                expired,
                forged,
                lax,
                row(".other.org", "other", 24, "o"),
            ],
        );
        let calls = Cell::new(0);
        let mut cookies = read_with(
            &chrome_source(path),
            &domains(&["google.com"]),
            password_once(&calls),
        )
        .unwrap();
        cookies.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(calls.get(), 1);
        let got: Vec<_> = cookies
            .iter()
            .map(|c| {
                (
                    c.name.as_str(),
                    c.value.as_str(),
                    c.expires.is_some(),
                    c.same_site,
                    c.priority,
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("lax", "l", true, SameSite::Lax, Priority::Medium),
                ("plain", "visible", true, SameSite::Strict, Priority::High),
                ("session", "s3", false, SameSite::None, Priority::Low),
            ]
        );
        assert_eq!(cookies[1].domain, ".google.com");
        assert_eq!(cookies[1].path, "/p");
        assert_eq!(cookies[1].expires, Some(NEVER));
        assert!(cookies[1].secure && cookies[1].http_only);
        assert!(!format!("{:?}", cookies[1]).contains("visible"));
    }

    #[test]
    fn chromium_version_23_rows_have_no_host_hash() {
        let scratch = Scratch::new().unwrap();
        let path = scratch.0.join("Cookies");
        let _conn = chromium_db(&path, 23, &[row(".a.com", "old", 23, "legacy")]);
        let cookies = read_with(&chrome_source(path), &domains(&["a.com"]), |_| {
            Ok(PASSWORD.to_vec())
        })
        .unwrap();
        assert_eq!(cookies[0].value, "legacy");
    }

    #[test]
    fn the_keychain_is_asked_only_for_cookies_that_need_it() {
        let scratch = Scratch::new().unwrap();
        let path = scratch.0.join("Cookies");
        let mut plain = row(".a.com", "plain", 24, "");
        plain.plain = "visible";
        let _conn = chromium_db(&path, 24, &[plain, row(".b.com", "secret", 24, "s")]);
        let calls = Cell::new(0);
        let cookies = read_with(
            &chrome_source(path),
            &domains(&["a.com"]),
            password_once(&calls),
        )
        .unwrap();
        assert_eq!((cookies.len(), calls.get()), (1, 0));
    }

    #[test]
    fn a_keychain_refusal_fails_the_read() {
        let scratch = Scratch::new().unwrap();
        let path = scratch.0.join("Cookies");
        let _conn = chromium_db(&path, 24, &[row(".a.com", "secret", 24, "s")]);
        let result = read_with(&chrome_source(path), &domains(&["a.com"]), |_| {
            anyhow::bail!("denied")
        });
        assert!(result.is_err());
    }

    #[test]
    fn reads_uncheckpointed_wal() {
        let scratch = Scratch::new().unwrap();
        let path = scratch.0.join("Cookies");
        let conn = chromium_db(&path, 24, &[]);
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
        conn.execute(
            "INSERT INTO cookies VALUES('.wal.com', 'n', 'v', x'', '/', ?1, 0, 0, 1, 1, '')",
            [NEVER],
        )
        .unwrap();
        assert!(path.with_file_name("Cookies-wal").metadata().unwrap().len() > 0);
        assert_eq!(
            sites(&chrome_source(path)).unwrap(),
            [Site {
                domain: "wal.com".into(),
                count: 1
            }]
        );
        assert!(sites(&chrome_source(scratch.0.join("missing"))).is_err());
    }

    #[test]
    fn the_private_copy_is_owner_only_and_removed_with_its_connection() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = Scratch::new().unwrap();
        let path = scratch.0.join("Cookies");
        let _conn = chromium_db(&path, 24, &[]);
        let db = Db::open_copy(&path).unwrap();
        let copy = db._scratch.0.clone();
        assert_eq!(copy.metadata().unwrap().permissions().mode() & 0o777, 0o700);
        assert!(copy.join("Cookies").is_file());
        drop(db);
        assert!(!copy.exists());
    }

    #[test]
    fn firefox_reads_seconds_and_milliseconds_and_skips_containers() {
        let scratch = Scratch::new().unwrap();
        let path = scratch.0.join("cookies.sqlite");
        let future_secs = (now_micros() / 1_000_000 - EPOCH_1601_TO_1970_SECS) + 86_400;
        firefox_db(
            &path,
            &[
                ("www.mozilla.org", "secs", future_secs, ""),
                (".mozilla.org", "ms", future_secs * 1000, ""),
                (".mozilla.org", "container", future_secs, "^userContextId=2"),
                (".mozilla.org", "expired", 1_000, ""),
                (".mozilla.org", "expired_ms", 1_000_000_000_000, ""),
                (".other.net", "other", future_secs, ""),
            ],
        );
        let source = firefox_source(path);
        assert_eq!(
            sites(&source).unwrap(),
            [
                Site {
                    domain: "mozilla.org".into(),
                    count: 2
                },
                Site {
                    domain: "other.net".into(),
                    count: 1
                },
            ]
        );
        let calls = Cell::new(0);
        let mut cookies =
            read_with(&source, &domains(&["mozilla.org"]), password_once(&calls)).unwrap();
        cookies.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(calls.get(), 0);
        assert_eq!(
            cookies.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            ["ms", "secs"]
        );
        assert_eq!(cookies[0].expires, Some(firefox_expires(future_secs)));
        assert_eq!(cookies[1].expires, cookies[0].expires);
        assert_eq!(cookies[0].priority, Priority::Medium);
        assert_eq!(cookies[0].same_site, SameSite::Unspecified);
        assert!(cookies[0].http_only && !cookies[0].secure);
    }

    #[test]
    fn finds_chromium_profiles_by_their_shown_names() {
        let scratch = Scratch::new().unwrap();
        let support = &scratch.0;
        let root = support.join("Google/Chrome");
        for dir in ["Default", "Profile 2", "Profile 10", "Profile 1"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        fs::write(root.join("Default/Cookies"), "").unwrap();
        fs::create_dir_all(root.join("Profile 2/Network")).unwrap();
        fs::write(root.join("Profile 2/Network/Cookies"), "").unwrap();
        fs::write(root.join("Profile 2/Cookies"), "").unwrap();
        fs::write(root.join("Profile 10/Cookies"), "").unwrap();
        // Profile 1 has no cookie database, Profile 3 no directory.
        fs::write(
            root.join("Local State"),
            r#"{"profile":{"last_used":"Profile 2","info_cache":{
                "Default":{"name":"Andrej"},"Profile 1":{"name":"Empty"},
                "Profile 2":{"name":"Agents"},"Profile 3":{"name":"Gone"},"Profile 10":{}}}}"#,
        )
        .unwrap();
        let brave = support.join("BraveSoftware/Brave-Browser");
        fs::create_dir_all(brave.join("Default")).unwrap();
        fs::write(brave.join("Default/Cookies"), "").unwrap();
        fs::write(
            brave.join("Local State"),
            r#"{"profile":{"profiles_order":["Default"],"info_cache":{"Default":{"name":"Person 1"}}}}"#,
        )
        .unwrap();
        // Unreadable state is skipped, not fatal.
        fs::create_dir_all(support.join("Microsoft Edge")).unwrap();
        fs::write(support.join("Microsoft Edge/Local State"), "{").unwrap();

        let found: Vec<_> = sources_in(support)
            .into_iter()
            .map(|s| {
                (
                    s.id,
                    s.browser,
                    s.profile,
                    s.db.strip_prefix(support).unwrap().to_owned(),
                )
            })
            .collect();
        assert_eq!(
            found,
            [
                (
                    "chrome:Profile 2",
                    "Google Chrome",
                    "Agents",
                    "Google/Chrome/Profile 2/Network/Cookies"
                ),
                (
                    "chrome:Default",
                    "Google Chrome",
                    "Andrej",
                    "Google/Chrome/Default/Cookies"
                ),
                (
                    "chrome:Profile 10",
                    "Google Chrome",
                    "Profile 10",
                    "Google/Chrome/Profile 10/Cookies"
                ),
                (
                    "brave:Default",
                    "Brave",
                    "Person 1",
                    "BraveSoftware/Brave-Browser/Default/Cookies"
                ),
            ]
            .map(|(a, b, c, d)| (
                a.to_owned(),
                b.to_owned(),
                c.to_owned(),
                PathBuf::from(d)
            ))
        );
    }

    #[test]
    fn finds_firefox_profiles_and_puts_the_one_in_use_first() {
        let scratch = Scratch::new().unwrap();
        let support = &scratch.0;
        let root = support.join("Firefox");
        fs::create_dir_all(root.join("Profiles/aaa.old")).unwrap();
        fs::create_dir_all(root.join("Profiles/bbb.release")).unwrap();
        fs::create_dir_all(root.join("Profiles/ccc.nodb")).unwrap();
        fs::create_dir_all(support.join("elsewhere")).unwrap();
        for dir in ["Profiles/aaa.old", "Profiles/bbb.release"] {
            fs::write(root.join(dir).join("cookies.sqlite"), "").unwrap();
        }
        fs::write(support.join("elsewhere/cookies.sqlite"), "").unwrap();
        fs::write(
            root.join("profiles.ini"),
            format!(
                "[Profile2]\nName=abs\nIsRelative=0\nPath={}\n\n\
                 [Profile1]\nName=default\nIsRelative=1\nPath=Profiles/aaa.old\nDefault=1\n\n\
                 [Profile0]\nName=default-release\nIsRelative=1\nPath=Profiles/bbb.release\n\n\
                 [Profile3]\nName=nodb\nIsRelative=1\nPath=Profiles/ccc.nodb\n\n\
                 [General]\nVersion=2\n\n\
                 [Install1234]\nDefault=Profiles/bbb.release\nLocked=1\n",
                support.join("elsewhere").display()
            ),
        )
        .unwrap();
        let found: Vec<_> = sources_in(support)
            .into_iter()
            .map(|s| (s.id, s.profile))
            .collect();
        assert_eq!(
            found,
            [
                ("firefox:bbb.release", "default-release"),
                ("firefox:aaa.old", "default"),
                ("firefox:elsewhere", "abs"),
            ]
            .map(|(a, b)| (a.to_owned(), b.to_owned()))
        );
    }
    #[test]
    fn the_dialogs_get_sources_and_sites_as_json_without_paths_or_values() {
        let source = chrome_source(PathBuf::from("/secret/path/Cookies"));
        assert_eq!(
            sources_json(&[source]).to_string(),
            r#"[{"id":"chrome:Default","browser":"Google Chrome","profile":"Test"}]"#
        );
        let sites = [Site {
            domain: "a.com".into(),
            count: 3,
        }];
        assert_eq!(
            sites_json(&sites).to_string(),
            r#"[{"domain":"a.com","count":3}]"#
        );
    }

    #[test]
    fn a_keychain_refusal_says_why_to_the_person() {
        assert_eq!(locked_reason(-128, None), "access denied");
        assert_eq!(locked_reason(-25293, Some("x".into())), "access denied");
        assert_eq!(
            locked_reason(-25300, None),
            "its key is not in the keychain"
        );
        assert_eq!(locked_reason(-1, Some("odd".into())), "odd");
        assert_eq!(locked_reason(-1, None), "keychain error -1");
        let error: anyhow::Error = Locked("access denied".into()).into();
        assert_eq!(
            error.downcast_ref::<Locked>().map(|l| l.0.as_str()),
            Some("access denied")
        );
    }
    #[test]
    fn a_cookie_is_set_for_the_address_its_host_and_path_name() {
        let mut cookie = Cookie {
            name: "n".into(),
            value: "v".into(),
            domain: ".example.com".into(),
            path: "/a/b".into(),
            secure: true,
            http_only: false,
            expires: None,
            same_site: SameSite::Lax,
            priority: Priority::Medium,
        };
        assert_eq!(cookie.url(), "https://example.com/a/b");
        cookie.secure = false;
        cookie.domain = "127.0.0.1".into();
        cookie.path = "".into();
        assert_eq!(cookie.url(), "http://127.0.0.1/");
        cookie.domain = "::1".into();
        assert_eq!(cookie.url(), "http://[::1]/");
        cookie.domain = "[::1]".into();
        assert_eq!(cookie.url(), "http://[::1]/");
    }
}
