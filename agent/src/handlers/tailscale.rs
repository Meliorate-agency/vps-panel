//! Tailscale on this server. The quick-install recipe calls [`install`] for
//! Tailscale's apt repository and the package; everything after that goes
//! through the `tailscale` CLI, which talks to tailscaled: status, log in (a
//! browser link or an auth key), settings, connect, disconnect, log out.
//!
//! Logging in waits for a person: `tailscale up` prints a link and blocks
//! until someone signs in with it. So it runs as a task (`tailscale:up`), and
//! the link is also kept in LOGIN for GET /tailscale/status, which the Apps
//! dialog polls. Whoever opens that link can put this server in THEIR
//! tailnet, so the panel shows it (and the status) to admins only.

use crate::error::{AgentError, AgentResult};
use crate::exec;
use crate::fs_atomic;
use crate::ipc::tasks::{TaskHandle, TaskTracker};
use crate::ipc::TaskAccepted;
use crate::state::AppState;
use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::fs;
use tokio::process::Command;
use tokio::sync::Notify;

const KEYRING: &str = "/usr/share/keyrings/tailscale-archive-keyring.gpg";
const SOURCES_LIST: &str = "/etc/apt/sources.list.d/tailscale.list";
/// IP forwarding, which an exit node needs (Tailscale's own instructions).
/// Written when the panel turns the exit node on; removed when it turns it
/// off and no other route is advertised.
const FORWARDING_CONF: &str = "/etc/sysctl.d/99-vps-panel-tailscale.conf";
const FORWARDING: &str = "# Managed by vps-panel: IP forwarding for the Tailscale exit node (Apps > Tailscale).\n\
net.ipv4.ip_forward = 1\n\
net.ipv6.conf.all.forwarding = 1\n";
/// Where the tailnet admin approves machines and exit nodes.
const ADMIN_MACHINES: &str = "https://login.tailscale.com/admin/machines";

const LOGIN_KIND: &str = "tailscale:up";
/// How long a login waits for someone to open its link.
const LOGIN_WAIT: Duration = Duration::from_secs(15 * 60);
/// Reads (status, prefs). The Apps page waits for these.
const READ_TIMEOUT: Duration = Duration::from_secs(8);
/// Commands that change something (set, up, down, logout).
const CLI_TIMEOUT: Duration = Duration::from_secs(45);

/// The login task still running, if any.
struct Login {
    task_id: String,
    cancel: Arc<Notify>,
    /// The link `tailscale up` printed, kept until the task ends.
    auth_url: Option<String>,
}

static LOGIN: Mutex<Option<Login>> = Mutex::new(None);
/// Serialises starting a login, so two clicks cannot leave two `tailscale up`
/// running with only one of them cancellable.
static LOGIN_START: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

// ─── install (quick-install recipe) ──────────────────────────────────────

/// Quick install: Tailscale's apt repository for this distro and release,
/// the package, and tailscaled enabled. The same two files Tailscale's manual
/// install writes, so either way ends the same.
pub(crate) async fn install(task: TaskHandle) {
    match install_steps(&task).await {
        Ok(()) => {
            task.log("Tailscale installed. Next: Log in on the Tailscale card to add this server to your tailnet.");
            task.finish(true);
        }
        Err(e) => {
            task.log(e);
            task.finish(false);
        }
    }
}

async fn install_steps(task: &TaskHandle) -> Result<(), String> {
    let os = fs::read_to_string("/etc/os-release")
        .await
        .map_err(|e| format!("cannot read /etc/os-release: {e}"))?;
    let (distro, codename) = apt_release(&os).ok_or_else(|| {
        "Tailscale's apt repository covers Ubuntu and Debian; /etc/os-release names neither \
         (or no release codename)"
            .to_string()
    })?;
    let base = format!("https://pkgs.tailscale.com/stable/{distro}");
    let key_url = format!("{base}/{codename}.noarmor.gpg");

    task.log(format!("[1/5] {distro} {codename}: Tailscale's signing key -> {KEYRING}"));
    let tmp = tempfile::NamedTempFile::new().map_err(|e| format!("temp file: {e}"))?;
    let tmp_path = tmp.path().to_string_lossy().into_owned();
    let args = ["-fsSL", "--proto", "=https", "--max-time", "120", "-o", tmp_path.as_str(), key_url.as_str()];
    task.log(format!("$ curl {}", args.join(" ")));
    let code = exec::run_streaming("curl", args, |l| task.log(l.to_string()))
        .await
        .map_err(|e| format!("curl: {e}"))?;
    if code != 0 {
        return Err(format!(
            "could not download {key_url} (curl exit {code}). If this release is too new or too \
             old for Tailscale's repository, check {base}/"
        ));
    }
    let key = fs::read(tmp.path()).await.map_err(|e| format!("read {tmp_path}: {e}"))?;
    if !looks_like_pgp_key(&key) {
        return Err(format!("{key_url} did not return an OpenPGP key"));
    }
    fs_atomic::write(Path::new(KEYRING), &key, 0o644)
        .await
        .map_err(|e| format!("write {KEYRING}: {e}"))?;

    task.log(format!("[2/5] {SOURCES_LIST}"));
    fs_atomic::write(Path::new(SOURCES_LIST), sources_list(distro, &codename).as_bytes(), 0o644)
        .await
        .map_err(|e| format!("write {SOURCES_LIST}: {e}"))?;

    let steps: [&[&str]; 3] = [
        &["apt-get", "update"],
        &["apt-get", "install", "-y", "tailscale"],
        &["systemctl", "enable", "--now", "tailscaled"],
    ];
    for (i, step) in steps.iter().enumerate() {
        task.log(format!("[{}/5] $ {}", i + 3, step.join(" ")));
        let code = exec::run_streaming_envs(
            step[0],
            step[1..].iter().copied(),
            [("DEBIAN_FRONTEND", "noninteractive")],
            |l| task.log(l.to_string()),
        )
        .await
        .map_err(|e| format!("{}: {e}", step[0]))?;
        if code != 0 {
            return Err(format!("`{}` exited with code {code}", step.join(" ")));
        }
    }
    Ok(())
}

/// `ubuntu`/`debian` and the release codename from /etc/os-release, as
/// pkgs.tailscale.com names them. Ubuntu derivatives use their Ubuntu base.
fn apt_release(os_release: &str) -> Option<(&'static str, String)> {
    let field = |key: &str| {
        os_release.lines().find_map(|l| {
            let v = l.strip_prefix(key)?.strip_prefix('=')?;
            Some(v.trim().trim_matches('"').trim_matches('\'').to_ascii_lowercase())
        })
    };
    let id = field("ID").unwrap_or_default();
    let like = field("ID_LIKE").unwrap_or_default();
    let (distro, codename) = if id == "ubuntu" {
        ("ubuntu", field("VERSION_CODENAME").or_else(|| field("UBUNTU_CODENAME")))
    } else if id == "debian" {
        ("debian", field("VERSION_CODENAME"))
    } else if like.split_whitespace().any(|w| w == "ubuntu") {
        ("ubuntu", field("UBUNTU_CODENAME"))
    } else {
        return None;
    };
    // It goes into a URL and an apt line.
    let codename = codename.filter(|c| !c.is_empty() && c.chars().all(|ch| ch.is_ascii_lowercase()))?;
    Some((distro, codename))
}

/// Byte for byte the `<codename>.tailscale-keyring.list` Tailscale serves.
fn sources_list(distro: &str, codename: &str) -> String {
    format!(
        "# Tailscale packages for {distro} {codename}\n\
         deb [signed-by={KEYRING}] https://pkgs.tailscale.com/stable/{distro} {codename} main\n"
    )
}

/// A binary (non-armored) OpenPGP public key starts with a public-key packet
/// (tag 6), in the old or the new packet format. Catches an error page or an
/// empty body before apt does, with a clearer message.
fn looks_like_pgp_key(b: &[u8]) -> bool {
    if b.len() < 64 || b.len() > 1 << 20 {
        return false;
    }
    let h = b[0];
    match h & 0xC0 {
        0x80 => (h >> 2) & 0x0F == 6,
        0xC0 => h & 0x3F == 6,
        _ => false,
    }
}

// ─── status ──────────────────────────────────────────────────────────────

#[derive(Serialize, Default)]
pub struct TailscaleStatus {
    /// The `tailscale` command exists.
    pub installed: bool,
    /// Short version, e.g. "1.90.6".
    pub version: Option<String>,
    /// `systemctl is-active tailscaled`.
    pub daemon: String,
    /// The server's own hostname: the tailnet name when none is set.
    pub server_hostname: String,
    /// Running, NeedsLogin, NeedsMachineAuth, Stopped, Starting or NoState.
    pub backend_state: Option<String>,
    /// The link that logs this server in, while a login waits for it.
    pub auth_url: Option<String>,
    /// The login task still running, so a reopened dialog can follow it.
    pub login_task: Option<String>,
    /// This server as the tailnet sees it.
    pub node: Option<Node>,
    pub tailnet: Option<String>,
    /// The account this server is logged in as.
    pub user: Option<String>,
    /// Online first, then by name.
    pub peers: Vec<Node>,
    /// tailscaled's own warnings.
    pub health: Vec<String>,
    pub prefs: Option<Prefs>,
    /// Why status could not be read (tailscaled down, timeout).
    pub error: Option<String>,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct Node {
    pub name: String,
    /// MagicDNS name without the trailing dot.
    pub dns_name: String,
    pub ips: Vec<String>,
    pub os: String,
    pub online: bool,
    /// RFC 3339, only for a machine that is offline.
    pub last_seen: Option<String>,
    /// This server sends its internet traffic through that machine.
    pub exit_node: bool,
    /// That machine offers itself as an exit node.
    pub exit_node_option: bool,
}

/// The settings the panel edits, plus the ones it only shows.
#[derive(Serialize, Debug, PartialEq, Default)]
pub struct Prefs {
    /// Empty = the server's own hostname.
    pub hostname: String,
    pub ssh: bool,
    pub accept_routes: bool,
    pub accept_dns: bool,
    /// Advertises 0.0.0.0/0 and ::/0.
    pub exit_node: bool,
    /// Subnet routes set by hand (`tailscale set --advertise-routes`).
    pub advertised_routes: Vec<String>,
    pub shields_up: bool,
}

pub async fn status() -> Json<TailscaleStatus> {
    Json(read_all().await)
}

async fn read_all() -> TailscaleStatus {
    let unit = daemon_unit().await;
    let (status, prefs, daemon) = tokio::join!(
        tailscale(&["status", "--json"], READ_TIMEOUT),
        tailscale(&["debug", "prefs"], READ_TIMEOUT),
        is_active(unit),
    );
    let server_hostname = fs::read_to_string("/proc/sys/kernel/hostname")
        .await
        .map(|h| h.trim().to_string())
        .unwrap_or_default();
    let mut out = TailscaleStatus { daemon, server_hostname, ..Default::default() };
    match status {
        Err(e) if e.kind() == ErrorKind::NotFound => return out,
        Err(e) => {
            out.installed = true;
            out.error = Some(e.to_string());
        }
        Ok(o) => {
            out.installed = true;
            // Parsed whatever the exit code: the JSON is the answer, and the
            // text on stderr only matters when there is none.
            match serde_json::from_str::<Value>(&o.stdout) {
                Ok(v) => apply_status(&v, &mut out),
                Err(_) => out.error = Some(first_line(&o.stderr, &o.stdout)),
            }
        }
    }
    if let Ok(o) = prefs {
        if let Ok(v) = serde_json::from_str::<Value>(&o.stdout) {
            out.prefs = Some(parse_prefs(&v));
        }
    }
    // A link is shown only while a login started here waits for it:
    // tailscaled keeps an unused one after `tailscale logout`, and after the
    // login task gave up.
    match LOGIN.lock().unwrap().as_ref() {
        Some(l) => {
            out.login_task = Some(l.task_id.clone());
            if out.auth_url.is_none() {
                out.auth_url = l.auth_url.clone();
            }
        }
        None => out.auth_url = None,
    }
    if !matches!(out.backend_state.as_deref(), None | Some("NeedsLogin") | Some("NoState")) {
        out.auth_url = None;
    }
    out
}

fn apply_status(v: &Value, out: &mut TailscaleStatus) {
    out.backend_state = text(&v["BackendState"]);
    out.auth_url = text(&v["AuthURL"]);
    out.version = text(&v["Version"]).map(|s| s.split('-').next().unwrap_or_default().to_string());
    out.tailnet = text(&v["CurrentTailnet"]["Name"]);
    out.health = strings(&v["Health"]);
    let me = &v["Self"];
    if me.is_object() {
        let node = node(me);
        // Before the first login Self is a placeholder with no address.
        if !node.ips.is_empty() || !node.dns_name.is_empty() {
            out.node = Some(node);
        }
        if let Some(id) = me["UserID"].as_i64().filter(|id| *id != 0) {
            out.user = text(&v["User"][id.to_string()]["LoginName"]);
        }
    }
    let mut peers: Vec<Node> = v["Peer"]
        .as_object()
        .map(|m| m.values().map(node).collect())
        .unwrap_or_default();
    peers.sort_by(|a, b| {
        b.online
            .cmp(&a.online)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    out.peers = peers;
}

fn node(p: &Value) -> Node {
    let online = p["Online"].as_bool().unwrap_or(false);
    Node {
        name: text(&p["HostName"]).unwrap_or_default(),
        dns_name: text(&p["DNSName"]).unwrap_or_default().trim_end_matches('.').to_string(),
        ips: strings(&p["TailscaleIPs"]),
        os: text(&p["OS"]).unwrap_or_default(),
        online,
        // Zero time ("0001-01-01T00:00:00Z") = never, or online right now.
        last_seen: text(&p["LastSeen"]).filter(|s| !online && !s.starts_with("0001-")),
        exit_node: p["ExitNode"].as_bool().unwrap_or(false),
        exit_node_option: p["ExitNodeOption"].as_bool().unwrap_or(false),
    }
}

/// `tailscale debug prefs`: ipn.Prefs as JSON.
fn parse_prefs(v: &Value) -> Prefs {
    let routes = strings(&v["AdvertiseRoutes"]);
    let is_default = |r: &String| r == "0.0.0.0/0" || r == "::/0";
    Prefs {
        hostname: text(&v["Hostname"]).unwrap_or_default(),
        ssh: v["RunSSH"].as_bool().unwrap_or(false),
        accept_routes: v["RouteAll"].as_bool().unwrap_or(false),
        accept_dns: v["CorpDNS"].as_bool().unwrap_or(false),
        exit_node: routes.iter().any(|r| r == "0.0.0.0/0") && routes.iter().any(|r| r == "::/0"),
        advertised_routes: routes.iter().filter(|r| !is_default(r)).cloned().collect(),
        shields_up: v["ShieldsUp"].as_bool().unwrap_or(false),
    }
}

fn text(v: &Value) -> Option<String> {
    v.as_str().map(str::trim).filter(|s| !s.is_empty()).map(String::from)
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| a.iter().filter_map(text).collect())
        .unwrap_or_default()
}

// ─── log in ──────────────────────────────────────────────────────────────

/// What the panel sets. Login applies all of it (`up --reset`); a save on a
/// logged-in server changes exactly these and keeps the rest (`set`).
#[derive(Deserialize, Clone, Debug, Default)]
pub struct Options {
    /// Name on the tailnet. Empty = the server's hostname.
    #[serde(default)]
    pub hostname: String,
    #[serde(default)]
    pub ssh: bool,
    #[serde(default)]
    pub accept_routes: bool,
    #[serde(default)]
    pub accept_dns: bool,
    #[serde(default)]
    pub exit_node: bool,
}

#[derive(Deserialize)]
pub struct LoginReq {
    #[serde(flatten)]
    pub options: Options,
    /// Empty = log in with a link.
    #[serde(default)]
    pub auth_key: String,
}

#[derive(Serialize)]
pub struct Steps {
    pub steps: Vec<String>,
}

pub async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginReq>,
) -> AgentResult<Json<TaskAccepted>> {
    start_login(&state.tasks, req).await.map(Json)
}

async fn start_login(tasks: &TaskTracker, req: LoginReq) -> AgentResult<TaskAccepted> {
    let mut options = req.options;
    options.hostname = check_hostname(&options.hostname).map_err(AgentError::Invalid)?;
    let auth_key = check_auth_key(&req.auth_key).map_err(AgentError::Invalid)?;
    let now = read_all().await;
    if !now.installed {
        return Err(AgentError::Conflict("Tailscale is not installed".into()));
    }
    if let Some(s) = now.backend_state.as_deref() {
        refuse_login_in(s)?;
    }

    let _one_at_a_time = LOGIN_START.lock().await;
    // Another click on Log in replaces a login still waiting: a fresh link,
    // or an auth key after all.
    cancel_login().await;
    let task = tasks.create(LOGIN_KIND);
    let task_id = task.id.clone();
    let cancel = Arc::new(Notify::new());
    *LOGIN.lock().unwrap() = Some(Login {
        task_id: task_id.clone(),
        cancel: cancel.clone(),
        auth_url: None,
    });
    tokio::spawn(run_login(task, cancel, options, auth_key));
    Ok(TaskAccepted { task_id })
}

/// A server that is logged in changes settings, connects or logs out; it does
/// not log in again.
fn refuse_login_in(backend_state: &str) -> AgentResult<()> {
    match backend_state {
        "Running" | "Starting" | "Stopped" | "NeedsMachineAuth" => Err(AgentError::Conflict(format!(
            "this server is already logged in to Tailscale ({backend_state}); log out first to use another account"
        ))),
        _ => Ok(()),
    }
}

enum Outcome {
    Connected,
    NeedsApproval,
    CliExit(i32),
    CliError(String),
    Cancelled,
    TimedOut,
}

async fn run_login(task: TaskHandle, cancel: Arc<Notify>, options: Options, auth_key: Option<String>) {
    task.start();
    let ok = login_steps(&task, &cancel, &options, auth_key.as_deref()).await;
    {
        let mut login = LOGIN.lock().unwrap();
        if login.as_ref().is_some_and(|l| l.task_id == task.id) {
            *login = None;
        }
    }
    task.finish(ok);
}

async fn login_steps(task: &TaskHandle, cancel: &Notify, o: &Options, auth_key: Option<&str>) -> bool {
    let mut steps = Vec::new();
    let prepared = prepare_login(o, &mut steps).await;
    for s in steps.drain(..) {
        task.log(s);
    }
    let before = match prepared {
        Ok(v) => v,
        Err(e) => {
            task.log(e.to_string());
            return false;
        }
    };
    // tailscale reads the key from a file: never on a command line (`ps`)
    // or in this log. /run is RAM; the file goes when `key_file` drops.
    let key_file = match auth_key.map(write_key_file).transpose() {
        Ok(f) => f,
        Err(e) => {
            task.log(format!("could not hand the auth key to tailscale: {e}"));
            return false;
        }
    };
    // `up` asks for a new link by itself only when there is no node key. A
    // server whose key expired has one, and needs --force-reauth instead.
    let reauth = before["HaveNodeKey"].as_bool() == Some(true)
        && before["BackendState"].as_str() == Some("NeedsLogin");
    let args = up_args(o, key_file.as_ref().map(|f| f.path()), reauth);
    let Some(bin) = cli_path() else {
        task.log("Tailscale is not installed.");
        return false;
    };
    let bin = bin.to_string_lossy().into_owned();
    task.log(format!("$ tailscale {}", args.join(" ")));
    if auth_key.is_none() {
        task.log("Waiting for someone to open the login link (up to 15 minutes)...");
    }

    let mut links = LinkWatcher::default();
    let outcome = tokio::select! {
        r = exec::run_streaming(&bin, args.iter(), |line: &str| {
            task.log(line.to_string());
            if let Some(url) = links.feed(line) {
                if let Some(l) = LOGIN.lock().unwrap().as_mut().filter(|l| l.task_id == task.id) {
                    l.auth_url = Some(url);
                }
            }
        }) => match r {
            Ok(0) => Outcome::Connected,
            Ok(code) => Outcome::CliExit(code),
            Err(e) => Outcome::CliError(e.to_string()),
        },
        // `up` itself can miss the moment (an expired key re-used the same
        // link): tailscaled's state is what counts.
        o = logged_in() => o,
        _ = cancel.notified() => Outcome::Cancelled,
        _ = tokio::time::sleep(LOGIN_WAIT) => Outcome::TimedOut,
    };
    drop(key_file);

    let mut steps = Vec::new();
    let ok = match outcome {
        Outcome::Connected => {
            let now = read_all().await;
            match now.node {
                Some(n) => task.log(format!(
                    "Connected: {} ({}){}",
                    if n.dns_name.is_empty() { &n.name } else { &n.dns_name },
                    n.ips.join(", "),
                    now.tailnet.map(|t| format!(" in {t}")).unwrap_or_default()
                )),
                None => task.log("Connected."),
            }
            if o.exit_node {
                task.log(format!(
                    "Exit node offered: approve it at {ADMIN_MACHINES} (this server > Edit route settings)."
                ));
            }
            true
        }
        Outcome::NeedsApproval => {
            task.log(format!(
                "Logged in. This tailnet approves new machines by hand: an admin approves this server at {ADMIN_MACHINES}."
            ));
            true
        }
        Outcome::CliExit(code) => {
            task.log(format!("tailscale up exited with code {code}"));
            false
        }
        Outcome::CliError(e) => {
            task.log(format!("tailscale up: {e}"));
            false
        }
        Outcome::Cancelled => {
            task.log("Stopped: another login was started, or Log out was clicked.");
            false
        }
        Outcome::TimedOut => {
            task.log("Nobody logged in within 15 minutes. Click Log in again for a new link.");
            false
        }
    };
    if ok && !o.exit_node {
        release_forwarding(&mut steps).await;
    }
    for s in steps {
        task.log(s);
    }
    ok
}

/// tailscaled running, not logged in yet (else the login stops here), and
/// forwarding in place for an exit node. Returns `tailscale status --json`.
async fn prepare_login(o: &Options, steps: &mut Vec<String>) -> AgentResult<Value> {
    ensure_daemon(steps).await?;
    let mut status = read_status().await;
    // Right after tailscaled starts it is briefly NoState.
    for _ in 0..10 {
        if status.as_ref().is_some_and(|v| v["BackendState"].as_str() != Some("NoState")) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        status = read_status().await;
    }
    let status = status.ok_or_else(|| AgentError::Conflict("tailscaled does not answer".into()))?;
    if let Some(s) = status["BackendState"].as_str() {
        refuse_login_in(s)?;
    }
    if o.exit_node {
        ensure_forwarding(steps).await?;
    }
    Ok(status)
}

/// Resolves once tailscaled is logged in: Running, or waiting for an admin to
/// approve the machine.
async fn logged_in() -> Outcome {
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        match read_status().await.as_ref().and_then(|v| v["BackendState"].as_str()) {
            Some("Running") => return Outcome::Connected,
            Some("NeedsMachineAuth") => return Outcome::NeedsApproval,
            _ => {}
        }
    }
}

/// Stops a login still waiting for its link. Returns once its task ended, so
/// two `tailscale up` never run at the same time.
async fn cancel_login() -> bool {
    let Some((id, cancel)) = LOGIN
        .lock()
        .unwrap()
        .as_ref()
        .map(|l| (l.task_id.clone(), l.cancel.clone()))
    else {
        return false;
    };
    cancel.notify_one();
    for _ in 0..100 {
        if LOGIN.lock().unwrap().as_ref().is_none_or(|l| l.task_id != id) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    true
}

/// `tailscale up` arguments for a login: `--reset` makes the options the
/// whole configuration (no "requires mentioning all non-default flags").
fn up_args(o: &Options, key_file: Option<&Path>, reauth: bool) -> Vec<String> {
    let mut a = vec![
        "up".to_string(),
        "--reset".to_string(),
        format!("--timeout={}s", LOGIN_WAIT.as_secs()),
    ];
    a.extend(option_flags(o, false));
    if reauth {
        a.push("--force-reauth".into());
    }
    if let Some(p) = key_file {
        a.push(format!("--auth-key=file:{}", p.display()));
    }
    a
}

/// The panel's options as flags. `up --reset` leaves an empty hostname out
/// (reset = the server's own name); `set` passes it empty for the same.
fn option_flags(o: &Options, hostname_always: bool) -> Vec<String> {
    let mut a = Vec::new();
    if hostname_always || !o.hostname.is_empty() {
        a.push(format!("--hostname={}", o.hostname));
    }
    a.push(format!("--ssh={}", o.ssh));
    a.push(format!("--accept-routes={}", o.accept_routes));
    a.push(format!("--accept-dns={}", o.accept_dns));
    a.push(format!("--advertise-exit-node={}", o.exit_node));
    a
}

/// Picks the login link out of `tailscale up`'s output: the https line that
/// follows "To authenticate, visit:" (a blank line sits between them).
#[derive(Default)]
struct LinkWatcher {
    armed: bool,
}

impl LinkWatcher {
    fn feed(&mut self, line: &str) -> Option<String> {
        let t = line.trim();
        if t.is_empty() {
            return None;
        }
        if t == "To authenticate, visit:" {
            self.armed = true;
            return None;
        }
        let hit = self.armed && t.starts_with("https://") && !t.contains(char::is_whitespace);
        self.armed = false;
        hit.then(|| t.to_string())
    }
}

fn write_key_file(key: &str) -> std::io::Result<tempfile::NamedTempFile> {
    let dir = Path::new("/run/vps-panel");
    let mut b = tempfile::Builder::new();
    b.prefix("tailscale-authkey-");
    // 0600 by default: only root can read it.
    let mut f = if dir.is_dir() { b.tempfile_in(dir)? } else { b.tempfile()? };
    use std::io::Write;
    writeln!(f, "{key}")?;
    f.flush()?;
    Ok(f)
}

/// Empty stays empty (= the server's own hostname). Otherwise one DNS label,
/// which is what MagicDNS turns it into anyway.
fn check_hostname(raw: &str) -> Result<String, String> {
    let h = raw.trim();
    let ok = h.is_empty()
        || (h.len() <= 63
            && h.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            && !h.starts_with('-')
            && !h.ends_with('-'));
    if ok {
        Ok(h.to_string())
    } else {
        Err("The name on the tailnet takes letters, digits and hyphens (no dots or spaces), 63 characters at most.".into())
    }
}

/// None = log in with a link. The key never reaches a command line, so this
/// only catches a bad paste (two lines, a space, a truncated key).
fn check_auth_key(raw: &str) -> Result<Option<String>, String> {
    let k = raw.trim();
    if k.is_empty() {
        return Ok(None);
    }
    if k.len() < 16 || k.len() > 512 || !k.chars().all(|c| c.is_ascii_graphic()) {
        return Err("That does not look like an auth key (tskey-auth-..., one line, no spaces).".into());
    }
    Ok(Some(k.to_string()))
}

// ─── settings, connect, disconnect, log out ──────────────────────────────

pub async fn settings(Json(mut o): Json<Options>) -> AgentResult<Json<Steps>> {
    o.hostname = check_hostname(&o.hostname).map_err(AgentError::Invalid)?;
    let state = read_status().await.and_then(|v| text(&v["BackendState"]));
    if !matches!(state.as_deref(), Some("Running" | "Starting" | "Stopped" | "NeedsMachineAuth")) {
        return Err(AgentError::Conflict("log this server in to Tailscale first".into()));
    }
    let mut steps = Vec::new();
    if o.exit_node {
        ensure_forwarding(&mut steps).await?;
    }
    let mut args = vec!["set".to_string()];
    args.extend(option_flags(&o, true));
    run_cli(&args, &mut steps).await?;
    if !o.exit_node {
        release_forwarding(&mut steps).await;
    }
    Ok(Json(Steps { steps }))
}

/// Starts tailscaled if needed and, when it was disconnected, brings it back
/// up with every setting as it was (a bare `tailscale up`).
pub async fn connect() -> AgentResult<Json<Steps>> {
    let mut steps = Vec::new();
    ensure_daemon(&mut steps).await?;
    match read_status().await.and_then(|v| text(&v["BackendState"])).as_deref() {
        Some("Stopped") => run_cli(&["up".to_string()], &mut steps).await?,
        Some("Running" | "Starting") => steps.push("Already connected.".into()),
        _ => steps.push("tailscaled is running. Log in to connect.".into()),
    }
    Ok(Json(Steps { steps }))
}

pub async fn disconnect() -> AgentResult<Json<Steps>> {
    let mut steps = Vec::new();
    run_cli(&["down".to_string()], &mut steps).await?;
    Ok(Json(Steps { steps }))
}

/// Logs out, and cancels a login still waiting for its link.
pub async fn logout() -> AgentResult<Json<Steps>> {
    let mut steps = Vec::new();
    if cancel_login().await {
        steps.push("Stopped the login that was waiting.".into());
    }
    run_cli(&["logout".to_string()], &mut steps).await?;
    // tailscaled keeps an unused login link through `tailscale logout`, and
    // whoever opens it can still add this server to their tailnet. It only
    // lives in tailscaled's memory: a restart ends it (nothing is connected
    // at this point, so nothing is cut off).
    if read_status().await.and_then(|v| text(&v["AuthURL"])).is_some() {
        steps.push("$ systemctl restart tailscaled (the unused login link stops working)".into());
        exec::run("systemctl", ["restart", "tailscaled"]).await?;
    }
    Ok(Json(Steps { steps }))
}

// ─── helpers ─────────────────────────────────────────────────────────────

/// tailscaled answering is what counts: a hand install may run it under
/// another unit, or none. Only a silent one gets its unit started.
async fn ensure_daemon(steps: &mut Vec<String>) -> AgentResult<()> {
    if read_status().await.is_some() {
        return Ok(());
    }
    let unit = daemon_unit().await;
    steps.push(format!("$ systemctl enable --now {unit}"));
    exec::run("systemctl", ["enable", "--now", unit]).await?;
    for _ in 0..20 {
        if read_status().await.is_some() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Err(AgentError::Conflict(
        "tailscaled started but does not answer; see its log under Logs".into(),
    ))
}

async fn ensure_forwarding(steps: &mut Vec<String>) -> AgentResult<()> {
    if fs::read_to_string(FORWARDING_CONF).await.ok().as_deref() != Some(FORWARDING) {
        fs_atomic::write(Path::new(FORWARDING_CONF), FORWARDING.as_bytes(), 0o644).await?;
        steps.push(format!("wrote {FORWARDING_CONF} (IP forwarding, which an exit node needs)"));
    }
    // Also when the file was there: the running value may have been reset.
    exec::run("sysctl", ["-p", FORWARDING_CONF]).await?;
    steps.push(format!("$ sysctl -p {FORWARDING_CONF}"));
    Ok(())
}

/// Removes the forwarding file once nothing is advertised any more (routes
/// added by hand keep it). The running values stay as they are: Docker and
/// others turn forwarding on too, and switching it off under them would cut
/// their traffic. Without the file it is off after the next reboot, unless
/// something else sets it.
async fn release_forwarding(steps: &mut Vec<String>) {
    if !fs::try_exists(FORWARDING_CONF).await.unwrap_or(false) {
        return;
    }
    let prefs = match tailscale(&["debug", "prefs"], READ_TIMEOUT).await {
        Ok(o) => serde_json::from_str::<Value>(&o.stdout).ok().map(|v| parse_prefs(&v)),
        Err(_) => None,
    };
    let Some(prefs) = prefs else { return };
    if prefs.exit_node || !prefs.advertised_routes.is_empty() {
        return;
    }
    match fs_atomic::remove_if_exists(Path::new(FORWARDING_CONF)).await {
        Ok(()) => steps.push(format!(
            "removed {FORWARDING_CONF}; IP forwarding stays on until the next reboot"
        )),
        Err(e) => steps.push(format!("could not remove {FORWARDING_CONF}: {e}")),
    }
}

/// Runs `tailscale <args>` for an endpoint. Its output goes into `steps`; a
/// non-zero exit becomes the error the panel shows, in tailscale's words.
async fn run_cli(args: &[String], steps: &mut Vec<String>) -> AgentResult<()> {
    steps.push(format!("$ tailscale {}", args.join(" ")));
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = tailscale(&refs, CLI_TIMEOUT).await.map_err(|e| match e.kind() {
        ErrorKind::NotFound => AgentError::Conflict("Tailscale is not installed".into()),
        _ => AgentError::Io(e),
    })?;
    let lines: Vec<String> = format!("{}\n{}", out.stdout, out.stderr)
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .map(String::from)
        .collect();
    if out.code != 0 {
        let msg = if lines.is_empty() {
            format!("tailscale {} exited with code {}", args[0], out.code)
        } else {
            lines.join("\n")
        };
        return Err(AgentError::Conflict(msg));
    }
    steps.extend(lines);
    Ok(())
}

struct CliOut {
    code: i32,
    stdout: String,
    stderr: String,
}

/// `tailscale <args>` with a time limit (the child is killed past it). An
/// Err of kind NotFound means Tailscale is not installed.
async fn tailscale(args: &[&str], limit: Duration) -> std::io::Result<CliOut> {
    let bin = cli_path().ok_or_else(|| std::io::Error::new(ErrorKind::NotFound, "Tailscale is not installed"))?;
    let child = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let out = tokio::time::timeout(limit, child.wait_with_output())
        .await
        .map_err(|_| {
            std::io::Error::new(
                ErrorKind::TimedOut,
                format!("`tailscale {}` did not answer within {} s", args.join(" "), limit.as_secs()),
            )
        })??;
    Ok(CliOut {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

async fn read_status() -> Option<Value> {
    let o = tailscale(&["status", "--json"], READ_TIMEOUT).await.ok()?;
    serde_json::from_str(&o.stdout).ok()
}

/// Installed, whichever way: the package (`tailscale` in /usr/bin and the
/// `tailscaled` unit), the official install script (the same package), the
/// static binaries copied in by hand, or the snap. The quick-install card
/// uses this, so a server set up over SSH shows Tailscale as installed.
pub(crate) async fn installed() -> bool {
    cli_path().is_some() || unit_exists("tailscaled").await
}

/// Where the `tailscale` command is: the agent's PATH, then the places a
/// hand install puts it (/usr/local/bin for the static binaries, /snap/bin,
/// which a systemd service's PATH does not include).
fn cli_path() -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    dirs.extend(["/usr/local/sbin", "/usr/local/bin", "/usr/sbin", "/usr/bin", "/snap/bin"].map(PathBuf::from));
    find_executable(&dirs, "tailscale")
}

fn find_executable(dirs: &[PathBuf], name: &str) -> Option<PathBuf> {
    dirs.iter().map(|d| d.join(name)).find(|p| is_executable(p))
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

/// tailscaled's unit: the package's, or the snap's.
async fn daemon_unit() -> &'static str {
    for unit in ["tailscaled", "snap.tailscale.tailscaled"] {
        if unit_exists(unit).await {
            return unit;
        }
    }
    "tailscaled"
}

/// `systemctl status` exits 4 for a unit it does not know, 0-3 otherwise.
async fn unit_exists(unit: &str) -> bool {
    let run = Command::new("systemctl")
        .args(["status", unit])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .status();
    matches!(
        tokio::time::timeout(Duration::from_secs(5), run).await,
        Ok(Ok(s)) if (0..=3).contains(&s.code().unwrap_or(-1))
    )
}

async fn is_active(unit: &str) -> String {
    let run = Command::new("systemctl")
        .args(["is-active", unit])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(Duration::from_secs(5), run).await {
        Ok(Ok(o)) => {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if s.is_empty() {
                "unknown".into()
            } else {
                s
            }
        }
        _ => "unknown".into(),
    }
}

fn first_line(stderr: &str, stdout: &str) -> String {
    stderr
        .lines()
        .chain(stdout.lines())
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("tailscale status gave no answer")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn apt_release_reads_ubuntu_debian_and_derivatives() {
        let ubuntu = "PRETTY_NAME=\"Ubuntu 24.04.4 LTS\"\nID=ubuntu\nID_LIKE=debian\nVERSION_CODENAME=noble\nUBUNTU_CODENAME=noble\n";
        assert_eq!(apt_release(ubuntu), Some(("ubuntu", "noble".to_string())));
        let debian = "ID=debian\nVERSION_CODENAME=bookworm\n";
        assert_eq!(apt_release(debian), Some(("debian", "bookworm".to_string())));
        let mint = "ID=linuxmint\nID_LIKE=\"ubuntu debian\"\nVERSION_CODENAME=wilma\nUBUNTU_CODENAME=noble\n";
        assert_eq!(apt_release(mint), Some(("ubuntu", "noble".to_string())));
        assert_eq!(apt_release("ID=fedora\nVERSION_CODENAME=\n"), None);
        assert_eq!(apt_release("ID=ubuntu\n"), None);
        // Nothing that could break out of the URL or the apt line.
        assert_eq!(apt_release("ID=ubuntu\nVERSION_CODENAME=noble main\n"), None);
    }

    #[test]
    fn sources_list_matches_tailscales_file() {
        assert_eq!(
            sources_list("ubuntu", "noble"),
            "# Tailscale packages for ubuntu noble\n\
             deb [signed-by=/usr/share/keyrings/tailscale-archive-keyring.gpg] https://pkgs.tailscale.com/stable/ubuntu noble main\n"
        );
    }

    #[test]
    fn pgp_key_check() {
        let mut old = vec![0x99u8, 0x01, 0x0d];
        old.resize(200, 0);
        assert!(looks_like_pgp_key(&old));
        let mut new = vec![0xC6u8, 0x33];
        new.resize(200, 0);
        assert!(looks_like_pgp_key(&new));
        assert!(!looks_like_pgp_key(b"<html><body>404 Not Found</body></html> padding padding padding padding"));
        let mut armored = b"-----BEGIN PGP PUBLIC KEY BLOCK-----".to_vec();
        armored.resize(200, b'a');
        assert!(!looks_like_pgp_key(&armored));
        assert!(!looks_like_pgp_key(&[0x99, 0x01]));
    }

    #[test]
    fn link_is_the_line_after_to_authenticate() {
        let mut w = LinkWatcher::default();
        let out: Vec<Option<String>> = [
            "",
            "To authenticate, visit:",
            "",
            "\thttps://login.tailscale.com/a/1a2b3c4d5e",
            "",
            "To approve your machine, visit (as admin):",
            "",
            "\thttps://login.tailscale.com/admin/machines",
            "Success.",
        ]
        .iter()
        .map(|l| w.feed(l))
        .collect();
        let found: Vec<String> = out.into_iter().flatten().collect();
        assert_eq!(found, vec!["https://login.tailscale.com/a/1a2b3c4d5e".to_string()]);
    }

    #[test]
    fn hostname_and_key_checks() {
        assert_eq!(check_hostname("  "), Ok(String::new()));
        assert_eq!(check_hostname("vps-1"), Ok("vps-1".to_string()));
        assert!(check_hostname("vps.example.com").is_err());
        assert!(check_hostname("-vps").is_err());
        assert!(check_hostname("my server").is_err());
        assert!(check_hostname(&"a".repeat(64)).is_err());
        assert_eq!(check_auth_key(""), Ok(None));
        assert_eq!(
            check_auth_key(" tskey-auth-kAbc123CNTRL-0123456789abcdef \n"),
            Ok(Some("tskey-auth-kAbc123CNTRL-0123456789abcdef".to_string()))
        );
        assert!(check_auth_key("tskey-auth-k1 tskey-auth-k2-xxxxxxxxxx").is_err());
        assert!(check_auth_key("short").is_err());
    }

    #[test]
    fn up_and_set_arguments() {
        let o = Options {
            hostname: String::new(),
            ssh: true,
            accept_routes: false,
            accept_dns: false,
            exit_node: true,
        };
        assert_eq!(
            up_args(&o, Some(Path::new("/run/vps-panel/tailscale-authkey-x")), false),
            vec![
                "up",
                "--reset",
                "--timeout=900s",
                "--ssh=true",
                "--accept-routes=false",
                "--accept-dns=false",
                "--advertise-exit-node=true",
                "--auth-key=file:/run/vps-panel/tailscale-authkey-x",
            ]
        );
        let named = Options { hostname: "vps-1".into(), ..o.clone() };
        let a = up_args(&named, None, true);
        assert!(a.contains(&"--hostname=vps-1".to_string()));
        assert!(a.contains(&"--force-reauth".to_string()));
        assert!(!a.iter().any(|x| x.starts_with("--auth-key")));
        // `set` always names the hostname, so clearing it takes effect.
        assert_eq!(option_flags(&o, true)[0], "--hostname=");
    }

    #[test]
    fn status_json_becomes_the_dialog_shape() {
        let v = json!({
            "Version": "1.90.6-t1234567-gabcdef",
            "BackendState": "Running",
            "AuthURL": "",
            "Self": {
                "HostName": "vps-1", "DNSName": "vps-1.tail1234.ts.net.", "OS": "linux",
                "UserID": 12345, "TailscaleIPs": ["100.101.102.103", "fd7a:115c:a1e0::1"],
                "Online": true, "LastSeen": "0001-01-01T00:00:00Z"
            },
            "Health": [],
            "CurrentTailnet": {"Name": "adrian@example.com", "MagicDNSSuffix": "tail1234.ts.net", "MagicDNSEnabled": true},
            "User": {"12345": {"ID": 12345, "LoginName": "adrian@example.com", "DisplayName": "Adrian"}},
            "Peer": {
                "nodekey:bb": {"HostName": "phone", "DNSName": "phone.tail1234.ts.net.", "OS": "iOS",
                    "TailscaleIPs": ["100.64.0.3"], "Online": false, "LastSeen": "2026-09-20T10:00:00Z",
                    "ExitNode": false, "ExitNodeOption": false},
                "nodekey:aa": {"HostName": "Laptop", "DNSName": "laptop.tail1234.ts.net.", "OS": "windows",
                    "TailscaleIPs": ["100.64.0.2"], "Online": true, "LastSeen": "2026-09-29T10:00:00Z",
                    "ExitNode": false, "ExitNodeOption": true}
            }
        });
        let mut s = TailscaleStatus::default();
        apply_status(&v, &mut s);
        assert_eq!(s.backend_state.as_deref(), Some("Running"));
        assert_eq!(s.version.as_deref(), Some("1.90.6"));
        assert_eq!(s.auth_url, None);
        assert_eq!(s.user.as_deref(), Some("adrian@example.com"));
        assert_eq!(s.tailnet.as_deref(), Some("adrian@example.com"));
        let me = s.node.unwrap();
        assert_eq!(me.dns_name, "vps-1.tail1234.ts.net");
        assert_eq!(me.ips[0], "100.101.102.103");
        assert_eq!(me.last_seen, None);
        let names: Vec<&str> = s.peers.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["Laptop", "phone"]);
        assert_eq!(s.peers[0].last_seen, None);
        assert_eq!(s.peers[1].last_seen.as_deref(), Some("2026-09-20T10:00:00Z"));
        assert!(s.peers[0].exit_node_option);
    }

    #[test]
    fn logged_out_status_has_a_link_and_no_node() {
        let v = json!({
            "Version": "1.90.6-t1234567-gabcdef",
            "BackendState": "NeedsLogin",
            "AuthURL": "https://login.tailscale.com/a/abc123",
            "Self": {"HostName": "vps-1", "DNSName": "", "OS": "linux", "UserID": 0,
                     "TailscaleIPs": null, "Online": false, "LastSeen": "0001-01-01T00:00:00Z"},
            "Health": ["You are logged out."],
            "CurrentTailnet": null, "Peer": null, "User": null
        });
        let mut s = TailscaleStatus::default();
        apply_status(&v, &mut s);
        assert_eq!(s.auth_url.as_deref(), Some("https://login.tailscale.com/a/abc123"));
        assert!(s.node.is_none());
        assert!(s.user.is_none());
        assert!(s.peers.is_empty());
        assert_eq!(s.health, vec!["You are logged out."]);
    }

    #[test]
    fn prefs_json() {
        let v = json!({
            "Hostname": "edge", "RunSSH": true, "RouteAll": false, "CorpDNS": false,
            "AdvertiseRoutes": ["10.0.0.0/24", "0.0.0.0/0", "::/0"], "ShieldsUp": false
        });
        assert_eq!(
            parse_prefs(&v),
            Prefs {
                hostname: "edge".into(),
                ssh: true,
                accept_routes: false,
                accept_dns: false,
                exit_node: true,
                advertised_routes: vec!["10.0.0.0/24".into()],
                shields_up: false,
            }
        );
        let none = parse_prefs(&json!({"AdvertiseRoutes": null, "CorpDNS": true}));
        assert!(!none.exit_node && none.accept_dns && none.advertised_routes.is_empty());
    }

    #[test]
    fn login_is_refused_once_logged_in() {
        for s in ["Running", "Starting", "Stopped", "NeedsMachineAuth"] {
            assert!(refuse_login_in(s).is_err(), "{s}");
        }
        for s in ["NeedsLogin", "NoState"] {
            assert!(refuse_login_in(s).is_ok(), "{s}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn finds_a_hand_installed_cli_by_its_exec_bit() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let (plain, bin) = (tmp.path().join("a"), tmp.path().join("b"));
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        // A file of that name without the exec bit is not the command.
        std::fs::write(plain.join("tailscale"), "not a program").unwrap();
        std::fs::write(bin.join("tailscale"), "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(bin.join("tailscale"), std::fs::Permissions::from_mode(0o755)).unwrap();
        let dirs = vec![PathBuf::from("/nonexistent"), plain.clone(), bin.clone()];
        assert_eq!(find_executable(&dirs, "tailscale"), Some(bin.join("tailscale")));
        assert_eq!(find_executable(&dirs[..2], "tailscale"), None);
    }

    /// A TaskTracker on a throwaway panel.db.
    fn tracker() -> (tempfile::TempDir, TaskTracker) {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::PanelDb::open(&dir.path().join("panel.db")).unwrap();
        db.with(|c| {
            c.execute_batch(
                "CREATE TABLE tasks (id TEXT PRIMARY KEY, kind TEXT NOT NULL, status TEXT NOT NULL,
                 started_at TEXT NOT NULL, finished_at TEXT, log_blob TEXT NOT NULL DEFAULT '');",
            )
        })
        .unwrap();
        (dir, TaskTracker::new(db))
    }

    async fn finished(tasks: &TaskTracker, id: &str) -> crate::ipc::tasks::TaskState {
        use crate::ipc::tasks::TaskStatus;
        for _ in 0..300 {
            let t = tasks.get(id).unwrap();
            if matches!(t.status, TaskStatus::Succeeded | TaskStatus::Failed) {
                return t;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("task {id} did not finish");
    }

    #[tokio::test]
    #[ignore = "root only: installs Tailscale on this machine (apt) and starts a login without finishing it; purge it afterwards"]
    async fn installs_and_hands_out_a_login_link() {
        use crate::ipc::tasks::TaskStatus;
        let (_dir, tasks) = tracker();

        let task = tasks.create("quick-install:tailscale");
        let id = task.id.clone();
        install(task).await;
        let done = tasks.get(&id).unwrap();
        println!("--- install log\n{}", done.log.join("\n"));
        assert!(matches!(done.status, TaskStatus::Succeeded));
        let os = std::fs::read_to_string("/etc/os-release").unwrap();
        let (distro, codename) = apt_release(&os).unwrap();
        assert_eq!(std::fs::read_to_string(SOURCES_LIST).unwrap(), sources_list(distro, &codename));
        assert!(looks_like_pgp_key(&std::fs::read(KEYRING).unwrap()));

        let st = read_all().await;
        println!(
            "--- after install: state {:?}, version {:?}, daemon {}, error {:?}, prefs {:?}",
            st.backend_state, st.version, st.daemon, st.error, st.prefs
        );
        assert!(st.installed);
        assert!(installed().await, "the quick-install card sees it");
        assert_eq!(st.daemon, "active");
        assert!(st.node.is_none());
        assert!(settings(Json(Options::default())).await.is_err());

        let req = || LoginReq {
            options: Options { hostname: "vps-panel-test".into(), ..Default::default() },
            auth_key: String::new(),
        };
        let first = start_login(&tasks, req()).await.unwrap().task_id;
        let mut url = None;
        for _ in 0..60 {
            let st = read_all().await;
            if st.auth_url.is_some() {
                assert_eq!(st.login_task.as_deref(), Some(first.as_str()));
                url = st.auth_url;
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        let url = url.expect("a login link within 30 s");
        println!("--- login link: {url}");
        assert!(url.starts_with("https://login.tailscale.com/"), "{url}");
        let log = tasks.get(&first).unwrap().log;
        println!("--- first login log\n{}", log.join("\n"));
        assert!(log.iter().any(|l| l.trim() == url), "the CLI printed the link");
        let captured = LOGIN.lock().unwrap().as_ref().and_then(|l| l.auth_url.clone());
        assert_eq!(captured.as_deref(), Some(url.as_str()));
        let prefs = read_all().await.prefs.unwrap();
        assert_eq!(prefs.hostname, "vps-panel-test");
        assert!(!prefs.accept_dns && !prefs.ssh && !prefs.exit_node);

        // Log in again: the first login stops, the new one gets a link too.
        let second = start_login(&tasks, req()).await.unwrap().task_id;
        let old = finished(&tasks, &first).await;
        assert!(matches!(old.status, TaskStatus::Failed));
        assert!(old.log.iter().any(|l| l.contains("another login")), "{:?}", old.log);
        let mut again = None;
        for _ in 0..60 {
            let st = read_all().await;
            if st.login_task.as_deref() == Some(second.as_str()) && st.auth_url.is_some() {
                again = st.auth_url;
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        println!("--- second link: {again:?} (same as the first: {})", again.as_deref() == Some(url.as_str()));
        assert!(again.is_some());

        // Log out stops the waiting login.
        match logout().await {
            Ok(Json(s)) => println!("--- logout: {:?}", s.steps),
            Err(e) => println!("--- logout error: {e}"),
        }
        let stopped = finished(&tasks, &second).await;
        assert!(matches!(stopped.status, TaskStatus::Failed));
        assert!(LOGIN.lock().unwrap().is_none());
        let st = read_all().await;
        println!("--- after logout: state {:?}, link {:?}", st.backend_state, st.auth_url);
        assert!(st.auth_url.is_none());
        // tailscaled itself no longer holds the link either.
        let raw = read_status().await.unwrap();
        assert_eq!(text(&raw["AuthURL"]), None, "{raw}");
        assert!(!st.server_hostname.is_empty());

        match connect().await {
            Ok(Json(s)) => println!("--- connect while logged out: {:?}", s.steps),
            Err(e) => println!("--- connect error: {e}"),
        }

        // The exit node's forwarding file comes and goes.
        let mut steps = Vec::new();
        ensure_forwarding(&mut steps).await.unwrap();
        assert_eq!(std::fs::read_to_string(FORWARDING_CONF).unwrap(), FORWARDING);
        release_forwarding(&mut steps).await;
        println!("--- forwarding: {steps:?}");
        assert!(!Path::new(FORWARDING_CONF).exists());
    }
}
