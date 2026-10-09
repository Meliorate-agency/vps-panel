//! Swap: the one swap file the panel manages, `/swapfile`. When RAM runs
//! out, Linux moves memory no process is touching into it instead of letting
//! the OOM killer end a process. A hand-made `/swapfile` (the usual name) is
//! simply taken over. Other swap areas (partitions, zram) are only listed.

use crate::error::{AgentError, AgentResult};
use crate::exec;
use crate::fs_atomic;
use crate::ipc::TaskAccepted;
use crate::state::AppState;
use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Arc, OnceLock};
use tokio::fs;
use tokio::sync::Mutex;

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;
/// Smallest and largest swap file the panel makes, in MiB.
const MIN_MB: u64 = 512;
const MAX_MB: u64 = 64 * 1024;
/// RAM that must stay free after swapped-out memory is pulled back in.
const RAM_SPARE: u64 = 256 * MIB;
/// Disk that must stay free while a temporary bridge file exists.
const TEMP_DISK_FLOOR: u64 = GIB;

struct Paths {
    file: String,
    /// Temporary swap file that holds what RAM cannot while `file` is
    /// rebuilt. Only exists during a change (or after an interrupted one).
    bridge: String,
    fstab: String,
    sysctl: String,
}

impl Paths {
    fn system() -> Paths {
        Paths {
            file: "/swapfile".into(),
            bridge: "/swapfile.resizing".into(),
            fstab: "/etc/fstab".into(),
            sysctl: "/etc/sysctl.d/99-vps-panel-swap.conf".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SwapArea {
    pub name: String,
    /// `file` or `partition`, as /proc/swaps says.
    pub kind: String,
    pub size_bytes: u64,
    pub used_bytes: u64,
}

#[derive(Serialize)]
pub struct SwapStatus {
    pub path: String,
    /// Why this server cannot have a swap file (a container); None when it can.
    pub unsupported: Option<String>,
    /// Size of the swap file on disk; None when there is none.
    pub file_bytes: Option<u64>,
    pub active: bool,
    /// As the kernel uses it (a little under file_bytes); 0 when not active.
    pub size_bytes: u64,
    pub used_bytes: u64,
    /// `/etc/fstab` turns it on at boot.
    pub in_fstab: bool,
    /// Every other active swap area.
    pub other: Vec<SwapArea>,
    /// A temporary file an interrupted change left behind; the next save removes it.
    pub leftover_bridge: Option<String>,
    pub swappiness: u32,
    pub mem_total_bytes: u64,
    pub mem_available_bytes: u64,
    pub disk_total_bytes: u64,
    pub disk_free_bytes: u64,
    /// Always kept free: the larger of 2 GiB and 10% of the disk.
    pub disk_reserve_bytes: u64,
    pub min_mb: u64,
    /// Largest swap file the disk takes now, the current file's space included.
    pub max_mb: u64,
    /// A swap change is running.
    pub busy: bool,
}

#[derive(Deserialize)]
pub struct SetSwapReq {
    /// 0 turns swap off and deletes the file.
    pub size_mb: u64,
    pub swappiness: u32,
}

/// Everything a swap change depends on, read at one moment.
#[derive(Debug, Clone)]
struct Snapshot {
    file_bytes: Option<u64>,
    /// Disk the swap file really takes (a sparse file takes less than its
    /// size); what removing it gives back.
    file_disk_bytes: u64,
    bridge_bytes: Option<u64>,
    areas: Vec<SwapArea>,
    mem_total: u64,
    mem_available: u64,
    disk_total: u64,
    disk_free: u64,
    swappiness: u32,
    in_fstab: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct Plan {
    /// Swap file size after the change, bytes; 0 = no swap file.
    target: u64,
    /// Size of the swap file before the change.
    from: Option<u64>,
    /// The file is active and goes away or is rebuilt.
    swapoff: bool,
    remove: bool,
    /// Write a new file of `target` bytes and turn it on.
    create: bool,
    /// Size of a temporary bridge swap file to turn on first; 0 = none.
    bridge: u64,
    /// Memory in the file when it is swapped off.
    moving: u64,
}

fn swap_lock() -> Arc<Mutex<()>> {
    static LOCK: OnceLock<Arc<Mutex<()>>> = OnceLock::new();
    LOCK.get_or_init(|| Arc::new(Mutex::new(()))).clone()
}

pub async fn get() -> AgentResult<Json<SwapStatus>> {
    let p = Paths::system();
    let s = read_snapshot(&p).await?;
    let area = s.areas.iter().find(|a| a.name == p.file);
    Ok(Json(SwapStatus {
        unsupported: unsupported().await,
        file_bytes: s.file_bytes,
        active: area.is_some(),
        size_bytes: area.map_or(0, |a| a.size_bytes),
        used_bytes: area.map_or(0, |a| a.used_bytes),
        in_fstab: s.in_fstab,
        other: s.areas.iter().filter(|a| a.name != p.file).cloned().collect(),
        leftover_bridge: s.bridge_bytes.map(|_| p.bridge.clone()),
        swappiness: s.swappiness,
        mem_total_bytes: s.mem_total,
        mem_available_bytes: s.mem_available,
        disk_total_bytes: s.disk_total,
        disk_free_bytes: s.disk_free,
        disk_reserve_bytes: disk_reserve(s.disk_total),
        min_mb: MIN_MB,
        max_mb: max_mb(&s),
        busy: swap_lock().try_lock().is_err(),
        path: p.file,
    }))
}

/// Create, resize or remove the swap file and set vm.swappiness, as a task.
/// Anything that would fill the disk or leave swapped-out memory nowhere to
/// go is refused here, before anything changes.
pub async fn set(
    State(state): State<AppState>,
    Json(req): Json<SetSwapReq>,
) -> AgentResult<Json<TaskAccepted>> {
    // Kernels since 5.8 take up to 200 (meant for zram).
    if req.swappiness > 200 {
        return Err(AgentError::Invalid("swappiness must be 0-200".into()));
    }
    if let Some(why) = unsupported().await {
        return Err(AgentError::Conflict(why));
    }
    let lock = swap_lock()
        .try_lock_owned()
        .map_err(|_| AgentError::Conflict("a swap change is already running".into()))?;
    let paths = Paths::system();
    let snapshot = read_snapshot(&paths).await?;
    let plan = plan(&snapshot, &paths, req.size_mb).map_err(AgentError::Conflict)?;

    let task = state.tasks.create("system:swap");
    let task_id = task.id.clone();
    tokio::spawn(async move {
        let _lock = lock;
        task.start();
        let ok = apply(&paths, &plan, req.swappiness, &|line| task.log(line)).await;
        task.finish(ok);
    });
    Ok(Json(TaskAccepted { task_id }))
}

/// Inside a container the host owns memory and swapon is not allowed.
async fn unsupported() -> Option<String> {
    // Exit 0 and the container type inside one; exit 1 and "none" otherwise.
    let out = exec::run("systemd-detect-virt", ["--container"]).await.ok()?;
    let kind = out.stdout.trim();
    if kind.is_empty() || kind == "none" {
        return None;
    }
    Some(format!(
        "This server runs in a container ({kind}). Its memory is set by the host, which does \
         not let it turn on swap of its own; ask the provider for more RAM or swap."
    ))
}

async fn read_snapshot(p: &Paths) -> AgentResult<Snapshot> {
    let meminfo = fs::read_to_string("/proc/meminfo").await.unwrap_or_default();
    let meminfo_bytes = |key| crate::handlers::diag::parse_meminfo(&meminfo, key) * 1024;
    let dir = Path::new(&p.file).parent().unwrap_or(Path::new("/"));
    let (disk_total, disk_free) = disk_space(dir)?;
    let fstab = fs::read_to_string(&p.fstab).await.unwrap_or_default();
    let file = regular_file_size(&p.file).await?;
    Ok(Snapshot {
        file_bytes: file.map(|(size, _)| size),
        file_disk_bytes: file.map_or(0, |(_, disk)| disk),
        bridge_bytes: regular_file_size(&p.bridge).await?.map(|(size, _)| size),
        areas: read_areas().await,
        mem_total: meminfo_bytes("MemTotal:"),
        mem_available: meminfo_bytes("MemAvailable:"),
        disk_total,
        disk_free,
        swappiness: read_swappiness().await.unwrap_or(60),
        in_fstab: fstab_has(&fstab, &p.file),
    })
}

/// (size, bytes on disk) of a regular file, None when absent. Anything else
/// at that path (a directory, a symlink) is the operator's and is left alone.
async fn regular_file_size(path: &str) -> AgentResult<Option<(u64, u64)>> {
    match fs::symlink_metadata(path).await {
        Ok(m) if m.file_type().is_file() => Ok(Some((m.len(), disk_bytes(&m)))),
        Ok(_) => Err(AgentError::Conflict(format!(
            "{path} exists but is not a regular file; move it away by hand first"
        ))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(AgentError::Io(e)),
    }
}

#[cfg(unix)]
fn disk_bytes(m: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    m.blocks() * 512
}

#[cfg(not(unix))]
fn disk_bytes(m: &std::fs::Metadata) -> u64 {
    m.len()
}

async fn read_areas() -> Vec<SwapArea> {
    parse_proc_swaps(&fs::read_to_string("/proc/swaps").await.unwrap_or_default())
}

/// /proc/swaps rows; sizes there are KiB.
fn parse_proc_swaps(s: &str) -> Vec<SwapArea> {
    s.lines()
        .skip(1)
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let name = f.next()?.to_string();
            let kind = f.next()?.to_string();
            let size: u64 = f.next()?.parse().ok()?;
            let used: u64 = f.next()?.parse().ok()?;
            Some(SwapArea {
                name,
                kind,
                size_bytes: size * 1024,
                used_bytes: used * 1024,
            })
        })
        .collect()
}

async fn read_swappiness() -> Option<u32> {
    fs::read_to_string("/proc/sys/vm/swappiness")
        .await
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// (total, free) bytes of the filesystem holding `dir`.
#[cfg(unix)]
fn disk_space(dir: &Path) -> AgentResult<(u64, u64)> {
    let s = nix::sys::statvfs::statvfs(dir).map_err(|e| AgentError::Io(e.into()))?;
    let frag = s.fragment_size() as u64;
    Ok((s.blocks() as u64 * frag, s.blocks_available() as u64 * frag))
}

#[cfg(not(unix))]
fn disk_space(_dir: &Path) -> AgentResult<(u64, u64)> {
    Ok((0, 0))
}

/// Disk that a swap file may never take: the larger of 2 GiB and 10%, so
/// it cannot push the disk past the 90% that raises the disk.full alert.
fn disk_reserve(disk_total: u64) -> u64 {
    (disk_total / 10).max(2 * GIB)
}

fn max_mb(s: &Snapshot) -> u64 {
    let room = (s.disk_free + s.file_disk_bytes).saturating_sub(disk_reserve(s.disk_total));
    (room / MIB).min(MAX_MB)
}

/// What it takes to get from the snapshot to a `target_mb` swap file, or
/// why it cannot be done now.
fn plan(s: &Snapshot, p: &Paths, target_mb: u64) -> Result<Plan, String> {
    let file_area = s.areas.iter().find(|a| a.name == p.file);
    let bridge_area = s.areas.iter().find(|a| a.name == p.bridge);
    // The file in use keeps its size even outside the range the panel
    // creates (a hand-made 500 MB one), so other changes still save.
    let keep = target_mb > 0
        && file_area.is_some()
        && s.file_bytes.map(|b| (b + MIB / 2) / MIB) == Some(target_mb);
    let swapoff = file_area.is_some() && !keep;
    let remove = s.file_bytes.is_some() && !keep;
    let create = target_mb > 0 && !keep;
    if create && !(MIN_MB..=MAX_MB).contains(&target_mb) {
        return Err(format!(
            "A new swap file has to be between {} and {}.",
            human(MIN_MB * MIB),
            human(MAX_MB * MIB)
        ));
    }
    let target = target_mb * MIB;

    // Memory in the swap areas going away has to fit in free RAM (less a
    // spare) plus free space on the disk swap that stays. zram keeps its
    // pages in RAM, so its free space is no extra room.
    let free_in = |a: &SwapArea| a.size_bytes.saturating_sub(a.used_bytes);
    let spare = s.mem_available.saturating_sub(RAM_SPARE)
        + s.areas
            .iter()
            .filter(|a| a.name != p.file && a.name != p.bridge && !a.name.starts_with("/dev/zram"))
            .map(free_in)
            .sum::<u64>();
    let moving = if swapoff { file_area.map_or(0, |a| a.used_bytes) } else { 0 };
    let leaving = moving + bridge_area.map_or(0, |a| a.used_bytes);
    let new_free = if keep { file_area.map_or(0, free_in) } else { target };
    if leaving > spare + new_free {
        return Err(if target_mb == 0 {
            format!(
                "{} of memory is in swap and only {} of RAM is free to take it back. Turning \
                 swap off now would make Linux kill processes; stop something that uses memory \
                 first.",
                human(leaving),
                human(spare)
            )
        } else {
            format!(
                "{} of memory is in swap and only {} of RAM is free, so a {} swap file cannot \
                 hold it. Pick at least {}, or stop something that uses memory first.",
                human(leaving),
                human(spare),
                human(target),
                human((leaving - spare).div_ceil(512 * MIB) * 512 * MIB)
            )
        });
    }

    // The old file is swapped off before the new one exists. When RAM cannot
    // take its memory meanwhile, a temporary bridge file does.
    let mut bridge = 0;
    if moving > spare + bridge_area.map_or(0, free_in) {
        if bridge_area.is_some() {
            return Err(format!(
                "{} from an interrupted change is still in use and there is not enough free RAM \
                 to rebuild {} now. Try again when the server is less busy.",
                p.bridge, p.file
            ));
        }
        bridge = (moving - spare).div_ceil(MIB) * MIB + RAM_SPARE;
    }

    // Only what the disk loses counts: the old file's blocks come back first,
    // so rebuilding a full-size file at the same size needs no more disk.
    let growth = if create { target.saturating_sub(s.file_disk_bytes) } else { 0 };
    let reserve = disk_reserve(s.disk_total);
    if growth > 0 && s.disk_free.saturating_sub(growth) < reserve {
        return Err(format!(
            "Not enough disk space: a {} swap file would leave {} free, and {} has to stay free \
             (the larger of 2 GB and 10% of the disk). The most that fits now is {}.",
            human(target),
            human(s.disk_free.saturating_sub(growth)),
            human(reserve),
            human(max_mb(s) * MIB)
        ));
    }
    if bridge > 0 && s.disk_free.saturating_sub(growth + bridge) < TEMP_DISK_FLOOR {
        return Err(format!(
            "Not enough disk space to rebuild {} while it holds {}: that needs a temporary {} \
             file. Free some disk space, or try again when less memory is in swap.",
            p.file,
            human(moving),
            human(bridge)
        ));
    }

    Ok(Plan {
        target,
        from: s.file_bytes,
        swapoff,
        remove,
        create,
        bridge,
        moving,
    })
}

type Log<'a> = dyn Fn(String) + Sync + 'a;

/// The whole change as the task runs it; false when it failed.
async fn apply(p: &Paths, plan: &Plan, swappiness: u32, log: &Log<'_>) -> bool {
    let ok = match execute(p, plan, swappiness, log).await {
        Ok(()) => true,
        Err(e) => {
            log(format!("error: {e}"));
            clean_up(p, log).await;
            false
        }
    };
    log_state(p, log).await;
    ok
}

async fn execute(p: &Paths, plan: &Plan, swappiness: u32, log: &Log<'_>) -> Result<(), String> {
    match (plan.from, plan.create, plan.remove) {
        (None, true, _) => log(format!(
            "Creating a {} swap file at {}.",
            human(plan.target),
            p.file
        )),
        (Some(from), true, _) => log(format!(
            "Rebuilding {} ({}) as a {} swap file.",
            p.file,
            human(from),
            human(plan.target)
        )),
        (Some(from), false, true) => log(format!(
            "{} {} ({}).",
            if plan.swapoff { "Turning off and deleting" } else { "Deleting the unused" },
            p.file,
            human(from)
        )),
        _ => {}
    }
    if plan.bridge > 0 {
        log(format!(
            "Free RAM cannot take the {} in {} while it is rebuilt, so a temporary {} holds it \
             meanwhile.",
            human(plan.moving),
            p.file,
            p.bridge
        ));
        make_swap_file(&p.bridge, plan.bridge, log).await?;
        cmd(log, "swapon", &[&p.bridge]).await?;
    }
    if plan.swapoff {
        if plan.moving > 0 {
            log(format!(
                "Moving {} out of {} (this can take a minute).",
                human(plan.moving),
                p.file
            ));
        }
        cmd(log, "swapoff", &[&p.file]).await?;
    }
    if plan.remove {
        log(format!("$ rm {}", p.file));
        fs_atomic::remove_if_exists(Path::new(&p.file))
            .await
            .map_err(|e| e.to_string())?;
    }
    if plan.create {
        make_swap_file(&p.file, plan.target, log).await?;
        cmd(log, "swapon", &[&p.file]).await?;
    }
    set_fstab_entry(p, plan.target > 0, log).await?;
    drain_bridge(p, log).await?;
    set_swappiness(p, swappiness, log).await
}

async fn make_swap_file(path: &str, bytes: u64, log: &Log<'_>) -> Result<(), String> {
    fs_atomic::remove_if_exists(Path::new(path))
        .await
        .map_err(|e| e.to_string())?;
    create_private(path)
        .await
        .map_err(|e| format!("create {path}: {e}"))?;
    let dir = Path::new(path)
        .parent()
        .map_or_else(|| "/".to_string(), |d| d.to_string_lossy().into_owned());
    // Btrfs only swaps to a file that is not copy-on-write, set while empty.
    if fs_type(&dir).await.as_deref() == Some("btrfs") {
        cmd(log, "chattr", &["+C", path]).await?;
    }
    // Every filesystem that can hold a swap file here (ext4, xfs, btrfs)
    // allocates with fallocate. No dd fallback: after an ENOSPC it would
    // write zeros until the disk is full.
    cmd(log, "fallocate", &["-l", &bytes.to_string(), path]).await?;
    cmd(log, "mkswap", &[path]).await
}

/// Swap holds other processes' memory, so the file is root-only from the start.
#[cfg(unix)]
async fn create_private(path: &str) -> std::io::Result<()> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .await
        .map(|_| ())
}

#[cfg(not(unix))]
async fn create_private(path: &str) -> std::io::Result<()> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .await
        .map(|_| ())
}

async fn fs_type(dir: &str) -> Option<String> {
    let out = exec::run("findmnt", ["-n", "-o", "FSTYPE", "--target", dir]).await.ok()?;
    Some(out.stdout.trim().to_string())
}

async fn cmd(log: &Log<'_>, program: &str, args: &[&str]) -> Result<(), String> {
    log(format!("$ {program} {}", args.join(" ")));
    let out = exec::run(program, args).await.map_err(|e| e.to_string())?;
    for line in out.stdout.lines().filter(|l| !l.trim().is_empty()) {
        log(line.to_string());
    }
    Ok(())
}

/// True for an fstab entry (not a comment) whose device field is `file`.
fn fstab_line_is(line: &str, file: &str) -> bool {
    let line = line.trim();
    !line.starts_with('#') && line.split_whitespace().next() == Some(file)
}

fn fstab_has(fstab: &str, file: &str) -> bool {
    fstab.lines().any(|l| fstab_line_is(l, file))
}

/// `fstab` with a swap entry for `file`. An existing entry is kept as written.
fn fstab_with(fstab: &str, file: &str) -> String {
    if fstab_has(fstab, file) {
        return fstab.to_string();
    }
    let mut out = fstab.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!("{file} none swap sw 0 0\n"));
    out
}

/// `fstab` without any entry for `file`; every other byte is kept.
fn fstab_without(fstab: &str, file: &str) -> String {
    fstab
        .split_inclusive('\n')
        .filter(|l| !fstab_line_is(l, file))
        .collect()
}

async fn set_fstab_entry(p: &Paths, present: bool, log: &Log<'_>) -> Result<(), String> {
    let current = match fs::read_to_string(&p.fstab).await {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("read {}: {e}", p.fstab)),
    };
    let next = if present {
        fstab_with(&current, &p.file)
    } else {
        fstab_without(&current, &p.file)
    };
    if next == current {
        return Ok(());
    }
    fs_atomic::write(Path::new(&p.fstab), next.as_bytes(), 0o644)
        .await
        .map_err(|e| e.to_string())?;
    log(if present {
        format!("Added {} to {} so it is used again after a reboot.", p.file, p.fstab)
    } else {
        format!("Removed {} from {}.", p.file, p.fstab)
    });
    // systemd turns fstab into its swap units.
    if let Err(e) = cmd(log, "systemctl", &["daemon-reload"]).await {
        log(format!("warning: {e}"));
    }
    Ok(())
}

/// Swap off and delete the bridge file: the one this change made, or one an
/// interrupted change left.
async fn drain_bridge(p: &Paths, log: &Log<'_>) -> Result<(), String> {
    if read_areas().await.iter().any(|a| a.name == p.bridge) {
        log(format!("Moving what the temporary {} holds back.", p.bridge));
        cmd(log, "swapoff", &[&p.bridge]).await?;
    }
    if fs::symlink_metadata(&p.bridge).await.is_ok() {
        log(format!("$ rm {}", p.bridge));
        fs_atomic::remove_if_exists(Path::new(&p.bridge))
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

async fn set_swappiness(p: &Paths, value: u32, log: &Log<'_>) -> Result<(), String> {
    let conf = format!(
        "# Managed by vps-panel (Settings > Server > Swap).\n\
         # How early Linux moves idle memory to swap: lower waits until RAM is nearly full.\n\
         vm.swappiness = {value}\n"
    );
    if fs::read_to_string(&p.sysctl).await.ok().as_deref() != Some(conf.as_str()) {
        fs_atomic::write(Path::new(&p.sysctl), conf.as_bytes(), 0o644)
            .await
            .map_err(|e| e.to_string())?;
    }
    if read_swappiness().await != Some(value) {
        cmd(log, "sysctl", &["-w", &format!("vm.swappiness={value}")]).await?;
    }
    Ok(())
}

/// After a failed step: an unused swap file is wasted (or half-written)
/// disk, and an fstab entry for a file that is not in use makes the next
/// boot report a failed swap unit.
async fn clean_up(p: &Paths, log: &Log<'_>) {
    let areas = read_areas().await;
    let active = |name: &str| areas.iter().any(|a| a.name == name);
    for path in [&p.file, &p.bridge] {
        if !active(path)
            && fs::symlink_metadata(path).await.is_ok()
            && fs_atomic::remove_if_exists(Path::new(path)).await.is_ok()
        {
            log(format!("Removed the unused {path}."));
        }
    }
    if !active(&p.file) {
        if let Err(e) = set_fstab_entry(p, false, log).await {
            log(format!("warning: {e}"));
        }
    }
    // Draining it here could need more RAM than is free; the next save does
    // it with the same checks as any other change.
    if active(&p.bridge) {
        log(format!(
            "{} stays on for now (it holds memory); saving again removes it.",
            p.bridge
        ));
    }
}

async fn log_state(p: &Paths, log: &Log<'_>) {
    match read_areas().await.iter().find(|a| a.name == p.file) {
        Some(a) => log(format!(
            "Swap file {}: {}, {} in use.",
            p.file,
            human(a.size_bytes),
            human(a.used_bytes)
        )),
        None => log("No swap file is in use.".to_string()),
    }
    if let Some(v) = read_swappiness().await {
        log(format!("vm.swappiness = {v}"));
    }
}

/// "4 GB", "1.5 GB", "512 MB" (GB = GiB, like the dashboard). The kernel
/// counts a swap area one page short of its file, hence the MiB of slack.
fn human(bytes: u64) -> String {
    if bytes >= GIB - MIB {
        let gb = format!("{:.1}", bytes as f64 / GIB as f64);
        format!("{} GB", gb.strip_suffix(".0").unwrap_or(&gb))
    } else {
        format!("{} MB", (bytes + MIB / 2) / MIB)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(dir: &str) -> Paths {
        Paths {
            file: format!("{dir}/swapfile"),
            bridge: format!("{dir}/swapfile.resizing"),
            fstab: format!("{dir}/fstab"),
            sysctl: format!("{dir}/99-swap.conf"),
        }
    }

    fn area(name: &str, size_mb: u64, used_mb: u64) -> SwapArea {
        SwapArea {
            name: name.into(),
            kind: "file".into(),
            size_bytes: size_mb * MIB,
            used_bytes: used_mb * MIB,
        }
    }

    /// 24 GiB disk (2.4 GiB reserve) with `free_gb` free and `avail_mb` of RAM available.
    fn snapshot(file_mb: Option<u64>, areas: Vec<SwapArea>, avail_mb: u64, free_gb: u64) -> Snapshot {
        Snapshot {
            file_bytes: file_mb.map(|m| m * MIB),
            file_disk_bytes: file_mb.map_or(0, |m| m * MIB),
            bridge_bytes: None,
            areas,
            mem_total: 1024 * MIB,
            mem_available: avail_mb * MIB,
            disk_total: 24 * GIB,
            disk_free: free_gb * GIB,
            swappiness: 60,
            in_fstab: false,
        }
    }

    #[test]
    fn parses_proc_swaps() {
        let s = "Filename\t\t\t\tType\t\tSize\t\tUsed\t\tPriority\n\
                 /swapfile                               file\t\t4194300\t\t1024\t\t-2\n\
                 /dev/zram0                              partition\t1048572\t\t0\t\t100\n";
        assert_eq!(
            parse_proc_swaps(s),
            vec![
                SwapArea {
                    name: "/swapfile".into(),
                    kind: "file".into(),
                    size_bytes: 4194300 * 1024,
                    used_bytes: 1024 * 1024,
                },
                SwapArea {
                    name: "/dev/zram0".into(),
                    kind: "partition".into(),
                    size_bytes: 1048572 * 1024,
                    used_bytes: 0,
                },
            ]
        );
        assert!(parse_proc_swaps("Filename\tType\tSize\tUsed\tPriority\n").is_empty());
    }

    #[test]
    fn fstab_entry_is_added_once_and_removed_without_touching_the_rest() {
        let base = "# /etc/fstab\nLABEL=root / ext4 defaults 0 1\n# /swapfile none swap sw 0 0\n/swapfile2 none swap sw 0 0";
        assert!(!fstab_has(base, "/swapfile"));
        let with = fstab_with(base, "/swapfile");
        assert_eq!(with, format!("{base}\n/swapfile none swap sw 0 0\n"));
        assert_eq!(fstab_with(&with, "/swapfile"), with);
        assert_eq!(fstab_without(&with, "/swapfile"), format!("{base}\n"));

        // An entry the operator wrote is kept as written, and removed whole.
        let own = "LABEL=root / ext4 defaults 0 1\n\t/swapfile  swap  swap  defaults,pri=5  0 0\n";
        assert!(fstab_has(own, "/swapfile"));
        assert_eq!(fstab_with(own, "/swapfile"), own);
        assert_eq!(fstab_without(own, "/swapfile"), "LABEL=root / ext4 defaults 0 1\n");
        assert_eq!(fstab_with("", "/swapfile"), "/swapfile none swap sw 0 0\n");
    }

    #[test]
    fn creates_keeps_and_rebuilds() {
        let p = paths("");
        let create = plan(&snapshot(None, vec![], 500, 10), &p, 2048).unwrap();
        assert_eq!(
            create,
            Plan { target: 2048 * MIB, create: true, ..Plan::default() }
        );

        let active = || snapshot(Some(4096), vec![area("/swapfile", 4096, 100)], 800, 10);
        let keep = plan(&active(), &p, 4096).unwrap();
        assert!(!keep.swapoff && !keep.remove && !keep.create && keep.bridge == 0);

        let resize = plan(&active(), &p, 2048).unwrap();
        assert!(resize.swapoff && resize.remove && resize.create);
        assert_eq!((resize.bridge, resize.moving), (0, 100 * MIB));

        // A file that exists but is not in use is rebuilt, never swapped off.
        let unused = plan(&snapshot(Some(1024), vec![], 500, 10), &p, 1024).unwrap();
        assert!(!unused.swapoff && unused.remove && unused.create);

        let off = plan(&active(), &p, 0).unwrap();
        assert!(off.swapoff && off.remove && !off.create && off.target == 0);
    }

    #[test]
    fn a_full_swap_file_grows_through_a_bridge() {
        let p = paths("");
        let s = snapshot(Some(1024), vec![area("/swapfile", 1024, 1024)], 300, 10);
        let grow = plan(&s, &p, 4096).unwrap();
        assert!(grow.swapoff && grow.create);
        // RAM takes 300 - 256 MiB spare = 44 MiB, the bridge the rest plus a spare.
        assert_eq!(grow.bridge, (1024 - 44) * MIB + RAM_SPARE);

        // Free space on other disk swap counts as room too.
        let with_partition = snapshot(
            Some(1024),
            vec![area("/swapfile", 1024, 1024), area("/dev/vda2", 2048, 0)],
            300,
            10,
        );
        assert_eq!(plan(&with_partition, &p, 4096).unwrap().bridge, 0);
        assert!(plan(&with_partition, &p, 0).is_ok());

        // zram's does not: its pages live in the same RAM.
        let with_zram = snapshot(
            Some(1024),
            vec![area("/swapfile", 1024, 1024), area("/dev/zram0", 2048, 0)],
            300,
            10,
        );
        assert_eq!(plan(&with_zram, &p, 4096).unwrap().bridge, grow.bridge);
        assert!(plan(&with_zram, &p, 0).unwrap_err().contains("Turning swap off now"));
    }

    #[test]
    fn keeps_a_hand_made_size_but_only_creates_sizes_in_range() {
        let p = paths("");
        // A 500 MiB file in use keeps its size, so swappiness-only saves work.
        let small = snapshot(Some(500), vec![area("/swapfile", 500, 10)], 500, 10);
        let keep = plan(&small, &p, 500).unwrap();
        assert!(!keep.swapoff && !keep.create);
        let new = plan(&small, &p, 256).unwrap_err();
        assert!(new.contains("between 512 MB and 64 GB"), "{new}");
        assert!(plan(&snapshot(None, vec![], 500, 10), &p, 500).is_err());
        assert!(plan(&snapshot(None, vec![], 500, 10), &p, u64::MAX).is_err());
    }

    #[test]
    fn a_sparse_file_gives_back_only_the_disk_it_takes() {
        let p = paths("");
        // A 4 GiB file made with truncate takes no disk and swapon refuses it.
        let mut s = snapshot(Some(4096), vec![], 500, 3);
        s.file_disk_bytes = 0;
        let disk = plan(&s, &p, 4096).unwrap_err();
        assert!(disk.contains("Not enough disk space"), "{disk}");
        // Its 4 GiB are not counted as space a new file could reuse.
        assert_eq!(max_mb(&s), (3 * GIB - disk_reserve(24 * GIB)) / MIB);
    }

    #[test]
    fn refuses_what_memory_or_disk_cannot_take() {
        let p = paths("");
        let busy = snapshot(Some(4096), vec![area("/swapfile", 4096, 3000)], 500, 10);
        let off = plan(&busy, &p, 0).unwrap_err();
        assert!(off.contains("Turning swap off now"), "{off}");
        // 3000 MiB in swap, 244 MiB of RAM to spare: 1 GiB cannot hold it.
        let small = plan(&busy, &p, 1024).unwrap_err();
        assert!(small.contains("Pick at least 3 GB"), "{small}");

        // 3 GiB free on a 24 GiB disk leaves room for no swap file at all.
        let disk = plan(&snapshot(None, vec![], 500, 3), &p, 2048).unwrap_err();
        assert!(disk.contains("Not enough disk space"), "{disk}");
        // Rebuilding at the same size needs no more disk.
        assert!(plan(&snapshot(Some(2048), vec![], 500, 1), &p, 2048).is_ok());

        // The bridge may dip into the reserve, never below 1 GiB free: 4 GiB
        // free less 1 GiB growth less a 2260 MiB bridge leaves 812 MiB.
        let tight = snapshot(Some(2048), vec![area("/swapfile", 2048, 2048)], 300, 4);
        let bridge = plan(&tight, &p, 3072).unwrap_err();
        assert!(bridge.contains("temporary"), "{bridge}");
    }

    #[test]
    fn max_size_counts_the_current_file() {
        let s = snapshot(Some(4096), vec![], 500, 6);
        // 6 GiB free + 4 GiB file - 2.4 GiB reserve.
        assert_eq!(max_mb(&s), (10 * GIB - disk_reserve(24 * GIB)) / MIB);
        assert_eq!(max_mb(&snapshot(None, vec![], 500, 1)), 0);
    }

    #[test]
    fn human_sizes() {
        assert_eq!(human(4 * GIB), "4 GB");
        assert_eq!(human(3 * GIB / 2), "1.5 GB");
        assert_eq!(human(4194300 * 1024), "4 GB");
        assert_eq!(human(1048572 * 1024), "1 GB");
        assert_eq!(human(512 * MIB), "512 MB");
        assert_eq!(human(0), "0 MB");
    }

    async fn active_mb(name: &str) -> Option<u64> {
        read_areas()
            .await
            .into_iter()
            .find(|a| a.name == name)
            .map(|a| (a.size_bytes + MIB / 2) / MIB)
    }

    /// Turns the test's swap files off and deletes them even when an assert fails.
    struct Cleanup(String);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            for f in ["swapfile", "swapfile.resizing"] {
                let _ = std::process::Command::new("swapoff")
                    .arg(format!("{}/{f}", self.0))
                    .stderr(std::process::Stdio::null())
                    .status();
            }
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A scratch directory under `base` with a small fstab, cleaned up on drop.
    fn scratch(base: &str) -> (Paths, Cleanup, &'static str) {
        let dir = format!("{base}/vps-panel-swap-test-{}", uuid::Uuid::new_v4().simple());
        std::fs::create_dir_all(&dir).unwrap();
        let p = paths(&dir);
        let fstab = "# test fstab\nLABEL=root / ext4 defaults 0 1\n";
        std::fs::write(&p.fstab, fstab).unwrap();
        (p, Cleanup(dir), fstab)
    }

    #[tokio::test]
    #[ignore = "root only: makes, resizes and removes a real swap file (cargo test -- --ignored)"]
    async fn creates_resizes_and_removes_a_real_swap_file() {
        let (p, _cleanup, fstab) = scratch("/var/tmp");
        let swappiness = read_swappiness().await.unwrap();
        let log = |l: String| println!("{l}");

        let s = read_snapshot(&p).await.unwrap();
        assert!(apply(&p, &plan(&s, &p, 512).unwrap(), swappiness, &log).await);
        assert_eq!(active_mb(&p.file).await, Some(512));
        assert_eq!(
            std::fs::read_to_string(&p.fstab).unwrap(),
            format!("{fstab}{} none swap sw 0 0\n", p.file)
        );
        assert!(std::fs::read_to_string(&p.sysctl).unwrap().contains("vm.swappiness"));

        // Grow through a bridge, as when RAM cannot take what is in swap.
        let s = read_snapshot(&p).await.unwrap();
        let mut grow = plan(&s, &p, 1024).unwrap();
        assert!(grow.swapoff && grow.create && grow.bridge == 0);
        grow.bridge = 64 * MIB;
        assert!(apply(&p, &grow, swappiness, &log).await);
        assert_eq!(active_mb(&p.file).await, Some(1024));
        assert_eq!(active_mb(&p.bridge).await, None);
        assert!(!Path::new(&p.bridge).exists());
        assert_eq!(std::fs::read_to_string(&p.fstab).unwrap().matches(&p.file).count(), 1);

        // Nothing to do at the same size.
        let s = read_snapshot(&p).await.unwrap();
        assert!(!plan(&s, &p, 1024).unwrap().create);

        let s = read_snapshot(&p).await.unwrap();
        assert!(apply(&p, &plan(&s, &p, 0).unwrap(), swappiness, &log).await);
        assert_eq!(active_mb(&p.file).await, None);
        assert!(!Path::new(&p.file).exists());
        assert_eq!(std::fs::read_to_string(&p.fstab).unwrap(), fstab);
    }

    #[tokio::test]
    #[ignore = "root only: a swapon that fails leaves no file and no fstab entry (cargo test -- --ignored)"]
    async fn a_failed_swapon_leaves_nothing_behind() {
        // tmpfs cannot hold swap: the file is written, then swapon refuses it.
        let (p, _cleanup, fstab) = scratch("/dev/shm");
        let swappiness = read_swappiness().await.unwrap();
        let lines = std::sync::Mutex::new(Vec::new());
        let log = |l: String| lines.lock().unwrap().push(l);

        let s = read_snapshot(&p).await.unwrap();
        assert!(!apply(&p, &plan(&s, &p, 512).unwrap(), swappiness, &log).await);
        let lines = lines.into_inner().unwrap();
        println!("{}", lines.join("\n"));
        assert!(lines.iter().any(|l| l.starts_with("error:") && l.contains("swapon")));
        assert!(!Path::new(&p.file).exists());
        assert_eq!(std::fs::read_to_string(&p.fstab).unwrap(), fstab);
        assert_eq!(lines.last().map(String::as_str), Some(format!("vm.swappiness = {swappiness}").as_str()));
    }
}
