//! Writing and reading the snapshot stream: tar -> zstd -> age -> file.
//!
//! Everything here is blocking I/O; the export runs it in spawn_blocking.
//! Memory stays flat whatever the size: each layer streams.

use super::format::{self, HashWriter, OpenError};
use super::plan::{self, Excludes, Found, Root};
use std::collections::{HashMap, HashSet};
use std::io::{self, BufReader, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use tar::{EntryType, Header};

type Sink<W> = zstd::stream::write::Encoder<'static, age::stream::StreamWriter<HashWriter<W>>>;

/// Totals of one section as written.
#[derive(Clone, Debug, Default)]
pub struct TreeStats {
    pub bytes: u64,
    pub files: u64,
    pub skipped: u64,
    pub changed: u64,
    /// The first few paths worth telling the operator about.
    pub notes: Vec<String>,
}

const MAX_NOTES: usize = 20;

impl TreeStats {
    fn note(&mut self, msg: String) {
        if self.notes.len() < MAX_NOTES {
            self.notes.push(msg);
        }
    }
}

/// uid/gid -> name, looked up once each. The import maps owners by name:
/// `apps`, `_rspamd` and the /users accounts have other numbers on another
/// server.
#[derive(Default)]
struct Names {
    users: HashMap<u32, Option<String>>,
    groups: HashMap<u32, Option<String>>,
}

impl Names {
    fn user(&mut self, uid: u32) -> Option<String> {
        self.users
            .entry(uid)
            .or_insert_with(|| {
                nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid)).ok().flatten().map(|u| u.name)
            })
            .clone()
    }

    fn group(&mut self, gid: u32) -> Option<String> {
        self.groups
            .entry(gid)
            .or_insert_with(|| {
                nix::unistd::Group::from_gid(nix::unistd::Gid::from_raw(gid)).ok().flatten().map(|g| g.name)
            })
            .clone()
    }
}

/// Where a path on disk goes in the archive: `fs/<path without its leading slash>`.
pub fn fs_entry(path: &Path) -> PathBuf {
    Path::new("fs").join(path.strip_prefix("/").unwrap_or(path))
}

pub struct SnapshotWriter<W: Write> {
    tar: tar::Builder<Sink<W>>,
    names: Names,
    /// Scratch folder for consistent SQLite copies; None to copy them raw.
    sqlite_staging: Option<PathBuf>,
    written: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl<W: Write> SnapshotWriter<W> {
    pub fn new(out: W, passphrase: &str, log_n: u8, threads: u32) -> io::Result<Self> {
        let hashed = HashWriter::new(out);
        let written = hashed.counter();
        let age = format::encrypt_writer(hashed, passphrase, log_n)?;
        let mut zstd = zstd::stream::write::Encoder::new(age, 3)?;
        zstd.include_checksum(true)?;
        if threads > 1 {
            zstd.multithread(threads)?;
        }
        let mut tar = tar::Builder::new(zstd);
        tar.mode(tar::HeaderMode::Complete);
        tar.follow_symlinks(false);
        Ok(SnapshotWriter { tar, names: Names::default(), sqlite_staging: None, written })
    }

    pub fn copy_sqlite_via(&mut self, staging: PathBuf) {
        self.sqlite_staging = Some(staging);
    }

    /// Bytes written to the file so far (after compression and encryption).
    pub fn written(&self) -> u64 {
        self.written.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The same count, readable while a tree is being appended.
    pub fn written_counter(&self) -> std::sync::Arc<std::sync::atomic::AtomicU64> {
        self.written.clone()
    }

    /// A generated file (manifest, README, system/*.json), root's, 0600.
    pub fn append_bytes(&mut self, path: &str, data: &[u8]) -> io::Result<()> {
        let mut h = Header::new_gnu();
        h.set_entry_type(EntryType::Regular);
        h.set_size(data.len() as u64);
        h.set_mode(0o600);
        h.set_uid(0);
        h.set_gid(0);
        let _ = h.set_username("root");
        let _ = h.set_groupname("root");
        h.set_mtime(chrono::Utc::now().timestamp().max(0) as u64);
        self.tar.append_data(&mut h, path, data)
    }

    /// A staged file (a dump, panel.db) under `archive_path`. It is ours and
    /// nobody writes it any more, so its size is final.
    pub fn append_staged(&mut self, archive_path: &str, src: &Path) -> io::Result<()> {
        let f = std::fs::File::open(src)?;
        let meta = f.metadata()?;
        let mut h = Header::new_gnu();
        h.set_entry_type(EntryType::Regular);
        h.set_size(meta.len());
        h.set_mode(0o600);
        h.set_uid(0);
        h.set_gid(0);
        let _ = h.set_username("root");
        let _ = h.set_groupname("root");
        h.set_mtime(meta.mtime().max(0) as u64);
        self.tar.append_data(&mut h, archive_path, BufReader::with_capacity(256 * 1024, f))
    }

    /// One root of a section, as `fs/<path>` entries. A file that cannot be
    /// read is skipped and noted, never an error: only failing to WRITE the
    /// snapshot (a full disk) stops it. `progress` is called with the bytes
    /// copied so far in this section; it returns an error to stop the export.
    pub fn append_tree(
        &mut self,
        root: &Root,
        excludes: &Excludes,
        stats: &mut TreeStats,
        progress: &mut dyn FnMut(u64) -> io::Result<()>,
    ) -> io::Result<()> {
        let mut failure: Option<io::Error> = None;
        // -wal / -shm / -journal of a database copied consistently.
        let mut sqlite_siblings: HashSet<PathBuf> = HashSet::new();
        plan::walk(root, excludes, &mut |found| {
            let result = match found {
                Found::Dir(p, m) => self.append_dir(p, m).map(|_| stats.files += 1),
                Found::Symlink(p, m) => match std::fs::read_link(p) {
                    Ok(target) => self.append_symlink(p, m, &target).map(|_| stats.files += 1),
                    Err(e) => {
                        stats.skipped += 1;
                        stats.note(format!("skipped {} (link unreadable: {e})", p.display()));
                        Ok(())
                    }
                },
                Found::File(p, m) => {
                    if sqlite_siblings.remove(p) {
                        return true;
                    }
                    self.append_file(p, m, stats, &mut sqlite_siblings)
                        .and_then(|_| progress(stats.bytes))
                }
                Found::Special(p) => {
                    stats.skipped += 1;
                    let _ = p;
                    Ok(())
                }
                Found::OtherFilesystem(p) => {
                    stats.skipped += 1;
                    stats.note(format!("not copied: {} is on another filesystem", p.display()));
                    Ok(())
                }
                Found::Unreadable(p, e) => {
                    stats.skipped += 1;
                    stats.note(format!("skipped {} ({e})", p.display()));
                    Ok(())
                }
            };
            match result {
                Ok(()) => true,
                Err(e) => {
                    failure = Some(e);
                    false
                }
            }
        });
        match failure {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn header(&mut self, meta: &std::fs::Metadata, kind: EntryType) -> Header {
        let mut h = Header::new_gnu();
        h.set_entry_type(kind);
        h.set_mode(meta.mode() & 0o7777);
        h.set_uid(meta.uid() as u64);
        h.set_gid(meta.gid() as u64);
        h.set_mtime(meta.mtime().max(0) as u64);
        h.set_size(0);
        if let Some(name) = self.names.user(meta.uid()) {
            let _ = h.set_username(&name);
        }
        if let Some(name) = self.names.group(meta.gid()) {
            let _ = h.set_groupname(&name);
        }
        h
    }

    fn append_dir(&mut self, path: &Path, meta: &std::fs::Metadata) -> io::Result<()> {
        let mut h = self.header(meta, EntryType::Directory);
        self.tar.append_data(&mut h, fs_entry(path), io::empty())
    }

    fn append_symlink(&mut self, path: &Path, meta: &std::fs::Metadata, target: &Path) -> io::Result<()> {
        let mut h = self.header(meta, EntryType::Symlink);
        self.tar.append_link(&mut h, fs_entry(path), target)
    }

    fn append_file(
        &mut self,
        path: &Path,
        listed: &std::fs::Metadata,
        stats: &mut TreeStats,
        sqlite_siblings: &mut HashSet<PathBuf>,
    ) -> io::Result<()> {
        // O_NOFOLLOW: a file swapped for a link after the listing is not
        // followed to wherever it points.
        let file = match std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(nix::fcntl::OFlag::O_NOFOLLOW.bits() | nix::fcntl::OFlag::O_NONBLOCK.bits())
            .open(path)
        {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                stats.skipped += 1;
                stats.note(format!("skipped {} ({e})", path.display()));
                return Ok(());
            }
        };
        // The header is built from the open file, not the listing: that is
        // the size the copy below delivers, whatever happens to the file
        // meanwhile.
        let meta = match file.metadata() {
            Ok(m) if m.is_file() => m,
            _ => {
                stats.skipped += 1;
                return Ok(());
            }
        };
        let _ = listed;

        if self.sqlite_staging.is_some() && is_sqlite(&file) {
            if let Some(copy) = self.sqlite_copy(path) {
                let result = self.append_file_copy(path, &meta, &copy, stats);
                let _ = std::fs::remove_file(&copy);
                if result.is_ok() {
                    for suffix in ["-wal", "-shm", "-journal"] {
                        let mut s = path.as_os_str().to_owned();
                        s.push(suffix);
                        sqlite_siblings.insert(PathBuf::from(s));
                    }
                }
                return result;
            }
        }

        let size = meta.len();
        let mut h = self.header(&meta, EntryType::Regular);
        h.set_size(size);
        let mut reader = ExactReader::new(BufReader::with_capacity(256 * 1024, file), size);
        self.tar.append_data(&mut h, fs_entry(path), &mut reader)?;
        stats.bytes += size;
        stats.files += 1;
        if reader.short || reader.error.is_some() {
            stats.changed += 1;
            let why = match &reader.error {
                Some(e) => format!("read error: {e}"),
                None => "it shrank while being copied".to_string(),
            };
            stats.note(format!("{}: {why}; the copy is padded to its size", path.display()));
        } else if std::fs::symlink_metadata(path)
            .map(|now| now.len() != size || now.mtime() != meta.mtime() || now.mtime_nsec() != meta.mtime_nsec())
            .unwrap_or(false)
        {
            stats.changed += 1;
            stats.note(format!("{} changed while being copied", path.display()));
        }
        Ok(())
    }

    /// A consistent copy of a live SQLite database (VACUUM INTO), or None to
    /// copy the file as it is (not a database after all, encrypted, or a
    /// WAL database without its -shm that a read-only open cannot use).
    fn sqlite_copy(&self, path: &Path) -> Option<PathBuf> {
        let staging = self.sqlite_staging.as_ref()?;
        let target = staging.join(format!("sqlite-{}.db", uuid::Uuid::new_v4()));
        let conn = rusqlite::Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .ok()?;
        let _ = conn.busy_timeout(std::time::Duration::from_secs(5));
        match conn.execute("VACUUM INTO ?1", rusqlite::params![target.to_string_lossy()]) {
            Ok(_) => Some(target),
            Err(_) => {
                let _ = std::fs::remove_file(&target);
                None
            }
        }
    }

    /// `copy` stands in for `path`: its bytes, the original's owner, mode
    /// and time.
    fn append_file_copy(
        &mut self,
        path: &Path,
        meta: &std::fs::Metadata,
        copy: &Path,
        stats: &mut TreeStats,
    ) -> io::Result<()> {
        let f = std::fs::File::open(copy)?;
        let size = f.metadata()?.len();
        let mut h = self.header(meta, EntryType::Regular);
        h.set_size(size);
        self.tar.append_data(&mut h, fs_entry(path), BufReader::with_capacity(256 * 1024, f))?;
        stats.bytes += size;
        stats.files += 1;
        Ok(())
    }

    /// Ends the tar stream, the zstd frame and the age stream, in that
    /// order. Returns the file writer, the bytes written and their sha256.
    pub fn finish(self) -> io::Result<(W, u64, String)> {
        let zstd = self.tar.into_inner()?;
        let age = zstd.finish()?;
        let hashed = age.finish()?;
        Ok(hashed.into_parts())
    }
}

fn is_sqlite(file: &std::fs::File) -> bool {
    use std::os::unix::fs::FileExt;
    let mut head = [0u8; 16];
    matches!(file.read_at(&mut head, 0), Ok(16)) && &head == b"SQLite format 3\0"
}

/// Delivers exactly `size` bytes: what the file holds up to its size when
/// it was opened, zeros for whatever has gone since. A tar entry's header
/// promises a size; delivering fewer or more bytes would corrupt every
/// entry after it.
struct ExactReader<R> {
    inner: R,
    remaining: u64,
    short: bool,
    error: Option<io::Error>,
}

impl<R: Read> ExactReader<R> {
    fn new(inner: R, size: u64) -> Self {
        ExactReader { inner, remaining: size, short: false, error: None }
    }
}

impl<R: Read> Read for ExactReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 || buf.is_empty() {
            return Ok(0);
        }
        let want = buf.len().min(self.remaining.min(usize::MAX as u64) as usize);
        let n = if self.short {
            0
        } else {
            loop {
                match self.inner.read(&mut buf[..want]) {
                    Ok(n) => break n,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => {
                        self.error = Some(e);
                        self.short = true;
                        break 0;
                    }
                }
            }
        };
        let n = if n == 0 {
            self.short = true;
            buf[..want].fill(0);
            want
        } else {
            n
        };
        self.remaining -= n as u64;
        Ok(n)
    }
}

// --------------------------------------------------------------- reading

pub type SnapshotArchive<R> =
    tar::Archive<zstd::stream::read::Decoder<'static, BufReader<age::stream::StreamReader<R>>>>;

/// Opens a snapshot for reading. A wrong passphrase fails here.
pub fn open<R: Read>(input: R, passphrase: &str) -> Result<SnapshotArchive<R>, OpenError> {
    let plain = format::decrypt_reader(input, passphrase)?;
    let zstd = zstd::stream::read::Decoder::new(plain).map_err(OpenError::Io)?;
    Ok(tar::Archive::new(zstd))
}

/// The manifest, which is the first entry. Reads only the start of the file.
pub fn read_manifest(path: &Path, passphrase: &str) -> Result<format::Manifest, OpenError> {
    let file = std::fs::File::open(path).map_err(OpenError::Io)?;
    let mut archive = open(BufReader::with_capacity(256 * 1024, file), passphrase)?;
    let mut entries = archive.entries().map_err(OpenError::Io)?;
    let Some(first) = entries.next() else {
        return Err(OpenError::NotASnapshot("it is empty".into()));
    };
    let mut first = first.map_err(OpenError::Io)?;
    let name = first.path().map_err(OpenError::Io)?.to_string_lossy().into_owned();
    if name != format::MANIFEST {
        return Err(OpenError::NotASnapshot(format!("its first entry is {name}, not the manifest")));
    }
    if first.header().size().unwrap_or(0) > 16 * 1024 * 1024 {
        return Err(OpenError::NotASnapshot("its manifest is too large".into()));
    }
    let mut text = String::new();
    first.read_to_string(&mut text).map_err(OpenError::Io)?;
    let manifest: format::Manifest =
        serde_json::from_str(&text).map_err(|e| OpenError::NotASnapshot(format!("unreadable manifest: {e}")))?;
    if manifest.kind != format::KIND {
        return Err(OpenError::NotASnapshot(format!("its manifest says it is a {:?}", manifest.kind)));
    }
    Ok(manifest)
}

/// Reads the whole file through every layer (a full check: the age chunks'
/// tags, the zstd checksum, the tar framing) and returns the index, which is
/// the last entry. A cut, damaged or altered file fails here.
pub fn verify(path: &Path, passphrase: &str) -> Result<format::Index, OpenError> {
    let file = std::fs::File::open(path).map_err(OpenError::Io)?;
    let mut archive = open(BufReader::with_capacity(256 * 1024, file), passphrase)?;
    let mut index: Option<format::Index> = None;
    let mut last = String::new();
    for entry in archive.entries().map_err(OpenError::Io)? {
        let mut entry = entry.map_err(OpenError::Io)?;
        last = entry.path().map_err(OpenError::Io)?.to_string_lossy().into_owned();
        if last == format::INDEX {
            let mut text = String::new();
            entry.read_to_string(&mut text).map_err(OpenError::Io)?;
            index = serde_json::from_str(&text).ok();
        } else {
            io::copy(&mut entry, &mut io::sink()).map_err(OpenError::Io)?;
        }
    }
    match index {
        Some(i) if last == format::INDEX && i.completed => Ok(i),
        _ => Err(OpenError::NotASnapshot("it is incomplete (no index at its end)".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::plan::ChildFilter;
    use std::os::unix::ffi::OsStrExt;

    const PASS: &str = "Test passphrase 1";

    fn build_tree(root: &Path) {
        std::fs::create_dir_all(root.join("empty")).unwrap();
        std::fs::create_dir_all(root.join("deep/".to_string() + &"d".repeat(120))).unwrap();
        std::fs::write(root.join("zero"), b"").unwrap();
        std::fs::write(root.join("secret"), b"top").unwrap();
        std::fs::set_permissions(root.join("secret"), std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
        std::fs::write(root.join("deep/".to_string() + &"d".repeat(120) + "/long"), b"long name").unwrap();
        let odd = std::ffi::OsStr::from_bytes(b"odd-\xff-name");
        std::fs::write(root.join(odd), b"bytes").unwrap();
        std::os::unix::fs::symlink("secret", root.join("link")).unwrap();
        nix::unistd::mkfifo(&root.join("fifo"), nix::sys::stat::Mode::from_bits_truncate(0o600)).unwrap();
        let db = rusqlite::Connection::open(root.join("app.db")).unwrap();
        db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE t(x); INSERT INTO t VALUES (42);").unwrap();
        std::mem::forget(db); // keep the WAL open like a running app
    }

    fn write_snapshot(root: &Path, out: &Path, staging: &Path) -> (u64, String, TreeStats) {
        let file = std::fs::File::create(out).unwrap();
        let mut w = SnapshotWriter::new(file, PASS, 10, 2).unwrap();
        w.copy_sqlite_via(staging.to_path_buf());
        w.append_bytes(format::MANIFEST, br#"{"kind":"vps-panel-server-snapshot","format":1}"#).unwrap();
        let mut stats = TreeStats::default();
        let r = Root { path: root.to_path_buf(), children: ChildFilter::All };
        w.append_tree(&r, &Excludes::default(), &mut stats, &mut |_| Ok(())).unwrap();
        let index = format::Index { completed: true, ..Default::default() };
        w.append_bytes(format::INDEX, &serde_json::to_vec(&index).unwrap()).unwrap();
        let (_, bytes, sha) = w.finish().unwrap();
        (bytes, sha, stats)
    }

    #[test]
    fn round_trip_keeps_tree_and_skips_specials() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let staging = tmp.path().join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        build_tree(&src);
        let out = tmp.path().join("snap.age");
        let (bytes, sha, stats) = write_snapshot(&src, &out, &staging);
        assert_eq!(bytes, std::fs::metadata(&out).unwrap().len());
        assert_eq!(sha, format::hash_file(&out).unwrap().sha256);
        assert!(stats.skipped >= 1, "fifo skipped");
        assert!(std::fs::read_dir(&staging).unwrap().next().is_none(), "staging cleaned");

        assert_eq!(read_manifest(&out, PASS).unwrap().format, 1);
        verify(&out, PASS).unwrap();

        let dest = tmp.path().join("dest");
        let mut archive = open(std::fs::File::open(&out).unwrap(), PASS).unwrap();
        archive.set_preserve_permissions(true);
        archive.unpack(&dest).unwrap();
        let got = dest.join(fs_entry(&src));
        assert_eq!(std::fs::read(got.join("secret")).unwrap(), b"top");
        assert_eq!(
            std::fs::metadata(got.join("secret")).unwrap().mode() & 0o777,
            0o600
        );
        assert_eq!(std::fs::read(got.join("zero")).unwrap(), b"");
        assert_eq!(std::fs::read_link(got.join("link")).unwrap(), Path::new("secret"));
        assert!(got.join("empty").is_dir());
        assert_eq!(
            std::fs::read(got.join("deep/".to_string() + &"d".repeat(120) + "/long")).unwrap(),
            b"long name"
        );
        assert_eq!(std::fs::read(got.join(std::ffi::OsStr::from_bytes(b"odd-\xff-name"))).unwrap(), b"bytes");
        assert!(!got.join("fifo").exists());
        // The live WAL database came out whole, without its -wal.
        let copy = rusqlite::Connection::open(got.join("app.db")).unwrap();
        let x: i64 = copy.query_row("SELECT x FROM t", [], |r| r.get(0)).unwrap();
        assert_eq!(x, 42);
        assert!(!got.join("app.db-wal").exists());
    }

    #[test]
    fn wrong_passphrase_cut_and_flipped_files_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("f"), vec![7u8; 300_000]).unwrap();
        let out = tmp.path().join("snap.age");
        write_snapshot(&src, &out, tmp.path());

        assert!(matches!(read_manifest(&out, "Not the passphrase 1"), Err(OpenError::Passphrase)));

        let full = std::fs::read(&out).unwrap();
        let cut = tmp.path().join("cut.age");
        std::fs::write(&cut, &full[..full.len() - 100]).unwrap();
        assert!(verify(&cut, PASS).is_err(), "a cut file must not verify");

        let mut flipped = full.clone();
        let i = flipped.len() / 2;
        flipped[i] ^= 0x01;
        let bad = tmp.path().join("flipped.age");
        std::fs::write(&bad, &flipped).unwrap();
        assert!(verify(&bad, PASS).is_err(), "a changed byte must not verify");
    }

    #[test]
    fn exact_reader_pads_a_file_that_shrank() {
        let mut r = ExactReader::new(&b"abc"[..], 6);
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        assert_eq!(out, b"abc\0\0\0");
        assert!(r.short);
        let mut r = ExactReader::new(&b"abcdef"[..], 3);
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        assert_eq!(out, b"abc");
        assert!(!r.short);
    }
}
