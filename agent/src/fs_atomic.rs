use crate::error::{AgentError, AgentResult};
use std::path::Path;
use tokio::fs;
use tokio::io::AsyncWriteExt;

/// Write `content` to `path` atomically: write a fresh temp file next to it,
/// fsync, set `mode` (octal, e.g. 0o640), rename over `path`.
///
/// The temp name is unique per call and created with O_EXCL (`create_new`):
/// with a name that was only unique per process, two concurrent writes of the
/// same file (two cert tasks for one domain) shared one temp file and could
/// rename each other's half-written content into place, and a symlink planted
/// at the predictable name would have been followed. The temp file is removed
/// when any step fails.
///
/// A parent path that runs through a symlink owned by a non-root user is
/// refused (see reject_untrusted_symlinks). The target itself may be a
/// symlink: the rename replaces the link, it never writes through it.
pub async fn write(path: &Path, content: &[u8], mode: u32) -> AgentResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| AgentError::Invalid(format!("path has no parent: {}", path.display())))?;
    reject_untrusted_symlinks(parent).await?;
    fs::create_dir_all(parent).await.map_err(AgentError::Io)?;

    let tmp = parent.join(format!(
        ".vps-panel.{}.{}.{}.tmp",
        path.file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        std::process::id(),
        &uuid::Uuid::new_v4().simple().to_string()[..12],
    ));

    let result = async {
        {
            let mut f = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)
                .await
                .map_err(AgentError::Io)?;
            f.write_all(content).await.map_err(AgentError::Io)?;
            f.sync_all().await.map_err(AgentError::Io)?;
        }
        chmod(&tmp, mode).await?;
        fs::rename(&tmp, path).await.map_err(AgentError::Io)
    }
    .await;
    if result.is_err() {
        let _ = fs::remove_file(&tmp).await;
    }
    result
}

/// Refuse a path that runs through a symlink owned by a non-root user. The
/// agent runs as root and writes into directories other users control (their
/// home for SSH keys, an app's folder); a symlink planted there redirected
/// the write, and the chown after it, anywhere: `~/.ssh -> /root/.ssh` made
/// adding a key for that user replace root's authorized_keys and hand the
/// user ownership of it. A user can only create symlinks it owns, so
/// root-owned ones (`/var/run -> /run`) are still followed.
#[cfg(unix)]
pub async fn reject_untrusted_symlinks(path: &Path) -> AgentResult<()> {
    use std::os::unix::fs::MetadataExt;
    let mut chain: Vec<&Path> = path.ancestors().collect();
    chain.reverse();
    for p in chain.into_iter().filter(|p| !p.as_os_str().is_empty()) {
        match fs::symlink_metadata(p).await {
            Ok(m) if m.file_type().is_symlink() && m.uid() != 0 => {
                return Err(AgentError::Invalid(format!(
                    "refusing to follow {}: it is a symlink owned by uid {}",
                    p.display(),
                    m.uid()
                )));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(e) => return Err(AgentError::Io(e)),
        }
    }
    Ok(())
}

#[cfg(not(unix))]
pub async fn reject_untrusted_symlinks(_path: &Path) -> AgentResult<()> {
    Ok(())
}

/// chmod that never follows a symlink (it fails on one instead), so a link a
/// user swapped in cannot point the new mode at another file.
#[cfg(unix)]
pub async fn chmod(path: &Path, mode: u32) -> AgentResult<()> {
    let owned = path.to_path_buf();
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        // O_NONBLOCK so opening a FIFO cannot hang.
        let f = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
            .open(&owned)?;
        f.set_permissions(std::fs::Permissions::from_mode(mode))
    })
    .await
    .map_err(|e| AgentError::Other(anyhow::anyhow!("chmod task: {e}")))?
    .map_err(|e| {
        if e.raw_os_error() == Some(nix::libc::ELOOP) {
            AgentError::Invalid(format!("refusing to chmod {}: it is a symlink", path.display()))
        } else {
            AgentError::Io(e)
        }
    })
}

#[cfg(not(unix))]
pub async fn chmod(_path: &Path, _mode: u32) -> AgentResult<()> {
    Ok(())
}

/// lchown: a symlink gets its own owner changed, never the file it points
/// at. chown(2) followed it, so a user who swapped a file the agent was
/// about to hand to them for a link to /etc/shadow got /etc/shadow.
#[cfg(unix)]
pub fn chown_blocking(path: &Path, uid: u32, gid: u32) -> AgentResult<()> {
    std::os::unix::fs::lchown(path, Some(uid), Some(gid)).map_err(AgentError::Io)
}

#[cfg(not(unix))]
pub fn chown_blocking(_path: &Path, _uid: u32, _gid: u32) -> AgentResult<()> {
    Ok(())
}

pub async fn ensure_dir(path: &Path, mode: u32) -> AgentResult<()> {
    // Apply `mode` to every directory we actually create, not just the leaf.
    // The agent service runs with UMask=0007 (so panel.db sidecars are group-
    // writable), which means `create_dir_all` builds intermediate directories
    // with mode 0o770. For paths like /var/www/vps-panel-acme/.well-known/
    // acme-challenge/ that breaks HTTP-01 validation: nginx (www-data, not in
    // the root group) can't traverse the umask-default `.well-known/` even
    // though the leaf is chmod'd 0o755.
    //
    // Strategy: walk up to the first existing ancestor, then create each
    // missing directory from there down and chmod it. Pre-existing
    // directories are left untouched - we never broaden perms on a path the
    // operator may have deliberately locked down.
    reject_untrusted_symlinks(path).await?;
    let mut to_create: Vec<std::path::PathBuf> = Vec::new();
    let mut cur = path.to_path_buf();
    loop {
        if fs::try_exists(&cur).await.map_err(AgentError::Io)? {
            break;
        }
        to_create.push(cur.clone());
        match cur.parent() {
            Some(p) if !p.as_os_str().is_empty() => cur = p.to_path_buf(),
            _ => break,
        }
    }
    for p in to_create.into_iter().rev() {
        match fs::create_dir(&p).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(AgentError::Io(e)),
        }
        chmod(&p, mode).await?;
    }
    Ok(())
}

pub async fn remove_if_exists(path: &Path) -> AgentResult<()> {
    match fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(AgentError::Io(e)),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::{chmod, reject_untrusted_symlinks, write};
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fs-atomic-{name}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // The checks key on the symlink's owner, so they only mean something when
    // the tests run as a normal user (a root-owned link is trusted).
    fn as_root() -> bool {
        nix::unistd::getuid().is_root()
    }

    #[tokio::test]
    async fn refuses_a_user_owned_symlink_in_the_parent_chain() {
        if as_root() {
            return;
        }
        let dir = scratch("chain");
        let real = dir.join("real");
        std::fs::create_dir(&real).unwrap();
        symlink(&real, dir.join("link")).unwrap();

        assert!(reject_untrusted_symlinks(&dir.join("real/file")).await.is_ok());
        assert!(reject_untrusted_symlinks(&dir.join("link/file")).await.is_err());
        assert!(write(&dir.join("link/file"), b"x", 0o600).await.is_err());
        assert!(!real.join("file").exists());
        assert!(write(&dir.join("real/file"), b"x", 0o600).await.is_ok());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn write_replaces_a_symlink_target_instead_of_following_it() {
        let dir = scratch("target");
        let victim = dir.join("victim");
        std::fs::write(&victim, b"keep").unwrap();
        symlink(&victim, dir.join("out")).unwrap();
        write(&dir.join("out"), b"new", 0o644).await.unwrap();
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep");
        assert!(!std::fs::symlink_metadata(dir.join("out")).unwrap().file_type().is_symlink());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn chmod_refuses_symlinks_and_sets_modes() {
        let dir = scratch("chmod");
        let file = dir.join("file");
        std::fs::write(&file, b"x").unwrap();
        symlink(&file, dir.join("link")).unwrap();
        assert!(chmod(&dir.join("link"), 0o777).await.is_err());
        chmod(&file, 0o640).await.unwrap();
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o640);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
