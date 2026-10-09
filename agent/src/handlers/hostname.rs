//! The server's own hostname (what `hostname` prints, the shell prompt, log
//! lines). Separate from PANEL_HOST, the public web and mail name. Setting it
//! is hostnamectl plus the two things a bare `hostnamectl set-hostname` leaves
//! out: the 127.0.1.1 line in /etc/hosts, and a cloud-init drop-in, because
//! Ubuntu's cloud.cfg says `preserve_hostname: false` and cloud-init puts the
//! provider's name back at every boot.

use crate::error::{AgentError, AgentResult};
use crate::exec;
use crate::fs_atomic;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio::fs;

struct Paths {
    hosts: String,
    /// cloud-init's directory; the drop-in is written only when it exists.
    cloud: String,
    /// Sorts after every NN_*.cfg in cloud.cfg.d, so its values win.
    cloud_dropin: String,
}

impl Paths {
    fn system() -> Paths {
        Paths {
            hosts: "/etc/hosts".into(),
            cloud: "/etc/cloud".into(),
            cloud_dropin: "/etc/cloud/cloud.cfg.d/zz-vps-panel-hostname.cfg".into(),
        }
    }
}

#[derive(Serialize)]
pub struct HostnameStatus {
    /// The static hostname (/etc/hostname): the name after a reboot.
    pub hostname: String,
    /// The running kernel's hostname.
    pub running: String,
    /// cloud-init is installed, so it resets the hostname at boot unless told not to.
    pub cloud_init: bool,
    /// The panel's cloud-init drop-in is in place.
    pub preserved: bool,
}

#[derive(Deserialize)]
pub struct SetHostnameReq {
    pub hostname: String,
}

#[derive(Serialize)]
pub struct SetHostnameResp {
    pub hostname: String,
    /// What was changed, one line per step.
    pub steps: Vec<String>,
}

pub async fn get() -> AgentResult<Json<HostnameStatus>> {
    let p = Paths::system();
    let running = first_line("/proc/sys/kernel/hostname").await.unwrap_or_default();
    let hostname = first_line("/etc/hostname")
        .await
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| running.clone());
    Ok(Json(HostnameStatus {
        hostname,
        running,
        cloud_init: is_dir(&p.cloud).await,
        preserved: fs::try_exists(&p.cloud_dropin).await.unwrap_or(false),
    }))
}

pub async fn set(Json(req): Json<SetHostnameReq>) -> AgentResult<Json<SetHostnameResp>> {
    let hostname = normalize(&req.hostname).map_err(AgentError::Invalid)?;
    let steps = apply(&Paths::system(), &hostname).await?;
    Ok(Json(SetHostnameResp { hostname, steps }))
}

async fn apply(p: &Paths, name: &str) -> AgentResult<Vec<String>> {
    let mut steps = Vec::new();
    // Static and running hostname in one go.
    exec::run("hostnamectl", ["set-hostname", name]).await?;
    steps.push(format!("hostnamectl set-hostname {name}"));

    let hosts = match fs::read_to_string(&p.hosts).await {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(AgentError::Io(e)),
    };
    let next = hosts_with(&hosts, name);
    if next != hosts {
        fs_atomic::write(Path::new(&p.hosts), next.as_bytes(), 0o644).await?;
        steps.push(format!("{}: {}", p.hosts, hosts_line(name).trim_end()));
    }

    if is_dir(&p.cloud).await {
        let conf = cloud_init_conf(name);
        if fs::read_to_string(&p.cloud_dropin).await.ok().as_deref() != Some(conf.as_str()) {
            fs_atomic::write(Path::new(&p.cloud_dropin), conf.as_bytes(), 0o644).await?;
            steps.push(format!("{}: preserve_hostname: true", p.cloud_dropin));
        }
    }
    Ok(steps)
}

async fn first_line(path: &str) -> Option<String> {
    let s = fs::read_to_string(path).await.ok()?;
    Some(s.lines().next().unwrap_or("").trim().to_string())
}

async fn is_dir(path: &str) -> bool {
    fs::metadata(path).await.map(|m| m.is_dir()).unwrap_or(false)
}

/// Lowercase and without a trailing dot. RFC 1123 labels, at most 64
/// characters (the kernel's limit), and not an IP address.
fn normalize(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    let name = trimmed.strip_suffix('.').unwrap_or(trimmed).to_ascii_lowercase();
    if name.is_empty() || name.len() > 64 {
        return Err("a hostname is 1-64 characters".into());
    }
    if name.parse::<std::net::IpAddr>().is_ok() {
        return Err("a hostname cannot be an IP address".into());
    }
    if name == "localhost" || name.starts_with("localhost.") {
        return Err("pick a real name, not localhost".into());
    }
    let label_ok = |l: &str| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    };
    if !name.split('.').all(label_ok) {
        return Err(
            "use letters, digits and hyphens, with dots between the parts (vps.example.com)".into(),
        );
    }
    Ok(name)
}

/// `127.0.1.1 <fqdn> <short>`: Debian's convention, and what cloud-init's
/// template writes. A name without dots is listed once.
fn hosts_line(name: &str) -> String {
    match name.split_once('.') {
        Some((short, _)) => format!("127.0.1.1 {name} {short}\n"),
        None => format!("127.0.1.1 {name}\n"),
    }
}

/// `hosts` with a 127.0.1.1 line naming this server. The first existing one
/// is replaced where it is; further ones may be aliases the operator added,
/// so they stay. Without one, it goes after the 127.0.0.1 line, or at the
/// end. Every other line is kept byte for byte.
fn hosts_with(hosts: &str, name: &str) -> String {
    let ours = hosts_line(name);
    let lines: Vec<&str> = hosts.split_inclusive('\n').collect();
    let is = |line: &str, addr: &str| {
        let t = line.trim_start();
        !t.starts_with('#') && t.split_whitespace().next() == Some(addr)
    };
    let at = lines
        .iter()
        .position(|l| is(l, "127.0.1.1"))
        .or_else(|| lines.iter().position(|l| is(l, "127.0.0.1")).map(|i| i + 1))
        .unwrap_or(lines.len());
    let mut out = String::with_capacity(hosts.len() + ours.len());
    let push_ours = |out: &mut String| {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&ours);
    };
    let replaced = lines.get(at).is_some_and(|l| is(l, "127.0.1.1"));
    for (i, line) in lines.iter().enumerate() {
        if i == at {
            push_ours(&mut out);
            if replaced {
                continue;
            }
        }
        out.push_str(line);
    }
    if at == lines.len() {
        push_ours(&mut out);
    }
    out
}

/// Keeps cloud-init from putting the provider's hostname back at boot. When
/// the provider has it manage /etc/hosts (`manage_etc_hosts`), it takes the
/// names for the 127.0.1.1 line from `hostname` and `fqdn`.
fn cloud_init_conf(name: &str) -> String {
    let short = name.split('.').next().unwrap_or(name);
    format!(
        "# Managed by vps-panel (Settings > Server > Server hostname).\n\
         preserve_hostname: true\n\
         hostname: \"{short}\"\n\
         fqdn: \"{name}\"\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_and_refuses_what_is_not_a_hostname() {
        assert_eq!(normalize(" VPS.Example.Org. ").unwrap(), "vps.example.org");
        assert_eq!(normalize("u1").unwrap(), "u1");
        assert_eq!(normalize("web-01.example.com").unwrap(), "web-01.example.com");
        let too_long = "a".repeat(65);
        let long_label = "a".repeat(64);
        for bad in [
            "",
            ".",
            "-vps.example.com",
            "vps-.example.com",
            "vps..example.com",
            "under_score",
            "space name",
            "1.2.3.4",
            "::1",
            "localhost",
            "localhost.localdomain",
            too_long.as_str(),
            long_label.as_str(),
        ] {
            assert!(normalize(bad).is_err(), "{bad:?} was accepted");
        }
        assert!(normalize(&"a".repeat(63)).is_ok());
    }

    #[test]
    fn replaces_the_127_0_1_1_line_where_it_is() {
        // Ubuntu cloud image (cloud-init template) and WSL layouts.
        let cloud = "# Your system has configured 'manage_etc_hosts' as True.\n\
                     127.0.1.1 u1.local u1\n\
                     127.0.0.1 localhost\n\
                     \n\
                     ::1 localhost ip6-localhost ip6-loopback\n";
        assert_eq!(
            hosts_with(cloud, "vps.example.org"),
            "# Your system has configured 'manage_etc_hosts' as True.\n\
             127.0.1.1 vps.example.org vps\n\
             127.0.0.1 localhost\n\
             \n\
             ::1 localhost ip6-localhost ip6-loopback\n"
        );
        // Only the first one is ours; a later 127.0.1.1 alias stays.
        let wsl = "127.0.0.1\tlocalhost\n127.0.1.1\tLAPTOP.localdomain\tLAPTOP\n# 127.0.1.1 old\n127.0.1.1 myapp.local\n";
        let with = hosts_with(wsl, "vps");
        assert_eq!(
            with,
            "127.0.0.1\tlocalhost\n127.0.1.1 vps\n# 127.0.1.1 old\n127.0.1.1 myapp.local\n"
        );
        assert_eq!(hosts_with(&with, "vps"), with);
    }

    #[test]
    fn adds_the_line_after_localhost_or_at_the_end() {
        let minimal = "127.0.0.1 localhost\n::1 ip6-localhost ip6-loopback\n";
        let with = hosts_with(minimal, "vps.example.com");
        assert_eq!(
            with,
            "127.0.0.1 localhost\n127.0.1.1 vps.example.com vps\n::1 ip6-localhost ip6-loopback\n"
        );
        assert_eq!(hosts_with(&with, "vps.example.com"), with);
        assert_eq!(
            hosts_with("127.0.0.1 localhost", "vps"),
            "127.0.0.1 localhost\n127.0.1.1 vps\n"
        );
        assert_eq!(hosts_with("::1 localhost\n", "vps"), "::1 localhost\n127.0.1.1 vps\n");
        assert_eq!(hosts_with("", "vps"), "127.0.1.1 vps\n");
    }

    #[test]
    fn cloud_init_keeps_the_name() {
        assert_eq!(
            cloud_init_conf("vps.example.org"),
            "# Managed by vps-panel (Settings > Server > Server hostname).\n\
             preserve_hostname: true\n\
             hostname: \"vps\"\n\
             fqdn: \"vps.example.org\"\n"
        );
        assert!(cloud_init_conf("u1").contains("hostname: \"u1\"\nfqdn: \"u1\"\n"));
    }

    /// Puts the machine's hostname, /etc/hosts and cloud-init back even when
    /// an assert fails.
    struct Restore {
        hostname_file: String,
        running: String,
        hosts: String,
        machine_info: Option<String>,
        dropin_existed: bool,
    }

    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = std::fs::write("/etc/hostname", &self.hostname_file);
            let _ = std::fs::write("/etc/hosts", &self.hosts);
            match &self.machine_info {
                Some(s) => {
                    let _ = std::fs::write("/etc/machine-info", s);
                }
                None => {
                    let _ = std::fs::remove_file("/etc/machine-info");
                }
            }
            if !self.dropin_existed {
                let _ = std::fs::remove_file(Paths::system().cloud_dropin);
            }
            let _ = std::process::Command::new("hostname").arg(&self.running).status();
        }
    }

    #[tokio::test]
    #[ignore = "root only: renames this machine for a moment, then puts everything back (cargo test -- --ignored)"]
    async fn renames_the_machine_and_puts_it_back() {
        let p = Paths::system();
        let _restore = Restore {
            hostname_file: std::fs::read_to_string("/etc/hostname").unwrap(),
            running: first_line("/proc/sys/kernel/hostname").await.unwrap(),
            hosts: std::fs::read_to_string("/etc/hosts").unwrap(),
            machine_info: std::fs::read_to_string("/etc/machine-info").ok(),
            dropin_existed: Path::new(&p.cloud_dropin).exists(),
        };
        let name = "vps-panel-test.example.test";
        let steps = apply(&p, name).await.unwrap();
        println!("{}", steps.join("\n"));

        assert_eq!(std::fs::read_to_string("/etc/hostname").unwrap().trim(), name);
        assert_eq!(first_line("/proc/sys/kernel/hostname").await.unwrap(), name);
        let hosts = std::fs::read_to_string("/etc/hosts").unwrap();
        assert_eq!(hosts.matches("127.0.1.1").count(), 1, "{hosts}");
        assert!(hosts.contains("127.0.1.1 vps-panel-test.example.test vps-panel-test\n"));
        let fqdn = std::process::Command::new("hostname").arg("-f").output().unwrap();
        assert_eq!(String::from_utf8_lossy(&fqdn.stdout).trim(), name);
        if is_dir(&p.cloud).await {
            let conf = std::fs::read_to_string(&p.cloud_dropin).unwrap();
            assert!(conf.contains("preserve_hostname: true"), "{conf}");
        }
        // Saving the same name again changes no file.
        let again = apply(&p, name).await.unwrap();
        assert_eq!(again.len(), 1, "{again:?}");
    }
}
