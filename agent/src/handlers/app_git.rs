//! git for panel apps, without running anything from an app's checkout as
//! root.
//!
//! Root only runs git in a bare mirror it owns (`MIRROR_ROOT/<app>.git`),
//! fetching with its own credentials (the deploy keys in /root/.ssh).
//! Everything inside a checkout (fetch from the mirror, pull, clone, status)
//! runs as the checkout's owner. Root used to run git in apps-owned
//! checkouts with `-c safe.directory=*`, so a hook or `core.fsmonitor` an app
//! (or a compromised dependency) wrote into its own .git ran as root on the
//! next pull or update check.

use crate::error::{AgentError, AgentResult};
use crate::exec;
use crate::fs_atomic;
use crate::ipc::tasks::TaskHandle;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

pub const MIRROR_ROOT: &str = "/var/lib/vps-panel-git";

pub fn mirror_path(app: &str) -> String {
    format!("{MIRROR_ROOT}/{app}.git")
}

/// The user a checkout belongs to. None: root owns it, git runs as root.
#[derive(Clone, Debug)]
pub struct Owner {
    pub name: String,
    pub home: String,
    pub gid: u32,
}

pub fn owner_of(dir: &Path) -> AgentResult<Option<Owner>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::symlink_metadata(dir.join(".git"))
            .or_else(|_| std::fs::symlink_metadata(dir))
            .map_err(AgentError::Io)?;
        owner_for_uid(meta.uid())
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(None)
    }
}

pub fn owner_for_uid(uid: u32) -> AgentResult<Option<Owner>> {
    if uid == 0 {
        return Ok(None);
    }
    #[cfg(unix)]
    {
        let user = nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid))
            .map_err(|e| AgentError::Other(anyhow::anyhow!("getpwuid({uid}): {e}")))?
            .ok_or_else(|| AgentError::Invalid(format!("no user has uid {uid}")))?;
        Ok(Some(Owner {
            name: user.name,
            home: user.dir.display().to_string(),
            gid: user.gid.as_raw(),
        }))
    }
    #[cfg(not(unix))]
    {
        Ok(None)
    }
}

/// `git <args>` (in `dir`) as `owner`. `mirror` goes into safe.directory:
/// it is root's repository, which git otherwise refuses to read for another
/// user.
pub fn owner_git_argv(
    owner: Option<&Owner>,
    dir: Option<&str>,
    mirror: Option<&str>,
    args: &[&str],
) -> (String, Vec<String>) {
    let mut git: Vec<String> = Vec::new();
    if let Some(m) = mirror {
        git.push("-c".into());
        git.push(format!("safe.directory={m}"));
    }
    if let Some(d) = dir {
        git.push("-C".into());
        git.push(d.into());
    }
    git.extend(args.iter().map(|a| a.to_string()));
    match owner {
        None => ("git".into(), git),
        Some(o) => {
            let mut argv: Vec<String> = vec![
                "-u".into(),
                o.name.clone(),
                "--".into(),
                "env".into(),
                format!("HOME={}", o.home),
                "GIT_TERMINAL_PROMPT=0".into(),
                "git".into(),
            ];
            argv.extend(git);
            ("runuser".into(), argv)
        }
    }
}

pub async fn owner_git(
    owner: Option<&Owner>,
    dir: Option<&str>,
    mirror: Option<&str>,
    args: &[&str],
) -> AgentResult<exec::ExecOutput> {
    let (cmd, argv) = owner_git_argv(owner, dir, mirror, args);
    exec::run(&cmd, &argv).await
}

/// Like owner_git, with the output in the task log.
pub async fn owner_git_logged(
    owner: Option<&Owner>,
    dir: Option<&str>,
    mirror: Option<&str>,
    args: &[&str],
    task: &TaskHandle,
) -> AgentResult<()> {
    let (cmd, argv) = owner_git_argv(owner, dir, mirror, args);
    logged(&cmd, &argv, task).await
}

async fn logged(cmd: &str, argv: &[String], task: &TaskHandle) -> AgentResult<()> {
    let code = exec::run_streaming(cmd, argv, |l| task.log(l.to_string())).await?;
    if code != 0 {
        return Err(AgentError::Subprocess {
            cmd: cmd.to_string(),
            code,
            stderr: format!("{cmd} exited with code {code} (see task log above)"),
        });
    }
    Ok(())
}

/// One mirror operation per app at a time (a deploy and an update check can
/// overlap).
fn mirror_lock(app: &str) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    let mut map = LOCKS.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap();
    map.entry(app.to_string()).or_default().clone()
}

/// Fetch `url` into the app's mirror as root, creating it on first use,
/// readable by `group` (the checkout owner's group) and nobody else.
/// Returns the mirror path.
pub async fn update_mirror(
    app: &str,
    url: &str,
    group: Option<u32>,
    task: Option<&TaskHandle>,
) -> AgentResult<String> {
    let lock = mirror_lock(app);
    let _guard = lock.lock().await;
    let mirror = mirror_path(app);
    // Traversable, not listable: an owner reaches its own mirror by name.
    fs_atomic::ensure_dir(Path::new(MIRROR_ROOT), 0o711).await?;
    remove_leftovers(app).await;

    if !Path::new(&mirror).join("HEAD").exists() {
        let _ = tokio::fs::remove_dir_all(&mirror).await;
        let tmp = format!("{mirror}.tmp-{}", uuid::Uuid::new_v4().simple());
        if let Some(t) = task {
            t.log(format!("mirroring {url}"));
        }
        let made = async {
            // 0640 files / group-readable directories (git adds g+s), so the
            // owner's group can read what root fetches later too.
            root_git(&["clone", "--bare", "--config", "core.sharedRepository=0640", url, &tmp], task)
                .await?;
            exec::run(
                "git",
                ["--git-dir", &tmp, "config", "remote.origin.fetch", "+refs/heads/*:refs/heads/*"],
            )
            .await?;
            match group {
                Some(gid) => share_with_group(&tmp, gid).await?,
                None => fs_atomic::chmod(Path::new(&tmp), 0o2750).await?,
            }
            tokio::fs::rename(&tmp, &mirror).await.map_err(AgentError::Io)
        }
        .await;
        if let Err(e) = made {
            let _ = tokio::fs::remove_dir_all(&tmp).await;
            return Err(access_hint(e));
        }
    } else {
        // The Repository URL may have changed on the app page.
        let current = exec::run("git", ["--git-dir", &mirror, "remote", "get-url", "origin"])
            .await
            .map(|o| o.stdout.trim().to_string())
            .ok();
        if current.as_deref() != Some(url) {
            exec::run("git", ["--git-dir", &mirror, "remote", "set-url", "origin", url]).await?;
        }
        root_git(&["--git-dir", &mirror, "fetch", "--prune", "origin"], task)
            .await
            .map_err(access_hint)?;
        // Mirrors made before share_with_group covered every directory are
        // repaired here, the first time they are used.
        if let Some(gid) = group {
            if !shared_with(&mirror, gid).await {
                if let Some(t) = task {
                    t.log(format!("making {mirror} readable for the checkout's owner again"));
                }
                share_with_group(&mirror, gid).await?;
            }
        }
    }
    Ok(mirror)
}

/// Say what a refusing remote usually means. GitHub answers "Repository not
/// found" for a private repository it will not admit exists to whoever is
/// asking, so the URL looks wrong when the missing piece is this server's
/// access to it.
fn access_hint(e: AgentError) -> AgentError {
    let text = e.to_string();
    let low = text.to_lowercase();
    let hint = if low.contains("repository not found") || low.contains("does not appear to be a git repository") {
        "the repository is not there under that URL, or it is private and this server cannot read it. \
         Check the Repository URL on the app's page; for a private repository add this server's public key \
         (SSH keys in the panel, or /root/.ssh/*.pub) to it as a deploy key, and use the SSH form of the URL \
         (git@github.com:owner/repo.git)."
            .to_string()
    } else if low.contains("permission denied (publickey)") {
        "this server's SSH key is not authorized for that repository: add its public key (SSH keys in the \
         panel) as a deploy key on the repository, or as a key on the account that owns it."
            .to_string()
    } else if low.contains("authentication failed") {
        "the remote refused these credentials. For a private repository over https a token in the URL expires; \
         the SSH form (git@host:owner/repo.git) with a deploy key does not."
            .to_string()
    } else if let Some(host) = unresolved_host(&text) {
        if host.contains('.') {
            format!(
                "'{host}' did not resolve. Check the host in the app's Repository URL, and that this \
                 server can reach DNS."
            )
        } else {
            // A name with no dot in `git@<name>:owner/repo` is an SSH Host
            // alias, and ssh found no block for it. Generate keypair writes
            // that block into the chosen account's ~/.ssh/config, while app
            // repositories are always fetched as root - an alias made for
            // another account (apps, a deploy user) is invisible here.
            format!(
                "'{host}' is not a host name, so it is an SSH Host alias - and ssh found no block for it. \
                 App repositories are fetched as root, so the alias has to be in /root/.ssh/config: on the \
                 SSH keys page generate the keypair for the user root, name it '{host}', and add its public \
                 key to the repository as a deploy key. A key generated for another account (apps) is not \
                 used here."
            )
        }
    } else {
        return e;
    };
    AgentError::Invalid(format!("{text}\n\nWhat this usually means: {hint}"))
}

/// The name ssh could not resolve, out of `ssh: Could not resolve hostname
/// <name>: Temporary failure in name resolution`.
fn unresolved_host(text: &str) -> Option<&str> {
    const MARK: &str = "could not resolve hostname ";
    // to_ascii_lowercase keeps every byte offset, so the match indexes `text`.
    let at = text.to_ascii_lowercase().find(MARK)? + MARK.len();
    let rest = &text[at..];
    let end = rest
        .find(|c: char| c == ':' || c.is_whitespace())
        .unwrap_or(rest.len());
    let host = &rest[..end];
    (!host.is_empty()).then_some(host)
}

/// Make everything in the mirror readable by `gid` (the checkout owner's
/// group), and keep what root's later fetches write that way: group on every
/// entry, g+r (g+x on directories), nothing for others, and the setgid bit on
/// every directory, so new refs, objects and packs inherit the group.
/// `git clone --bare` creates refs/ and objects/ before core.sharedRepository
/// applies, and only the mirror's top directory used to get setgid: each ref
/// root fetched later (every push) was root's group, and the owner's fetch
/// failed with "upload-pack: not our ref 0000000...".
async fn share_with_group(path: &str, gid: u32) -> AgentResult<()> {
    // -h: a symlink itself, never what it points to (root alone writes here).
    exec::run("chgrp", ["-hR", &gid.to_string(), path]).await?;
    exec::run("chmod", ["-R", "g+rX,o-rwx", path]).await?;
    exec::run("find", [path, "-type", "d", "-exec", "chmod", "g+s", "{}", "+"]).await?;
    Ok(())
}

/// Whether every entry is already shared with `gid` as share_with_group
/// leaves it. Stops at the first one that is not.
async fn shared_with(path: &str, gid: u32) -> bool {
    let gid = gid.to_string();
    let out = exec::run(
        "find",
        [
            path, "(", "!", "-group", &gid, "-o", "-type", "d", "!", "-perm", "-2000", "-o", "!",
            "-perm", "-g+r", ")", "-print", "-quit",
        ],
    )
    .await;
    matches!(out, Ok(o) if o.stdout.trim().is_empty())
}

/// A clone killed half-way (the update check's timeout) leaves its temp dir.
async fn remove_leftovers(app: &str) {
    let prefix = format!("{app}.git.tmp-");
    let Ok(mut rd) = tokio::fs::read_dir(MIRROR_ROOT).await else {
        return;
    };
    while let Ok(Some(entry)) = rd.next_entry().await {
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            let _ = tokio::fs::remove_dir_all(entry.path()).await;
        }
    }
}

async fn root_git(args: &[&str], task: Option<&TaskHandle>) -> AgentResult<()> {
    match task {
        Some(t) => {
            let argv: Vec<String> = args.iter().map(|a| a.to_string()).collect();
            logged("git", &argv, t).await
        }
        None => exec::run("git", args).await.map(|_| ()),
    }
}

/// The URL the mirror was made from, if it exists (root-owned, so it is the
/// URL root chose, not one an app wrote).
pub async fn mirror_url(app: &str) -> Option<String> {
    let mirror = mirror_path(app);
    if !Path::new(&mirror).join("HEAD").exists() {
        return None;
    }
    exec::run("git", ["--git-dir", &mirror, "remote", "get-url", "origin"])
        .await
        .ok()
        .map(|o| o.stdout.trim().to_string())
        .filter(|u| !u.is_empty())
}

/// Let the owner read root's mirror. git refuses a repository another user
/// owns ("dubious ownership") unless safe.directory lists it, and a fetch
/// from a local path runs upload-pack in a child process that drops `-c`
/// options, so the entry has to be in the owner's own git config. Without
/// it every update check and pull failed with "ownership blocked".
pub async fn trust_mirror(owner: Option<&Owner>, mirror: &str) -> AgentResult<()> {
    if owner.is_none() {
        return Ok(()); // root owns the mirror
    }
    let listed = owner_git(owner, None, None, &["config", "--global", "--get-all", "safe.directory"])
        .await
        .map(|o| o.stdout)
        .unwrap_or_default();
    if listed.lines().any(|l| l.trim() == mirror || l.trim() == "*") {
        return Ok(());
    }
    owner_git(owner, None, None, &["config", "--global", "--add", "safe.directory", mirror]).await?;
    Ok(())
}

/// Point the checkout's origin at the mirror, as the checkout's owner.
pub async fn point_origin_at_mirror(owner: Option<&Owner>, dir: &str, mirror: &str) -> AgentResult<()> {
    trust_mirror(owner, mirror).await?;
    let current = owner_git(owner, Some(dir), None, &["remote", "get-url", "origin"])
        .await
        .map(|o| o.stdout.trim().to_string())
        .ok();
    if current.as_deref() == Some(mirror) {
        return Ok(());
    }
    let verb = if current.is_some() { "set-url" } else { "add" };
    owner_git(owner, Some(dir), None, &["remote", verb, "origin", mirror]).await?;
    Ok(())
}

pub async fn remove_mirror(app: &str) {
    let lock = mirror_lock(app);
    let _guard = lock.lock().await;
    let _ = tokio::fs::remove_dir_all(mirror_path(app)).await;
}

#[cfg(test)]
mod tests {
    use super::{access_hint, owner_git_argv, unresolved_host, Owner};
    use crate::error::AgentError;

    /// The panel's Generate keypair writes `Host <name>` into the chosen
    /// account's ~/.ssh/config. Pick any account but root and the mirror
    /// clone, which runs as root, never sees it: ssh looks the alias up in
    /// DNS instead. Say so, rather than leaving a name resolution error.
    #[test]
    fn an_unknown_host_alias_points_at_roots_ssh_config() {
        let err = AgentError::Other(anyhow::anyhow!(
            "ssh: Could not resolve hostname github_example_web: Temporary failure in name resolution"
        ));
        let said = access_hint(err).to_string();
        assert!(said.contains("/root/.ssh/config"), "{said}");
        assert!(said.contains("github_example_web"), "{said}");
        assert!(said.contains("deploy key"), "{said}");

        // A real host name that does not resolve is a different problem.
        let dns = AgentError::Other(anyhow::anyhow!(
            "ssh: Could not resolve hostname githbu.com: Name or service not known"
        ));
        let said = dns.to_string();
        let hinted = access_hint(AgentError::Other(anyhow::anyhow!(said.clone()))).to_string();
        assert!(hinted.contains("did not resolve"), "{hinted}");
        assert!(!hinted.contains("/root/.ssh/config"), "{hinted}");
    }

    #[test]
    fn the_unresolved_name_is_read_out_of_ssh_s_message() {
        assert_eq!(
            unresolved_host("ssh: Could not resolve hostname gh_alias: Temporary failure"),
            Some("gh_alias")
        );
        assert_eq!(unresolved_host("fatal: repository not found"), None);
    }

    #[test]
    fn owner_commands_drop_to_the_owner() {
        let owner = Owner { name: "apps".into(), home: "/srv/apps".into(), gid: 999 };
        let (cmd, argv) = owner_git_argv(
            Some(&owner),
            Some("/srv/apps/web"),
            Some("/var/lib/vps-panel-git/web.git"),
            &["pull", "--ff-only"],
        );
        assert_eq!(cmd, "runuser");
        assert_eq!(
            argv,
            [
                "-u", "apps", "--", "env", "HOME=/srv/apps", "GIT_TERMINAL_PROMPT=0", "git",
                "-c", "safe.directory=/var/lib/vps-panel-git/web.git",
                "-C", "/srv/apps/web", "pull", "--ff-only",
            ]
        );
        let (cmd, argv) = owner_git_argv(None, Some("/root/x"), None, &["status"]);
        assert_eq!(cmd, "git");
        assert_eq!(argv, ["-C", "/root/x", "status"]);
        assert!(!argv.iter().any(|a| a.contains("safe.directory=*")));
    }

    /// A group the test may hand files to: any gid as root, else one of the
    /// user's supplementary groups. None skips the test.
    #[cfg(unix)]
    fn spare_group() -> Option<u32> {
        if nix::unistd::getuid().is_root() {
            return Some(4242);
        }
        let primary = nix::unistd::getgid();
        nix::unistd::getgroups().ok()?.into_iter().find(|g| *g != primary).map(|g| g.as_raw())
    }

    /// The live failure: a mirror shared the old way (setgid on its top
    /// directory only) got every ref root fetched later in root's group, so
    /// the owner's fetch died with "not our ref 0000...". The check spots it,
    /// the repair fixes it, and later fetches stay in the owner's group.
    #[cfg(unix)]
    #[tokio::test]
    async fn refs_fetched_later_stay_readable_by_the_owner_group() {
        use super::{share_with_group, shared_with};
        use std::os::unix::fs::MetadataExt;
        let Some(gid) = spare_group() else { return };
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(d)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .expect("git");
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        let push = |msg: &str| {
            git(&["-C", "work", "commit", "-q", "--allow-empty", "-m", msg]);
            git(&["-C", "work", "push", "-q", "origin", "main"]);
            git(&["--git-dir", "mirror.git", "fetch", "-q", "--prune", "origin"]);
        };
        git(&["init", "-q", "--bare", "up.git"]);
        git(&["--git-dir", "up.git", "symbolic-ref", "HEAD", "refs/heads/main"]);
        git(&["clone", "-q", "up.git", "work"]);
        git(&["-C", "work", "symbolic-ref", "HEAD", "refs/heads/main"]);
        git(&["-C", "work", "commit", "-q", "--allow-empty", "-m", "one"]);
        git(&["-C", "work", "push", "-q", "origin", "main"]);
        git(&["clone", "-q", "--bare", "--config", "core.sharedRepository=0640", "up.git", "mirror.git"]);
        git(&["--git-dir", "mirror.git", "config", "remote.origin.fetch", "+refs/heads/*:refs/heads/*"]);
        let mirror = d.join("mirror.git").display().to_string();
        let status = std::process::Command::new("chgrp").args(["-R", &gid.to_string(), &mirror]).status();
        assert!(status.unwrap().success());
        std::process::Command::new("chmod").args(["2750", &mirror]).status().unwrap();

        push("two");
        assert!(!shared_with(&mirror, gid).await, "a fetch into the old layout leaves the owner's group");
        share_with_group(&mirror, gid).await.unwrap();
        assert!(shared_with(&mirror, gid).await);

        push("three");
        assert!(shared_with(&mirror, gid).await, "later fetches keep the owner's group");
        let r = std::fs::metadata(d.join("mirror.git/refs/heads/main")).unwrap();
        assert_eq!(r.gid(), gid);
        assert_eq!(r.mode() & 0o040, 0o040);
    }
}
