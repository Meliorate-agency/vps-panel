//! The server snapshot file: names, the manifest and index it carries, and
//! the age layer around it.
//!
//! A snapshot is `tar` -> `zstd` -> `age` (scrypt passphrase). Each layer is
//! a standard format, so a snapshot can be opened without the panel:
//! `age -d f | zstd -dc | tar -xpf -` (README.txt inside says the same).
//! The first tar entry is `manifest.json`, the last one `index.json`.

use age::secrecy::SecretString;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::{self, Read, Write};

/// What `manifest.kind` says, so another tar.zst.age is never taken for one.
pub const KIND: &str = "vps-panel-server-snapshot";

/// The layout version. Readers accept every format up to their own; fields
/// added later are optional (`#[serde(default)]`) and need no new number.
pub const FORMAT: u32 = 1;

pub const EXTENSION: &str = ".vpssnap.tar.zst.age";

/// Root-only. Never inside Drive (the Files page could hand it out around
/// the audited download) or the restic repository (operators copy that to
/// other machines).
pub const SNAPSHOT_DIR: &str = "/var/backups/vps-panel-snapshots";

/// Snapshots pulled from another server, waiting to be imported.
pub const INCOMING_DIR: &str = "/var/backups/vps-panel-snapshots/incoming";

/// scrypt work factor (2^18, what the age CLI picks on a typical machine:
/// about a second and 256 MiB to open). A reader accepts up to MAX_LOG_N, so
/// a crafted file cannot ask for hours of work and gigabytes of memory.
pub const LOG_N: u8 = 18;
pub const MAX_LOG_N: u8 = 20;

pub const MANIFEST: &str = "manifest.json";
pub const INDEX: &str = "index.json";

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Manifest {
    pub kind: String,
    pub format: u32,
    pub created_at: String,
    pub panel: PanelInfo,
    pub source: SourceInfo,
    pub platform: Platform,
    pub options: ExportOptions,
    pub crypto: CryptoInfo,
    pub inventory: Inventory,
    pub sections: Vec<SectionInfo>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct PanelInfo {
    /// Full commit of the panel that wrote the file.
    pub commit: String,
    /// The panel's source had local changes: the commit alone does not say
    /// what wrote the file.
    pub dirty: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct SourceInfo {
    pub hostname: String,
    pub panel_host: String,
    pub ipv4: Option<String>,
    pub ipv6: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Platform {
    pub os: String,
    pub os_version: String,
    pub arch: String,
    pub mariadb: Option<String>,
    pub postgres: Option<String>,
    pub node: Option<String>,
    pub docker: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct ExportOptions {
    pub include_drive: bool,
    /// Containers frozen while their volumes were copied.
    pub paused_containers: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct CryptoInfo {
    pub scheme: String,
    pub log_n: u8,
    pub compression: String,
}

/// What the server hosted, for the import preview. The import itself reads
/// the database dumps, panel.db and system/*.json, not this.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Inventory {
    pub apps: Vec<String>,
    pub sites: Vec<String>,
    pub certs: Vec<String>,
    pub dns_zones: Vec<String>,
    pub mail_domains: Vec<String>,
    pub mailboxes: u64,
    pub mariadb_databases: Vec<String>,
    pub mariadb_users: u64,
    pub postgres_databases: Vec<String>,
    pub cron_jobs: u64,
    pub system_users: Vec<String>,
    pub panel_users: u64,
    pub docker_volumes: Vec<String>,
    pub containers: Vec<String>,
    pub tailscale: bool,
    /// Services set up by hand (a unit in /etc/systemd/system no package
    /// and no panel feature made): their program, settings and data are
    /// unknown to the panel, so they are not moved; the import names them.
    pub foreign_services: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct SectionInfo {
    pub id: String,
    pub title: String,
    pub paths: Vec<String>,
    pub est_bytes: u64,
    pub est_files: u64,
}

/// The last entry: what was actually written.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Index {
    pub completed: bool,
    pub finished_at: String,
    pub sections: Vec<SectionDone>,
    /// sha256 of each database dump and system file (db/..., panel/...,
    /// system/...), checked by the import before it uses them.
    pub entries: BTreeMap<String, EntryHash>,
    pub warnings: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct SectionDone {
    pub id: String,
    pub bytes: u64,
    pub files: u64,
    /// Sockets, fifos, devices, unreadable files, other filesystems.
    pub skipped: u64,
    /// Files that changed while they were copied (the copy holds the size
    /// they had when opened).
    pub changed: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct EntryHash {
    pub bytes: u64,
    pub sha256: String,
}

/// `<stem>.json` next to a snapshot: what the list shows without opening
/// the file (the passphrase is not kept anywhere).
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Sidecar {
    pub name: String,
    pub kind: String,
    pub format: u32,
    pub size: u64,
    pub sha256: String,
    pub created_at: String,
    pub panel_commit: String,
    pub panel_dirty: bool,
    pub source_host: String,
    pub include_drive: bool,
    pub sections: Vec<SectionDone>,
    /// "local" (made here) or "pulled" (fetched from another server).
    pub origin: String,
}

// ------------------------------------------------------------------ names

/// The host part of a file name: lowercase letters, digits and dashes.
pub fn host_slug(host: &str) -> String {
    let mut out = String::new();
    for c in host.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches('-');
    let mut out: String = out.chars().take(40).collect();
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "server".to_string()
    } else {
        out
    }
}

pub fn stem_for(host: &str, now: chrono::DateTime<chrono::Utc>) -> String {
    format!("{}-{}Z", host_slug(host), now.format("%Y%m%d-%H%M%S"))
}

/// `^[a-z0-9][a-z0-9-]{0,40}-\d{8}-\d{6}Z$`. A caller never names a path:
/// only a stem of this shape, joined to SNAPSHOT_DIR.
pub fn is_valid_stem(s: &str) -> bool {
    if !s.is_ascii() {
        return false;
    }
    let Some(head) = s.strip_suffix('Z') else { return false };
    // head = <host>-YYYYMMDD-HHMMSS
    if head.len() < 1 + 1 + 8 + 1 + 6 {
        return false;
    }
    let (host_dash, stamp) = head.split_at(head.len() - 15);
    let stamp = stamp.as_bytes();
    let stamp_ok = stamp[..8].iter().all(u8::is_ascii_digit)
        && stamp[8] == b'-'
        && stamp[9..].iter().all(u8::is_ascii_digit);
    let Some(host) = host_dash.strip_suffix('-') else { return false };
    let host_ok = !host.is_empty()
        && host.len() <= 41
        && host.bytes().next().is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && host.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    stamp_ok && host_ok
}

pub fn file_name(stem: &str) -> String {
    format!("{stem}{EXTENSION}")
}

/// The stem of a snapshot file name, or None for anything else.
pub fn stem_of(file_name: &str) -> Option<&str> {
    file_name.strip_suffix(EXTENSION).filter(|s| is_valid_stem(s))
}

// ------------------------------------------------------------- passphrase

/// At least 12 characters with three kinds of characters (lowercase,
/// uppercase, digits, others), or at least 20 of any kind. The file holds
/// every secret of the server and can be copied anywhere; the passphrase is
/// all that protects it.
pub fn check_passphrase(p: &str) -> Result<(), String> {
    let len = p.chars().count();
    if len < 12 {
        return Err("the passphrase needs at least 12 characters".into());
    }
    if len >= 20 {
        return Ok(());
    }
    let kinds = [
        p.chars().any(|c| c.is_lowercase()),
        p.chars().any(|c| c.is_uppercase()),
        p.chars().any(|c| c.is_ascii_digit()),
        p.chars().any(|c| !c.is_alphanumeric()),
    ]
    .iter()
    .filter(|k| **k)
    .count();
    if kinds < 3 {
        return Err(
            "a passphrase under 20 characters needs three of: lowercase, uppercase, digits, other characters"
                .into(),
        );
    }
    Ok(())
}

// ------------------------------------------------------------- age layer

/// Encrypts everything written to it for `passphrase`. `finish()` on the
/// returned writer writes the last chunk, which is what lets a reader tell a
/// complete file from a cut one.
pub fn encrypt_writer<W: Write>(
    out: W,
    passphrase: &str,
    log_n: u8,
) -> io::Result<age::stream::StreamWriter<W>> {
    let mut recipient = age::scrypt::Recipient::new(SecretString::from(passphrase.to_owned()));
    recipient.set_work_factor(log_n);
    let encryptor = age::Encryptor::with_recipients(std::iter::once(&recipient as &dyn age::Recipient))
        .map_err(|e| io::Error::other(format!("age: {e}")))?;
    encryptor.wrap_output(out)
}

/// Why a snapshot could not be opened, in words for the operator.
#[derive(Debug)]
pub enum OpenError {
    /// The passphrase does not open it.
    Passphrase,
    /// Not an age file, or one not made for a passphrase.
    NotASnapshot(String),
    /// It asks for more scrypt work than MAX_LOG_N allows.
    TooExpensive,
    Io(io::Error),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::Passphrase => write!(f, "the passphrase does not open this snapshot"),
            OpenError::NotASnapshot(why) => write!(f, "this is not a server snapshot file ({why})"),
            OpenError::TooExpensive => write!(
                f,
                "this file asks for more decryption work than a snapshot ever uses; it was not made by the panel"
            ),
            OpenError::Io(e) => write!(f, "reading the snapshot failed: {e}"),
        }
    }
}

impl std::error::Error for OpenError {}

pub fn decrypt_reader<R: Read>(
    input: R,
    passphrase: &str,
) -> Result<age::stream::StreamReader<R>, OpenError> {
    let decryptor = age::Decryptor::new(input).map_err(|e| match e {
        // A file too short or too odd to hold an age header comes back as
        // an I/O error from the header parser.
        age::DecryptError::Io(e)
            if matches!(e.kind(), io::ErrorKind::UnexpectedEof | io::ErrorKind::InvalidData) =>
        {
            OpenError::NotASnapshot("it does not start like an encrypted snapshot".into())
        }
        age::DecryptError::Io(e) => OpenError::Io(e),
        other => OpenError::NotASnapshot(other.to_string()),
    })?;
    if !decryptor.is_scrypt() {
        return Err(OpenError::NotASnapshot("it is not encrypted with a passphrase".into()));
    }
    let mut identity = age::scrypt::Identity::new(SecretString::from(passphrase.to_owned()));
    identity.set_max_work_factor(MAX_LOG_N);
    decryptor
        .decrypt(std::iter::once(&identity as &dyn age::Identity))
        .map_err(|e| match e {
            age::DecryptError::DecryptionFailed | age::DecryptError::NoMatchingKeys => OpenError::Passphrase,
            age::DecryptError::ExcessiveWork { .. } => OpenError::TooExpensive,
            age::DecryptError::Io(e) => OpenError::Io(e),
            other => OpenError::NotASnapshot(other.to_string()),
        })
}

// ----------------------------------------------------------------- hashing

/// Counts and hashes the bytes on their way to the file: the sha256 of the
/// whole snapshot without reading it back.
pub struct HashWriter<W> {
    inner: W,
    hasher: Sha256,
    bytes: u64,
    /// The same count, readable from outside the layers stacked on top
    /// (age's writer does not hand out what it wraps).
    counter: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl<W: Write> HashWriter<W> {
    pub fn new(inner: W) -> Self {
        HashWriter { inner, hasher: Sha256::new(), bytes: 0, counter: Default::default() }
    }

    pub fn counter(&self) -> std::sync::Arc<std::sync::atomic::AtomicU64> {
        self.counter.clone()
    }

    pub fn into_parts(self) -> (W, u64, String) {
        (self.inner, self.bytes, hex::encode(self.hasher.finalize()))
    }
}

impl<W: Write> Write for HashWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        self.bytes += n as u64;
        self.counter.store(self.bytes, std::sync::atomic::Ordering::Relaxed);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// sha256 + size of a file, streamed.
pub fn hash_file(path: &std::path::Path) -> io::Result<EntryHash> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut bytes = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        bytes += n as u64;
    }
    Ok(EntryHash { bytes, sha256: hex::encode(hasher.finalize()) })
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stems_round_trip_and_reject_paths() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-11-27T13:05:09Z").unwrap().with_timezone(&chrono::Utc);
        let stem = stem_for("Panel.Example.com", now);
        assert_eq!(stem, "panel-example-com-20261127-130509Z");
        assert!(is_valid_stem(&stem));
        assert_eq!(stem_of(&file_name(&stem)), Some(stem.as_str()));
        for bad in [
            "",
            "../etc-20261127-130509Z",
            "Panel-20261127-130509Z",
            "-x-20261127-130509Z",
            "x-20261127-130509",
            "x-2026112-130509Z",
            "x_y-20261127-130509Z",
            "x/y-20261127-130509Z",
        ] {
            assert!(!is_valid_stem(bad), "{bad}");
        }
        assert_eq!(stem_of("x-20261127-130509Z.tar"), None);
        assert_eq!(host_slug("..."), "server");
        assert_eq!(host_slug(&"a".repeat(80)).len(), 40);
    }

    #[test]
    fn passphrase_rules() {
        assert!(check_passphrase("short1A!").is_err());
        assert!(check_passphrase("alllowercaseletters").is_err());
        assert!(check_passphrase("Lowercase12345").is_ok());
        assert!(check_passphrase("twenty characters long").is_ok());
    }

    #[test]
    fn age_round_trip_and_wrong_passphrase() {
        let mut out = Vec::new();
        {
            let mut w = encrypt_writer(&mut out, "Correct horse 42", 10).unwrap();
            w.write_all(b"hello snapshot").unwrap();
            w.finish().unwrap();
        }
        let mut r = decrypt_reader(&out[..], "Correct horse 42").unwrap();
        let mut plain = String::new();
        r.read_to_string(&mut plain).unwrap();
        assert_eq!(plain, "hello snapshot");
        assert!(matches!(decrypt_reader(&out[..], "Wrong horse 42"), Err(OpenError::Passphrase)));
        assert!(matches!(decrypt_reader(&b"not age"[..], "x"), Err(OpenError::NotASnapshot(_))));
    }

    #[test]
    fn manifest_reads_unknown_and_missing_fields() {
        let m: Manifest = serde_json::from_str(r#"{"kind":"vps-panel-server-snapshot","format":1,"future":{"x":1}}"#).unwrap();
        assert_eq!(m.format, 1);
        assert!(m.sections.is_empty());
    }
}
