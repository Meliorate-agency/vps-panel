//! Pulling a server snapshot from another server's one-time link into
//! `incoming/`, on the server that will import it.
//!
//! curl does the transfer: resumable (`-C -` with If-Range on the file's
//! ETag, so a changed file is never stitched onto an old part), no
//! redirects, http(s) only. The link's secret reaches curl on stdin (`-K -`),
//! never on its command line, and is not kept on disk: a pull interrupted by
//! an agent restart resumes when the same link is pulled again (the part
//! file and the client id stay in `incoming/<link id>.pull.json`).

use super::format::{self, Sidecar};
use crate::error::{AgentError, AgentResult};
use crate::ipc::tasks::TaskHandle;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

#[derive(Deserialize)]
pub struct PullReq {
    /// The link without its fragment.
    pub url: String,
    /// The fragment: the link's secret.
    pub secret: String,
}

/// What the pull keeps between attempts (and agent restarts). No secret.
#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(default)]
struct PullState {
    url: String,
    client_id: String,
    name: String,
    size: u64,
    sha256: String,
    etag: String,
}

/// The link's id when `url` is a snapshot link: http(s), a host, the exact
/// path, nothing else. Limits what a pasted address can make this server
/// fetch.
pub fn link_id(url: &str) -> Result<String, String> {
    let u = url::Url::parse(url).map_err(|_| "that is not a link".to_string())?;
    if !matches!(u.scheme(), "https" | "http") {
        return Err("the link must start with https:// (or http://)".into());
    }
    if u.host_str().is_none_or(str::is_empty) || !u.username().is_empty() || u.password().is_some() {
        return Err("the link has no plain host name".into());
    }
    if u.query().is_some() || u.fragment().is_some() {
        return Err("the link must not carry a query or a fragment here".into());
    }
    let id = u
        .path()
        .strip_prefix("/api/snapshot-pull/")
        .filter(|id| id.len() == 22 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
        .ok_or_else(|| "that is not a snapshot link (.../api/snapshot-pull/<id>)".to_string())?;
    Ok(id.to_string())
}

fn valid_secret(s: &str) -> bool {
    s.len() == 43 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub async fn start(state: AppState, req: PullReq) -> AgentResult<String> {
    let id = link_id(&req.url).map_err(AgentError::Invalid)?;
    if !valid_secret(&req.secret) {
        return Err(AgentError::Invalid("the link's secret (after #) is missing or cut".into()));
    }
    super::export::refuse_overlap(&state)?;
    super::export::ensure_private_dir(Path::new(format::SNAPSHOT_DIR)).await?;
    super::export::ensure_private_dir(Path::new(format::INCOMING_DIR)).await?;
    let task = state.tasks.create("snapshots.pull");
    let task_id = task.id.clone();
    tokio::spawn(async move {
        task.start();
        match run(&req, &id, &task).await {
            Ok(()) => task.finish(true),
            Err(e) => {
                task.log(format!("the snapshot was not pulled: {e}"));
                task.finish(false);
            }
        }
    });
    Ok(task_id)
}

fn state_path(id: &str) -> PathBuf {
    Path::new(format::INCOMING_DIR).join(format!("{id}.pull.json"))
}

async fn run(req: &PullReq, id: &str, task: &TaskHandle) -> AgentResult<()> {
    let mut st: PullState = match tokio::fs::read(state_path(id)).await {
        Ok(b) => serde_json::from_slice(&b).unwrap_or_default(),
        Err(_) => PullState::default(),
    };
    if st.client_id.is_empty() || st.url != req.url {
        st = PullState { url: req.url.clone(), client_id: random_client_id(), ..Default::default() };
    }

    task.log(format!("asking {} about the snapshot", host_of(&req.url)));
    let head = head(req, &st.client_id).await?;
    let name = head.header("x-snapshot-name").unwrap_or_default();
    let size: u64 = head.header("x-snapshot-size").and_then(|s| s.parse().ok()).unwrap_or(0);
    let sha256 = head.header("x-snapshot-sha256").unwrap_or_default().to_ascii_lowercase();
    let fmt: u32 = head.header("x-snapshot-format").and_then(|s| s.parse().ok()).unwrap_or(0);
    let commit = head.header("x-snapshot-panel-commit").unwrap_or_default();
    let etag = head.header("etag").unwrap_or_default();
    if !format::is_valid_stem(&name) || size == 0 || sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(AgentError::Invalid("the other server did not describe a snapshot (is it a vps-panel?)".into()));
    }
    if fmt > format::FORMAT {
        return Err(AgentError::Conflict(format!(
            "the snapshot is in format {fmt}, newer than this panel reads ({}); update this panel first",
            format::FORMAT
        )));
    }
    task.log(format!("snapshot {name}: {} (sha256 {sha256})", super::export::human(size)));
    if !commit.is_empty() && !knows_commit(&commit).await {
        task.log(format!(
            "note: this panel does not have the commit that made the snapshot ({}); update this panel before importing it",
            &commit[..commit.len().min(12)]
        ));
    }

    let final_path = Path::new(format::INCOMING_DIR).join(format::file_name(&name));
    let part = final_path.with_file_name(format!("{}.part", format::file_name(&name)));
    if tokio::fs::metadata(&final_path).await.is_ok() {
        return Err(AgentError::Conflict(format!("{name} is here already; delete it first to pull it again")));
    }
    // A different file behind the same link (or a first attempt): start over.
    if st.name != name || st.size != size || st.sha256 != sha256 || st.etag != etag {
        let _ = tokio::fs::remove_file(&part).await;
        st.name = name.clone();
        st.size = size;
        st.sha256 = sha256.clone();
        st.etag = etag.clone();
    }
    save_state(id, &st).await?;

    let have = tokio::fs::metadata(&part).await.map(|m| m.len()).unwrap_or(0);
    let free = crate::handlers::files::available_bytes(Path::new(format::INCOMING_DIR)).unwrap_or(u64::MAX);
    let need = size.saturating_sub(have) + MIN_FREE_BYTES;
    if free < need {
        return Err(AgentError::Conflict(format!(
            "not enough free disk space: {} more is needed and {} is free",
            super::export::human(need),
            super::export::human(free)
        )));
    }
    if have > 0 {
        task.log(format!("resuming at {}", super::export::human(have)));
    }

    download(req, &st, &part, task).await?;

    task.log("checking the file's sha256");
    let p = part.clone();
    let got = tokio::task::spawn_blocking(move || format::hash_file(&p))
        .await
        .map_err(|e| AgentError::Other(anyhow::anyhow!("hash: {e}")))??;
    if got.bytes != size || got.sha256 != sha256 {
        let _ = tokio::fs::remove_file(&part).await;
        let _ = tokio::fs::remove_file(state_path(id)).await;
        return Err(AgentError::Conflict(
            "the file arrived damaged (its sha256 does not match); pull it again".into(),
        ));
    }
    crate::fs_atomic::chmod(&part, 0o600).await?;
    tokio::fs::rename(&part, &final_path).await?;
    let sidecar = Sidecar {
        name: name.clone(),
        kind: format::KIND.to_string(),
        format: fmt,
        size,
        sha256,
        created_at: created_at_of(&name),
        panel_commit: commit,
        source_host: host_of(&req.url),
        origin: "pulled".into(),
        ..Default::default()
    };
    crate::fs_atomic::write(
        &final_path.with_file_name(format!("{name}.json")),
        &serde_json::to_vec_pretty(&sidecar).unwrap_or_default(),
        0o600,
    )
    .await?;
    let _ = tokio::fs::remove_file(state_path(id)).await;
    // Tell the other server the link is used up (best effort).
    if let Err(e) = curl_simple(req, &st.client_id, "DELETE").await {
        task.log(format!("note: the other server was not told the pull is done ({e}); its link expires on its own"));
    }
    task.log(format!("{name} is here; import it from the list on Backups"));
    Ok(())
}

const MIN_FREE_BYTES: u64 = 1024 * 1024 * 1024;

/// Attempts before giving up, and the longest wait between two.
const MAX_ATTEMPTS: u32 = 40;
const MAX_BACKOFF: Duration = Duration::from_secs(60);

async fn download(req: &PullReq, st: &PullState, part: &Path, task: &TaskHandle) -> AgentResult<()> {
    let mut backoff = Duration::from_secs(5);
    let mut restarted = false;
    for attempt in 1..=MAX_ATTEMPTS {
        let outcome = curl_get(req, st, part, task).await?;
        match outcome {
            Get::Done => return Ok(()),
            // The server answered the whole file instead of the rest: the
            // part held here is of another version. Once, start over.
            Get::NoResume if !restarted => {
                restarted = true;
                task.log("the other server sent the whole file again; starting over");
                let _ = tokio::fs::remove_file(part).await;
                continue;
            }
            Get::Refused(code, why) => {
                return Err(AgentError::Conflict(format!("the other server refused (HTTP {code}): {why}")));
            }
            Get::NoResume | Get::Broken(_) | Get::Busy => {
                let why = match &outcome {
                    Get::Broken(e) => e.clone(),
                    Get::Busy => "too many requests".into(),
                    _ => "no resume".into(),
                };
                let have = tokio::fs::metadata(part).await.map(|m| m.len()).unwrap_or(0);
                task.log(format!(
                    "transfer stopped at {} ({why}); trying again in {} s (attempt {attempt} of {MAX_ATTEMPTS})",
                    super::export::human(have),
                    backoff.as_secs()
                ));
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }
    Err(AgentError::Conflict(
        "the transfer kept breaking off; the part is kept, pull with the same link again to continue".into(),
    ))
}

enum Get {
    Done,
    /// curl exit 33: a range was asked for and the whole file came back.
    NoResume,
    /// 4xx the pull cannot get past (expired, revoked, taken).
    Refused(u16, String),
    /// 429.
    Busy,
    /// Network trouble; worth another attempt.
    Broken(String),
}

fn curl_config(req: &PullReq, client_id: &str) -> String {
    // curl config syntax: a quoted value with \" and \\ escaped. The secret
    // and client id are base64url, so nothing needs escaping.
    format!(
        "header = \"Authorization: Bearer {}\"\nheader = \"X-Snapshot-Client: {}\"\n",
        req.secret, client_id
    )
}

fn curl_base() -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("curl");
    for name in crate::exec::AGENT_SECRET_ENV {
        cmd.env_remove(name);
    }
    cmd.kill_on_drop(true);
    cmd.args([
        "-sS",
        "--no-location",
        "--proto",
        "=https,http",
        "--connect-timeout",
        "20",
        "-K",
        "-",
    ]);
    cmd
}

async fn run_curl(mut cmd: tokio::process::Command, config: String) -> std::io::Result<(i32, String, String)> {
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(config.as_bytes()).await?;
        drop(stdin);
    }
    let out = child.wait_with_output().await?;
    Ok((
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    ))
}

async fn curl_get(req: &PullReq, st: &PullState, part: &Path, task: &TaskHandle) -> AgentResult<Get> {
    let mut cmd = curl_base();
    // --fail: an error answer (410 and the like) must never be written into
    // the part file, which a later attempt would resume from.
    cmd.args(["--fail", "--speed-limit", "1024", "--speed-time", "120", "-C", "-", "-w", "%{http_code}", "-o"]);
    cmd.arg(part);
    if !st.etag.is_empty() {
        cmd.arg("-H").arg(format!("If-Range: {}", st.etag));
    }
    cmd.arg(&req.url);
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(curl_config(req, &st.client_id).as_bytes()).await?;
        drop(stdin);
    }
    // Progress from the part file's size while curl runs.
    let started = std::time::Instant::now();
    let start_size = tokio::fs::metadata(part).await.map(|m| m.len()).unwrap_or(0);
    let mut tick = tokio::time::interval(Duration::from_secs(10));
    tick.tick().await;
    let wait = child.wait_with_output();
    tokio::pin!(wait);
    let out = loop {
        tokio::select! {
            out = &mut wait => break out?,
            _ = tick.tick() => {
                let now = tokio::fs::metadata(part).await.map(|m| m.len()).unwrap_or(0);
                let pct = (now * 100).checked_div(st.size).unwrap_or(0).min(100);
                let rate = now.saturating_sub(start_size) / started.elapsed().as_secs().max(1);
                task.log(format!(
                    "progress {pct}% | {} of {} | {}/s",
                    super::export::human(now),
                    super::export::human(st.size),
                    super::export::human(rate)
                ));
            }
        }
    };
    let code = out.status.code().unwrap_or(-1);
    let http: u16 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(0);
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if code == 33 {
        return Ok(Get::NoResume);
    }
    // -C - on a part that is already whole: the server answers 416.
    if http == 416 {
        let have = tokio::fs::metadata(part).await.map(|m| m.len()).unwrap_or(0);
        return Ok(if have == st.size { Get::Done } else { Get::NoResume });
    }
    if code == 0 && (http == 200 || http == 206) {
        return Ok(Get::Done);
    }
    Ok(classify(http, stderr))
}

fn classify(http: u16, stderr: String) -> Get {
    match http {
        429 => Get::Busy,
        404 => Get::Refused(404, "no such link (was all of it copied?)".into()),
        409 => Get::Refused(409, "another server is already pulling with this link".into()),
        410 => Get::Refused(410, "the link expired, was revoked or is used up; make a new one".into()),
        400..=499 => Get::Refused(http, "the link was not accepted".into()),
        _ => Get::Broken(if stderr.is_empty() { format!("HTTP {http}") } else { stderr }),
    }
}

struct Head {
    headers: Vec<(String, String)>,
}

impl Head {
    fn header(&self, name: &str) -> Option<String> {
        self.headers.iter().rev().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.clone())
    }
}

async fn head(req: &PullReq, client_id: &str) -> AgentResult<Head> {
    let mut cmd = curl_base();
    cmd.args(["--max-time", "60", "-I", "-w", "\n%{http_code}"]);
    cmd.arg(&req.url);
    let (code, stdout, stderr) = run_curl(cmd, curl_config(req, client_id)).await?;
    if code != 0 {
        return Err(AgentError::Conflict(format!("the other server did not answer: {stderr}")));
    }
    let http: u16 = stdout.lines().last().unwrap_or("").trim().parse().unwrap_or(0);
    if http != 200 {
        return Err(match classify(http, stderr) {
            Get::Refused(c, why) => AgentError::Conflict(format!("the other server refused (HTTP {c}): {why}")),
            Get::Busy => AgentError::Conflict("the other server says too many requests; try again in a minute".into()),
            _ => AgentError::Conflict(format!("the other server answered HTTP {http}")),
        });
    }
    let headers = stdout
        .lines()
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();
    Ok(Head { headers })
}

async fn curl_simple(req: &PullReq, client_id: &str, method: &str) -> Result<(), String> {
    let mut cmd = curl_base();
    cmd.args(["--max-time", "30", "-o", "/dev/null", "-w", "%{http_code}", "-X", method]);
    cmd.arg(&req.url);
    let (code, stdout, stderr) = run_curl(cmd, curl_config(req, client_id)).await.map_err(|e| e.to_string())?;
    match (code, stdout.trim()) {
        (0, "200") => Ok(()),
        (0, other) => Err(format!("HTTP {other}")),
        _ => Err(stderr),
    }
}

async fn save_state(id: &str, st: &PullState) -> AgentResult<()> {
    crate::fs_atomic::write(&state_path(id), &serde_json::to_vec(st).unwrap_or_default(), 0o600).await
}

fn random_client_id() -> String {
    use rand::Rng;
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut rng = rand::thread_rng();
    (0..32).map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char).collect()
}

fn host_of(url: &str) -> String {
    url::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default()
}

/// The time in a snapshot's name, as RFC 3339 (the manifest's own time is
/// inside the encrypted file).
fn created_at_of(stem: &str) -> String {
    let Some(stamp) = stem.strip_suffix('Z').and_then(|s| s.get(s.len().saturating_sub(15)..)) else {
        return String::new();
    };
    chrono::NaiveDateTime::parse_from_str(stamp, "%Y%m%d-%H%M%S")
        .map(|t| t.and_utc().to_rfc3339())
        .unwrap_or_default()
}

/// True when the panel's source has `commit` (so it is at that commit or
/// newer, or knows it from another branch).
pub async fn knows_commit(commit: &str) -> bool {
    if commit.is_empty() || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        return false;
    }
    let dir = crate::handlers::system::repo_dir();
    crate::exec::run("git", ["-C", dir.as_str(), "merge-base", "--is-ancestor", commit, "HEAD"])
        .await
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_snapshot_links_are_pulled() {
        let id = "AbCdEfGhIjKlMnOpQrStUv";
        assert_eq!(link_id(&format!("https://panel.example.com/api/snapshot-pull/{id}")).unwrap(), id);
        assert_eq!(link_id(&format!("http://203.0.113.10/api/snapshot-pull/{id}")).unwrap(), id);
        for bad in [
            format!("ftp://panel.example.com/api/snapshot-pull/{id}"),
            format!("https://panel.example.com/api/snapshot-pull/{id}?x=1"),
            format!("https://panel.example.com/api/snapshot-pull/{id}#secret"),
            format!("https://user:pw@panel.example.com/api/snapshot-pull/{id}"),
            format!("https://panel.example.com/api/snapshot-pull/{id}/x"),
            format!("https://panel.example.com/api/files/download?{id}"),
            "https://panel.example.com/api/snapshot-pull/short".to_string(),
            "file:///etc/shadow".to_string(),
            "not a url".to_string(),
        ] {
            assert!(link_id(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn secrets_and_config() {
        assert!(valid_secret(&"a".repeat(43)));
        assert!(!valid_secret(&"a".repeat(42)));
        assert!(!valid_secret(&format!("{}\"", "a".repeat(42))));
        let req = PullReq { url: String::new(), secret: "s".repeat(43) };
        let c = curl_config(&req, "client");
        assert_eq!(c.lines().count(), 2);
        assert!(c.contains("Authorization: Bearer sss"));
    }

    #[test]
    fn refusals_and_times() {
        assert!(matches!(classify(410, String::new()), Get::Refused(410, _)));
        assert!(matches!(classify(429, String::new()), Get::Busy));
        assert!(matches!(classify(0, "Connection reset".into()), Get::Broken(_)));
        assert_eq!(created_at_of("panel-example-com-20261127-130509Z"), "2026-11-27T13:05:09+00:00");
        assert_eq!(random_client_id().len(), 32);
    }
}
