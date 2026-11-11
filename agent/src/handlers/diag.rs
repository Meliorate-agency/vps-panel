use crate::error::{AgentError, AgentResult};
use axum::{extract::Query, Json};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::fs;

#[derive(Serialize)]
pub struct SysInfo {
    pub uptime_secs: u64,
    pub load_1: f64,
    pub load_5: f64,
    pub load_15: f64,
    pub mem_total_kb: u64,
    pub mem_available_kb: u64,
    pub swap_total_kb: u64,
    pub swap_free_kb: u64,
    pub disk: Vec<DiskUsage>,
    pub cpu_count: usize,
    pub net: Vec<NetCounter>,
}

#[derive(Serialize)]
pub struct DiskUsage {
    pub mount: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
}

#[derive(Serialize)]
pub struct NetCounter {
    pub iface: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

pub async fn sysinfo() -> Result<Json<SysInfo>, AgentError> {
    let uptime = fs::read_to_string("/proc/uptime").await.unwrap_or_default();
    let uptime_secs = uptime
        .split_whitespace()
        .next()
        .and_then(|s| s.parse::<f64>().ok())
        .map(|f| f as u64)
        .unwrap_or(0);

    let loadavg = fs::read_to_string("/proc/loadavg").await.unwrap_or_default();
    let mut parts = loadavg.split_whitespace();
    let load_1 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let load_5 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let load_15 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);

    let meminfo = fs::read_to_string("/proc/meminfo").await.unwrap_or_default();
    let mem_total_kb = parse_meminfo(&meminfo, "MemTotal:");
    let mem_available_kb = parse_meminfo(&meminfo, "MemAvailable:");
    let swap_total_kb = parse_meminfo(&meminfo, "SwapTotal:");
    let swap_free_kb = parse_meminfo(&meminfo, "SwapFree:");

    let cpu_count = num_cpus_from_proc().await;

    let disk = disk_usage().await;
    let net = net_counters().await;

    Ok(Json(SysInfo {
        uptime_secs,
        load_1,
        load_5,
        load_15,
        mem_total_kb,
        mem_available_kb,
        swap_total_kb,
        swap_free_kb,
        cpu_count,
        disk,
        net,
    }))
}

pub(crate) fn parse_meminfo(content: &str, key: &str) -> u64 {
    content
        .lines()
        .find(|l| l.starts_with(key))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

async fn num_cpus_from_proc() -> usize {
    let cpuinfo = fs::read_to_string("/proc/cpuinfo").await.unwrap_or_default();
    cpuinfo
        .lines()
        .filter(|l| l.starts_with("processor"))
        .count()
        .max(1)
}

async fn disk_usage() -> Vec<DiskUsage> {
    // Exclude pseudo / overlay filesystems whose stats just reflect the
    // underlying disk:
    //   - tmpfs, devtmpfs     -> in-memory (irrelevant for storage planning)
    //   - overlay             -> every Docker layer mount inherits / stats and
    //                            would otherwise appear as a duplicate row
    //   - squashfs, fuse.snapfuse -> snap package mounts (read-only, identical)
    // Path-level filter below catches mounts that slipped through (e.g. bind
    // mounts under /var/lib/docker that aren't typed as `overlay`).
    let out = match crate::exec::run(
        "df",
        [
            "-B1",
            "--output=target,size,avail",
            "-x", "tmpfs",
            "-x", "devtmpfs",
            "-x", "overlay",
            "-x", "squashfs",
            "-x", "fuse.snapfuse",
        ],
    )
    .await
    {
        Ok(o) => o.stdout,
        Err(_) => return Vec::new(),
    };
    out.lines()
        .skip(1)
        .filter_map(|l| {
            let mut parts = l.split_whitespace();
            let mount = parts.next()?.to_string();
            if mount.starts_with("/var/lib/docker/")
                || mount.starts_with("/var/snap/")
                || mount.starts_with("/snap/")
                || mount.starts_with("/run/")
                || mount.starts_with("/proc/")
                || mount.starts_with("/sys/")
            {
                return None;
            }
            let total: u64 = parts.next()?.parse().ok()?;
            let free: u64 = parts.next()?.parse().ok()?;
            Some(DiskUsage {
                mount,
                total_bytes: total,
                free_bytes: free,
            })
        })
        .collect()
}

// ─── Folder sizes on / (dashboard "What uses the space") ────────────────

/// How long a measurement is served from memory. `du` reads every inode of
/// Docker's layers and every node_modules tree, so a dashboard visit must not
/// start one each time.
const FOLDER_CACHE_TTL: Duration = Duration::from_secs(600);
/// A refresh asked for within this long of the last measurement reuses it
/// (a double click, two tabs).
const FOLDER_REFRESH_MIN: Duration = Duration::from_secs(30);
/// One `du` gives up after this; its folder then shows as not measured.
const DU_TIMEOUT: Duration = Duration::from_secs(120);
/// `du`s running at once, each at low CPU and IO priority, so a measurement
/// never starves the apps of disk.
const DU_PARALLEL: usize = 3;

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FolderKind {
    /// Something people or apps keep.
    Data,
    /// The OS, the panel and the tools it needs.
    System,
    /// Downloads that are fetched again when needed: safe to clear.
    Cache,
    /// Left behind by builds and updates.
    Leftover,
}

/// Folders worth knowing the size of on `/`. One nested in another listed
/// folder is shown under it and not counted twice. Missing paths, symlinks
/// and paths on another filesystem are left out. The panel's source
/// checkout (REPO_DIR) is added at run time.
const FOLDERS: &[(&str, &str, FolderKind)] = &[
    ("/srv/apps", "Apps: code, dependencies and builds", FolderKind::Data),
    ("/srv/apps/.npm", "npm download cache of app builds", FolderKind::Cache),
    ("/var/lib/docker", "Docker: images, containers, volumes, build cache", FolderKind::Data),
    ("/var/lib/mysql", "MariaDB databases", FolderKind::Data),
    ("/var/lib/postgresql", "PostgreSQL databases", FolderKind::Data),
    ("/var/vmail", "Mailboxes", FolderKind::Data),
    ("/srv/drive", "Drive files and backup restores", FolderKind::Data),
    ("/var/www", "Static websites", FolderKind::Data),
    ("/var/lib/vps-panel", "Panel data", FolderKind::Data),
    ("/var/lib/vps-panel/trash", "Files trash, emptied after 30 days", FolderKind::Data),
    ("/var/lib/vps-panel/db-dumps", "Database dumps for backups", FolderKind::Data),
    ("/var/lib/vps-panel/build-snapshots", "Snapshots left by interrupted app builds", FolderKind::Leftover),
    ("/var/lib/vps-panel/.npm", "npm download cache of panel updates", FolderKind::Cache),
    ("/opt/vps-panel", "Panel install", FolderKind::System),
    ("/opt/vps-panel/app.prev", "Previous panel version, kept until the next update", FolderKind::Leftover),
    ("/root/.rustup", "Rust toolchain, needed for panel updates", FolderKind::System),
    ("/root/.cargo", "Rust tools and package cache", FolderKind::System),
    ("/root/.cargo/registry", "Downloaded Rust packages", FolderKind::Cache),
    ("/root/.npm", "npm download cache of root", FolderKind::Cache),
    ("/root/.cache", "Other caches of root", FolderKind::Cache),
    ("/var/log", "Logs", FolderKind::Data),
    ("/var/log/journal", "System journal", FolderKind::Data),
    ("/var/cache", "Package caches (apt)", FolderKind::Cache),
    ("/var/backups", "System backups", FolderKind::Data),
    ("/home", "User home folders", FolderKind::Data),
    ("/tmp", "Temporary files", FolderKind::Cache),
    ("/swapfile", "Swap file", FolderKind::System),
    ("/usr", "Operating system and programs", FolderKind::System),
    ("/var/lib/snapd", "Snap packages", FolderKind::System),
];

#[derive(Clone, Serialize)]
pub struct FolderSize {
    pub path: String,
    pub label: &'static str,
    pub kind: FolderKind,
    /// The listed folder this one sits in; its bytes are part of that one's.
    pub parent: Option<String>,
    /// None when `du` failed or timed out.
    pub bytes: Option<u64>,
}

#[derive(Clone, Serialize)]
pub struct FolderSizes {
    pub mount: &'static str,
    pub total_bytes: u64,
    /// Blocks in use (`df` Used). total - used - avail is the filesystem's
    /// reserve for root.
    pub used_bytes: u64,
    pub avail_bytes: u64,
    pub measured_at: String,
    pub took_ms: u64,
    pub folders: Vec<FolderSize>,
}

#[derive(Deserialize)]
pub struct FolderSizesQuery {
    #[serde(default)]
    pub refresh: bool,
}

/// GET /sysinfo/folders[?refresh=true]: how much of `/` each key folder
/// takes. Served from memory for FOLDER_CACHE_TTL; one measurement at a time.
pub async fn folder_sizes(Query(q): Query<FolderSizesQuery>) -> AgentResult<Json<FolderSizes>> {
    static CACHE: OnceLock<tokio::sync::Mutex<Option<(Instant, FolderSizes)>>> = OnceLock::new();
    let mut cache = CACHE.get_or_init(|| tokio::sync::Mutex::new(None)).lock().await;
    if let Some((at, sizes)) = cache.as_ref() {
        let fresh_for = if q.refresh { FOLDER_REFRESH_MIN } else { FOLDER_CACHE_TTL };
        if at.elapsed() < fresh_for {
            return Ok(Json(sizes.clone()));
        }
    }
    let sizes = measure_folders(&crate::handlers::system::repo_dir()).await?;
    *cache = Some((Instant::now(), sizes.clone()));
    Ok(Json(sizes))
}

async fn measure_folders(repo: &str) -> AgentResult<FolderSizes> {
    let started = Instant::now();
    let (total_bytes, used_bytes, avail_bytes) = root_fs_usage().await?;
    let root_dev = fs::metadata("/").await?.dev();

    let repo = repo.trim_end_matches('/');
    let wanted = FOLDERS
        .iter()
        .map(|&(p, label, kind)| (p.to_string(), label, kind))
        .chain([
            (repo.to_string(), "Panel source checkout", FolderKind::System),
            (
                format!("{repo}/target"),
                "Rust build output of panel updates, rebuilt on the next one",
                FolderKind::Leftover,
            ),
        ]);
    let mut present: Vec<(String, &'static str, FolderKind)> = Vec::new();
    for (path, label, kind) in wanted {
        if path.is_empty() || present.iter().any(|(p, ..)| *p == path) {
            continue;
        }
        if let Ok(m) = fs::symlink_metadata(&path).await {
            if !m.file_type().is_symlink() && m.dev() == root_dev {
                present.push((path, label, kind));
            }
        }
    }

    let paths: Vec<String> = present.iter().map(|(p, ..)| p.clone()).collect();
    let sizes: HashMap<String, Option<u64>> = futures::stream::iter(paths.clone())
        .map(|p| async move {
            let bytes = du_bytes(&p).await;
            (p, bytes)
        })
        .buffer_unordered(DU_PARALLEL)
        .collect()
        .await;

    let folders = present
        .into_iter()
        .map(|(path, label, kind)| FolderSize {
            parent: parent_of(&path, &paths),
            bytes: sizes.get(&path).copied().flatten(),
            path,
            label,
            kind,
        })
        .collect();
    Ok(FolderSizes {
        mount: "/",
        total_bytes,
        used_bytes,
        avail_bytes,
        measured_at: chrono::Utc::now().to_rfc3339(),
        took_ms: started.elapsed().as_millis() as u64,
        folders,
    })
}

/// The closest listed folder `path` sits in.
fn parent_of(path: &str, all: &[String]) -> Option<String> {
    all.iter()
        .filter(|p| p.as_str() != path && path.starts_with(&format!("{}/", p.trim_end_matches('/'))))
        .max_by_key(|p| p.len())
        .cloned()
}

/// Bytes on disk under `path`, this filesystem only. `du` exits 1 when a file
/// vanished or could not be read mid-walk (Docker, logs) but still prints the
/// total, so its output is read whatever the exit code.
async fn du_bytes(path: &str) -> Option<u64> {
    let mut cmd = tokio::process::Command::new("nice");
    cmd.args(["-n", "19", "ionice", "-c", "2", "-n", "7", "du", "-s", "-x", "-B1", "--", path])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let out = tokio::time::timeout(DU_TIMEOUT, cmd.output()).await.ok()?.ok()?;
    parse_du_total(&String::from_utf8_lossy(&out.stdout))
}

fn parse_du_total(stdout: &str) -> Option<u64> {
    stdout.lines().last()?.split_whitespace().next()?.parse().ok()
}

/// (size, used, avail) of `/` in bytes, as `df` reports them.
async fn root_fs_usage() -> AgentResult<(u64, u64, u64)> {
    let out = crate::exec::run("df", ["-B1", "--output=size,used,avail", "/"]).await?;
    let nums: Vec<u64> = out
        .stdout
        .lines()
        .nth(1)
        .unwrap_or_default()
        .split_whitespace()
        .filter_map(|s| s.parse().ok())
        .collect();
    match *nums.as_slice() {
        [size, used, avail] => Ok((size, used, avail)),
        _ => Err(AgentError::Other(anyhow::anyhow!("unexpected df output: {}", out.stdout.trim()))),
    }
}

async fn net_counters() -> Vec<NetCounter> {
    let content = fs::read_to_string("/proc/net/dev").await.unwrap_or_default();
    content
        .lines()
        .skip(2)
        .filter_map(|l| {
            let mut parts = l.trim().split_whitespace();
            let iface_raw = parts.next()?.trim_end_matches(':').to_string();
            if iface_raw == "lo" { return None; }
            let rx_bytes: u64 = parts.next()?.parse().ok()?;
            let _rx_packets = parts.next()?;
            for _ in 0..6 { parts.next()?; }
            let tx_bytes: u64 = parts.next()?.parse().ok()?;
            Some(NetCounter {
                iface: iface_raw,
                rx_bytes,
                tx_bytes,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_is_the_closest_listed_ancestor() {
        let all: Vec<String> = ["/var/lib/vps-panel", "/var/lib/vps-panel/trash", "/var/lib/vps-panel/trash/x", "/var/lib/vps-panelx", "/srv/apps"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(parent_of("/var/lib/vps-panel/trash/x", &all).as_deref(), Some("/var/lib/vps-panel/trash"));
        assert_eq!(parent_of("/var/lib/vps-panel/trash", &all).as_deref(), Some("/var/lib/vps-panel"));
        // A shared name prefix is not nesting.
        assert_eq!(parent_of("/var/lib/vps-panelx", &all), None);
        assert_eq!(parent_of("/srv/apps", &all), None);
    }

    #[test]
    fn du_total_is_the_first_field_of_the_last_line() {
        assert_eq!(parse_du_total("123456\t/srv/apps\n"), Some(123456));
        assert_eq!(parse_du_total(""), None);
        assert_eq!(parse_du_total("du: cannot read\n"), None);
    }
}
