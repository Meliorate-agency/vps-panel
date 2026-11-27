//! What a server snapshot holds, as a fixed list of sections, and the walk
//! over them that both the size estimate and the archive use.
//!
//! The list is code, not input: no request can add a path. The import
//! decides separately what it puts back where; anything here that holds a
//! per-install secret or this machine's identity is either left out
//! (`.env`, the agent token, the restic key, SSH host keys, Tailscale's node
//! state) or carried only for reference (the Postfix / Dovecot / PowerDNS
//! configuration).

use std::collections::HashSet;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// Which direct children of a root are taken.
#[derive(Clone, Debug)]
pub enum ChildFilter {
    All,
    /// Children with one of these names.
    Only(Vec<String>),
    /// Every child but these.
    Except(Vec<String>),
    /// Children whose name starts with one of these.
    Prefix(Vec<String>),
    /// Children whose name contains this.
    Contains(String),
}

impl ChildFilter {
    fn keeps(&self, name: &str) -> bool {
        match self {
            ChildFilter::All => true,
            ChildFilter::Only(names) => names.iter().any(|n| n == name),
            ChildFilter::Except(names) => !names.iter().any(|n| n == name),
            ChildFilter::Prefix(prefixes) => prefixes.iter().any(|p| name.starts_with(p.as_str())),
            ChildFilter::Contains(part) => name.contains(part.as_str()),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Root {
    pub path: PathBuf,
    pub children: ChildFilter,
}

impl Root {
    fn all(path: &str) -> Root {
        Root { path: PathBuf::from(path), children: ChildFilter::All }
    }

    fn with(path: &str, children: ChildFilter) -> Root {
        Root { path: PathBuf::from(path), children }
    }
}

#[derive(Clone, Debug)]
pub struct Section {
    pub id: &'static str,
    pub title: &'static str,
    pub roots: Vec<Root>,
}

/// What is true of this server, gathered before planning (so planning
/// itself touches nothing and can be tested).
#[derive(Clone, Debug, Default)]
pub struct Facts {
    /// Home folders of the accounts made on the /users page.
    pub home_dirs: Vec<PathBuf>,
    /// `_data` folders of the Docker volumes, by volume name.
    pub docker_volumes: Vec<(String, PathBuf)>,
    /// Redis keeps its dump here when it is installed.
    pub redis_dir: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PlanOptions {
    pub include_drive: bool,
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

pub fn plan_sections(facts: &Facts, opts: &PlanOptions) -> Vec<Section> {
    let mut sections = vec![
        Section {
            id: "config",
            title: "Panel and service settings",
            roots: vec![
                // Certificates, app env files, the ACME account. Never the
                // per-install secrets: the importing panel keeps its own.
                Root::with(
                    "/etc/vps-panel",
                    ChildFilter::Except(names(&[".env", "agent.token", "backup.key"])),
                ),
                Root::with(
                    "/etc/nginx",
                    ChildFilter::Only(names(&[
                        "sites-available",
                        "sites-enabled",
                        "vps-redirects",
                        "vps-panel-access.conf",
                    ])),
                ),
                // The apps' units and their drop-ins, MariaDB's Docker
                // bridge drop-in; not the provider's own units.
                Root::with(
                    "/etc/systemd/system",
                    ChildFilter::Prefix(names(&["vps-app-", "mariadb.service.d"])),
                ),
                Root::all("/etc/postfix"),
                Root::all("/etc/dovecot"),
                Root::all("/etc/rspamd"),
                Root::all("/etc/powerdns"),
                Root::all("/var/lib/rspamd/dkim"),
                Root::with("/etc/cron.d", ChildFilter::Prefix(names(&["vps-panel-"]))),
                Root::with("/etc/fail2ban/jail.d", ChildFilter::Contains("vps-panel".into())),
                Root::with("/etc/ufw", ChildFilter::Only(names(&["user.rules", "user6.rules"]))),
                Root::with("/etc/ssh/sshd_config.d", ChildFilter::Only(names(&["00-vps-panel.conf"]))),
                Root::with("/etc/sysctl.d", ChildFilter::Prefix(names(&["99-vps-panel"]))),
                Root::with(
                    "/etc/mysql/mariadb.conf.d",
                    ChildFilter::Only(names(&["99-vps-panel-docker.cnf"])),
                ),
                // Deploy keys, Host aliases, known_hosts, authorized_keys.
                Root::all("/root/.ssh"),
            ],
        },
        Section { id: "certs", title: "Let's Encrypt (certbot)", roots: vec![Root::all("/etc/letsencrypt")] },
        Section { id: "mail", title: "Mailboxes", roots: vec![Root::all("/var/vmail")] },
        Section {
            id: "apps",
            title: "Apps",
            roots: vec![Root::all("/srv/apps"), Root::all("/var/lib/vps-panel-git")],
        },
        Section {
            id: "web",
            title: "Website folders",
            roots: vec![Root::with("/var/www", ChildFilter::Except(names(&["vps-panel-acme"])))],
        },
    ];

    if !facts.home_dirs.is_empty() {
        sections.push(Section {
            id: "homes",
            title: "Home folders of /users accounts",
            roots: facts.home_dirs.iter().map(|p| Root { path: p.clone(), children: ChildFilter::All }).collect(),
        });
    }

    let mut services: Vec<Root> = facts
        .docker_volumes
        .iter()
        .map(|(_, data)| Root { path: data.clone(), children: ChildFilter::All })
        .collect();
    if let Some(redis) = &facts.redis_dir {
        services.push(Root { path: redis.clone(), children: ChildFilter::All });
    }
    if !services.is_empty() {
        sections.push(Section { id: "docker", title: "Docker volumes and Redis", roots: services });
    }

    if opts.include_drive {
        sections.push(Section {
            id: "drive",
            title: "Drive",
            roots: vec![Root::with("/srv/drive", ChildFilter::Except(names(&["restores"])))],
        });
    }
    sections
}

/// Left out wherever they are found: rebuildable caches, and the panel's
/// own dumps, backups and snapshots (a snapshot inside a snapshot).
#[derive(Clone, Debug)]
pub struct Excludes {
    paths: Vec<PathBuf>,
    /// Directories whose path ends with one of these (`.next/cache`).
    dir_suffixes: Vec<String>,
}

impl Default for Excludes {
    fn default() -> Self {
        Excludes {
            paths: [
                super::format::SNAPSHOT_DIR,
                "/var/backups/vps-panel-db-dumps",
                "/var/backups/vps-panel",
                "/var/lib/vps-panel",
                "/srv/drive/restores",
            ]
            .iter()
            .map(PathBuf::from)
            .collect(),
            dir_suffixes: vec!["/.next/cache".to_string()],
        }
    }
}

impl Excludes {
    pub fn with_paths(mut self, extra: impl IntoIterator<Item = PathBuf>) -> Self {
        self.paths.extend(extra);
        self
    }

    fn excluded(&self, path: &Path, is_dir: bool) -> bool {
        if self.paths.iter().any(|p| path == p || path.starts_with(p)) {
            return true;
        }
        if is_dir {
            let s = path.to_string_lossy();
            return self.dir_suffixes.iter().any(|suffix| s.ends_with(suffix.as_str()));
        }
        false
    }
}

/// What the walk found at one path.
pub enum Found<'a> {
    Dir(&'a Path, &'a std::fs::Metadata),
    File(&'a Path, &'a std::fs::Metadata),
    Symlink(&'a Path, &'a std::fs::Metadata),
    /// A socket, fifo or device: never copied.
    Special(&'a Path),
    /// A folder on another filesystem below the root: not descended into.
    OtherFilesystem(&'a Path),
    /// Could not be read (the error says why).
    Unreadable(&'a Path, std::io::Error),
}

/// Walks one root depth first in name order, so a database file comes
/// before its `-wal` / `-shm` siblings. The visitor returns false to stop.
/// A root that does not exist is no error: not every server has every
/// service.
pub fn walk(root: &Root, excludes: &Excludes, visit: &mut dyn FnMut(Found) -> bool) {
    let Ok(meta) = std::fs::symlink_metadata(&root.path) else { return };
    if excludes.excluded(&root.path, meta.is_dir()) {
        return;
    }
    if !meta.is_dir() {
        visit_one(&root.path, &meta, visit);
        return;
    }
    let dev = meta.dev();
    if !visit(Found::Dir(&root.path, &meta)) {
        return;
    }
    let mut first_level = true;
    let mut stack: Vec<PathBuf> = vec![root.path.clone()];
    // Directories are expanded in the order they were pushed; children are
    // pushed in reverse name order so they pop in name order.
    let mut seen: HashSet<PathBuf> = HashSet::new();
    while let Some(dir) = stack.pop() {
        if !seen.insert(dir.clone()) {
            continue;
        }
        let mut children: Vec<(std::ffi::OsString, PathBuf)> = match std::fs::read_dir(&dir) {
            Ok(rd) => rd.filter_map(|e| e.ok()).map(|e| (e.file_name(), e.path())).collect(),
            Err(e) => {
                if !visit(Found::Unreadable(&dir, e)) {
                    return;
                }
                continue;
            }
        };
        children.sort_by(|a, b| a.0.cmp(&b.0));
        let filter_here = first_level && dir == root.path;
        first_level = false;
        let mut subdirs: Vec<PathBuf> = Vec::new();
        for (name, path) in children {
            if filter_here && !root.children.keeps(&name.to_string_lossy()) {
                continue;
            }
            let meta = match std::fs::symlink_metadata(&path) {
                Ok(m) => m,
                // Gone between the listing and the look: nothing to copy.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    if !visit(Found::Unreadable(&path, e)) {
                        return;
                    }
                    continue;
                }
            };
            if excludes.excluded(&path, meta.is_dir()) {
                continue;
            }
            if meta.is_dir() {
                if meta.dev() != dev {
                    if !visit(Found::OtherFilesystem(&path)) {
                        return;
                    }
                    continue;
                }
                if !visit(Found::Dir(&path, &meta)) {
                    return;
                }
                subdirs.push(path);
            } else if !visit_one(&path, &meta, visit) {
                return;
            }
        }
        // Files of a folder first, then its subfolders in name order.
        for sub in subdirs.into_iter().rev() {
            stack.push(sub);
        }
    }
}

fn visit_one(path: &Path, meta: &std::fs::Metadata, visit: &mut dyn FnMut(Found) -> bool) -> bool {
    let ft = meta.file_type();
    if ft.is_symlink() {
        visit(Found::Symlink(path, meta))
    } else if ft.is_file() {
        visit(Found::File(path, meta))
    } else {
        visit(Found::Special(path))
    }
}

/// Apparent size and file count of one section.
pub fn estimate(section: &Section, excludes: &Excludes) -> (u64, u64) {
    let mut bytes = 0u64;
    let mut files = 0u64;
    for root in &section.roots {
        walk(root, excludes, &mut |found| {
            match found {
                Found::File(_, m) => {
                    bytes += m.len();
                    files += 1;
                }
                Found::Dir(..) | Found::Symlink(..) => files += 1,
                _ => {}
            }
            true
        });
    }
    (bytes, files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(sections: &[Section]) -> Vec<&'static str> {
        sections.iter().map(|s| s.id).collect()
    }

    #[test]
    fn drive_only_when_asked() {
        let facts = Facts::default();
        assert!(!ids(&plan_sections(&facts, &PlanOptions { include_drive: false })).contains(&"drive"));
        assert!(ids(&plan_sections(&facts, &PlanOptions { include_drive: true })).contains(&"drive"));
    }

    #[test]
    fn secrets_and_machine_identity_are_never_planned() {
        let sections = plan_sections(
            &Facts { home_dirs: vec![PathBuf::from("/home/ann")], ..Default::default() },
            &PlanOptions { include_drive: true },
        );
        let roots: Vec<&Root> = sections.iter().flat_map(|s| s.roots.iter()).collect();
        for r in &roots {
            let p = r.path.to_string_lossy();
            assert!(!p.starts_with("/var/lib/tailscale"), "{p}");
            assert!(!p.starts_with("/etc/ssh/ssh_host"), "{p}");
            assert!(p != "/etc/ssh", "{p}");
            assert!(p != "/etc/shadow", "{p}");
        }
        let vps = roots.iter().find(|r| r.path == Path::new("/etc/vps-panel")).unwrap();
        for secret in [".env", "agent.token", "backup.key"] {
            assert!(!vps.children.keeps(secret), "{secret}");
        }
        assert!(vps.children.keeps("certs"));
        let units = roots.iter().find(|r| r.path == Path::new("/etc/systemd/system")).unwrap();
        assert!(units.children.keeps("vps-app-shop.service"));
        assert!(units.children.keeps("vps-app-shop.service.d"));
        assert!(!units.children.keeps("ssh.service"));
        assert!(!units.children.keeps("multi-user.target.wants"));
    }

    #[test]
    fn walk_follows_filters_excludes_and_order() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("apps");
        std::fs::create_dir_all(root.join("shop/.next/cache")).unwrap();
        std::fs::create_dir_all(root.join("shop/node_modules/x")).unwrap();
        std::fs::write(root.join("shop/.next/cache/big"), b"cache").unwrap();
        std::fs::write(root.join("shop/node_modules/x/index.js"), b"js").unwrap();
        std::fs::write(root.join("shop/app.db"), b"db").unwrap();
        std::fs::write(root.join("shop/app.db-wal"), b"w").unwrap();
        std::os::unix::fs::symlink("app.db", root.join("shop/link")).unwrap();
        std::fs::create_dir_all(root.join("skipme")).unwrap();
        std::fs::write(root.join("skipme/f"), b"x").unwrap();

        let r = Root { path: root.clone(), children: ChildFilter::Except(vec!["skipme".into()]) };
        let mut seen: Vec<String> = Vec::new();
        walk(&r, &Excludes::default(), &mut |found| {
            let (kind, p) = match found {
                Found::Dir(p, _) => ("d", p),
                Found::File(p, _) => ("f", p),
                Found::Symlink(p, _) => ("l", p),
                _ => return true,
            };
            seen.push(format!("{kind} {}", p.strip_prefix(&root).unwrap().display()));
            true
        });
        let pos = |s: &str| seen.iter().position(|x| x == s).unwrap_or_else(|| panic!("{s} in {seen:?}"));
        assert!(!seen.iter().any(|s| s.contains("skipme")), "{seen:?}");
        assert!(!seen.iter().any(|s| s.contains(".next/cache")), "{seen:?}");
        assert!(seen.contains(&"f shop/node_modules/x/index.js".to_string()), "{seen:?}");
        assert!(pos("f shop/app.db") < pos("f shop/app.db-wal"));
        assert!(seen.contains(&"l shop/link".to_string()));

        let (bytes, _) = estimate(
            &Section { id: "t", title: "t", roots: vec![r] },
            &Excludes::default(),
        );
        assert_eq!(bytes, 2 + 2 + 1); // index.js, app.db, app.db-wal
    }
}
