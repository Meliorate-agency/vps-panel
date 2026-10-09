use crate::error::{AgentError, AgentResult};
use crate::exec;
use crate::ipc::TaskAccepted;
use crate::state::AppState;
use axum::{
    extract::{Path, Query, State},
    Json,
};
use futures::future::join_all;
use serde::{Deserialize, Serialize};
use std::process::Stdio;
use tokio::process::Command;

type Step = &'static [&'static str];
type Recipe = &'static [Step];

#[derive(Clone, Copy)]
enum Check {
    SystemdUnit(&'static str),
    DockerContainer(&'static str),
    /// A PostgreSQL extension, considered installed when its `<name>.control`
    /// file is present under any installed major version's extension dir. Used
    /// for pgvector, whose apt package is version-pinned and not a service.
    PgExtension(&'static str),
    /// Tailscale, however it got there: the `tailscale` command on any of
    /// the usual paths or the `tailscaled` unit (tailscale::installed), so
    /// one installed by hand over SSH is not offered for install again.
    Tailscale,
}

const MARIADB: Recipe = &[
    &["apt-get", "update"],
    &["apt-get", "install", "-y", "mariadb-server", "mariadb-client"],
    &["systemctl", "enable", "mariadb"],
    &["systemctl", "start", "mariadb"],
];

const POSTGRESQL: Recipe = &[
    &["apt-get", "update"],
    &["apt-get", "install", "-y", "postgresql", "postgresql-contrib"],
    &["systemctl", "enable", "postgresql"],
    &["systemctl", "start", "postgresql"],
];

const REDIS: Recipe = &[
    &["apt-get", "update"],
    &["apt-get", "install", "-y", "redis-server"],
    &["systemctl", "enable", "redis-server"],
    &["systemctl", "start", "redis-server"],
];

const QDRANT: Recipe = &[
    &["apt-get", "update"],
    &["apt-get", "install", "-y", "docker.io"],
    &["systemctl", "enable", "docker"],
    &["systemctl", "start", "docker"],
    &["docker", "volume", "create", "qdrant_storage"],
    &[
        "docker",
        "run",
        "-d",
        "--name",
        "qdrant",
        "--restart",
        "unless-stopped",
        "-p",
        "127.0.0.1:6333:6333",
        "-p",
        "127.0.0.1:6334:6334",
        "-v",
        "qdrant_storage:/qdrant/storage",
        "qdrant/qdrant",
    ],
];

const DOCKER: Recipe = &[
    &["apt-get", "update"],
    &["apt-get", "install", "-y", "docker.io"],
    &["systemctl", "enable", "docker"],
    &["systemctl", "start", "docker"],
];

const NGINX: Recipe = &[
    &["apt-get", "update"],
    &["apt-get", "install", "-y", "nginx"],
    &["systemctl", "enable", "nginx"],
    &["systemctl", "start", "nginx"],
];

const PGADMIN: Recipe = &[
    &["apt-get", "update"],
    &["apt-get", "install", "-y", "docker.io"],
    &["systemctl", "enable", "docker"],
    &["systemctl", "start", "docker"],
    &["docker", "volume", "create", "pgadmin_data"],
    &[
        "docker",
        "run",
        "-d",
        "--name",
        "pgadmin",
        "--restart",
        "unless-stopped",
        "-p",
        "127.0.0.1:5050:80",
        "-e",
        "PGADMIN_DEFAULT_EMAIL=admin@example.com",
        "-e",
        "PGADMIN_DEFAULT_PASSWORD=changeme",
        "--add-host=host.docker.internal:host-gateway",
        "-v",
        "pgadmin_data:/var/lib/pgadmin",
        "dpage/pgadmin4",
    ],
];

const PHPMYADMIN: Recipe = &[
    &["apt-get", "update"],
    &["apt-get", "install", "-y", "docker.io"],
    &["systemctl", "enable", "docker"],
    &["systemctl", "start", "docker"],
    &[
        "docker",
        "run",
        "-d",
        "--name",
        "phpmyadmin",
        "--restart",
        "unless-stopped",
        "-p",
        "127.0.0.1:8081:80",
        "-e",
        "PMA_HOST=host.docker.internal",
        "--add-host=host.docker.internal:host-gateway",
        "phpmyadmin/phpmyadmin",
    ],
];

// Sentinel for the dispatcher; the actual install runs `install_roundcube_dynamic`
// because the docker run step depends on the mail server's FQDN, which we read
// from postfix at install time. Static Recipe entries can't do that.
const ROUNDCUBE: Recipe = &[];

// Sentinel for the dispatcher; the actual install runs `install_pgvector_dynamic`
// because the apt package is version-pinned (postgresql-<major>-pgvector) and the
// running major version is only known at install time. Static Recipe entries
// can't do that.
const PGVECTOR: Recipe = &[];

// Sentinel for the dispatcher; the actual install runs tailscale::install,
// which adds Tailscale's apt repository for this distro + release (read from
// /etc/os-release) before installing the package.
const TAILSCALE: Recipe = &[];

const N8N: Recipe = &[
    &["apt-get", "update"],
    &["apt-get", "install", "-y", "docker.io"],
    &["systemctl", "enable", "docker"],
    &["systemctl", "start", "docker"],
    &["docker", "volume", "create", "n8n_data"],
    &[
        "docker",
        "run",
        "-d",
        "--name",
        "n8n",
        "--restart",
        "unless-stopped",
        "-p",
        "127.0.0.1:5678:5678",
        "-v",
        "n8n_data:/home/node/.n8n",
        "n8nio/n8n",
    ],
];

#[derive(Serialize, Clone, Copy)]
pub struct RecipeInfo {
    pub slug: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub default_port: u16,
}

#[derive(Serialize)]
pub struct RecipeListItem {
    pub slug: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub default_port: u16,
    pub installed: bool,
    pub web_ui: bool,
    pub bind: Option<String>,
    /// For installed docker recipes whose image we track (currently n8n):
    /// `Some(true)` when the registry serves a newer image than the running
    /// container, `Some(false)` when up to date, `None` when not applicable
    /// or we couldn't reach the registry. Drives the "update available" badge
    /// on the quick-install card.
    pub update_available: Option<bool>,
}

const RECIPES: &[(RecipeInfo, Recipe, Check)] = &[
    (
        RecipeInfo {
            slug: "mariadb",
            name: "MariaDB",
            description: "Drop-in MySQL replacement (community fork). Listens on 3306.",
            default_port: 3306,
        },
        MARIADB,
        Check::SystemdUnit("mariadb"),
    ),
    (
        RecipeInfo {
            slug: "postgresql",
            name: "PostgreSQL",
            description: "Reliable relational database with rich SQL support. Listens on 5432.",
            default_port: 5432,
        },
        POSTGRESQL,
        Check::SystemdUnit("postgresql"),
    ),
    (
        RecipeInfo {
            slug: "pgvector",
            name: "pgvector",
            description: "PostgreSQL extension for vector similarity search / embeddings. Adds the `vector` type. Requires PostgreSQL — after installing, enable it per-database from the Postgres section.",
            default_port: 0,
        },
        PGVECTOR,
        Check::PgExtension("vector"),
    ),
    (
        RecipeInfo {
            slug: "redis",
            name: "Redis",
            description: "In-memory key-value store. Listens on 6379.",
            default_port: 6379,
        },
        REDIS,
        Check::SystemdUnit("redis-server"),
    ),
    (
        RecipeInfo {
            slug: "qdrant",
            name: "Qdrant",
            description: "Vector database (runs via Docker). REST 6333, gRPC 6334.",
            default_port: 6333,
        },
        QDRANT,
        Check::DockerContainer("qdrant"),
    ),
    (
        RecipeInfo {
            slug: "docker",
            name: "Docker",
            description: "Container runtime. Required for containerized apps and services.",
            default_port: 0,
        },
        DOCKER,
        Check::SystemdUnit("docker"),
    ),
    (
        RecipeInfo {
            slug: "nginx",
            name: "Nginx",
            description: "HTTP server and reverse proxy. Listens on 80/443.",
            default_port: 80,
        },
        NGINX,
        Check::SystemdUnit("nginx"),
    ),
    (
        RecipeInfo {
            slug: "pgadmin",
            name: "pgAdmin",
            description: "Web UI for PostgreSQL. Runs via Docker on port 5050. Default login: admin@example.com / changeme.",
            default_port: 5050,
        },
        PGADMIN,
        Check::DockerContainer("pgadmin"),
    ),
    (
        RecipeInfo {
            slug: "phpmyadmin",
            name: "phpMyAdmin",
            description: "Web UI for MariaDB / MySQL. Runs via Docker on port 8081.",
            default_port: 8081,
        },
        PHPMYADMIN,
        Check::DockerContainer("phpmyadmin"),
    ),
    (
        RecipeInfo {
            slug: "roundcube",
            name: "Roundcube",
            description: "Webmail for the panel's IMAP/SMTP server. Runs via Docker on port 8082. At login enter your mail server (e.g. mail.<domain>), email, and password.",
            default_port: 8082,
        },
        ROUNDCUBE,
        Check::DockerContainer("roundcube"),
    ),
    (
        RecipeInfo {
            slug: "n8n",
            name: "n8n",
            description: "Workflow automation with a visual editor and 400+ integrations. Runs via Docker on port 5678.",
            default_port: 5678,
        },
        N8N,
        Check::DockerContainer("n8n"),
    ),
    (
        RecipeInfo {
            slug: "tailscale",
            name: "Tailscale",
            description: "Private network between this server and your other devices (WireGuard). After installing, log in from this card.",
            default_port: 0,
        },
        TAILSCALE,
        Check::Tailscale,
    ),
];

struct DockerService {
    container_name: &'static str,
    host_port: u16,
    container_port: u16,
    image: &'static str,
    extra_args: &'static [&'static str],
}

const PGADMIN_SERVICE: DockerService = DockerService {
    container_name: "pgadmin",
    host_port: 5050,
    container_port: 80,
    image: "dpage/pgadmin4",
    extra_args: &[
        "-e",
        "PGADMIN_DEFAULT_EMAIL=admin@example.com",
        "-e",
        "PGADMIN_DEFAULT_PASSWORD=changeme",
        "--add-host=host.docker.internal:host-gateway",
        "-v",
        "pgadmin_data:/var/lib/pgadmin",
    ],
};

const PHPMYADMIN_SERVICE: DockerService = DockerService {
    container_name: "phpmyadmin",
    host_port: 8081,
    container_port: 80,
    image: "phpmyadmin/phpmyadmin",
    extra_args: &[
        "-e",
        "PMA_HOST=host.docker.internal",
        "--add-host=host.docker.internal:host-gateway",
    ],
};

const N8N_SERVICE: DockerService = DockerService {
    container_name: "n8n",
    host_port: 5678,
    container_port: 5678,
    image: "n8nio/n8n",
    extra_args: &["-v", "n8n_data:/home/node/.n8n"],
};

// Roundcube is intentionally NOT in web_ui_service: its container env vars
// depend on the host's mail FQDN, so rebind would need to re-detect that. The
// expected access path is through a Sites mapping (https://webmail.<domain>),
// not direct port exposure — no bind toggle needed.

fn web_ui_service(slug: &str) -> Option<&'static DockerService> {
    match slug {
        "pgadmin" => Some(&PGADMIN_SERVICE),
        "phpmyadmin" => Some(&PHPMYADMIN_SERVICE),
        "n8n" => Some(&N8N_SERVICE),
        _ => None,
    }
}

fn lookup(slug: &str) -> Option<&'static (RecipeInfo, Recipe, Check)> {
    RECIPES.iter().find(|(info, _, _)| info.slug == slug)
}

async fn is_installed(check: Check) -> bool {
    match check {
        Check::SystemdUnit(unit) => matches!(
            Command::new("systemctl")
                .args(["status", unit])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await,
            Ok(s) if (0..=3).contains(&s.code().unwrap_or(-1))
        ),
        Check::DockerContainer(name) => matches!(
            Command::new("docker")
                .args(["inspect", name])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await,
            Ok(s) if s.success()
        ),
        Check::PgExtension(name) => pg_extension_available(name).await,
        Check::Tailscale => super::tailscale::installed().await,
    }
}

/// True when `<name>.control` exists under any installed PostgreSQL major
/// version's extension directory (`/usr/share/postgresql/<ver>/extension/`).
/// That control file is exactly what `CREATE EXTENSION <name>` needs, so its
/// presence is the reliable "is the package installed" signal — independent of
/// the version-pinned apt package name (postgresql-<ver>-pgvector). Shared with
/// the Postgres handler's pgvector status endpoint.
pub(crate) async fn pg_extension_available(name: &str) -> bool {
    let mut rd = match tokio::fs::read_dir("/usr/share/postgresql").await {
        Ok(rd) => rd,
        Err(_) => return false,
    };
    let control = format!("{name}.control");
    while let Ok(Some(entry)) = rd.next_entry().await {
        if entry.path().join("extension").join(&control).exists() {
            return true;
        }
    }
    false
}

// MariaDB drop-in that lets the Dockerised phpMyAdmin (and any other bridged
// container) reach the host's MariaDB. 172.17.0.1 is the default docker0
// bridge gateway IP, reachable only from local containers — never from the
// public network. UFW must still keep 3306/tcp closed externally.
const MARIADB_DOCKER_DROPIN_PATH: &str = "/etc/mysql/mariadb.conf.d/99-vps-panel-docker.cnf";

const MARIADB_DOCKER_DROPIN: &str = "# Managed by vps-panel — do not edit by hand.\n# Extends MariaDB bind so the Dockerised phpMyAdmin (PMA_HOST=host.docker.internal)\n# can reach MariaDB over the docker0 bridge. 172.17.0.1 is the bridge gateway and\n# is NOT reachable from outside the host. Keep UFW closed on 3306/tcp.\n\n[mysqld]\nbind-address = 127.0.0.1,172.17.0.1\n";

/// Ensure MariaDB listens on both 127.0.0.1 and the docker bridge gateway so a
/// Dockerised phpMyAdmin can reach it. Idempotent — writes the drop-in only if
/// it would change on disk, and restarts mariadb only when the file changed.
async fn ensure_mariadb_docker_bridge_bind(task: &crate::ipc::tasks::TaskHandle) {
    // 1. Drop-in: makes MariaDB listen on the docker bridge gateway.
    //    Only restart mariadb if the file content actually changed —
    //    cheap idempotency, no spurious restarts on every install.
    let path = std::path::Path::new(MARIADB_DOCKER_DROPIN_PATH);
    let current = tokio::fs::read_to_string(path).await.ok();
    if current.as_deref() == Some(MARIADB_DOCKER_DROPIN) {
        task.log(format!(
            "{MARIADB_DOCKER_DROPIN_PATH} already up to date — MariaDB bridge bind unchanged"
        ));
    } else {
        task.log(format!(
            "writing {MARIADB_DOCKER_DROPIN_PATH} so MariaDB accepts connections from the docker bridge"
        ));
        if let Err(e) = crate::fs_atomic::write(path, MARIADB_DOCKER_DROPIN.as_bytes(), 0o644).await
        {
            task.log(format!(
                "failed to write {MARIADB_DOCKER_DROPIN_PATH}: {e} — phpMyAdmin will not be able to reach MariaDB"
            ));
            // Fall through anyway: the UFW step below is independent and
            // might still close a gap from a prior partial run.
        } else {
            task.log("$ systemctl restart mariadb".to_string());
            let res = exec::run_streaming("systemctl", ["restart", "mariadb"], |l| {
                task.log(l.to_string())
            })
            .await;
            match res {
                Ok(0) => task.log("mariadb restarted; bridge bind active".to_string()),
                Ok(code) => {
                    task.log(format!("systemctl restart mariadb exited with code {code}"))
                }
                Err(e) => task.log(format!("systemctl restart mariadb error: {e}")),
            }
        }
    }

    // 2. UFW rule: even with MariaDB bound to 172.17.0.1, the host's
    //    INPUT chain (default DROP under UFW) silently drops the SYN
    //    packet from container → host. `ufw status` may report
    //    "inactive" while the kernel rules are still loaded — meaning
    //    the only symptom is "Connection timed out" from phpMyAdmin
    //    with no log entry anywhere. Adding the explicit allow rule
    //    closes the gap for both the active-UFW and weird-inactive
    //    cases. `ufw allow ...` is idempotent: re-running it prints
    //    "Skipping adding existing rule" and exits 0. Scoped to `in on
    //    docker0` so it only matches traffic from the actual bridge
    //    interface, not arbitrary external traffic that happens to be
    //    NAT'd onto 172.17.0.0/16.
    task.log("$ ufw allow in on docker0 to any port 3306 proto tcp".to_string());
    let ufw_res = exec::run_streaming(
        "ufw",
        [
            "allow", "in", "on", "docker0", "to", "any", "port", "3306", "proto", "tcp",
        ],
        |l| task.log(l.to_string()),
    )
    .await;
    match ufw_res {
        Ok(0) => task.log("UFW: docker0 → 3306 rule in place".to_string()),
        Ok(code) => task.log(format!(
            "ufw allow exited with code {code} — phpMyAdmin may still time out connecting to MariaDB"
        )),
        Err(e) => task.log(format!(
            "ufw allow error: {e} — phpMyAdmin may still time out connecting to MariaDB"
        )),
    }
}

/// Read a single env var from a running container's `Config.Env`. Returns
/// `None` if the container isn't found, the var isn't set, or docker fails.
/// Used by set_bind to preserve env that was added by *_settings updates
/// (e.g. PMA_ABSOLUTE_URI on phpmyadmin) — those values aren't in the
/// static `extra_args` of the DockerService, so a naive rebind would
/// drop them on the floor.
async fn current_docker_env(name: &str, var: &str) -> Option<String> {
    let out = Command::new("docker")
        .args([
            "inspect",
            name,
            "--format",
            "{{range .Config.Env}}{{println .}}{{end}}",
        ])
        .stdin(Stdio::null())
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let prefix = format!("{var}=");
    for line in stdout.lines() {
        if let Some(v) = line.strip_prefix(&prefix) {
            if v.is_empty() {
                return None;
            }
            return Some(v.to_string());
        }
    }
    None
}

async fn current_docker_bind(name: &str, host_port: u16) -> Option<String> {
    let format = format!(
        "{{{{ (index (index .HostConfig.PortBindings \"{}/tcp\") 0).HostIp }}}}",
        host_port
    );
    let out = Command::new("docker")
        .args(["inspect", "--format", &format, name])
        .stdin(Stdio::null())
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() || s == "<no value>" {
        Some("0.0.0.0".to_string())
    } else {
        Some(s)
    }
}

/// The image config digest (`sha256:...`) the *running* container was created
/// from. `docker inspect <c> --format '{{.Image}}'` returns the image ID,
/// which is exactly the config-blob digest the registry manifest references —
/// so it's directly comparable to the registry's current config digest.
/// Comparing against the container's image (not the local tag) means we report
/// "update available" even if a newer image was pulled but the container was
/// never recreated.
async fn current_docker_image_id(name: &str) -> Option<String> {
    let out = Command::new("docker")
        .args(["inspect", "--format", "{{.Image}}", name])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// All per-platform image *config* digests the registry currently serves for
/// `image` (e.g. "n8nio/n8n", implicit `:latest`). Uses
/// `docker manifest inspect --verbose`, which returns one entry per platform
/// for a multi-arch tag (a single object for single-arch). The config digest
/// lives under `SchemaV2Manifest` (Docker schema2) or `OCIManifest` (OCI)
/// depending on the image's media type, so we read both. Network round-trip;
/// callers wrap it in a timeout. Returns an empty Vec on any failure (no
/// network, registry error, old docker) so the caller degrades to "unknown"
/// rather than a false positive.
async fn remote_image_config_digests(image: &str) -> Vec<String> {
    let out = match Command::new("docker")
        .args(["manifest", "inspect", "--verbose", image])
        // `docker manifest` graduated out of experimental in CLI v23, but set
        // the flag anyway so older docker.io builds still answer instead of
        // erroring with "only supported on a Docker cli with experimental".
        .env("DOCKER_CLI_EXPERIMENTAL", "enabled")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await
    {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };

    let parsed: serde_json::Value = match serde_json::from_slice(&out.stdout) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let entries: Vec<&serde_json::Value> = match &parsed {
        serde_json::Value::Array(a) => a.iter().collect(),
        other => vec![other],
    };

    let mut digests = Vec::new();
    for el in entries {
        let cfg = el
            .get("SchemaV2Manifest")
            .or_else(|| el.get("OCIManifest"))
            .and_then(|m| m.get("config"))
            .and_then(|c| c.get("digest"))
            .and_then(|d| d.as_str());
        if let Some(d) = cfg {
            digests.push(d.to_string());
        }
    }
    digests
}

/// Whether the running `container` is behind the registry image `image`.
/// `Some(true)` = a newer image is available, `Some(false)` = up to date,
/// `None` = couldn't determine (offline, registry error, container gone).
/// The local config digest is unique per platform+build, so a simple
/// membership test against the registry's per-platform set is correct without
/// any host-arch matching.
async fn container_update_available(container: &str, image: &str) -> Option<bool> {
    let local = current_docker_image_id(container).await?;
    let remote = remote_image_config_digests(image).await;
    if remote.is_empty() {
        return None;
    }
    Some(!remote.contains(&local))
}

/// Detect whether a systemd-managed DB engine is currently listening on
/// 0.0.0.0 (PUBLIC) or just localhost / the docker bridge (LOCAL). We
/// parse `ss -tnlp` rather than reading config files because operators
/// (or other panel features) might have edited the config without
/// restarting the service — `ss` is authoritative about what's actually
/// accepting connections right now.
async fn current_system_db_bind(host_port: u16) -> Option<String> {
    let out = Command::new("ss")
        .args(["-tnlH"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let needle = format!(":{host_port}");
    let mut saw_public = false;
    let mut saw_listener = false;
    for line in stdout.lines() {
        // `ss -tnlH` lines look like:
        //   LISTEN 0 80 127.0.0.1:3306 0.0.0.0:*
        //   LISTEN 0 80 172.17.0.1:3306 0.0.0.0:*
        //   LISTEN 0 80 0.0.0.0:3306 0.0.0.0:*
        // We only care about the 4th column (the local address).
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 4 {
            continue;
        }
        let local = fields[3];
        if !local.ends_with(&needle) {
            continue;
        }
        saw_listener = true;
        // Local addr is "<ip>:<port>" — split on the LAST ':' so IPv6
        // brackets don't trip us up.
        let ip = local.rsplit_once(':').map(|(ip, _)| ip).unwrap_or(local);
        // Strip IPv6 brackets if any.
        let ip = ip.trim_start_matches('[').trim_end_matches(']');
        if ip == "0.0.0.0" || ip == "*" || ip == "::" {
            saw_public = true;
        }
    }
    if !saw_listener {
        return None;
    }
    Some(if saw_public {
        "0.0.0.0".to_string()
    } else {
        "127.0.0.1".to_string()
    })
}

#[derive(Deserialize)]
pub struct ListParams {
    /// When true, also probe the registry for image updates (currently the
    /// n8n card's `update_available`). Off by default so pages that don't
    /// show n8n (e.g. /databases) don't pay the network round-trip or the
    /// Docker Hub anonymous-pull rate-limit cost on every load.
    #[serde(default)]
    pub check_updates: bool,
}

pub async fn list(Query(params): Query<ListParams>) -> Json<Vec<RecipeListItem>> {
    let checks = RECIPES.iter().map(|(_, _, c)| is_installed(*c));
    let installed_flags = join_all(checks).await;

    let mut items: Vec<RecipeListItem> = Vec::with_capacity(RECIPES.len());
    for ((info, _, _), installed) in RECIPES.iter().zip(installed_flags) {
        let svc = web_ui_service(info.slug);
        let web_ui = svc.is_some();
        // Bind detection is engine-specific:
        // - docker web-UI containers → `docker inspect` (HostConfig.PortBindings)
        // - systemd DB engines (mariadb, postgresql) → `ss -tnlH` on the
        //   well-known port. Lets the UI show the LOCAL/PUBLIC dropdown
        //   on those cards too so the operator can flip external access
        //   without SSHing in.
        let bind = if installed {
            if let Some(s) = svc {
                current_docker_bind(s.container_name, s.host_port).await
            } else if info.slug == "mariadb" {
                current_system_db_bind(3306).await
            } else if info.slug == "postgresql" {
                current_system_db_bind(5432).await
            } else {
                None
            }
        } else {
            None
        };
        items.push(RecipeListItem {
            slug: info.slug,
            name: info.name,
            description: info.description,
            default_port: info.default_port,
            installed,
            web_ui,
            bind,
            update_available: None,
        });
    }

    // For the n8n card, surface whether the registry has a newer n8nio/n8n
    // image than the running container so the UI can show an "update" badge.
    // Only when the caller opted in (`?check_updates=true` — the /apps strip)
    // so /databases doesn't pay for a card it never renders. Bounded by a
    // timeout so a slow/unreachable registry never stalls the page; on timeout
    // or any error the field stays None (no badge — never a false positive).
    if params.check_updates {
        if let Some(item) = items.iter_mut().find(|i| i.slug == "n8n" && i.installed) {
            item.update_available = tokio::time::timeout(
                std::time::Duration::from_secs(6),
                container_update_available("n8n", "n8nio/n8n"),
            )
            .await
            .ok()
            .flatten();
        }
    }

    Json(items)
}

pub async fn run(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> AgentResult<Json<TaskAccepted>> {
    let (info, recipe, _) = lookup(&slug)
        .ok_or_else(|| AgentError::Invalid(format!("unknown package: {slug}")))?;

    let task = state.tasks.create(&format!("quick-install:{}", info.slug));
    let task_id = task.id.clone();
    let display_name = info.name;
    let slug_owned = slug.clone();
    // Clone the panel.db handle into the spawned task so the post-install
    // phpmyadmin hook can look up matching /domains routing slots without
    // touching the request-handler `state`.
    let panel_db = state.panel_db.clone();

    tokio::spawn(async move {
        task.start();
        task.log(format!("starting install: {display_name}"));

        if slug_owned == "roundcube" {
            install_roundcube_dynamic(task).await;
            return;
        }

        if slug_owned == "pgvector" {
            install_pgvector_dynamic(task).await;
            return;
        }

        if slug_owned == "tailscale" {
            super::tailscale::install(task).await;
            return;
        }

        for (idx, step) in recipe.iter().enumerate() {
            let cmd = step[0];
            let args = &step[1..];
            task.log(format!("[{}/{}] $ {} {}", idx + 1, recipe.len(), cmd, args.join(" ")));

            let res = exec::run_streaming_envs(
                cmd,
                args.iter().copied(),
                [("DEBIAN_FRONTEND", "noninteractive")],
                |line: &str| task.log(line.to_string()),
            )
            .await;

            match res {
                Ok(0) => {}
                Ok(code) => {
                    task.log(format!("step exited with code {code}"));
                    task.finish(false);
                    return;
                }
                Err(e) => {
                    task.log(format!("step error: {e}"));
                    task.finish(false);
                    return;
                }
            }
        }

        if slug_owned == "phpmyadmin" {
            ensure_mariadb_docker_bridge_bind(&task).await;

            // Auto-bake PMA_ABSOLUTE_URI when there's exactly one
            // /domains routing slot pointing at port 8081. The
            // session-cookie-secure error (phpMyAdmin doesn't trust
            // X-Forwarded-Proto by default) bites every first-time
            // install where the operator has already created the
            // subdomain — without this hook they'd hit the cookie
            // error on first login and have to open the settings cog
            // to fix it. With 0 slots there's nothing to guess; with
            // 2+ slots we can't pick one safely. Either way we log a
            // breadcrumb so the operator knows what knob to turn.
            let slots: Vec<String> = panel_db
                .with(|conn| {
                    let mut stmt = conn.prepare(
                        "SELECT domain FROM sites
                          WHERE mode = 'port' AND upstream_port = 8081
                          ORDER BY domain",
                    )?;
                    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
                    Ok::<Vec<String>, rusqlite::Error>(
                        rows.filter_map(Result::ok).collect(),
                    )
                })
                .unwrap_or_default();
            match slots.len() {
                1 => {
                    let uri = format!("https://{}/", slots[0]);
                    task.log(format!(
                        "auto-detected /domains slot {} → setting PMA_ABSOLUTE_URI={uri}",
                        slots[0]
                    ));
                    let ok = recreate_phpmyadmin("127.0.0.1", &uri, &task).await;
                    if !ok {
                        task.log(
                            "WARNING: PMA_ABSOLUTE_URI auto-set failed — set it manually via the phpMyAdmin Settings cog"
                                .to_string(),
                        );
                    }
                }
                0 => {
                    task.log(
                        "no /domains slot points at port 8081 yet — when you add one, click the phpMyAdmin Settings cog to set PMA_ABSOLUTE_URI. Without it, the HTTPS login page shows a 'failed to set session cookie' error."
                            .to_string(),
                    );
                }
                _ => {
                    task.log(format!(
                        "multiple /domains slots point at port 8081 ({}) — pick one via the phpMyAdmin Settings cog",
                        slots.join(", ")
                    ));
                }
            }
        }

        if slug_owned == "pgadmin" {
            // Postgres twin of the phpMyAdmin bridge bind above: make the
            // host's PostgreSQL reachable from the pgAdmin container over the
            // docker0 bridge. Without this, registering a server inside
            // pgAdmin (host.docker.internal:5432) fails with "connection
            // timeout expired" because the host firewall drops the SYN.
            ensure_postgres_docker_bridge_bind(&task).await;
        }

        task.log(format!("{display_name} installed successfully"));
        task.finish(true);
    });

    Ok(Json(TaskAccepted { task_id }))
}

async fn detect_mail_hostname() -> Option<String> {
    let out = Command::new("postconf")
        .args(["-h", "myhostname"])
        .stdin(Stdio::null())
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        return None;
    }
    // Reject all-numeric hostnames (postfix sometimes ends up with the raw IP).
    if s.parse::<std::net::IpAddr>().is_ok() {
        return None;
    }
    // Require a dotted FQDN — otherwise TLS verification can't match.
    if !s.contains('.') {
        return None;
    }
    Some(s)
}

async fn install_roundcube_dynamic(task: crate::ipc::tasks::TaskHandle) {
    let prep: &[&[&str]] = &[
        &["apt-get", "update"],
        &["apt-get", "install", "-y", "docker.io"],
        &["systemctl", "enable", "docker"],
        &["systemctl", "start", "docker"],
        &["docker", "volume", "create", "roundcube_db"],
        &["docker", "volume", "create", "roundcube_config"],
    ];
    let total = prep.len() + 1;
    for (idx, step) in prep.iter().enumerate() {
        let cmd = step[0];
        let args = &step[1..];
        task.log(format!(
            "[{}/{}] $ {} {}",
            idx + 1,
            total,
            cmd,
            args.join(" ")
        ));
        let res = exec::run_streaming_envs(
            cmd,
            args.iter().copied(),
            [("DEBIAN_FRONTEND", "noninteractive")],
            |line: &str| task.log(line.to_string()),
        )
        .await;
        match res {
            Ok(0) => {}
            Ok(code) => {
                task.log(format!("step exited with code {code}"));
                task.finish(false);
                return;
            }
            Err(e) => {
                task.log(format!("step error: {e}"));
                task.finish(false);
                return;
            }
        }
    }

    // Detect the mail server's FQDN so Roundcube can connect over TLS using a
    // hostname that matches the cert. Falls back to localhost (which won't work
    // out of the box but at least lets the operator override via env later).
    let mail_host = match detect_mail_hostname().await {
        Some(h) => {
            task.log(format!("detected mail hostname from postfix: {h}"));
            h
        }
        None => {
            task.log(
                "could not detect mail hostname from postfix; defaulting to localhost. \
                 You will need to edit ROUNDCUBEMAIL_DEFAULT_HOST manually."
                    .to_string(),
            );
            "localhost".to_string()
        }
    };

    let default_host = format!("ROUNDCUBEMAIL_DEFAULT_HOST=ssl://{mail_host}");
    let smtp_server = format!("ROUNDCUBEMAIL_SMTP_SERVER=tls://{mail_host}");
    // Map the mail FQDN(s) to the docker bridge gateway inside the container.
    // Without this, the container resolves `<mail_host>` to the host's public
    // IP and tries to connect to itself via the public address — which works
    // outbound but the reply hairpins through NAT and the connection is
    // refused/dropped depending on iptables. Pointing the name at
    // `host-gateway` keeps TLS SNI/cert verification intact (same hostname)
    // while routing the traffic over the bridge instead.
    let mail_host_arg = format!("--add-host={mail_host}:host-gateway");
    let mail_alias_arg = if mail_host.starts_with("mail.") {
        None
    } else {
        Some(format!("--add-host=mail.{mail_host}:host-gateway"))
    };
    let mut docker_args: Vec<&str> = vec![
        "run",
        "-d",
        "--name",
        "roundcube",
        "--restart",
        "unless-stopped",
        "-p",
        "127.0.0.1:8082:80",
        "-e",
        "ROUNDCUBEMAIL_DB_TYPE=sqlite",
        "-e",
        &default_host,
        "-e",
        "ROUNDCUBEMAIL_DEFAULT_PORT=993",
        "-e",
        &smtp_server,
        "-e",
        "ROUNDCUBEMAIL_SMTP_PORT=587",
        "--add-host=host.docker.internal:host-gateway",
        &mail_host_arg,
    ];
    if let Some(arg) = &mail_alias_arg {
        docker_args.push(arg);
    }
    docker_args.extend([
        "-v",
        "roundcube_db:/var/roundcube/db",
        "-v",
        "roundcube_config:/var/roundcube/config",
        "roundcube/roundcubemail:1.6.x-apache",
    ]);
    task.log(format!("[{total}/{total}] $ docker {}", docker_args.join(" ")));
    let res = exec::run_streaming("docker", docker_args.iter().copied(), |l| {
        task.log(l.to_string())
    })
    .await;
    match res {
        Ok(0) => {
            task.log("Roundcube installed successfully".to_string());
            task.finish(true);
        }
        Ok(code) => {
            task.log(format!("docker run exited with code {code}"));
            task.finish(false);
        }
        Err(e) => {
            task.log(format!("docker run error: {e}"));
            task.finish(false);
        }
    }
}

/// pgvector install. The apt package is version-pinned
/// (`postgresql-<major>-pgvector`), so we detect the running major version
/// first instead of baking a fixed package name into a static Recipe. No
/// `systemctl restart` is needed afterwards — pgvector is a plain extension
/// (not a `shared_preload_libraries` module), so `CREATE EXTENSION vector`
/// only needs the control/SQL files the package drops under
/// `/usr/share/postgresql/<ver>/extension/`. Enabling it per-database happens
/// separately via the Postgres handler.
async fn install_pgvector_dynamic(task: crate::ipc::tasks::TaskHandle) {
    let version = match detect_postgres_version().await {
        Some(v) => {
            task.log(format!("detected PostgreSQL major version {v}"));
            v
        }
        None => {
            task.log(
                "PostgreSQL is not installed — install it first, then install pgvector."
                    .to_string(),
            );
            task.finish(false);
            return;
        }
    };

    let package = format!("postgresql-{version}-pgvector");
    let steps: [Vec<&str>; 2] = [
        vec!["apt-get", "update"],
        vec!["apt-get", "install", "-y", package.as_str()],
    ];
    let total = steps.len();
    for (idx, step) in steps.iter().enumerate() {
        let cmd = step[0];
        let args = &step[1..];
        task.log(format!(
            "[{}/{}] $ {} {}",
            idx + 1,
            total,
            cmd,
            args.join(" ")
        ));
        let res = exec::run_streaming_envs(
            cmd,
            args.iter().copied(),
            [("DEBIAN_FRONTEND", "noninteractive")],
            |line: &str| task.log(line.to_string()),
        )
        .await;
        match res {
            Ok(0) => {}
            Ok(code) => {
                task.log(format!("step exited with code {code}"));
                task.finish(false);
                return;
            }
            Err(e) => {
                task.log(format!("step error: {e}"));
                task.finish(false);
                return;
            }
        }
    }

    task.log(
        "pgvector installed. Now enable it per-database from the Postgres section (CREATE EXTENSION vector)."
            .to_string(),
    );
    task.finish(true);
}

// ─── Roundcube settings (editable from the panel UI) ──────────────────
//
// The Roundcube container's IMAP/SMTP target lives in env vars baked into
// the container at `docker run` time. When PANEL_HOST changes after
// Roundcube was installed, the container drifts (still pointing at the
// install-time placeholder, often `vps-panel.local`) and login fails with
// "Connection to storage server failed" + an SSL alert 46 (cert mismatch)
// in dovecot. These two endpoints let the operator inspect + correct
// those values from the panel UI without dropping to docker CLI.

#[derive(Serialize)]
pub struct RoundcubeSettings {
    /// Current IMAP/SMTP target (hostname only, ssl:// prefix stripped).
    /// Pulled from the running container's env vars.
    pub mail_host: String,
    pub imap_port: u16,
    pub smtp_port: u16,
    /// What `postconf -h myhostname` reports right now — surfaced so the UI
    /// can show "Detected: <fqdn>" and warn the operator when the container
    /// is out of sync with postfix.
    pub detected_mail_host: Option<String>,
    /// True if the container is currently in sync with postfix's myhostname.
    /// Lets the UI render a green / amber pill without re-deriving the same
    /// comparison on the client.
    pub in_sync: bool,
}

pub async fn get_roundcube_settings() -> AgentResult<Json<RoundcubeSettings>> {
    let env_out = exec::run(
        "docker",
        [
            "inspect",
            "roundcube",
            "--format",
            "{{range .Config.Env}}{{println .}}{{end}}",
        ],
    )
    .await
    .map_err(|e| AgentError::Other(anyhow::anyhow!("docker inspect failed: {e}")))?;
    let mut mail_host = String::new();
    let mut imap_port = 993u16;
    let mut smtp_port = 587u16;
    for line in env_out.stdout.lines() {
        if let Some(v) = line.strip_prefix("ROUNDCUBEMAIL_DEFAULT_HOST=") {
            mail_host = v
                .trim_start_matches("ssl://")
                .trim_start_matches("tls://")
                .trim_start_matches("tcp://")
                .to_string();
        } else if let Some(v) = line.strip_prefix("ROUNDCUBEMAIL_DEFAULT_PORT=") {
            imap_port = v.parse().unwrap_or(993);
        } else if let Some(v) = line.strip_prefix("ROUNDCUBEMAIL_SMTP_PORT=") {
            smtp_port = v.parse().unwrap_or(587);
        }
    }
    let detected_mail_host = detect_mail_hostname().await;
    let in_sync = match detected_mail_host.as_deref() {
        Some(d) => d == mail_host,
        None => false,
    };
    Ok(Json(RoundcubeSettings {
        mail_host,
        imap_port,
        smtp_port,
        detected_mail_host,
        in_sync,
    }))
}

#[derive(Deserialize)]
pub struct UpdateRoundcubeSettingsReq {
    pub mail_host: String,
    pub imap_port: u16,
    pub smtp_port: u16,
}

pub async fn update_roundcube_settings(
    State(state): State<AppState>,
    Json(req): Json<UpdateRoundcubeSettingsReq>,
) -> AgentResult<Json<TaskAccepted>> {
    let mail_host = req.mail_host.trim().to_string();
    if mail_host.is_empty() {
        return Err(AgentError::Invalid("mail_host required".into()));
    }
    if !mail_host.contains('.') {
        return Err(AgentError::Invalid(
            "mail_host must be an FQDN (TLS cert verification requires a dotted name)".into(),
        ));
    }
    if mail_host.parse::<std::net::IpAddr>().is_ok() {
        return Err(AgentError::Invalid(
            "mail_host must be a hostname, not an IP (TLS SNI uses the name)".into(),
        ));
    }
    // Same character class postfix accepts for myhostname — keeps junk out
    // of the docker run command line.
    let safe = mail_host
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.');
    if !safe {
        return Err(AgentError::Invalid(
            "mail_host may only contain letters, digits, hyphens and dots".into(),
        ));
    }
    if !(1..65535).contains(&req.imap_port) {
        return Err(AgentError::Invalid("imap_port out of range".into()));
    }
    if !(1..65535).contains(&req.smtp_port) {
        return Err(AgentError::Invalid("smtp_port out of range".into()));
    }

    let task = state.tasks.create("quick-install-settings:roundcube");
    let task_id = task.id.clone();
    let imap_port = req.imap_port;
    let smtp_port = req.smtp_port;

    tokio::spawn(async move {
        task.start();

        task.log(format!(
            "applying settings: mail_host={mail_host} imap={imap_port} smtp={smtp_port}"
        ));

        task.log("$ docker stop roundcube".to_string());
        let _ = exec::run_streaming("docker", ["stop", "roundcube"], |l| {
            task.log(l.to_string())
        })
        .await;

        task.log("$ docker rm roundcube".to_string());
        let _ = exec::run_streaming("docker", ["rm", "roundcube"], |l| {
            task.log(l.to_string())
        })
        .await;

        // Wipe the config volume so the entrypoint regenerates
        // config.inc.php from the new env vars. Without this the cached
        // file in the volume wins and the new values are ignored. The
        // roundcube_db volume (user prefs, address books, identities) is
        // preserved.
        task.log(
            "$ docker volume rm roundcube_config (forces config.inc.php regeneration)"
                .to_string(),
        );
        let _ = exec::run_streaming("docker", ["volume", "rm", "roundcube_config"], |l| {
            task.log(l.to_string())
        })
        .await;
        task.log("$ docker volume create roundcube_config".to_string());
        let _ = exec::run_streaming(
            "docker",
            ["volume", "create", "roundcube_config"],
            |l| task.log(l.to_string()),
        )
        .await;

        let default_host = format!("ROUNDCUBEMAIL_DEFAULT_HOST=ssl://{mail_host}");
        let imap_port_env = format!("ROUNDCUBEMAIL_DEFAULT_PORT={imap_port}");
        let smtp_server = format!("ROUNDCUBEMAIL_SMTP_SERVER=tls://{mail_host}");
        let smtp_port_env = format!("ROUNDCUBEMAIL_SMTP_PORT={smtp_port}");
        let mail_host_arg = format!("--add-host={mail_host}:host-gateway");
        let mail_alias_arg = if mail_host.starts_with("mail.") {
            None
        } else {
            Some(format!("--add-host=mail.{mail_host}:host-gateway"))
        };
        let mut docker_args: Vec<&str> = vec![
            "run",
            "-d",
            "--name",
            "roundcube",
            "--restart",
            "unless-stopped",
            "-p",
            "127.0.0.1:8082:80",
            "-e",
            "ROUNDCUBEMAIL_DB_TYPE=sqlite",
            "-e",
            &default_host,
            "-e",
            &imap_port_env,
            "-e",
            &smtp_server,
            "-e",
            &smtp_port_env,
            "--add-host=host.docker.internal:host-gateway",
            &mail_host_arg,
        ];
        if let Some(arg) = &mail_alias_arg {
            docker_args.push(arg);
        }
        docker_args.extend([
            "-v",
            "roundcube_db:/var/roundcube/db",
            "-v",
            "roundcube_config:/var/roundcube/config",
            "roundcube/roundcubemail:1.6.x-apache",
        ]);
        task.log(format!("$ docker {}", docker_args.join(" ")));
        let res = exec::run_streaming("docker", docker_args.iter().copied(), |l| {
            task.log(l.to_string())
        })
        .await;
        match res {
            Ok(0) => {
                task.log("Roundcube settings applied".to_string());
                task.finish(true);
            }
            Ok(code) => {
                task.log(format!("docker run exited with code {code}"));
                task.finish(false);
            }
            Err(e) => {
                task.log(format!("docker run error: {e}"));
                task.finish(false);
            }
        }
    });

    Ok(Json(TaskAccepted { task_id }))
}

// ─── n8n settings (editable from the panel UI) ────────────────────────
//
// Like Roundcube, n8n's container needs env vars baked at `docker run`
// time to know its public URL. Without them, the generated webhook URLs
// look like `http://localhost:5678/webhook/<id>` even when the operator
// has set up an nginx vhost pointing at port 5678. These endpoints let
// the panel UI inspect + edit those values, and they auto-suggest any
// /domains routing slot already mapped to upstream_port=5678.

#[derive(Serialize)]
pub struct N8nSettings {
    /// Current N8N_HOST env value (empty string if not set).
    pub host: String,
    /// Current N8N_PROTOCOL ("http" / "https"). Empty if unset.
    pub protocol: String,
    /// Current WEBHOOK_URL (the canonical override). Empty if unset.
    pub webhook_url: String,
    /// Current host port bind (127.0.0.1 vs 0.0.0.0) — preserved across
    /// settings updates so changing the URL doesn't accidentally flip
    /// the container from local-only to public.
    pub bind: String,
    /// Domains in panel.db whose /domains routing slot points at port
    /// 5678. Surfaced as one-click suggestions in the panel UI.
    pub suggested_domains: Vec<String>,
}

pub async fn get_n8n_settings(
    State(state): State<AppState>,
) -> AgentResult<Json<N8nSettings>> {
    let env_out = exec::run(
        "docker",
        [
            "inspect",
            "n8n",
            "--format",
            "{{range .Config.Env}}{{println .}}{{end}}",
        ],
    )
    .await
    .map_err(|e| AgentError::Other(anyhow::anyhow!("docker inspect failed: {e}")))?;

    let mut host = String::new();
    let mut protocol = String::new();
    let mut webhook_url = String::new();
    for line in env_out.stdout.lines() {
        if let Some(v) = line.strip_prefix("N8N_HOST=") {
            host = v.to_string();
        } else if let Some(v) = line.strip_prefix("N8N_PROTOCOL=") {
            protocol = v.to_string();
        } else if let Some(v) = line.strip_prefix("WEBHOOK_URL=") {
            webhook_url = v.to_string();
        }
    }

    let bind = current_docker_bind("n8n", 5678)
        .await
        .unwrap_or_else(|| "127.0.0.1".to_string());

    // Pull /domains entries mapped to port 5678 so the panel can offer
    // them as one-click "use this domain" suggestions.
    let suggested_domains: Vec<String> = state
        .panel_db
        .with(|conn| {
            let mut stmt = conn.prepare(
                "SELECT domain FROM sites
                  WHERE mode = 'port' AND upstream_port = 5678
                  ORDER BY domain",
            )?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            Ok::<Vec<String>, rusqlite::Error>(rows.filter_map(Result::ok).collect())
        })
        .unwrap_or_default();

    Ok(Json(N8nSettings {
        host,
        protocol,
        webhook_url,
        bind,
        suggested_domains,
    }))
}

#[derive(Deserialize)]
pub struct UpdateN8nSettingsReq {
    pub host: String,
    pub protocol: String,
    pub webhook_url: String,
}

pub async fn update_n8n_settings(
    State(state): State<AppState>,
    Json(req): Json<UpdateN8nSettingsReq>,
) -> AgentResult<Json<TaskAccepted>> {
    let host = req.host.trim().to_string();
    let protocol = req.protocol.trim().to_lowercase();
    let webhook_url = req.webhook_url.trim().to_string();

    if host.is_empty() {
        return Err(AgentError::Invalid("host required".into()));
    }
    if !host.contains('.') {
        return Err(AgentError::Invalid(
            "host must be an FQDN (e.g. n8n.example.com)".into(),
        ));
    }
    let safe_host = host
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.');
    if !safe_host {
        return Err(AgentError::Invalid(
            "host may only contain letters, digits, hyphens and dots".into(),
        ));
    }
    if protocol != "http" && protocol != "https" {
        return Err(AgentError::Invalid(
            "protocol must be http or https".into(),
        ));
    }
    if webhook_url.is_empty() {
        return Err(AgentError::Invalid("webhook_url required".into()));
    }
    if !webhook_url.starts_with("http://") && !webhook_url.starts_with("https://") {
        return Err(AgentError::Invalid(
            "webhook_url must start with http:// or https://".into(),
        ));
    }
    // Defensive: keep shell-meaningful chars out of the docker run args.
    let safe_url = webhook_url
        .chars()
        .all(|c| !matches!(c, '\n' | '\r' | ' ' | '"' | '\'' | '`' | '$' | ';'));
    if !safe_url {
        return Err(AgentError::Invalid(
            "webhook_url contains disallowed characters".into(),
        ));
    }

    // Preserve the current bind across settings changes so editing the
    // URL doesn't accidentally flip the container from 127.0.0.1 to
    // 0.0.0.0 (or vice versa).
    let bind = current_docker_bind("n8n", 5678)
        .await
        .unwrap_or_else(|| "127.0.0.1".to_string());

    let task = state.tasks.create("quick-install-settings:n8n");
    let task_id = task.id.clone();

    tokio::spawn(async move {
        task.start();
        task.log(format!(
            "applying settings: host={host} protocol={protocol} webhook_url={webhook_url} bind={bind}"
        ));

        task.log("$ docker stop n8n".to_string());
        let _ = exec::run_streaming("docker", ["stop", "n8n"], |l| task.log(l.to_string())).await;
        task.log("$ docker rm n8n".to_string());
        let _ = exec::run_streaming("docker", ["rm", "n8n"], |l| task.log(l.to_string())).await;

        // The n8n_data volume holds workflows + credentials + the
        // SQLite DB. We DO NOT wipe it — the env-var changes propagate
        // through n8n's runtime config at startup without touching
        // stored state.

        let port_arg = format!("{bind}:5678:5678");
        let host_env = format!("N8N_HOST={host}");
        let protocol_env = format!("N8N_PROTOCOL={protocol}");
        let webhook_env = format!("WEBHOOK_URL={webhook_url}");
        let editor_env = format!("N8N_EDITOR_BASE_URL={webhook_url}");
        let docker_args: Vec<&str> = vec![
            "run",
            "-d",
            "--name",
            "n8n",
            "--restart",
            "unless-stopped",
            "-p",
            &port_arg,
            "-e",
            &host_env,
            "-e",
            &protocol_env,
            "-e",
            &webhook_env,
            "-e",
            &editor_env,
            "-v",
            "n8n_data:/home/node/.n8n",
            "n8nio/n8n",
        ];
        task.log(format!("$ docker {}", docker_args.join(" ")));
        let res = exec::run_streaming("docker", docker_args.iter().copied(), |l| {
            task.log(l.to_string())
        })
        .await;
        match res {
            Ok(0) => {
                task.log("n8n settings applied".to_string());
                task.finish(true);
            }
            Ok(code) => {
                task.log(format!("docker run exited with code {code}"));
                task.finish(false);
            }
            Err(e) => {
                task.log(format!("docker run error: {e}"));
                task.finish(false);
            }
        }
    });

    Ok(Json(TaskAccepted { task_id }))
}

/// Pull the latest `n8nio/n8n` image and recreate the container on it. This is
/// the "Update available → click to update + restart" path from the n8n card.
/// Preserves the operator's public-URL env (N8N_HOST / N8N_PROTOCOL /
/// WEBHOOK_URL / N8N_EDITOR_BASE_URL), the current port bind, and the
/// `n8n_data` volume (workflows, credentials, sqlite DB) — only the image
/// changes.
pub async fn update_n8n(State(state): State<AppState>) -> AgentResult<Json<TaskAccepted>> {
    // Capture the live container's env + bind BEFORE teardown so the recreate
    // keeps whatever public-URL the operator configured. `current_docker_env`
    // already returns None for empty values, so we only re-inject real ones.
    let host = current_docker_env("n8n", "N8N_HOST").await;
    let protocol = current_docker_env("n8n", "N8N_PROTOCOL").await;
    let webhook_url = current_docker_env("n8n", "WEBHOOK_URL").await;
    let editor_url = current_docker_env("n8n", "N8N_EDITOR_BASE_URL").await;
    let bind = current_docker_bind("n8n", 5678)
        .await
        .unwrap_or_else(|| "127.0.0.1".to_string());

    let task = state.tasks.create("quick-install-update:n8n");
    let task_id = task.id.clone();

    tokio::spawn(async move {
        task.start();
        task.log("updating n8n to the latest n8nio/n8n image".to_string());

        // 1. Pull the latest image FIRST. If this fails (offline, registry
        //    down) we abort before tearing down the running container so the
        //    operator is never left with no n8n.
        task.log("$ docker pull n8nio/n8n".to_string());
        let pull = exec::run_streaming("docker", ["pull", "n8nio/n8n"], |l| {
            task.log(l.to_string())
        })
        .await;
        if !matches!(pull, Ok(0)) {
            task.log(format!(
                "docker pull failed ({pull:?}) — container left running on the current image"
            ));
            task.finish(false);
            return;
        }

        // 2. Tear down the old container. The n8n_data volume is NOT removed,
        //    so workflows / credentials / the sqlite DB survive.
        task.log("$ docker stop n8n".to_string());
        let _ = exec::run_streaming("docker", ["stop", "n8n"], |l| task.log(l.to_string())).await;
        task.log("$ docker rm n8n".to_string());
        let _ = exec::run_streaming("docker", ["rm", "n8n"], |l| task.log(l.to_string())).await;

        // 3. Recreate on the freshly-pulled image, re-injecting preserved env.
        let port_arg = format!("{bind}:5678:5678");
        let mut docker_args: Vec<String> = vec![
            "run".into(),
            "-d".into(),
            "--name".into(),
            "n8n".into(),
            "--restart".into(),
            "unless-stopped".into(),
            "-p".into(),
            port_arg,
        ];
        for (key, val) in [
            ("N8N_HOST", &host),
            ("N8N_PROTOCOL", &protocol),
            ("WEBHOOK_URL", &webhook_url),
            ("N8N_EDITOR_BASE_URL", &editor_url),
        ] {
            if let Some(v) = val {
                if !v.is_empty() {
                    docker_args.push("-e".into());
                    docker_args.push(format!("{key}={v}"));
                }
            }
        }
        docker_args.push("-v".into());
        docker_args.push("n8n_data:/home/node/.n8n".into());
        docker_args.push("n8nio/n8n".into());

        task.log(format!("$ docker {}", docker_args.join(" ")));
        let res = exec::run_streaming("docker", docker_args.iter().map(String::as_str), |l| {
            task.log(l.to_string())
        })
        .await;
        match res {
            Ok(0) => {
                task.log("n8n updated and restarted".to_string());
                task.finish(true);
            }
            Ok(code) => {
                task.log(format!("docker run exited with code {code}"));
                task.finish(false);
            }
            Err(e) => {
                task.log(format!("docker run error: {e}"));
                task.finish(false);
            }
        }
    });

    Ok(Json(TaskAccepted { task_id }))
}

// ─── phpMyAdmin settings (editable from the panel UI) ──────────────────
//
// The phpmyadmin/phpmyadmin Docker image needs PMA_ABSOLUTE_URI baked at
// `docker run` time when it sits behind a reverse proxy. Without it, the
// PHP session cookie ends up Secure-flagged on an HTTP-from-PHP's-POV
// connection (nginx → container is plain HTTP internally even though the
// browser hit HTTPS) and the browser refuses to store it. Symptom:
// "Failed to set session cookie. Maybe you are using HTTP instead of HTTPS
// to access phpMyAdmin." on the login page.
//
// These endpoints let the panel UI inspect + edit that one env var, and
// auto-suggest any /domains routing slot already mapped to upstream port
// 8081 so the operator can one-click the matching URL.

#[derive(Serialize)]
pub struct PhpMyAdminSettings {
    /// Current PMA_ABSOLUTE_URI env value (empty string if not set).
    pub absolute_uri: String,
    /// Current host port bind (127.0.0.1 vs 0.0.0.0) — preserved across
    /// settings updates so changing the URI doesn't accidentally flip the
    /// container from local-only to public.
    pub bind: String,
    /// Domains in panel.db whose /domains routing slot points at port
    /// 8081. Surfaced as one-click suggestions in the panel UI.
    pub suggested_domains: Vec<String>,
}

pub async fn get_phpmyadmin_settings(
    State(state): State<AppState>,
) -> AgentResult<Json<PhpMyAdminSettings>> {
    let absolute_uri = current_docker_env("phpmyadmin", "PMA_ABSOLUTE_URI")
        .await
        .unwrap_or_default();
    let bind = current_docker_bind("phpmyadmin", 8081)
        .await
        .unwrap_or_else(|| "127.0.0.1".to_string());

    // Pull /domains entries mapped to port 8081 so the panel UI can offer
    // them as one-click "use this domain" suggestions.
    let suggested_domains: Vec<String> = state
        .panel_db
        .with(|conn| {
            let mut stmt = conn.prepare(
                "SELECT domain FROM sites
                  WHERE mode = 'port' AND upstream_port = 8081
                  ORDER BY domain",
            )?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            Ok::<Vec<String>, rusqlite::Error>(rows.filter_map(Result::ok).collect())
        })
        .unwrap_or_default();

    Ok(Json(PhpMyAdminSettings {
        absolute_uri,
        bind,
        suggested_domains,
    }))
}

#[derive(Deserialize)]
pub struct UpdatePhpMyAdminSettingsReq {
    /// Full public URL phpMyAdmin should think it's served at, e.g.
    /// "https://pma.example.com/". Becomes the PMA_ABSOLUTE_URI env var.
    /// Empty string clears the var (back to default phpmyadmin behaviour).
    pub absolute_uri: String,
}

pub async fn update_phpmyadmin_settings(
    State(state): State<AppState>,
    Json(req): Json<UpdatePhpMyAdminSettingsReq>,
) -> AgentResult<Json<TaskAccepted>> {
    let absolute_uri = req.absolute_uri.trim().to_string();

    // Empty string is allowed — it means "clear the var" (recreates the
    // container without the env so phpMyAdmin falls back to default
    // self-detection). Anything non-empty must look like a real URL.
    if !absolute_uri.is_empty() {
        if !absolute_uri.starts_with("http://") && !absolute_uri.starts_with("https://") {
            return Err(AgentError::Invalid(
                "absolute_uri must start with http:// or https://".into(),
            ));
        }
        // Defensive: keep shell-meaningful chars out of the docker run args.
        // Same allowlist as n8n's webhook_url so the behavior is consistent.
        let safe = absolute_uri
            .chars()
            .all(|c| !matches!(c, '\n' | '\r' | ' ' | '"' | '\'' | '`' | '$' | ';'));
        if !safe {
            return Err(AgentError::Invalid(
                "absolute_uri contains disallowed characters".into(),
            ));
        }
        // Parse the host portion to sanity-check it's an FQDN-shaped value.
        // We can't pull in `url`/`http` crates just for this — strip the
        // scheme by hand and check the next segment looks reasonable.
        let after_scheme = absolute_uri
            .splitn(2, "://")
            .nth(1)
            .unwrap_or("");
        let host_part = after_scheme
            .split(['/', '?', '#'])
            .next()
            .unwrap_or("");
        if host_part.is_empty() || !host_part.contains('.') {
            return Err(AgentError::Invalid(
                "absolute_uri must contain a host with a dot (e.g. https://pma.example.com/)".into(),
            ));
        }
    }

    // Preserve the current bind across settings changes so editing the URI
    // doesn't accidentally flip the container from 127.0.0.1 to 0.0.0.0
    // (or vice versa). Same defense as n8n's update.
    let bind = current_docker_bind("phpmyadmin", 8081)
        .await
        .unwrap_or_else(|| "127.0.0.1".to_string());

    let task = state.tasks.create("quick-install-settings:phpmyadmin");
    let task_id = task.id.clone();

    tokio::spawn(async move {
        task.start();
        if absolute_uri.is_empty() {
            task.log(format!("clearing PMA_ABSOLUTE_URI (bind={bind})"));
        } else {
            task.log(format!(
                "applying settings: PMA_ABSOLUTE_URI={absolute_uri} bind={bind}"
            ));
        }

        let ok = recreate_phpmyadmin(&bind, &absolute_uri, &task).await;
        if ok {
            // Self-heal the MariaDB bridge bind whenever phpMyAdmin is
            // touched — same reasoning as set_bind.
            ensure_mariadb_docker_bridge_bind(&task).await;
            task.log("phpMyAdmin settings applied".to_string());
            task.finish(true);
        } else {
            task.finish(false);
        }
    });

    Ok(Json(TaskAccepted { task_id }))
}

/// Tear down + recreate the phpmyadmin container with the given bind and
/// `PMA_ABSOLUTE_URI`. Empty `uri` means "don't pass the env var at all"
/// (reverts to default phpmyadmin behavior). Returns false if the docker
/// run exits non-zero or fails — callers decide whether to surface that
/// as a task failure or just a warning.
///
/// Shared by `update_phpmyadmin_settings` (settings cog → Apply), the
/// post-install auto-detect path in `run()`, and conceptually by
/// `set_bind` too (though that path inlines the recreate so it can also
/// preserve the previous URI).
async fn recreate_phpmyadmin(
    bind: &str,
    uri: &str,
    task: &crate::ipc::tasks::TaskHandle,
) -> bool {
    task.log("$ docker stop phpmyadmin".to_string());
    let _ = exec::run_streaming("docker", ["stop", "phpmyadmin"], |l| {
        task.log(l.to_string())
    })
    .await;
    task.log("$ docker rm phpmyadmin".to_string());
    let _ = exec::run_streaming("docker", ["rm", "phpmyadmin"], |l| {
        task.log(l.to_string())
    })
    .await;

    let port_arg = format!("{bind}:8081:80");
    let pma_env = format!("PMA_ABSOLUTE_URI={uri}");
    let mut docker_args: Vec<&str> = vec![
        "run",
        "-d",
        "--name",
        "phpmyadmin",
        "--restart",
        "unless-stopped",
        "-p",
        port_arg.as_str(),
        "-e",
        "PMA_HOST=host.docker.internal",
        "--add-host=host.docker.internal:host-gateway",
    ];
    if !uri.is_empty() {
        docker_args.push("-e");
        docker_args.push(pma_env.as_str());
    }
    docker_args.push("phpmyadmin/phpmyadmin");

    task.log(format!("$ docker {}", docker_args.join(" ")));
    let res = exec::run_streaming("docker", docker_args.iter().copied(), |l| {
        task.log(l.to_string())
    })
    .await;
    match res {
        Ok(0) => true,
        Ok(code) => {
            task.log(format!("docker run exited with code {code}"));
            false
        }
        Err(e) => {
            task.log(format!("docker run error: {e}"));
            false
        }
    }
}

#[derive(Deserialize)]
pub struct SetBindReq {
    pub bind: String,
}

pub async fn set_bind(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Json(req): Json<SetBindReq>,
) -> AgentResult<Json<TaskAccepted>> {
    if req.bind != "127.0.0.1" && req.bind != "0.0.0.0" {
        return Err(AgentError::Invalid(
            "bind must be 127.0.0.1 or 0.0.0.0".into(),
        ));
    }

    // Dispatch: docker web-UI recipes (pgadmin, phpmyadmin, n8n) go
    // through the existing container-recreate path below. The two
    // systemd-managed DB engines (mariadb, postgresql) need engine-
    // specific config-file edits and live in dedicated handlers — the
    // shape of the surface is identical from the panel's POV (POST
    // /quick-install/<slug>/bind with bind=127.0.0.1|0.0.0.0).
    if slug == "mariadb" {
        return set_mariadb_bind(state, req.bind).await;
    }
    if slug == "postgresql" {
        return set_postgres_bind(state, req.bind).await;
    }

    let svc = web_ui_service(&slug)
        .ok_or_else(|| AgentError::Invalid(format!("bind toggle not supported for {slug}")))?;

    let task = state.tasks.create(&format!("quick-install-bind:{slug}"));
    let task_id = task.id.clone();
    let bind = req.bind.clone();
    let host_port = svc.host_port;

    // Preserve env vars that `update_*_settings` set on the live container
    // but aren't in the static `extra_args` of the DockerService. Without
    // this, flipping the bind dropdown silently wipes whatever the
    // operator just configured in the settings card. Captured BEFORE
    // `docker stop` so the value is still readable.
    // n8n's public-URL settings were not in this list, so switching n8n
    // between Local and Public dropped them and webhooks fell back to
    // localhost:5678.
    let preserve_vars: &[&str] = match svc.container_name {
        "phpmyadmin" => &["PMA_ABSOLUTE_URI"],
        "n8n" => &["N8N_HOST", "N8N_PROTOCOL", "WEBHOOK_URL", "N8N_EDITOR_BASE_URL"],
        _ => &[],
    };
    let mut preserved_env: Vec<String> = Vec::new();
    for var in preserve_vars {
        if let Some(value) = current_docker_env(svc.container_name, var).await {
            preserved_env.push(format!("{var}={value}"));
        }
    }

    tokio::spawn(async move {
        task.start();
        task.log(format!(
            "rebinding {} to {}:{}",
            svc.container_name, bind, host_port
        ));

        task.log(format!("$ docker stop {}", svc.container_name));
        let _ = exec::run_streaming("docker", ["stop", svc.container_name], |l| {
            task.log(l.to_string())
        })
        .await;

        task.log(format!("$ docker rm {}", svc.container_name));
        let _ = exec::run_streaming("docker", ["rm", svc.container_name], |l| {
            task.log(l.to_string())
        })
        .await;

        let port_arg = format!("{}:{}:{}", bind, svc.host_port, svc.container_port);
        let mut docker_args: Vec<&str> = vec![
            "run",
            "-d",
            "--name",
            svc.container_name,
            "--restart",
            "unless-stopped",
            "-p",
            port_arg.as_str(),
        ];
        docker_args.extend_from_slice(svc.extra_args);
        // Re-inject preserved env after extra_args, so it wins over defaults.
        for env in &preserved_env {
            task.log(format!("preserving {env}"));
            docker_args.push("-e");
            docker_args.push(env.as_str());
        }
        docker_args.push(svc.image);

        task.log(format!("$ docker {}", docker_args.join(" ")));
        let res = exec::run_streaming("docker", docker_args.iter().copied(), |l| {
            task.log(l.to_string())
        })
        .await;

        match res {
            Ok(0) => {}
            Ok(code) => {
                task.log(format!("docker run exited with code {code}"));
                task.finish(false);
                return;
            }
            Err(e) => {
                task.log(format!("docker run error: {e}"));
                task.finish(false);
                return;
            }
        }

        if bind == "0.0.0.0" {
            task.log(format!("$ ufw allow {}/tcp", host_port));
            let _ = exec::run_streaming(
                "ufw",
                ["allow", &format!("{}/tcp", host_port)],
                |l| task.log(l.to_string()),
            )
            .await;
        } else {
            task.log(format!("$ ufw delete allow {}/tcp", host_port));
            let _ = exec::run_streaming(
                "ufw",
                ["delete", "allow", &format!("{}/tcp", host_port)],
                |l| task.log(l.to_string()),
            )
            .await;
        }

        // Self-heal MariaDB bridge bind whenever phpMyAdmin is touched.
        // Cheap (idempotent — only restarts mariadb if the file actually
        // changed) and rescues operators who installed phpMyAdmin before
        // this code shipped.
        if svc.container_name == "phpmyadmin" {
            ensure_mariadb_docker_bridge_bind(&task).await;
        }
        // Same self-heal for pgAdmin → Postgres. Rescues operators who
        // installed pgAdmin before the bridge bind shipped, and re-asserts it
        // after any container recreate.
        if svc.container_name == "pgadmin" {
            ensure_postgres_docker_bridge_bind(&task).await;
        }

        task.log(format!("rebound to {}:{}", bind, host_port));
        task.finish(true);
    });

    Ok(Json(TaskAccepted { task_id }))
}

// ─── MariaDB bind toggle ─────────────────────────────────────────────────
//
// PUBLIC means: rewrite the drop-in to `bind-address = 0.0.0.0`, restart
// mariadb, open UFW for 3306/tcp from anywhere.
//
// LOCAL means: revert to the panel's default
// `bind-address = 127.0.0.1,172.17.0.1` (loopback + docker bridge so
// phpMyAdmin keeps working), restart mariadb, remove the generic
// `3306/tcp` UFW rule but KEEP the docker0-scoped one
// (`in on docker0 to any port 3306`) — without that the dockerised
// phpMyAdmin can't reach MariaDB even though we're going "back to local".

const MARIADB_PUBLIC_DROPIN: &str = "# Managed by vps-panel — do not edit by hand.\n# PUBLIC bind: MariaDB listens on every interface. Set via the panel's\n# bind dropdown on the MariaDB quick-install card. Flip back to LOCAL to\n# revert.\n#\n# Security note: anyone with TCP reach to this host on 3306 can attempt\n# auth — make sure every MariaDB user has a strong password and no\n# `'user'@'%'` accounts have weak ones.\n\n[mysqld]\nbind-address = 0.0.0.0\n";

async fn set_mariadb_bind(
    state: AppState,
    bind: String,
) -> AgentResult<Json<TaskAccepted>> {
    let task = state.tasks.create("quick-install-bind:mariadb");
    let task_id = task.id.clone();

    tokio::spawn(async move {
        task.start();
        let path = std::path::Path::new(MARIADB_DOCKER_DROPIN_PATH);
        let want = if bind == "0.0.0.0" {
            MARIADB_PUBLIC_DROPIN
        } else {
            MARIADB_DOCKER_DROPIN
        };
        let current = tokio::fs::read_to_string(path).await.ok();
        if current.as_deref() == Some(want) {
            task.log(format!(
                "{MARIADB_DOCKER_DROPIN_PATH} already up to date for bind={bind} — no mariadb restart needed"
            ));
        } else {
            task.log(format!(
                "rewriting {MARIADB_DOCKER_DROPIN_PATH} for bind={bind}"
            ));
            if let Err(e) =
                crate::fs_atomic::write(path, want.as_bytes(), 0o644).await
            {
                task.log(format!("failed to write {MARIADB_DOCKER_DROPIN_PATH}: {e}"));
                task.finish(false);
                return;
            }
            task.log("$ systemctl restart mariadb".to_string());
            let res = exec::run_streaming("systemctl", ["restart", "mariadb"], |l| {
                task.log(l.to_string())
            })
            .await;
            if !matches!(res, Ok(0)) {
                task.log(format!("systemctl restart mariadb failed: {res:?}"));
                task.finish(false);
                return;
            }
        }

        if bind == "0.0.0.0" {
            task.log("$ ufw allow 3306/tcp".to_string());
            let _ = exec::run_streaming("ufw", ["allow", "3306/tcp"], |l| {
                task.log(l.to_string())
            })
            .await;
        } else {
            task.log("$ ufw delete allow 3306/tcp".to_string());
            let _ = exec::run_streaming(
                "ufw",
                ["delete", "allow", "3306/tcp"],
                |l| task.log(l.to_string()),
            )
            .await;
            // Re-ensure the docker0-scoped rule so phpMyAdmin keeps
            // working after flipping back to LOCAL. `ufw allow ... in on
            // docker0 ...` is idempotent (UFW prints "Skipping adding
            // existing rule" when it's there already).
            task.log("$ ufw allow in on docker0 to any port 3306 proto tcp".to_string());
            let _ = exec::run_streaming(
                "ufw",
                [
                    "allow", "in", "on", "docker0", "to", "any", "port", "3306",
                    "proto", "tcp",
                ],
                |l| task.log(l.to_string()),
            )
            .await;
        }

        task.log(format!("MariaDB bind set to {bind}"));
        task.finish(true);
    });

    Ok(Json(TaskAccepted { task_id }))
}

// ─── PostgreSQL bind toggle ──────────────────────────────────────────────
//
// PUBLIC means:
//   1. postgresql.conf: `listen_addresses = '*'`
//   2. pg_hba.conf: append a panel-managed `host all all 0.0.0.0/0 scram-sha-256`
//      line so remote clients can actually authenticate (without an entry
//      in pg_hba.conf they get "no pg_hba.conf entry for host" even if
//      the network reaches the daemon)
//   3. systemctl restart postgresql
//   4. ufw allow 5432/tcp
//
// LOCAL means: revert each of those (listen_addresses='localhost',
// remove the panel-managed pg_hba line, restart, ufw delete).
//
// The pg_hba line is tracked via a marker comment so we can remove
// exactly what we added — we never touch any other line the operator
// (or the distro) put in pg_hba.conf.

const PG_HBA_MARKER: &str = "# vps-panel:public-bind";
const PG_HBA_PUBLIC_LINE: &str = "host    all             all             0.0.0.0/0               scram-sha-256";

// Docker-bridge counterparts. Unlike the public rule above, these are NOT
// part of the LOCAL/PUBLIC toggle — they make the host's Postgres reachable
// by the Dockerised pgAdmin over the docker0 bridge (172.17.0.0/16), which
// is only routable from local containers, never the public network. Added
// when pgAdmin installs (parity with the MariaDB/phpMyAdmin bridge bind) and
// left in place across LOCAL toggles, exactly like the MariaDB docker0 rule.
const PG_HBA_BRIDGE_MARKER: &str = "# vps-panel:docker-bridge";
const PG_HBA_BRIDGE_LINE: &str = "host    all             all             172.17.0.0/16           scram-sha-256";

/// Find the active PostgreSQL major version under `/etc/postgresql/`.
/// Debian/Ubuntu layout uses one directory per major version (e.g.
/// `/etc/postgresql/16/main/`). We pick the highest numeric directory
/// that has a `main/postgresql.conf` inside.
async fn detect_postgres_version() -> Option<String> {
    let mut rd = tokio::fs::read_dir("/etc/postgresql").await.ok()?;
    let mut versions: Vec<u32> = Vec::new();
    while let Ok(Some(entry)) = rd.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        if let Ok(v) = name.parse::<u32>() {
            // Only count it if main/postgresql.conf actually exists —
            // skips half-removed versions.
            if entry.path().join("main").join("postgresql.conf").exists() {
                versions.push(v);
            }
        }
    }
    versions.sort();
    versions.last().map(|v| v.to_string())
}

/// Restore `postgres:postgres` ownership on a Postgres config file the agent
/// just rewrote. `fs_atomic::write` creates the replacement file as the
/// agent's own identity (root), so an atomic rewrite of `pg_hba.conf` (mode
/// 0640) leaves it `root:root` - which the `postgres` user cannot read, and
/// the cluster then refuses to start with
/// `could not open pg_hba.conf: Permission denied`. `postgresql.conf` is 0644
/// so root ownership was survivable there, but we fix both for consistency.
/// Best-effort: logs a warning rather than aborting, since the file content
/// is already correct and only the owner is wrong.
#[cfg(unix)]
fn postgres_uid_gid() -> Option<(u32, u32)> {
    use nix::unistd::User;
    User::from_name("postgres")
        .ok()
        .flatten()
        .map(|u| (u.uid.as_raw(), u.gid.as_raw()))
}

#[cfg(not(unix))]
fn postgres_uid_gid() -> Option<(u32, u32)> {
    None
}

fn chown_postgres_config(task: &crate::ipc::tasks::TaskHandle, path: &str) {
    match postgres_uid_gid() {
        Some((uid, gid)) => {
            if let Err(e) =
                crate::fs_atomic::chown_blocking(std::path::Path::new(path), uid, gid)
            {
                task.log(format!("warning: could not chown {path} to postgres: {e}"));
            }
        }
        None => task.log(format!(
            "warning: user 'postgres' not found; left {path} root-owned (cluster may fail to start)"
        )),
    }
}

/// True once the local Postgres cluster is accepting connections. Polls
/// `pg_isready` (exit 0 = accepting) for a few seconds. Used to verify a
/// restart actually brought the cluster up: the Ubuntu `postgresql.service`
/// umbrella unit returns success from `systemctl restart` even when the real
/// `postgresql@<ver>-main` unit failed to start (e.g. an unreadable
/// pg_hba.conf), so the restart exit code alone can't be trusted.
async fn postgres_is_ready() -> bool {
    for attempt in 0..10 {
        if exec::run("pg_isready", ["-q"]).await.is_ok() {
            return true;
        }
        if attempt < 9 {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }
    false
}

fn upsert_listen_addresses(content: &str, value: &str) -> String {
    // Set listen_addresses to exactly ONE active line: replace the first
    // occurrence (commented or not) with our value and DROP every later
    // occurrence. Postgres honors the last active setting, so a stray second
    // line - e.g. a docker-bridge line left beside a public-bind line - would
    // otherwise silently shadow the value we just wrote. Collapsing to a
    // single line here also self-heals a file already in that split state on
    // the next toggle. If no line exists at all, append one.
    let mut found = false;
    let mut out = String::with_capacity(content.len() + 64);
    for line in content.lines() {
        let stripped = line.trim_start_matches([' ', '\t', '#']);
        let is_match = stripped
            .strip_prefix("listen_addresses")
            .map(|rest| rest.trim_start().starts_with('='))
            .unwrap_or(false);
        if is_match {
            if !found {
                out.push_str(&format!("listen_addresses = '{value}'\n"));
                found = true;
            }
            // Drop any further listen_addresses lines (active or commented).
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if !found {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&format!("listen_addresses = '{value}'\n"));
    }
    out
}

fn ensure_public_hba(content: &str) -> String {
    // Idempotent: if the marker already exists in the file, return as-is.
    // Otherwise append our marker + rule on new lines.
    if content.lines().any(|l| l.trim() == PG_HBA_MARKER) {
        return content.to_string();
    }
    let mut out = content.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(PG_HBA_MARKER);
    out.push('\n');
    out.push_str(PG_HBA_PUBLIC_LINE);
    out.push('\n');
    out
}

fn remove_public_hba(content: &str) -> String {
    // Drop the marker line + the immediately following non-empty rule
    // line. Leaves any other operator-added rules alone.
    let mut out = String::with_capacity(content.len());
    let mut skip_next = false;
    for line in content.lines() {
        if skip_next {
            skip_next = false;
            // Only skip if this looks like our rule (defensive).
            if line.trim_start().starts_with("host") {
                continue;
            }
        }
        if line.trim() == PG_HBA_MARKER {
            skip_next = true;
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn ensure_bridge_hba(content: &str) -> String {
    // Idempotent: append the marker + docker-subnet rule only if the marker
    // isn't already present. Mirrors ensure_public_hba but for the bridge.
    if content.lines().any(|l| l.trim() == PG_HBA_BRIDGE_MARKER) {
        return content.to_string();
    }
    let mut out = content.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(PG_HBA_BRIDGE_MARKER);
    out.push('\n');
    out.push_str(PG_HBA_BRIDGE_LINE);
    out.push('\n');
    out
}

/// Ensure `listen_addresses` lets the docker bridge gateway (172.17.0.1)
/// accept connections, without clobbering a wider bind. Returns the new
/// file content. No-op when the effective value is already `*` (covers
/// every interface) or already lists `172.17.0.1`; otherwise rewrites the
/// active line to `localhost,172.17.0.1` (loopback + bridge), matching the
/// MariaDB `bind-address = 127.0.0.1,172.17.0.1` shape.
fn ensure_listen_includes_bridge(content: &str) -> String {
    // Effective value = the last *active* (uncommented) listen_addresses
    // line. A commented line means the compiled-in default ('localhost'),
    // which does not cover the bridge.
    let mut active_val: Option<String> = None;
    for line in content.lines() {
        let t = line.trim_start();
        if t.starts_with('#') {
            continue;
        }
        if let Some(rest) = t.strip_prefix("listen_addresses") {
            if let Some(rest) = rest.trim_start().strip_prefix('=') {
                // Take the text up to a comment/quote terminator and strip
                // surrounding quotes: `= 'localhost'   # comment` -> localhost
                let v = rest.trim().trim_start_matches(['\'', '"']);
                let v = v.split(['\'', '"', '#']).next().unwrap_or("").trim();
                active_val = Some(v.to_string());
            }
        }
    }
    if let Some(v) = &active_val {
        if v == "*" || v.split(',').any(|p| p.trim() == "172.17.0.1") {
            return content.to_string();
        }
    }
    // Preserve whatever the operator already had (default 'localhost' when
    // the line is commented/absent) and just append the bridge gateway, so
    // a custom bind isn't flattened.
    let base = match active_val.as_deref() {
        Some(v) if !v.is_empty() => v,
        _ => "localhost",
    };
    upsert_listen_addresses(content, &format!("{base},172.17.0.1"))
}

/// True when the docker0 bridge gateway 172.17.0.1 is actually assigned to a
/// local interface. Postgres refuses to start if `listen_addresses` names an
/// IP it can't bind, so the bridge bind is gated on this — safe to call even
/// on hosts without docker. The `/` suffix avoids matching container IPs like
/// 172.17.0.10.
async fn docker_bridge_gateway_present() -> bool {
    let out = Command::new("ip")
        .args(["-o", "-4", "addr", "show"])
        .stdin(Stdio::null())
        .output()
        .await;
    match out {
        Ok(o) if o.status.success() => {
            String::from_utf8_lossy(&o.stdout).contains("172.17.0.1/")
        }
        _ => false,
    }
}

/// Postgres twin of `ensure_mariadb_docker_bridge_bind`: make the host's
/// PostgreSQL reachable by the Dockerised pgAdmin over the docker0 bridge.
/// Three idempotent layers:
///   1. `listen_addresses` includes the bridge gateway 172.17.0.1
///   2. `pg_hba.conf` trusts the docker subnet (172.17.0.0/16, scram-sha-256)
///   3. `ufw allow in on docker0 to any port 5432` — without it the host's
///      INPUT chain (default DROP under UFW) silently swallows the
///      container's SYN, surfacing in pgAdmin as "connection timeout
///      expired" with no log entry anywhere.
///
/// Restarts postgresql only when (1) or (2) actually changed on disk.
async fn ensure_postgres_docker_bridge_bind(task: &crate::ipc::tasks::TaskHandle) {
    // Gate on the bridge actually existing — binding listen_addresses to an
    // absent IP makes postgresql fail to start.
    if !docker_bridge_gateway_present().await {
        task.log(
            "docker0 bridge (172.17.0.1) not present — skipping Postgres bridge bind".to_string(),
        );
        return;
    }
    let version = match detect_postgres_version().await {
        Some(v) => v,
        None => {
            task.log(
                "pgAdmin installed but no PostgreSQL cluster found under /etc/postgresql/ — install PostgreSQL from the Databases page, then pgAdmin can reach it at host.docker.internal:5432".to_string(),
            );
            return;
        }
    };
    task.log(format!(
        "wiring PostgreSQL {version} for the docker bridge so pgAdmin can reach it"
    ));

    let pgconf_path = format!("/etc/postgresql/{version}/main/postgresql.conf");
    let hba_path = format!("/etc/postgresql/{version}/main/pg_hba.conf");
    let mut restart = false;

    // 1. listen_addresses
    match tokio::fs::read_to_string(&pgconf_path).await {
        Ok(pgconf) => {
            let new_pgconf = ensure_listen_includes_bridge(&pgconf);
            if new_pgconf != pgconf {
                task.log(format!(
                    "{pgconf_path}: listen_addresses now includes 172.17.0.1 (docker bridge)"
                ));
                if let Err(e) = crate::fs_atomic::write(
                    std::path::Path::new(&pgconf_path),
                    new_pgconf.as_bytes(),
                    0o644,
                )
                .await
                {
                    task.log(format!(
                        "failed to write {pgconf_path}: {e} — pgAdmin will not reach Postgres"
                    ));
                } else {
                    chown_postgres_config(task, &pgconf_path);
                    restart = true;
                }
            } else {
                task.log(format!(
                    "{pgconf_path}: listen_addresses already covers the bridge"
                ));
            }
        }
        Err(e) => task.log(format!("could not read {pgconf_path}: {e}")),
    }

    // 2. pg_hba.conf
    match tokio::fs::read_to_string(&hba_path).await {
        Ok(hba) => {
            let new_hba = ensure_bridge_hba(&hba);
            if new_hba != hba {
                task.log(format!(
                    "{hba_path}: trusting docker subnet 172.17.0.0/16 (scram-sha-256)"
                ));
                if let Err(e) = crate::fs_atomic::write(
                    std::path::Path::new(&hba_path),
                    new_hba.as_bytes(),
                    0o640,
                )
                .await
                {
                    task.log(format!(
                        "failed to write {hba_path}: {e} — pgAdmin auth will fail"
                    ));
                } else {
                    chown_postgres_config(task, &hba_path);
                    restart = true;
                }
            } else {
                task.log(format!("{hba_path}: docker-bridge rule already present"));
            }
        }
        Err(e) => task.log(format!("could not read {hba_path}: {e}")),
    }

    if restart {
        task.log("$ systemctl restart postgresql".to_string());
        let res = exec::run_streaming("systemctl", ["restart", "postgresql"], |l| {
            task.log(l.to_string())
        })
        .await;
        match res {
            Ok(0) => task.log("postgresql restarted; bridge bind active".to_string()),
            Ok(code) => task.log(format!("systemctl restart postgresql exited with code {code}")),
            Err(e) => task.log(format!("systemctl restart postgresql error: {e}")),
        }
    }

    // 3. UFW: let the container's SYN through the host INPUT chain. Scoped to
    //    `in on docker0` so it only matches bridge traffic, never arbitrary
    //    external traffic NAT'd onto 172.17.0.0/16. Idempotent (`ufw allow`
    //    re-run prints "Skipping adding existing rule", exit 0).
    task.log("$ ufw allow in on docker0 to any port 5432 proto tcp".to_string());
    let ufw_res = exec::run_streaming(
        "ufw",
        [
            "allow", "in", "on", "docker0", "to", "any", "port", "5432", "proto", "tcp",
        ],
        |l| task.log(l.to_string()),
    )
    .await;
    match ufw_res {
        Ok(0) => task.log("UFW: docker0 → 5432 rule in place".to_string()),
        Ok(code) => task.log(format!(
            "ufw allow exited with code {code} — pgAdmin may still time out connecting to Postgres"
        )),
        Err(e) => task.log(format!(
            "ufw allow error: {e} — pgAdmin may still time out connecting to Postgres"
        )),
    }
}

async fn set_postgres_bind(
    state: AppState,
    bind: String,
) -> AgentResult<Json<TaskAccepted>> {
    let task = state.tasks.create("quick-install-bind:postgresql");
    let task_id = task.id.clone();

    tokio::spawn(async move {
        task.start();
        let version = match detect_postgres_version().await {
            Some(v) => v,
            None => {
                task.log(
                    "could not detect PostgreSQL version under /etc/postgresql/ — is the engine actually installed?"
                        .to_string(),
                );
                task.finish(false);
                return;
            }
        };
        task.log(format!("detected PostgreSQL version {version}"));

        let pgconf_path = format!("/etc/postgresql/{version}/main/postgresql.conf");
        let hba_path = format!("/etc/postgresql/{version}/main/pg_hba.conf");
        let want_listen = if bind == "0.0.0.0" { "*" } else { "localhost" };

        // postgresql.conf
        let pgconf = match tokio::fs::read_to_string(&pgconf_path).await {
            Ok(s) => s,
            Err(e) => {
                task.log(format!("could not read {pgconf_path}: {e}"));
                task.finish(false);
                return;
            }
        };
        let new_pgconf = upsert_listen_addresses(&pgconf, want_listen);
        let pg_changed = new_pgconf != pgconf;
        if pg_changed {
            task.log(format!(
                "updating {pgconf_path}: listen_addresses = '{want_listen}'"
            ));
            if let Err(e) = crate::fs_atomic::write(
                std::path::Path::new(&pgconf_path),
                new_pgconf.as_bytes(),
                0o644,
            )
            .await
            {
                task.log(format!("failed to write {pgconf_path}: {e}"));
                task.finish(false);
                return;
            }
            chown_postgres_config(&task, &pgconf_path);
        } else {
            task.log(format!(
                "{pgconf_path} already up to date for bind={bind}"
            ));
        }

        // pg_hba.conf
        let hba = tokio::fs::read_to_string(&hba_path).await.unwrap_or_default();
        let new_hba = if bind == "0.0.0.0" {
            ensure_public_hba(&hba)
        } else {
            remove_public_hba(&hba)
        };
        let hba_changed = new_hba != hba;
        if hba_changed {
            task.log(format!(
                "updating {hba_path}: {} panel-managed host-all-0.0.0.0/0 rule",
                if bind == "0.0.0.0" { "adding" } else { "removing" }
            ));
            if let Err(e) = crate::fs_atomic::write(
                std::path::Path::new(&hba_path),
                new_hba.as_bytes(),
                0o640,
            )
            .await
            {
                task.log(format!("failed to write {hba_path}: {e}"));
                task.finish(false);
                return;
            }
            chown_postgres_config(&task, &hba_path);
        } else {
            task.log(format!("{hba_path} already up to date for bind={bind}"));
        }

        if pg_changed || hba_changed {
            task.log("$ systemctl restart postgresql".to_string());
            let res = exec::run_streaming(
                "systemctl",
                ["restart", "postgresql"],
                |l| task.log(l.to_string()),
            )
            .await;
            if !matches!(res, Ok(0)) {
                task.log(format!("systemctl restart postgresql failed: {res:?}"));
                task.finish(false);
                return;
            }

            // The restart exit code is not trustworthy on Ubuntu (the
            // postgresql.service umbrella unit reports success even when the
            // real postgresql@<ver>-main cluster failed to start). Verify the
            // cluster is genuinely accepting connections BEFORE we open the
            // firewall - otherwise a bad config (historically: an unreadable
            // pg_hba.conf) would leave 5432 exposed to the internet over a
            // dead database. On failure we stop here without touching ufw.
            if !postgres_is_ready().await {
                task.log(
                    "postgresql did not come back up after restart - check \
                     /var/log/postgresql/postgresql-*-main.log. Firewall left unchanged; \
                     refusing to expose a down database."
                        .to_string(),
                );
                task.finish(false);
                return;
            }
        }

        if bind == "0.0.0.0" {
            task.log("$ ufw allow 5432/tcp".to_string());
            let _ = exec::run_streaming("ufw", ["allow", "5432/tcp"], |l| {
                task.log(l.to_string())
            })
            .await;
        } else {
            task.log("$ ufw delete allow 5432/tcp".to_string());
            let _ = exec::run_streaming(
                "ufw",
                ["delete", "allow", "5432/tcp"],
                |l| task.log(l.to_string()),
            )
            .await;
            // Reverting to LOCAL set listen_addresses back to 'localhost',
            // which drops the docker-bridge gateway and would break a
            // Dockerised pgAdmin. Re-assert the bridge bind (idempotent, and
            // a no-op when the bridge / Postgres isn't present). The public
            // PUBLIC branch needs no restore — '*' + the 0.0.0.0/0 pg_hba
            // rule already cover the bridge subnet.
            ensure_postgres_docker_bridge_bind(&task).await;
        }

        task.log(format!("PostgreSQL bind set to {bind}"));
        task.finish(true);
    });

    Ok(Json(TaskAccepted { task_id }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listen_bridge_appends_to_commented_default() {
        // Fresh Debian postgresql.conf: the directive is commented out, so
        // the effective value is the compiled-in 'localhost'. We activate it
        // with the bridge appended.
        let input = "#listen_addresses = 'localhost'\t\t# comment\nport = 5432\n";
        let out = ensure_listen_includes_bridge(input);
        assert!(out.contains("listen_addresses = 'localhost,172.17.0.1'"));
        assert!(out.contains("port = 5432"));
    }

    #[test]
    fn listen_bridge_appends_to_active_localhost() {
        let input = "listen_addresses = 'localhost'\n";
        let out = ensure_listen_includes_bridge(input);
        assert_eq!(out, "listen_addresses = 'localhost,172.17.0.1'\n");
    }

    #[test]
    fn listen_bridge_preserves_custom_bind() {
        // An operator's explicit bind must not be flattened — we append.
        let input = "listen_addresses = '10.0.0.5'\n";
        let out = ensure_listen_includes_bridge(input);
        assert_eq!(out, "listen_addresses = '10.0.0.5,172.17.0.1'\n");
    }

    #[test]
    fn listen_bridge_noop_on_wildcard() {
        // '*' already covers every interface including the bridge.
        let input = "listen_addresses = '*'\n";
        assert_eq!(ensure_listen_includes_bridge(input), input);
    }

    #[test]
    fn listen_bridge_idempotent() {
        let input = "listen_addresses = 'localhost,172.17.0.1'\n";
        assert_eq!(ensure_listen_includes_bridge(input), input);
    }

    #[test]
    fn bridge_hba_appends_once_and_is_idempotent() {
        let base = "local all all peer\nhost all all 127.0.0.1/32 scram-sha-256\n";
        let once = ensure_bridge_hba(base);
        assert!(once.contains(PG_HBA_BRIDGE_MARKER));
        assert!(once.contains("172.17.0.0/16"));
        // Untouched existing rules survive.
        assert!(once.contains("local all all peer"));
        // Second pass is a no-op (no duplicate marker/rule).
        let twice = ensure_bridge_hba(&once);
        assert_eq!(once, twice);
        assert_eq!(twice.matches(PG_HBA_BRIDGE_MARKER).count(), 1);
    }

    #[test]
    fn bridge_hba_independent_of_public_marker() {
        // The public-bind rule and the docker-bridge rule coexist without
        // either removal helper touching the other.
        let with_public = ensure_public_hba("local all all peer\n");
        let with_both = ensure_bridge_hba(&with_public);
        assert!(with_both.contains(PG_HBA_MARKER));
        assert!(with_both.contains(PG_HBA_BRIDGE_MARKER));
        // remove_public_hba drops only the public rule, leaving the bridge.
        let after_local = remove_public_hba(&with_both);
        assert!(!after_local.contains(PG_HBA_MARKER));
        assert!(after_local.contains(PG_HBA_BRIDGE_MARKER));
    }

    #[test]
    fn upsert_listen_replaces_commented_default() {
        let input = "#listen_addresses = 'localhost'\t\t# comment\nport = 5432\n";
        let out = upsert_listen_addresses(input, "*");
        assert_eq!(out, "listen_addresses = '*'\nport = 5432\n");
    }

    #[test]
    fn upsert_listen_collapses_duplicate_lines() {
        // The exact split-brain state the bind bug produced on the live VPS:
        // a '*' line near the top and a bridge line appended at the bottom.
        // Postgres honored the LAST one ('localhost,172.17.0.1'), silently
        // shadowing the public bind. upsert must collapse to a single line.
        let input = "listen_addresses = '*'\nport = 5432\nlisten_addresses = 'localhost,172.17.0.1'\n";
        let out = upsert_listen_addresses(input, "*");
        assert_eq!(out, "listen_addresses = '*'\nport = 5432\n");
        assert_eq!(out.matches("listen_addresses").count(), 1);
    }

    #[test]
    fn upsert_listen_drops_trailing_commented_duplicate() {
        // First match (active) is replaced; a later commented example is
        // dropped too, so nothing can be uncommented later to shadow us.
        let input = "listen_addresses = 'localhost'\n#listen_addresses = '*'\n";
        let out = upsert_listen_addresses(input, "localhost");
        assert_eq!(out, "listen_addresses = 'localhost'\n");
    }

    #[test]
    fn upsert_listen_appends_when_absent() {
        let input = "port = 5432\n";
        let out = upsert_listen_addresses(input, "*");
        assert_eq!(out, "port = 5432\nlisten_addresses = '*'\n");
    }
}
