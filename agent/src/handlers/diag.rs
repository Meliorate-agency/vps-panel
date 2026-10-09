use crate::error::AgentError;
use axum::Json;
use serde::Serialize;
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
