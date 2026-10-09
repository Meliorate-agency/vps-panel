//! Shell command execution behind the panel's /terminal page.
//!
//! # What this is, plainly
//!
//! Arbitrary command execution as root. There is no allowlist and no path
//! jail, because there couldn't be a meaningful one: `cat /etc/shadow` is just
//! a command, and anything that blocked it would also block the legitimate
//! reason someone opened a terminal. Filtering here would be theatre.
//!
//! The controls that are real:
//!
//! - **Admin-only**, enforced by the panel before the request ever reaches the
//!   socket, on top of the socket's own bearer token.
//! - **Audited.** The panel writes every command to `audit_log` before running
//!   it, so the record survives even if the command takes the box down.
//! - **Bounded.** Every run has a hard timeout, and the child is put in its own
//!   process group so a disconnect or timeout kills the whole tree — not just
//!   the `bash` that spawned it.
//!
//! # One-shot, not a session
//!
//! Each request is an independent `bash -lc`. Nothing persists between
//! commands: no shell variables, no `cd`, no interactive TUIs (`vim`, `top`).
//! That's deliberate — the panel's working directory comes from the file tree
//! on the left, so a shell that could `cd` out from under it would desync the
//! two. A persistent PTY would need a WebSocket, which Next.js route handlers
//! can't upgrade.

use crate::error::{AgentError, AgentResult};
use axum::response::Response;
use serde::Deserialize;
use serde_json::json;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

#[derive(Deserialize)]
pub struct ExecReq {
    /// Absolute working directory. Must exist.
    pub cwd: String,
    pub command: String,
    /// Wall-clock limit. Clamped to [1, MAX_TIMEOUT_SECS].
    pub timeout_secs: Option<u64>,
    /// Run the command as this Linux user instead of root.
    ///
    /// This exists because running an app's own tooling as root is a quiet
    /// footgun: `npm run build`, a migration that regenerates a client, or
    /// anything else writing into `/srv/apps/<app>` leaves root-owned files
    /// that the service — which runs as `apps` with `ProtectSystem=strict` —
    /// then can't write. The deploy path already drops to `apps` for exactly
    /// this reason (`apps::run_npm`); this lets the terminal match it.
    #[serde(default)]
    pub run_as: Option<String>,
    /// Files to `source` before the command, in order. Missing ones are
    /// skipped rather than failing the run.
    ///
    /// Sourcing always happens as ROOT, before dropping to `run_as`. That's
    /// load-bearing: `/etc/vps-panel/apps/` is `0750 root:root`, so the `apps`
    /// user can't read an app's EnvironmentFile itself — systemd reads it as
    /// root before dropping privileges, and so do we.
    #[serde(default)]
    pub env_files: Vec<String>,
}

const DEFAULT_TIMEOUT_SECS: u64 = 120;
const MAX_TIMEOUT_SECS: u64 = 1800;
/// Cap on a single output line. A command that prints a 500 MB line (a minified
/// bundle, a binary catted by accident) shouldn't be buffered whole.
const MAX_LINE_BYTES: usize = 64 * 1024;
/// Cap on lines forwarded per run, so `find /` can't stream forever into the
/// browser. The process is killed when this trips.
const MAX_LINES: usize = 20_000;

/// One NDJSON frame. The client reads the response body as a stream and parses
/// a frame per line — `{"t":"out"}` / `{"t":"err"}` / `{"t":"exit"}`.
enum Frame {
    Out(String),
    Err(String),
    Exit(i32),
    Notice(String),
}

impl Frame {
    fn encode(&self) -> String {
        let v = match self {
            Frame::Out(d) => json!({ "t": "out", "d": d }),
            Frame::Err(d) => json!({ "t": "err", "d": d }),
            Frame::Exit(code) => json!({ "t": "exit", "code": code }),
            Frame::Notice(d) => json!({ "t": "notice", "d": d }),
        };
        format!("{v}\n")
    }
}

/// Kills a process GROUP when dropped.
///
/// `bash -lc "sleep 100"` makes `sleep` a child of bash, so killing bash alone
/// orphans it. The child is spawned into its own process group (see
/// `process_group(0)` below), and this signals the whole group — which is what
/// makes "client closed the tab" and "timed out" actually stop the work rather
/// than just stop watching it.
struct ProcessGroup(Option<u32>);

impl ProcessGroup {
    fn kill(&mut self) {
        if let Some(pid) = self.0.take() {
            kill_group(pid);
        }
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.kill();
    }
}

#[cfg(unix)]
fn kill_group(pid: u32) {
    use nix::sys::signal::{killpg, Signal};
    use nix::unistd::Pid;
    // ESRCH just means it already exited — the common case on a clean run.
    let _ = killpg(Pid::from_raw(pid as i32), Signal::SIGKILL);
}

#[cfg(not(unix))]
fn kill_group(_pid: u32) {}

/// Single-quote for safe interpolation into a shell script.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn is_valid_username(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 32
        && s.starts_with(|c: char| c.is_ascii_lowercase() || c == '_')
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// Home directory of `user`, or an error naming the user that doesn't exist —
/// which is a far more useful failure than a shell error 200 lines later.
#[cfg(unix)]
fn home_of(user: &str) -> AgentResult<String> {
    let entry = nix::unistd::User::from_name(user)
        .map_err(|e| AgentError::Other(anyhow::anyhow!("getpwnam: {e}")))?
        .ok_or_else(|| AgentError::Invalid(format!("no such user: {user}")))?;
    Ok(entry.dir.to_string_lossy().into_owned())
}

#[cfg(not(unix))]
fn home_of(_user: &str) -> AgentResult<String> {
    Ok("/root".to_string())
}

/// Assemble the shell script for one run.
///
/// Everything is composed into a single `bash -lc` script rather than nested
/// argv, because the env sourcing has to happen in the same shell that later
/// hands the environment down to `runuser`.
fn build_script(command: &str, env_files: &[String], run_as: Option<(&str, &str)>) -> String {
    let mut script = String::new();
    for f in env_files {
        // `set -a` exports everything the file defines, which is what makes it
        // visible to the command (and, below, to runuser).
        script.push_str(&format!("set -a; . {}; set +a\n", shell_quote(f)));
    }
    match run_as {
        None => script.push_str(command),
        Some((user, home)) => {
            // -p preserves the environment we just sourced; HOME is then set
            // explicitly so npm/pip/cargo caches land in the target user's home
            // instead of /root, which they couldn't write anyway. umask 022
            // mirrors what the deploy path uses so file modes stay consistent.
            script.push_str(&format!(
                "exec runuser -u {} -p -- env HOME={} bash -lc {}\n",
                shell_quote(user),
                shell_quote(home),
                shell_quote(&format!("umask 022; {command}")),
            ));
        }
    }
    script
}

pub async fn exec(axum::Json(req): axum::Json<ExecReq>) -> AgentResult<Response> {
    if req.command.trim().is_empty() {
        return Err(AgentError::Invalid("command must not be empty".into()));
    }

    // No jail here on purpose (see the module docs) — this only resolves the
    // directory and proves it exists, so a typo'd cwd fails with a clear error
    // instead of silently running somewhere else.
    let cwd = tokio::fs::canonicalize(&req.cwd)
        .await
        .map_err(|_| AgentError::Invalid(format!("{} does not exist", req.cwd)))?;
    let meta = tokio::fs::metadata(&cwd).await.map_err(AgentError::Io)?;
    if !meta.is_dir() {
        return Err(AgentError::Invalid(format!(
            "{} is not a directory",
            cwd.display()
        )));
    }

    let timeout = req
        .timeout_secs
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
        .clamp(1, MAX_TIMEOUT_SECS);

    // Resolve run-as up front so an unknown user fails cleanly here rather
    // than as a confusing shell error mid-stream.
    let run_as = match req.run_as.as_deref() {
        None | Some("") | Some("root") => None,
        Some(user) => {
            if !is_valid_username(user) {
                return Err(AgentError::Invalid(format!("invalid user name: {user}")));
            }
            Some((user.to_string(), home_of(user)?))
        }
    };

    // Skip env files that aren't there. An app with no `.env.local` is normal,
    // and failing the whole run over it would be useless.
    let mut env_files = Vec::new();
    for f in &req.env_files {
        if tokio::fs::metadata(f)
            .await
            .map(|m| m.is_file())
            .unwrap_or(false)
        {
            env_files.push(f.clone());
        }
    }

    // Echoed back so the operator can see what the command actually ran with —
    // "it worked in my shell" is usually an env difference, and this makes the
    // difference visible instead of guessable.
    let mut preamble: Vec<Frame> = Vec::new();
    if let Some((user, _)) = &run_as {
        preamble.push(Frame::Notice(format!("running as {user}")));
    }
    if !env_files.is_empty() {
        preamble.push(Frame::Notice(format!("loaded env: {}", env_files.join(", "))));
    }

    let script = build_script(
        &req.command,
        &env_files,
        run_as.as_ref().map(|(u, h)| (u.as_str(), h.as_str())),
    );

    let mut cmd = Command::new("bash");
    cmd.arg("-lc")
        .arg(&script)
        .current_dir(&cwd)
        // stdin is /dev/null, not a pipe: a command that reads stdin gets EOF
        // and exits instead of hanging until the timeout.
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("HOME", "/root")
        .env("USER", "root")
        // Nothing here can interpret escape codes, and TERM=dumb stops tools
        // from emitting them (or trying to open a pager).
        .env("TERM", "dumb")
        .env("PAGER", "cat")
        .env("GIT_PAGER", "cat")
        .env("DEBIAN_FRONTEND", "noninteractive")
        .kill_on_drop(true);

    #[cfg(unix)]
    {
        // Put the child in its own process group (pgid 0 means "use my own
        // pid"), so `killpg` below reaches the whole tree — `bash -lc "sleep
        // 100"` makes sleep a child of bash, and killing bash alone orphans it.
        //
        // This replaced a `pre_exec(setsid)` closure: `process_group` is a safe
        // setpgid that tokio performs itself, so nothing of ours has to run in
        // the window between fork and exec, where almost nothing is legal.
        cmd.process_group(0);
    }

    let mut child = cmd.spawn().map_err(AgentError::Io)?;
    let group = ProcessGroup(child.id());
    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");

    let (tx, mut rx) = mpsc::channel::<Frame>(256);

    // Supervisor: drain both pipes to completion, THEN reap and report the
    // exit code, so the exit frame is always last.
    let out_tx = tx.clone();
    tokio::spawn(async move {
        let a = tokio::spawn(pump(stdout, out_tx.clone(), false));
        let b = tokio::spawn(pump(stderr, out_tx.clone(), true));
        let _ = a.await;
        let _ = b.await;
        let code = child
            .wait()
            .await
            .ok()
            .and_then(|s| s.code())
            // No exit code means it died on a signal — which is what our own
            // timeout and disconnect kills look like.
            .unwrap_or(-1);
        let _ = out_tx.send(Frame::Exit(code)).await;
    });
    drop(tx);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout);
    let stream = async_stream::stream! {
        // Moved into the generator, not borrowed: when the response body is
        // dropped — client closed the tab, navigated away, hit Stop — this
        // drops with it and the process group dies.
        let mut group = group;
        for frame in preamble {
            yield Ok::<Vec<u8>, std::io::Error>(frame.encode().into_bytes());
        }
        let mut lines = 0usize;
        loop {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(frame)) => {
                    let done = matches!(frame, Frame::Exit(_));
                    yield Ok::<Vec<u8>, std::io::Error>(frame.encode().into_bytes());
                    if done {
                        break;
                    }
                    lines += 1;
                    if lines >= MAX_LINES {
                        group.kill();
                        yield Ok(Frame::Notice(format!(
                            "stopped after {MAX_LINES} lines of output — process killed"
                        ))
                        .encode()
                        .into_bytes());
                        yield Ok(Frame::Exit(-1).encode().into_bytes());
                        break;
                    }
                }
                // All senders dropped without an exit frame: the supervisor
                // task itself died. Report rather than hang.
                Ok(None) => {
                    yield Ok(Frame::Exit(-1).encode().into_bytes());
                    break;
                }
                Err(_) => {
                    group.kill();
                    yield Ok(Frame::Notice(format!(
                        "timed out after {timeout}s — process killed"
                    ))
                    .encode()
                    .into_bytes());
                    yield Ok(Frame::Exit(-1).encode().into_bytes());
                    break;
                }
            }
        }
    };

    Response::builder()
        .header(axum::http::header::CONTENT_TYPE, "application/x-ndjson")
        .header(axum::http::header::CACHE_CONTROL, "no-store")
        // nginx buffers proxied responses by default, which would hold output
        // back until the command finished and defeat the whole point.
        .header("X-Accel-Buffering", "no")
        .body(axum::body::Body::from_stream(stream))
        .map_err(|e| AgentError::Other(anyhow::anyhow!("build exec response: {e}")))
}

/// Forwards one pipe, a line at a time.
///
/// Hand-rolled rather than `BufRead::split(b'\n')` because that buffers a whole
/// segment before handing it over: `cat`ing a large binary, or any output with
/// no newline in it, would be accumulated in full first. This flushes at
/// `MAX_LINE_BYTES` whether or not a newline has shown up, so the agent's
/// memory is bounded by that constant no matter what the command emits.
async fn pump<R>(reader: R, tx: mpsc::Sender<Frame>, is_err: bool)
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut reader = BufReader::new(reader);
    let mut line: Vec<u8> = Vec::with_capacity(1024);

    let emit = |buf: &mut Vec<u8>, truncated: bool| {
        let mut text = String::from_utf8_lossy(buf).into_owned();
        // Strip the \r of CRLF output so Windows-y tools leave no artefacts.
        if text.ends_with('\r') {
            text.pop();
        }
        if truncated {
            text.push_str(" …[line truncated]");
        }
        buf.clear();
        if is_err {
            Frame::Err(text)
        } else {
            Frame::Out(text)
        }
    };

    loop {
        // Copy in bulk up to the next newline (or to the cap), then consume
        // that much in one go — scanning byte by byte would cost an await per
        // byte across megabytes of output.
        let (complete, consumed) = {
            let buf = match reader.fill_buf().await {
                Ok(b) => b,
                Err(_) => break,
            };
            if buf.is_empty() {
                break; // EOF
            }
            match buf.iter().position(|b| *b == b'\n') {
                Some(i) => {
                    line.extend_from_slice(&buf[..i]);
                    (true, i + 1)
                }
                None => {
                    let room = MAX_LINE_BYTES.saturating_sub(line.len()).max(1);
                    let take = buf.len().min(room);
                    line.extend_from_slice(&buf[..take]);
                    (false, take)
                }
            }
        };
        reader.consume(consumed);

        if complete {
            // A send error means the client is gone; stop reading.
            if tx.send(emit(&mut line, false)).await.is_err() {
                return;
            }
        } else if line.len() >= MAX_LINE_BYTES && tx.send(emit(&mut line, true)).await.is_err() {
            return;
        }
    }

    // Trailing output with no final newline still deserves to be shown.
    if !line.is_empty() {
        let _ = tx.send(emit(&mut line, false)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_one_json_object_per_line() {
        let out = Frame::Out("hello".into()).encode();
        assert!(out.ends_with('\n'));
        assert_eq!(out.matches('\n').count(), 1);
        assert_eq!(out.trim(), r#"{"d":"hello","t":"out"}"#);
        assert_eq!(Frame::Exit(0).encode().trim(), r#"{"code":0,"t":"exit"}"#);
    }

    #[test]
    fn frame_payloads_are_json_escaped() {
        // Command output is arbitrary bytes. A newline or quote inside it must
        // not be able to forge a second frame.
        let sneaky = Frame::Out("a\"b\nc".into()).encode();
        assert_eq!(sneaky.matches('\n').count(), 1);
        assert!(sneaky.contains(r#"a\"b\nc"#));
    }

    #[test]
    fn env_files_are_sourced_before_the_command() {
        let s = build_script("npm run db:migrate", &["/etc/app.env".into()], None);
        assert!(s.starts_with("set -a; . '/etc/app.env'; set +a\n"));
        assert!(s.trim_end().ends_with("npm run db:migrate"));
    }

    #[test]
    fn run_as_sources_env_as_root_then_drops_privileges() {
        let s = build_script(
            "npm run db:migrate",
            &["/etc/vps-panel/apps/x.env".into()],
            Some(("apps", "/srv/apps")),
        );
        // Order matters: /etc/vps-panel/apps is 0750 root:root, so the `apps`
        // user could not source that file itself.
        let source_at = s.find("set -a").unwrap();
        let runuser_at = s.find("runuser").unwrap();
        assert!(source_at < runuser_at);
        assert!(s.contains("runuser -u 'apps' -p -- env HOME='/srv/apps'"));
        assert!(s.contains("umask 022; npm run db:migrate"));
    }

    #[test]
    fn shell_metacharacters_in_paths_and_users_are_quoted() {
        // A filename with a quote in it must not be able to break out of the
        // sourcing line and become its own command.
        let s = build_script("ls", &["/tmp/a'b; rm -rf /".into()], None);
        assert!(s.contains(r"'/tmp/a'\''b; rm -rf /'"));
        assert!(!s.contains("; rm -rf /;"));
    }

    #[test]
    fn usernames_are_validated_against_linux_naming() {
        assert!(is_valid_username("apps"));
        assert!(is_valid_username("_svc"));
        assert!(is_valid_username("deploy-bot"));
        assert!(!is_valid_username(""));
        assert!(!is_valid_username("Root"));
        assert!(!is_valid_username("app;rm -rf /"));
        assert!(!is_valid_username("../root"));
    }

    #[test]
    fn timeout_is_clamped_to_a_sane_window() {
        let clamp = |v: u64| v.clamp(1, MAX_TIMEOUT_SECS);
        assert_eq!(clamp(0), 1);
        assert_eq!(clamp(60), 60);
        assert_eq!(clamp(u64::MAX), MAX_TIMEOUT_SECS);
    }
}
