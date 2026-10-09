//! Visual filesystem browser backing the panel's /files page.
//!
//! # Why this isn't just "expose `/`"
//!
//! The agent runs as root with no sandboxing (see
//! `scripts/systemd/vps-panel-agent.service`), so an unguarded file manager
//! would be a root file manager. Three layers keep that from being a footgun:
//!
//! 1. **Roots.** Browsing starts from a fixed set of named roots
//!    (`/srv/apps`, `/var/www`, `/home`, ...), each of which is a jail: every
//!    request canonicalises the target and refuses anything that resolves
//!    outside its root, so `..` and symlink hops can't escape. The whole
//!    filesystem is reachable only through the explicit `fs` root.
//! 2. **A protected list.** Paths that hold secrets or hold the system
//!    together (`/etc/vps-panel`, `/etc/shadow`, `/usr`, `/boot`, ...) are
//!    never writable, in any root, and neither are their ancestors — so
//!    "delete /etc" can't take `/etc/vps-panel` with it.
//! 3. **Trash, not `rm`.** Deletes move into `/var/lib/vps-panel/trash/`
//!    with a manifest, restorable until the 30-day purge collects them.

use crate::error::{AgentError, AgentResult};
use crate::exec;
use crate::fs_atomic;
use crate::ipc::OkResponse;
use axum::{
    extract::{Path as AxPath, Query},
    Json,
};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use tokio::fs;
use uuid::Uuid;

/// A browsable jail. `path` is the only directory subtree a request scoped to
/// this root may touch; `writable` gates every mutating endpoint.
pub struct RootDef {
    pub id: &'static str,
    pub label: &'static str,
    pub path: &'static str,
    pub description: &'static str,
    pub writable: bool,
}

/// Order matters — the panel renders the roots screen in this order, and the
/// last entry (`fs`) is deliberately last because it's the opt-in escape
/// hatch, not a place to start.
pub const ROOTS: &[RootDef] = &[
    RootDef {
        id: "apps",
        label: "Apps",
        path: "/srv/apps",
        description: "Source trees of every deployed app.",
        writable: true,
    },
    RootDef {
        id: "drive",
        label: "Drive",
        path: DRIVE_DIR,
        description: "Your files. Nothing here is read or touched by the panel.",
        writable: true,
    },
    RootDef {
        id: "www",
        label: "Web root",
        path: "/var/www",
        description: "Document roots for folder-mode sites.",
        writable: true,
    },
    RootDef {
        id: "home",
        label: "Home directories",
        path: "/home",
        description: "Home folders of the Linux users you created.",
        writable: true,
    },
    RootDef {
        id: "backups",
        label: "Backups",
        path: "/var/backups",
        description: "Archives and dumps written by backup jobs.",
        writable: true,
    },
    RootDef {
        id: "logs",
        label: "Logs",
        path: "/var/log",
        description: "System and service logs. Browse-only.",
        writable: false,
    },
    RootDef {
        id: "nginx",
        label: "Nginx config",
        path: "/etc/nginx",
        description: "vhosts and server config. Browse-only — edit these from Domains so nginx -t still runs.",
        writable: false,
    },
    RootDef {
        id: "fs",
        label: "Whole filesystem",
        path: "/",
        description: "Everything. Critical system paths stay locked.",
        writable: true,
    },
];

/// Paths that may never be written, renamed, moved or deleted, in any root.
/// A path is protected when it IS one of these or lives inside one.
const PROTECTED: &[&str] = &[
    // Kernel + runtime pseudo-filesystems.
    "/proc",
    "/sys",
    "/dev",
    "/run",
    "/boot",
    // The OS itself. Touching these bricks the box faster than anything else
    // on this list.
    "/bin",
    "/sbin",
    "/lib",
    "/lib32",
    "/lib64",
    "/libx32",
    "/usr",
    // Panel's own state + secrets. /etc/vps-panel holds the agent bearer
    // token; /var/lib/vps-panel holds panel.db AND the trash itself.
    "/etc/vps-panel",
    "/var/lib/vps-panel",
    // Credentials and keys.
    "/etc/shadow",
    "/etc/gshadow",
    "/etc/passwd",
    "/etc/group",
    "/etc/sudoers",
    "/etc/sudoers.d",
    "/etc/ssh",
    "/etc/ssl/private",
    "/etc/letsencrypt",
    "/root/.ssh",
];

/// General-purpose storage for the operator's own files.
///
/// `/srv` is the FHS home for site-specific data this system serves, which
/// makes this a sibling of `/srv/apps` rather than a special case. Deliberately
/// NOT under `/var/lib/vps-panel` (protected, and wiped by
/// `uninstall.sh --purge`) or `/home/<user>` (destroyed by `userdel -r` from
/// the Users page). If the drive ever needs its own disk, mount the volume
/// here and nothing in the panel changes.
pub const DRIVE_DIR: &str = "/srv/drive";

const TRASH_DIR: &str = "/var/lib/vps-panel/trash";

/// Refuse an upload that would leave the filesystem with less than this free.
/// Everything on this box — mail, Postgres, nginx, the panel itself — shares
/// one partition, so a runaway upload doesn't just fill the drive, it takes
/// the server down.
const MIN_FREE_BYTES: u64 = 1024 * 1024 * 1024;

/// Sanity ceiling for a single uploaded file.
const MAX_UPLOAD_BYTES: u64 = 5 * 1024 * 1024 * 1024;
/// How long a trashed item survives before the purge loop collects it.
pub const TRASH_RETENTION_DAYS: i64 = 30;
/// Cap on entries returned for one directory. `/var/log` on a busy box and
/// `node_modules` both blow past any sane UI list length.
const MAX_ENTRIES: usize = 5000;

// ---------------------------------------------------------------------------
// Path guards
// ---------------------------------------------------------------------------

fn find_root(id: &str) -> AgentResult<&'static RootDef> {
    ROOTS
        .iter()
        .find(|r| r.id == id)
        .ok_or_else(|| AgentError::Invalid(format!("unknown root {id:?}")))
}

/// True when `p` is a protected path or lives inside one.
fn is_protected(p: &Path) -> bool {
    PROTECTED.iter().any(|prot| {
        let prot = Path::new(prot);
        p == prot || p.starts_with(prot)
    })
}

/// True when `p` CONTAINS a protected path. Deleting or renaming such a
/// directory would take the protected path with it, so mutations are refused
/// even though the directory itself isn't on the list (e.g. `/etc`, `/var`).
fn is_protected_ancestor(p: &Path) -> bool {
    PROTECTED
        .iter()
        .any(|prot| Path::new(prot).starts_with(p) && Path::new(prot) != p)
}

/// True for `/` and for top-level directories like `/srv` or `/opt`. Renaming
/// or deleting one of these is never something a file manager should do.
fn is_top_level(p: &Path) -> bool {
    p.components()
        .filter(|c| matches!(c, Component::Normal(_)))
        .count()
        <= 1
}

/// True when `p` may not be renamed, moved or deleted.
fn entry_locked(p: &Path) -> bool {
    is_protected(p) || is_protected_ancestor(p) || is_top_level(p)
}

/// Gate for every mutating operation. `root` must be writable, and the target
/// must not be locked.
fn guard_write(root: &RootDef, target: &Path) -> AgentResult<()> {
    if !root.writable {
        return Err(AgentError::Invalid(format!(
            "the {} root is browse-only",
            root.label
        )));
    }
    if entry_locked(target) {
        return Err(AgentError::Invalid(format!(
            "{} is a protected system path",
            target.display()
        )));
    }
    Ok(())
}

/// Gate for "may I create something inside this directory". Weaker than
/// [`guard_write`]: a directory can be an ancestor of a protected path (e.g.
/// `/etc`) and still accept a new file next to it.
fn guard_write_into(root: &RootDef, dir: &Path) -> AgentResult<()> {
    if !root.writable {
        return Err(AgentError::Invalid(format!(
            "the {} root is browse-only",
            root.label
        )));
    }
    if is_protected(dir) {
        return Err(AgentError::Invalid(format!(
            "{} is a protected system path",
            dir.display()
        )));
    }
    // Nothing may be created directly in `/`. A new top-level directory would
    // immediately be `is_top_level` and therefore undeletable — a one-way
    // door. `/` is for navigating through, not for putting things in.
    if dir.parent().is_none() {
        return Err(AgentError::Invalid(
            "cannot create items directly in / — open one of the folders below it".into(),
        ));
    }
    Ok(())
}

fn validate_name(name: &str) -> AgentResult<()> {
    if name.is_empty() {
        return Err(AgentError::Invalid("name must not be empty".into()));
    }
    if name == "." || name == ".." {
        return Err(AgentError::Invalid("name must not be `.` or `..`".into()));
    }
    if name.contains('/') || name.contains('\0') {
        return Err(AgentError::Invalid(
            "name must not contain `/` or a null byte".into(),
        ));
    }
    if name.trim() != name {
        return Err(AgentError::Invalid(
            "name must not start or end with whitespace".into(),
        ));
    }
    if name.len() > 255 {
        return Err(AgentError::Invalid("name must be 255 bytes or fewer".into()));
    }
    Ok(())
}

/// Normalise a caller-supplied relative path. Rejects `..` outright rather
/// than resolving it — canonicalisation below is the second line of defence,
/// this is the first.
fn clean_rel(rel: &str) -> AgentResult<PathBuf> {
    let mut out = PathBuf::new();
    for c in Path::new(rel.trim_start_matches('/')).components() {
        match c {
            Component::Normal(s) => {
                if s.to_string_lossy().contains('\0') {
                    return Err(AgentError::Invalid("path contains a null byte".into()));
                }
                out.push(s);
            }
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(AgentError::Invalid("`..` is not allowed in a path".into()))
            }
            _ => return Err(AgentError::Invalid("path must be relative to the root".into())),
        }
    }
    Ok(out)
}

/// Resolve `rel` to an existing directory inside `root`, following symlinks
/// and then verifying the result is still inside the root. This is what stops
/// a symlink in `/srv/apps` pointing at `/etc` from being browsable.
async fn resolve_dir(root: &RootDef, rel: &str) -> AgentResult<PathBuf> {
    let base = fs::canonicalize(root.path)
        .await
        .map_err(|_| AgentError::NotFound)?;
    let joined = base.join(clean_rel(rel)?);
    let canon = fs::canonicalize(&joined)
        .await
        .map_err(|_| AgentError::NotFound)?;
    if !canon.starts_with(&base) {
        return Err(AgentError::Invalid(
            "path resolves outside its root".into(),
        ));
    }
    let meta = fs::metadata(&canon).await.map_err(AgentError::Io)?;
    if !meta.is_dir() {
        return Err(AgentError::Invalid("path is not a directory".into()));
    }
    Ok(canon)
}

/// Resolve `rel` to a path inside `root` WITHOUT canonicalising the final
/// component. Only the parent is resolved, so a symlink target is never
/// operated on by mistake — renaming or trashing a symlink moves the link,
/// not what it points at.
async fn resolve_leaf(root: &RootDef, rel: &str) -> AgentResult<PathBuf> {
    let cleaned = clean_rel(rel)?;
    let name = cleaned
        .file_name()
        .ok_or_else(|| AgentError::Invalid("path must name a file or folder".into()))?
        .to_owned();
    let parent_rel = cleaned
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .to_string_lossy()
        .into_owned();
    let parent = resolve_dir(root, &parent_rel).await?;
    Ok(parent.join(name))
}

/// Resolve `rel` to an existing regular FILE inside the root named `root_id`,
/// canonicalising the LEAF as well as the parent and applying the protected
/// list to the resolved target. This is the read-primitive resolver: a symlink
/// followed here is equivalent to reading its target, so the same guards as
/// `download` apply. Shared by `download` and by handlers in other modules
/// that consume a file the operator staged through /files (e.g. the MariaDB
/// SQL-dump importer).
pub(crate) async fn resolve_read_file(
    root_id: &str,
    rel: &str,
) -> AgentResult<(PathBuf, std::fs::Metadata)> {
    let root = find_root(root_id)?;
    let base = fs::canonicalize(root.path)
        .await
        .map_err(|_| AgentError::NotFound)?;
    let joined = base.join(clean_rel(rel)?);
    let canon = fs::canonicalize(&joined)
        .await
        .map_err(|_| AgentError::NotFound)?;
    if !canon.starts_with(&base) {
        return Err(AgentError::Invalid("path resolves outside its root".into()));
    }
    if is_protected(&canon) {
        return Err(AgentError::Invalid(format!(
            "{} is a protected system path",
            canon.display()
        )));
    }
    let meta = fs::metadata(&canon).await.map_err(|_| AgentError::NotFound)?;
    if !meta.is_file() {
        return Err(AgentError::Invalid("path does not name a regular file".into()));
    }
    Ok((canon, meta))
}

/// `rename(2)`, falling back to `mv` when source and destination sit on
/// different mounts (EXDEV) — `/home` and `/var` are separate filesystems on
/// plenty of VPS images, and trashing must work across that boundary.
async fn move_path(src: &Path, dst: &Path) -> AgentResult<()> {
    match fs::rename(src, dst).await {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(18) => {
            exec::run(
                "mv",
                [
                    std::ffi::OsStr::new("-T"),
                    src.as_os_str(),
                    dst.as_os_str(),
                ],
            )
            .await?;
            Ok(())
        }
        Err(e) => Err(AgentError::Io(e)),
    }
}

// ---------------------------------------------------------------------------
// Listing
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct RootRow {
    pub id: &'static str,
    pub label: &'static str,
    pub path: &'static str,
    pub description: &'static str,
    pub writable: bool,
    /// False when the directory doesn't exist on this box (e.g. no
    /// `/var/backups` yet). The UI greys the card out.
    pub exists: bool,
}

#[derive(Serialize)]
pub struct Entry {
    pub name: String,
    /// "dir" | "file" | "symlink" | "other"
    pub kind: &'static str,
    pub size: u64,
    /// RFC3339, UTC.
    pub modified: Option<String>,
    /// Permission bits as four octal digits, e.g. "0755".
    pub mode: String,
    pub owner: String,
    pub group: String,
    /// Where a symlink points, verbatim (not resolved).
    pub link_target: Option<String>,
    /// True when a symlink resolves to a directory — the UI still lets you
    /// open it, and `resolve_dir` re-checks the jail on the way in.
    pub link_is_dir: bool,
    /// True when this entry can't be renamed, moved or deleted.
    pub locked: bool,
}

#[derive(Serialize)]
pub struct ListResponse {
    pub root: RootRow,
    /// Path relative to the root, forward-slashed. "" at the root itself.
    pub path: String,
    pub abs_path: String,
    /// True when new items may be created in THIS directory.
    pub writable: bool,
    pub entries: Vec<Entry>,
    /// True when the directory holds more than `MAX_ENTRIES` items.
    pub truncated: bool,
}

#[derive(Deserialize)]
pub struct ListQuery {
    pub root: String,
    #[serde(default)]
    pub path: String,
}

fn root_row(r: &'static RootDef, exists: bool) -> RootRow {
    RootRow {
        id: r.id,
        label: r.label,
        path: r.path,
        description: r.description,
        writable: r.writable,
        exists,
    }
}

pub async fn roots() -> AgentResult<Json<Vec<RootRow>>> {
    let mut rows = Vec::with_capacity(ROOTS.len());
    for r in ROOTS {
        let exists = fs::metadata(r.path).await.map(|m| m.is_dir()).unwrap_or(false);
        rows.push(root_row(r, exists));
    }
    Ok(Json(rows))
}

pub async fn list(Query(q): Query<ListQuery>) -> AgentResult<Json<ListResponse>> {
    let root = find_root(&q.root)?;
    let dir = resolve_dir(root, &q.path).await?;

    let mut rd = fs::read_dir(&dir).await.map_err(AgentError::Io)?;
    let mut entries: Vec<Entry> = Vec::new();
    let mut truncated = false;
    let mut users: HashMap<u32, String> = HashMap::new();
    let mut groups: HashMap<u32, String> = HashMap::new();

    while let Some(e) = rd.next_entry().await.map_err(AgentError::Io)? {
        if entries.len() >= MAX_ENTRIES {
            truncated = true;
            break;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        let path = e.path();
        // symlink_metadata, not metadata: a broken or hostile symlink must
        // still render as a symlink rather than erroring the whole listing.
        let meta = match fs::symlink_metadata(&path).await {
            Ok(m) => m,
            Err(_) => continue,
        };
        let ft = meta.file_type();
        let (kind, link_target, link_is_dir) = if ft.is_symlink() {
            let target = fs::read_link(&path)
                .await
                .ok()
                .map(|t| t.to_string_lossy().into_owned());
            let is_dir = fs::metadata(&path).await.map(|m| m.is_dir()).unwrap_or(false);
            ("symlink", target, is_dir)
        } else if ft.is_dir() {
            ("dir", None, false)
        } else if ft.is_file() {
            ("file", None, false)
        } else {
            ("other", None, false)
        };

        entries.push(Entry {
            name,
            kind,
            size: meta.len(),
            modified: mtime_rfc3339(&meta),
            mode: mode_string(&meta),
            owner: uid_name(owner_uid(&meta), &mut users),
            group: gid_name(owner_gid(&meta), &mut groups),
            link_target,
            link_is_dir,
            locked: entry_locked(&path),
        });
    }

    // Directories first, then case-insensitive by name — the ordering every
    // file manager uses, done server-side so the client stays dumb.
    entries.sort_by(|a, b| {
        let a_dir = a.kind == "dir" || (a.kind == "symlink" && a.link_is_dir);
        let b_dir = b.kind == "dir" || (b.kind == "symlink" && b.link_is_dir);
        b_dir
            .cmp(&a_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    let rel = clean_rel(&q.path)?.to_string_lossy().replace('\\', "/");
    Ok(Json(ListResponse {
        root: root_row(root, true),
        path: rel,
        abs_path: dir.to_string_lossy().into_owned(),
        writable: guard_write_into(root, &dir).is_ok(),
        entries,
        truncated,
    }))
}

fn mtime_rfc3339(meta: &std::fs::Metadata) -> Option<String> {
    let t: DateTime<Utc> = meta.modified().ok()?.into();
    Some(t.to_rfc3339_opts(SecondsFormat::Secs, true))
}

#[cfg(unix)]
fn mode_string(meta: &std::fs::Metadata) -> String {
    use std::os::unix::fs::PermissionsExt;
    format!("{:04o}", meta.permissions().mode() & 0o7777)
}

#[cfg(unix)]
fn owner_uid(meta: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    meta.uid()
}

#[cfg(unix)]
fn owner_gid(meta: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    meta.gid()
}

#[cfg(unix)]
fn uid_name(uid: u32, cache: &mut HashMap<u32, String>) -> String {
    cache
        .entry(uid)
        .or_insert_with(|| {
            nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid))
                .ok()
                .flatten()
                .map(|u| u.name)
                .unwrap_or_else(|| uid.to_string())
        })
        .clone()
}

#[cfg(unix)]
fn gid_name(gid: u32, cache: &mut HashMap<u32, String>) -> String {
    cache
        .entry(gid)
        .or_insert_with(|| {
            nix::unistd::Group::from_gid(nix::unistd::Gid::from_raw(gid))
                .ok()
                .flatten()
                .map(|g| g.name)
                .unwrap_or_else(|| gid.to_string())
        })
        .clone()
}

/// New files and folders inherit their parent's owner, so an app's tree stays
/// readable by the `apps` user instead of turning into root-owned islands the
/// service can't read. Best-effort: a failure here isn't worth failing the
/// whole create over.
#[cfg(unix)]
async fn inherit_owner(parent: &Path, child: &Path) {
    use std::os::unix::fs::MetadataExt;
    if let Ok(meta) = fs::metadata(parent).await {
        let _ = fs_atomic::chown_blocking(child, meta.uid(), meta.gid());
    }
}

// Non-unix stubs. The agent only ever runs on Linux (main.rs bails on other
// targets), but keeping the crate type-checkable on a Windows dev box is
// worth six trivial fallbacks — the same trade fs_atomic.rs already makes.

#[cfg(not(unix))]
fn mode_string(_meta: &std::fs::Metadata) -> String {
    "0000".to_string()
}

#[cfg(not(unix))]
fn owner_uid(_meta: &std::fs::Metadata) -> u32 {
    0
}

#[cfg(not(unix))]
fn owner_gid(_meta: &std::fs::Metadata) -> u32 {
    0
}

#[cfg(not(unix))]
fn uid_name(uid: u32, _cache: &mut HashMap<u32, String>) -> String {
    uid.to_string()
}

#[cfg(not(unix))]
fn gid_name(gid: u32, _cache: &mut HashMap<u32, String>) -> String {
    gid.to_string()
}

#[cfg(not(unix))]
async fn inherit_owner(_parent: &Path, _child: &Path) {}

// ---------------------------------------------------------------------------
// Create / rename / move
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct CreateReq {
    pub root: String,
    /// Directory to create in, relative to the root.
    #[serde(default)]
    pub path: String,
    pub name: String,
}

#[derive(Serialize)]
pub struct CreatedResponse {
    pub ok: bool,
    /// Path of the new item relative to the root.
    pub path: String,
}

pub async fn mkdir(Json(req): Json<CreateReq>) -> AgentResult<Json<CreatedResponse>> {
    create_entry(req, true).await
}

pub async fn new_file(Json(req): Json<CreateReq>) -> AgentResult<Json<CreatedResponse>> {
    create_entry(req, false).await
}

async fn create_entry(req: CreateReq, is_dir: bool) -> AgentResult<Json<CreatedResponse>> {
    let root = find_root(&req.root)?;
    validate_name(&req.name)?;
    let dir = resolve_dir(root, &req.path).await?;
    guard_write_into(root, &dir)?;

    let target = dir.join(&req.name);
    if fs::symlink_metadata(&target).await.is_ok() {
        return Err(AgentError::Conflict(format!(
            "{:?} already exists here",
            req.name
        )));
    }

    if is_dir {
        fs::create_dir(&target).await.map_err(AgentError::Io)?;
        fs_atomic::chmod(&target, 0o755).await?;
    } else {
        fs_atomic::write(&target, b"", 0o644).await?;
    }
    inherit_owner(&dir, &target).await;

    let rel = join_rel(&req.path, &req.name);
    Ok(Json(CreatedResponse { ok: true, path: rel }))
}

#[derive(Deserialize)]
pub struct RenameReq {
    pub root: String,
    /// Existing item, relative to the root.
    pub path: String,
    pub new_name: String,
}

pub async fn rename(Json(req): Json<RenameReq>) -> AgentResult<Json<CreatedResponse>> {
    let root = find_root(&req.root)?;
    validate_name(&req.new_name)?;
    let src = resolve_leaf(root, &req.path).await?;
    guard_write(root, &src)?;
    if fs::symlink_metadata(&src).await.is_err() {
        return Err(AgentError::NotFound);
    }

    let parent = src
        .parent()
        .ok_or_else(|| AgentError::Invalid("cannot rename this path".into()))?;
    let dst = parent.join(&req.new_name);
    if dst == src {
        return Ok(Json(CreatedResponse {
            ok: true,
            path: req.path,
        }));
    }
    if fs::symlink_metadata(&dst).await.is_ok() {
        return Err(AgentError::Conflict(format!(
            "{:?} already exists here",
            req.new_name
        )));
    }
    // The destination inherits the source's guard: renaming can't be used to
    // walk something INTO a protected location.
    guard_write(root, &dst)?;
    fs::rename(&src, &dst).await.map_err(AgentError::Io)?;

    let parent_rel = clean_rel(&req.path)?
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .to_string_lossy()
        .into_owned();
    Ok(Json(CreatedResponse {
        ok: true,
        path: join_rel(&parent_rel, &req.new_name),
    }))
}

#[derive(Deserialize)]
pub struct MoveReq {
    pub root: String,
    /// Items to move, each relative to the root.
    pub paths: Vec<String>,
    /// Destination directory, relative to the root. "" is the root itself.
    #[serde(default)]
    pub dest: String,
}

#[derive(Serialize)]
pub struct MoveResponse {
    pub ok: bool,
    pub moved: usize,
    /// Per-item failures, so a partially-successful drop still reports what
    /// landed and what didn't instead of failing the whole batch.
    pub errors: Vec<String>,
}

pub async fn move_items(Json(req): Json<MoveReq>) -> AgentResult<Json<MoveResponse>> {
    let root = find_root(&req.root)?;
    let dest = resolve_dir(root, &req.dest).await?;
    guard_write_into(root, &dest)?;

    let mut moved = 0usize;
    let mut errors = Vec::new();
    for rel in &req.paths {
        match move_one(root, rel, &dest).await {
            Ok(()) => moved += 1,
            Err(e) => errors.push(format!("{rel}: {e}")),
        }
    }
    Ok(Json(MoveResponse {
        ok: errors.is_empty(),
        moved,
        errors,
    }))
}

async fn move_one(root: &RootDef, rel: &str, dest: &Path) -> AgentResult<()> {
    let src = resolve_leaf(root, rel).await?;
    guard_write(root, &src)?;
    if fs::symlink_metadata(&src).await.is_err() {
        return Err(AgentError::NotFound);
    }
    let name = src
        .file_name()
        .ok_or_else(|| AgentError::Invalid("cannot move this path".into()))?;
    let target = dest.join(name);
    if target == src {
        return Ok(());
    }
    // Dropping a folder onto itself or onto one of its own descendants would
    // otherwise produce an unreachable subtree.
    if dest.starts_with(&src) {
        return Err(AgentError::Invalid(
            "cannot move a folder into itself".into(),
        ));
    }
    if fs::symlink_metadata(&target).await.is_ok() {
        return Err(AgentError::Conflict("already exists in the destination".into()));
    }
    move_path(&src, &target).await
}

fn join_rel(dir: &str, name: &str) -> String {
    let dir = dir.trim_matches('/');
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

// ---------------------------------------------------------------------------
// Upload / download
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct UploadQuery {
    pub root: String,
    /// Directory to upload into, relative to the root.
    #[serde(default)]
    pub path: String,
    pub name: String,
    /// Replace an existing file of the same name instead of 409ing.
    #[serde(default)]
    pub overwrite: bool,
}

/// Free bytes on the filesystem holding `path`. Shared with the SQL-dump
/// importer in `databases.rs`, which refuses an import that could fill the
/// partition (everything on this box shares one).
#[cfg(unix)]
pub(crate) fn available_bytes(path: &Path) -> Option<u64> {
    let s = nix::sys::statvfs::statvfs(path).ok()?;
    Some(s.blocks_available() as u64 * s.fragment_size() as u64)
}

#[cfg(not(unix))]
pub(crate) fn available_bytes(_path: &Path) -> Option<u64> {
    None
}

/// Streamed upload. The body is the file's raw bytes — no multipart — so the
/// bytes flow browser → panel → agent → disk without any hop buffering the
/// whole file in memory.
///
/// Writes to a dotfile temp in the destination directory and renames on
/// completion, so a half-finished 2 GB upload never appears in the listing as
/// a real file, and an aborted one leaves nothing behind.
pub async fn upload(
    Query(q): Query<UploadQuery>,
    body: axum::body::Body,
) -> AgentResult<Json<CreatedResponse>> {
    use futures::StreamExt;
    use tokio::io::AsyncWriteExt;

    let root = find_root(&q.root)?;
    validate_name(&q.name)?;
    let dir = resolve_dir(root, &q.path).await?;
    guard_write_into(root, &dir)?;

    let target = dir.join(&q.name);
    if !q.overwrite && fs::symlink_metadata(&target).await.is_ok() {
        return Err(AgentError::Conflict(format!(
            "{:?} already exists here",
            q.name
        )));
    }

    if let Some(free) = available_bytes(&dir) {
        if free < MIN_FREE_BYTES {
            return Err(AgentError::Conflict(format!(
                "only {} MB free on this filesystem — free some space first",
                free / (1024 * 1024)
            )));
        }
    }

    let tmp = dir.join(format!(".vps-panel-upload.{}.tmp", Uuid::new_v4()));
    let mut file = fs::File::create(&tmp).await.map_err(AgentError::Io)?;
    let mut stream = body.into_data_stream();
    let mut written: u64 = 0;
    // Re-check free space periodically rather than per chunk — statvfs on
    // every 8 KB chunk of a multi-GB upload would cost more than the write.
    let mut since_check: u64 = 0;

    let outcome: AgentResult<()> = async {
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| {
                AgentError::Invalid(format!("upload stream failed: {e}"))
            })?;
            written += chunk.len() as u64;
            since_check += chunk.len() as u64;
            if written > MAX_UPLOAD_BYTES {
                return Err(AgentError::Invalid(format!(
                    "file exceeds the {} GB upload limit",
                    MAX_UPLOAD_BYTES / (1024 * 1024 * 1024)
                )));
            }
            if since_check >= 8 * 1024 * 1024 {
                since_check = 0;
                if let Some(free) = available_bytes(&dir) {
                    if free < MIN_FREE_BYTES {
                        return Err(AgentError::Conflict(
                            "ran out of disk space partway through — upload aborted".into(),
                        ));
                    }
                }
            }
            file.write_all(&chunk).await.map_err(AgentError::Io)?;
        }
        file.flush().await.map_err(AgentError::Io)?;
        file.sync_all().await.map_err(AgentError::Io)
    }
    .await;

    drop(file);
    if let Err(e) = outcome {
        let _ = fs::remove_file(&tmp).await;
        return Err(e);
    }

    fs_atomic::chmod(&tmp, 0o644).await?;
    inherit_owner(&dir, &tmp).await;
    if let Err(e) = fs::rename(&tmp, &target).await {
        let _ = fs::remove_file(&tmp).await;
        return Err(AgentError::Io(e));
    }

    Ok(Json(CreatedResponse {
        ok: true,
        path: join_rel(&q.path, &q.name),
    }))
}

#[derive(Deserialize)]
pub struct DownloadQuery {
    pub root: String,
    pub path: String,
    /// Render in the browser instead of prompting a save, where the type is
    /// safe to inline. Ignored for everything else.
    #[serde(default)]
    pub inline: bool,
}

/// Content types that may be rendered in the panel's own origin.
///
/// SVG and HTML are deliberately absent: both can carry script, and serving
/// operator-uploaded script inline from the panel origin is stored XSS against
/// the panel session. They download instead.
fn inline_safe(mime: &str) -> bool {
    matches!(
        mime,
        "image/png"
            | "image/jpeg"
            | "image/gif"
            | "image/webp"
            | "image/avif"
            | "image/bmp"
            | "image/x-icon"
            | "application/pdf"
            | "text/plain"
    )
}

fn mime_for(name: &str) -> &'static str {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        Some("bmp") => "image/bmp",
        Some("ico") => "image/x-icon",
        Some("svg") => "image/svg+xml",
        Some("pdf") => "application/pdf",
        Some("zip") => "application/zip",
        Some("gz" | "tgz") => "application/gzip",
        Some("json") => "application/json",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mp3") => "audio/mpeg",
        Some(
            "txt" | "md" | "log" | "csv" | "xml" | "yaml" | "yml" | "toml" | "ini" | "conf"
            | "sh" | "rs" | "ts" | "tsx" | "js" | "jsx" | "css" | "sql" | "env",
        ) => "text/plain",
        _ => "application/octet-stream",
    }
}

/// Percent-encode for the RFC 5987 `filename*` parameter, so non-ASCII names
/// survive the download instead of being mangled.
fn pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Streamed download.
///
/// Unlike every mutating handler, this one canonicalises the LEAF as well as
/// the parent. A symlink is a read primitive: without following it here, a
/// link in `/srv/apps` pointing at `/etc/shadow` would turn "download" into
/// "exfiltrate". The protected list is then applied to the resolved target.
pub async fn download(Query(q): Query<DownloadQuery>) -> AgentResult<axum::response::Response> {
    use axum::http::header;
    use tokio::io::AsyncReadExt;

    let (canon, meta) = resolve_read_file(&q.root, &q.path).await?;

    let name = canon
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "download".to_string());
    let mime = mime_for(&name);
    let disposition = if q.inline && inline_safe(mime) {
        "inline"
    } else {
        "attachment"
    };
    // The quoted form can't carry a quote, a backslash or a newline without
    // breaking the header; filename* carries the real name.
    let ascii_name: String = name
        .chars()
        .map(|c| if c.is_ascii() && !matches!(c, '"' | '\\' | '\n' | '\r') { c } else { '_' })
        .collect();

    let mut file = fs::File::open(&canon).await.map_err(AgentError::Io)?;
    let stream = async_stream::stream! {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match file.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => yield Ok::<Vec<u8>, std::io::Error>(buf[..n].to_vec()),
                Err(e) => {
                    yield Err(e);
                    break;
                }
            }
        }
    };

    axum::response::Response::builder()
        .header(header::CONTENT_TYPE, mime)
        .header(header::CONTENT_LENGTH, meta.len())
        // Without nosniff a browser can decide an "octet-stream" is really
        // HTML and render it, which would undo the inline_safe gate above.
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(
            header::CONTENT_DISPOSITION,
            format!(
                "{disposition}; filename=\"{ascii_name}\"; filename*=UTF-8''{}",
                pct_encode(&name)
            ),
        )
        .body(axum::body::Body::from_stream(stream))
        .map_err(|e| AgentError::Other(anyhow::anyhow!("build download response: {e}")))
}

// ---------------------------------------------------------------------------
// Trash
// ---------------------------------------------------------------------------

/// On-disk manifest, written next to the payload as `meta.json`. The trash is
/// self-describing: everything needed to restore an item lives in its own
/// directory, so nothing breaks if panel.db is rebuilt.
#[derive(Serialize, Deserialize, Clone)]
pub struct TrashMeta {
    pub id: String,
    pub name: String,
    /// "dir" | "file" | "symlink" | "other"
    pub kind: String,
    pub root_id: String,
    /// Absolute path the item came from, for display and restore.
    pub original_path: String,
    pub size: u64,
    /// RFC3339, UTC.
    pub trashed_at: String,
}

#[derive(Serialize)]
pub struct TrashRow {
    #[serde(flatten)]
    pub meta: TrashMeta,
    /// Whole days until the purge loop collects this item. Never negative.
    pub days_left: i64,
}

#[derive(Deserialize)]
pub struct TrashReq {
    pub root: String,
    pub paths: Vec<String>,
}

#[derive(Serialize)]
pub struct TrashResponse {
    pub ok: bool,
    pub trashed: usize,
    pub errors: Vec<String>,
}

pub async fn trash_items(Json(req): Json<TrashReq>) -> AgentResult<Json<TrashResponse>> {
    let root = find_root(&req.root)?;
    fs_atomic::ensure_dir(Path::new(TRASH_DIR), 0o700).await?;

    let mut trashed = 0usize;
    let mut errors = Vec::new();
    for rel in &req.paths {
        match trash_one(root, rel).await {
            Ok(()) => trashed += 1,
            Err(e) => errors.push(format!("{rel}: {e}")),
        }
    }
    Ok(Json(TrashResponse {
        ok: errors.is_empty(),
        trashed,
        errors,
    }))
}

async fn trash_one(root: &RootDef, rel: &str) -> AgentResult<()> {
    let src = resolve_leaf(root, rel).await?;
    guard_write(root, &src)?;
    trash_path(root, &src).await.map(|_| ())
}

/// Trash an absolute path for another feature (e.g. a deleted mailbox's
/// maildir) under the whole-filesystem root, with the file browser's own
/// protected-path guard. It shows up in Files > Trash and can be restored
/// until the purge loop collects it after [`TRASH_RETENTION_DAYS`]. Returns
/// the trash entry id.
pub(crate) async fn trash_system_path(src: &Path) -> AgentResult<String> {
    let root = find_root("fs")?;
    guard_write(root, src)?;
    fs_atomic::ensure_dir(Path::new(TRASH_DIR), 0o700).await?;
    trash_path(root, src).await
}

/// Move an already resolved and guarded `src` into a new trash entry.
async fn trash_path(root: &RootDef, src: &Path) -> AgentResult<String> {
    let src = src.to_path_buf();
    let meta = fs::symlink_metadata(&src)
        .await
        .map_err(|_| AgentError::NotFound)?;
    let ft = meta.file_type();
    let kind = if ft.is_symlink() {
        "symlink"
    } else if ft.is_dir() {
        "dir"
    } else if ft.is_file() {
        "file"
    } else {
        "other"
    };
    let name = src
        .file_name()
        .ok_or_else(|| AgentError::Invalid("cannot delete this path".into()))?
        .to_string_lossy()
        .into_owned();

    let now = Utc::now();
    let id = format!(
        "{}-{}",
        now.format("%Y%m%d%H%M%S"),
        &Uuid::new_v4().to_string()[..8]
    );
    let entry_dir = Path::new(TRASH_DIR).join(&id);
    let payload_dir = entry_dir.join("payload");
    fs_atomic::ensure_dir(&payload_dir, 0o700).await?;

    move_path(&src, &payload_dir.join(&name)).await?;

    let manifest = TrashMeta {
        id: id.clone(),
        name,
        kind: kind.to_string(),
        root_id: root.id.to_string(),
        original_path: src.to_string_lossy().into_owned(),
        size: meta.len(),
        trashed_at: now.to_rfc3339_opts(SecondsFormat::Secs, true),
    };
    let body = serde_json::to_vec_pretty(&manifest)
        .map_err(|e| AgentError::Other(anyhow::anyhow!("serialize trash meta: {e}")))?;
    fs_atomic::write(&entry_dir.join("meta.json"), &body, 0o600).await?;
    Ok(id)
}

pub async fn trash_list() -> AgentResult<Json<Vec<TrashRow>>> {
    // Collect expired entries on read too, so the list is honest even if the
    // agent restarted between purge ticks.
    purge_expired().await;

    let mut rows = Vec::new();
    let mut rd = match fs::read_dir(TRASH_DIR).await {
        Ok(rd) => rd,
        // No trash directory yet == empty trash, not an error.
        Err(_) => return Ok(Json(rows)),
    };
    while let Ok(Some(e)) = rd.next_entry().await {
        let Some(meta) = read_meta(&e.path()).await else {
            continue;
        };
        rows.push(TrashRow {
            days_left: days_left(&meta),
            meta,
        });
    }
    // Newest first.
    rows.sort_by(|a, b| b.meta.trashed_at.cmp(&a.meta.trashed_at));
    Ok(Json(rows))
}

async fn read_meta(entry_dir: &Path) -> Option<TrashMeta> {
    let body = fs::read(entry_dir.join("meta.json")).await.ok()?;
    serde_json::from_slice::<TrashMeta>(&body).ok()
}

fn days_left(meta: &TrashMeta) -> i64 {
    let Ok(t) = DateTime::parse_from_rfc3339(&meta.trashed_at) else {
        return 0;
    };
    let expires = t.with_timezone(&Utc) + Duration::days(TRASH_RETENTION_DAYS);
    // Round UP to whole days. Truncating would show "29 days" the instant
    // something is deleted under a 30-day policy, and "0 days" for anything
    // with hours left — both read as the item expiring sooner than it does.
    let secs = (expires - Utc::now()).num_seconds();
    if secs <= 0 {
        return 0;
    }
    (secs + 86_399) / 86_400
}

fn is_expired(meta: &TrashMeta) -> bool {
    let Ok(t) = DateTime::parse_from_rfc3339(&meta.trashed_at) else {
        // An unparseable timestamp means a hand-edited or corrupt manifest.
        // Leave it alone rather than deleting on a guess.
        return false;
    };
    Utc::now() - t.with_timezone(&Utc) > Duration::days(TRASH_RETENTION_DAYS)
}

fn is_safe_trash_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

#[derive(Deserialize)]
pub struct RestoreReq {
    pub id: String,
}

#[derive(Serialize)]
pub struct RestoreResponse {
    pub ok: bool,
    pub restored_to: String,
}

pub async fn trash_restore(Json(req): Json<RestoreReq>) -> AgentResult<Json<RestoreResponse>> {
    if !is_safe_trash_id(&req.id) {
        return Err(AgentError::Invalid("invalid trash id".into()));
    }
    let entry_dir = Path::new(TRASH_DIR).join(&req.id);
    let meta = read_meta(&entry_dir).await.ok_or(AgentError::NotFound)?;
    let root = find_root(&meta.root_id)?;

    let original = PathBuf::from(&meta.original_path);
    // Re-run the guard on restore: the protected list may have grown, or the
    // original location may now sit inside one.
    guard_write(root, &original)?;
    let parent = original
        .parent()
        .ok_or_else(|| AgentError::Invalid("item has no parent directory".into()))?;
    if fs::symlink_metadata(&original).await.is_ok() {
        return Err(AgentError::Conflict(format!(
            "{} already exists — rename or move it first",
            meta.original_path
        )));
    }
    if fs::metadata(parent).await.is_err() {
        return Err(AgentError::Conflict(format!(
            "{} no longer exists",
            parent.display()
        )));
    }

    move_path(&entry_dir.join("payload").join(&meta.name), &original).await?;
    let _ = fs::remove_dir_all(&entry_dir).await;
    Ok(Json(RestoreResponse {
        ok: true,
        restored_to: meta.original_path,
    }))
}

pub async fn trash_purge_one(AxPath(id): AxPath<String>) -> AgentResult<Json<OkResponse>> {
    if !is_safe_trash_id(&id) {
        return Err(AgentError::Invalid("invalid trash id".into()));
    }
    let entry_dir = Path::new(TRASH_DIR).join(&id);
    if fs::metadata(&entry_dir).await.is_err() {
        return Err(AgentError::NotFound);
    }
    fs::remove_dir_all(&entry_dir).await.map_err(AgentError::Io)?;
    Ok(Json(OkResponse::default()))
}

pub async fn trash_empty() -> AgentResult<Json<OkResponse>> {
    let mut rd = match fs::read_dir(TRASH_DIR).await {
        Ok(rd) => rd,
        Err(_) => return Ok(Json(OkResponse::default())),
    };
    while let Ok(Some(e)) = rd.next_entry().await {
        let _ = fs::remove_dir_all(e.path()).await;
    }
    Ok(Json(OkResponse::default()))
}

/// Delete every trash entry older than [`TRASH_RETENTION_DAYS`].
pub async fn purge_expired() {
    let Ok(mut rd) = fs::read_dir(TRASH_DIR).await else {
        return;
    };
    while let Ok(Some(e)) = rd.next_entry().await {
        let path = e.path();
        let Some(meta) = read_meta(&path).await else {
            continue;
        };
        if is_expired(&meta) {
            match fs::remove_dir_all(&path).await {
                Ok(()) => tracing::info!(id = %meta.id, name = %meta.name, "purged expired trash entry"),
                Err(err) => tracing::warn!(id = %meta.id, %err, "failed to purge trash entry"),
            }
        }
    }
}

/// Create `/srv/drive` if it isn't there yet. Called at agent startup so the
/// Drive root works on boxes installed before it existed, not just fresh ones.
pub async fn ensure_drive_dir() {
    if let Err(e) = fs_atomic::ensure_dir(Path::new(DRIVE_DIR), 0o755).await {
        tracing::warn!(path = DRIVE_DIR, %e, "could not create the drive directory");
    }
}

/// Background purge, spawned from `main`. Runs once at boot then every 6h —
/// frequent enough that a 30-day retention is accurate to within a quarter of
/// a day, cheap enough to be invisible.
pub async fn purge_loop() {
    loop {
        purge_expired().await;
        tokio::time::sleep(std::time::Duration::from_secs(6 * 60 * 60)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> &Path {
        Path::new(s)
    }

    #[test]
    fn protected_covers_the_path_and_everything_under_it() {
        assert!(is_protected(p("/etc/vps-panel")));
        assert!(is_protected(p("/etc/vps-panel/.env")));
        assert!(is_protected(p("/usr/lib/systemd")));
        assert!(is_protected(p("/root/.ssh/authorized_keys")));
        assert!(!is_protected(p("/srv/apps/myapp")));
        assert!(!is_protected(p("/etc/nginx/sites-available/default")));
    }

    #[test]
    fn protected_matching_is_component_wise_not_string_prefix() {
        // `/usrlocal` and `/etc/vps-panel-backup` merely SHARE a string prefix
        // with a protected path. A naive starts_with on strings would lock
        // them; Path::starts_with compares components, which is why it's used.
        assert!(!is_protected(p("/usrlocal")));
        assert!(!is_protected(p("/etc/vps-panel-backup")));
        assert!(!is_protected(p("/booted")));
    }

    #[test]
    fn ancestors_of_protected_paths_cannot_be_mutated() {
        // Deleting /etc would take /etc/vps-panel with it.
        assert!(is_protected_ancestor(p("/etc")));
        assert!(is_protected_ancestor(p("/var/lib")));
        assert!(!is_protected_ancestor(p("/srv/apps")));
        // A protected path is not its own ancestor — that case is
        // is_protected's job, and double-counting would be confusing.
        assert!(!is_protected_ancestor(p("/etc/vps-panel")));
    }

    #[test]
    fn top_level_directories_are_locked() {
        assert!(is_top_level(p("/")));
        assert!(is_top_level(p("/srv")));
        assert!(is_top_level(p("/opt")));
        assert!(!is_top_level(p("/srv/apps")));
        assert!(!is_top_level(p("/home/adrian")));
    }

    #[test]
    fn ordinary_app_content_is_not_locked() {
        assert!(!entry_locked(p("/srv/apps/myapp")));
        assert!(!entry_locked(p("/srv/apps/myapp/node_modules")));
        assert!(!entry_locked(p("/var/www/example.com/index.html")));
        assert!(!entry_locked(p("/home/adrian/notes.txt")));
    }

    #[test]
    fn clean_rel_rejects_traversal_and_absolute_paths() {
        assert!(clean_rel("..").is_err());
        assert!(clean_rel("foo/../../etc").is_err());
        assert!(clean_rel("foo/..").is_err());
        // A leading slash is stripped rather than rejected — the panel sends
        // root-relative paths and an accidental "/" prefix shouldn't 400.
        assert_eq!(clean_rel("/foo/bar").unwrap(), PathBuf::from("foo/bar"));
        assert_eq!(clean_rel("./foo").unwrap(), PathBuf::from("foo"));
        assert_eq!(clean_rel("").unwrap(), PathBuf::from(""));
        // `..` inside a NAME is a legitimate filename, not traversal.
        assert_eq!(clean_rel("a..b").unwrap(), PathBuf::from("a..b"));
    }

    #[test]
    fn name_validation_matches_what_linux_allows() {
        assert!(validate_name("index.html").is_ok());
        assert!(validate_name(".env.local").is_ok());
        assert!(validate_name("a file with spaces").is_ok());
        assert!(validate_name("").is_err());
        assert!(validate_name(".").is_err());
        assert!(validate_name("..").is_err());
        assert!(validate_name("a/b").is_err());
        assert!(validate_name(" leading").is_err());
        assert!(validate_name("trailing ").is_err());
        assert!(validate_name(&"x".repeat(256)).is_err());
    }

    #[test]
    fn read_only_roots_refuse_every_mutation() {
        let logs = ROOTS.iter().find(|r| r.id == "logs").unwrap();
        assert!(guard_write(logs, p("/var/log/nginx/access.log")).is_err());
        assert!(guard_write_into(logs, p("/var/log/nginx")).is_err());
    }

    #[test]
    fn nothing_may_be_created_directly_in_slash() {
        let fs_root = ROOTS.iter().find(|r| r.id == "fs").unwrap();
        // Otherwise the new directory would be top-level, hence undeletable.
        assert!(guard_write_into(fs_root, p("/")).is_err());
        assert!(guard_write_into(fs_root, p("/srv/apps")).is_ok());
    }

    #[test]
    fn the_fs_root_still_honours_the_protected_list() {
        let fs_root = ROOTS.iter().find(|r| r.id == "fs").unwrap();
        assert!(guard_write(fs_root, p("/etc/vps-panel/.env")).is_err());
        assert!(guard_write(fs_root, p("/etc")).is_err());
        assert!(guard_write(fs_root, p("/srv")).is_err());
        assert!(guard_write(fs_root, p("/srv/apps/myapp")).is_ok());
    }

    #[test]
    fn the_drive_root_is_writable_and_ordinary() {
        let drive = ROOTS.iter().find(|r| r.id == "drive").unwrap();
        assert_eq!(drive.path, "/srv/drive");
        assert!(drive.writable);
        // It must NOT inherit any of the locks — it's the one root whose whole
        // point is that the operator can do what they like inside it.
        assert!(!entry_locked(p("/srv/drive/photos/2026")));
        assert!(guard_write(drive, p("/srv/drive/notes.txt")).is_ok());
        assert!(guard_write_into(drive, p("/srv/drive")).is_ok());
    }

    #[test]
    fn scriptable_types_are_never_served_inline() {
        // The panel serves downloads from its OWN origin, so an inline SVG or
        // HTML file would run script against the operator's panel session.
        assert!(!inline_safe(mime_for("payload.svg")));
        assert!(!inline_safe(mime_for("payload.html")));
        assert!(!inline_safe(mime_for("archive.zip")));
        // These are safe to render and are what the preview modal uses.
        assert!(inline_safe(mime_for("photo.PNG")));
        assert!(inline_safe(mime_for("scan.pdf")));
        assert!(inline_safe(mime_for("notes.md")));
    }

    #[test]
    fn mime_lookup_is_extension_based_and_falls_back() {
        assert_eq!(mime_for("a.jpeg"), "image/jpeg");
        assert_eq!(mime_for("a.JPG"), "image/jpeg");
        assert_eq!(mime_for("server.conf"), "text/plain");
        assert_eq!(mime_for("no-extension"), "application/octet-stream");
        assert_eq!(mime_for("mystery.qqq"), "application/octet-stream");
    }

    #[test]
    fn filenames_survive_the_content_disposition_header() {
        assert_eq!(pct_encode("plain.txt"), "plain.txt");
        assert_eq!(pct_encode("a b.txt"), "a%20b.txt");
        assert_eq!(pct_encode("š.txt"), "%C5%A1.txt");
        // A newline in a filename would otherwise split the header.
        assert_eq!(pct_encode("a\nb"), "a%0Ab");
    }

    #[test]
    fn trash_ids_are_constrained_to_what_we_generate() {
        assert!(is_safe_trash_id("20260815143007-1f4c8a2b"));
        assert!(!is_safe_trash_id("../../etc"));
        assert!(!is_safe_trash_id("has/slash"));
        assert!(!is_safe_trash_id(""));
        assert!(!is_safe_trash_id(&"a".repeat(65)));
    }

    #[test]
    fn expiry_is_measured_from_the_manifest_timestamp() {
        let mut meta = TrashMeta {
            id: "x".into(),
            name: "old".into(),
            kind: "file".into(),
            root_id: "apps".into(),
            original_path: "/srv/apps/old".into(),
            size: 0,
            trashed_at: (Utc::now() - Duration::days(31))
                .to_rfc3339_opts(SecondsFormat::Secs, true),
        };
        assert!(is_expired(&meta));
        assert_eq!(days_left(&meta), 0);

        meta.trashed_at = (Utc::now() - Duration::days(2))
            .to_rfc3339_opts(SecondsFormat::Secs, true);
        assert!(!is_expired(&meta));
        assert_eq!(days_left(&meta), TRASH_RETENTION_DAYS - 2);

        // A corrupt or hand-edited timestamp must not be purged on a guess.
        meta.trashed_at = "not a timestamp".into();
        assert!(!is_expired(&meta));
    }
}
