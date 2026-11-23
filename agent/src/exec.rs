use crate::error::{AgentError, AgentResult};
use std::ffi::OsStr;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};
use tokio::process::{Child, Command};

#[derive(Debug, Default)]
pub struct ExecOutput {
    pub stdout: String,
}

/// How long output is still read after the command itself exits. A
/// background process it started (a daemon, `nohup x &`) inherits the pipes
/// and can hold them open forever; waiting for their EOF hung the agent.
const DRAIN_AFTER_EXIT: Duration = Duration::from_secs(2);

/// git settings for every git the agent runs: never prompt, and give up on
/// a stalled network instead of hanging the deploy or update forever.
const GIT_ENV: [(&str, &str); 4] = [
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GIT_HTTP_LOW_SPEED_LIMIT", "1000"),
    ("GIT_HTTP_LOW_SPEED_TIME", "60"),
    (
        "GIT_SSH_COMMAND",
        "ssh -o BatchMode=yes -o ConnectTimeout=20 -o ServerAliveInterval=15 -o ServerAliveCountMax=4",
    ),
];

/// The agent's own secrets, which /etc/vps-panel/.env puts in its
/// environment. A child only gets one its caller passes explicitly (restic).
/// Every child used to inherit them, the ones that run as another user
/// included: git as `apps` runs the hooks of a checkout the app can write,
/// and the terminal's "run as" commands are an app's own build scripts.
pub const AGENT_SECRET_ENV: [&str; 4] = [
    "AGENT_TOKEN",
    "JWT_SECRET",
    "ADMIN_INITIAL_PASSWORD",
    "RESTIC_PASSWORD",
];

fn command<S, I>(cmd: &str, args: I) -> Command
where
    S: AsRef<OsStr>,
    I: IntoIterator<Item = S>,
{
    let mut command = Command::new(cmd);
    command.args(args);
    for name in AGENT_SECRET_ENV {
        command.env_remove(name);
    }
    if cmd == "git" {
        command.envs(GIT_ENV);
    }
    // A caller that times out drops the future; the child dies with it
    // instead of running on unobserved.
    command.kill_on_drop(true);
    command
}

pub async fn run<S, I>(cmd: &str, args: I) -> AgentResult<ExecOutput>
where
    S: AsRef<OsStr>,
    I: IntoIterator<Item = S>,
{
    run_envs(cmd, args, Vec::<(&str, String)>::new()).await
}

pub async fn run_envs<S, I, E, K, V>(cmd: &str, args: I, envs: E) -> AgentResult<ExecOutput>
where
    S: AsRef<OsStr>,
    I: IntoIterator<Item = S>,
    E: IntoIterator<Item = (K, V)>,
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
{
    let mut command = command(cmd, args);
    for (k, v) in envs {
        command.env(k, v);
    }
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(AgentError::Io)?;
    collect(cmd, child).await
}

pub async fn run_with_input<S, I>(cmd: &str, args: I, stdin_data: &str) -> AgentResult<ExecOutput>
where
    S: AsRef<OsStr>,
    I: IntoIterator<Item = S>,
{
    let mut child = command(cmd, args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(AgentError::Io)?;

    // The input is written while the output is read. Writing all of it first
    // deadlocked with a child that fills its stdout pipe before it has read
    // everything: each side waited for the other to empty a full pipe.
    let stdin = child.stdin.take();
    let write = async move {
        use tokio::io::AsyncWriteExt;
        if let Some(mut stdin) = stdin {
            stdin.write_all(stdin_data.as_bytes()).await?;
            stdin.shutdown().await?;
        }
        Ok::<(), std::io::Error>(())
    };
    tokio::pin!(write);
    let collected = collect(cmd, child);
    tokio::pin!(collected);
    let mut written: Option<std::io::Result<()>> = None;
    let out = loop {
        tokio::select! {
            // The writer first, so `written` is only None below when the
            // write was really still blocked.
            biased;
            r = &mut write, if written.is_none() => written = Some(r),
            out = &mut collected => break out,
        }
    };
    // A command that failed says why on stderr, which is worth more than the
    // broken pipe its early exit gave the writer.
    let out = out?;
    match written {
        Some(Ok(())) => Ok(out),
        Some(Err(e)) => Err(AgentError::Io(e)),
        // Still writing when the command exited, as the old write_all would
        // have been: it failed with EPIPE.
        None => Err(AgentError::Io(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            format!("{cmd} exited before it had read all of its input"),
        ))),
    }
}

/// Read stdout and stderr to the end (or until DRAIN_AFTER_EXIT after the
/// command exits) and fail on a non-zero exit.
async fn collect(cmd: &str, mut child: Child) -> AgentResult<ExecOutput> {
    let mut out = child.stdout.take().expect("stdout piped");
    let mut err = child.stderr.take().expect("stderr piped");
    let mut out_buf = Vec::new();
    let mut err_buf = Vec::new();
    let status = {
        let reads = async {
            let (a, b) = tokio::join!(read_all(&mut out, &mut out_buf), read_all(&mut err, &mut err_buf));
            a.and(b)
        };
        tokio::pin!(reads);
        tokio::select! {
            r = &mut reads => {
                r.map_err(AgentError::Io)?;
                child.wait().await.map_err(AgentError::Io)?
            }
            s = child.wait() => {
                let s = s.map_err(AgentError::Io)?;
                let _ = tokio::time::timeout(DRAIN_AFTER_EXIT, &mut reads).await;
                s
            }
        }
    };
    let stdout = String::from_utf8_lossy(&out_buf).to_string();
    if !status.success() {
        let stderr = String::from_utf8_lossy(&err_buf).to_string();
        return Err(AgentError::Subprocess {
            cmd: cmd.to_string(),
            code: status.code().unwrap_or(-1),
            stderr,
        });
    }
    Ok(ExecOutput { stdout })
}

async fn read_all<R: AsyncRead + Unpin>(r: &mut R, buf: &mut Vec<u8>) -> std::io::Result<()> {
    let mut chunk = [0u8; 8192];
    loop {
        let n = r.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

pub async fn run_streaming<S, I, F>(cmd: &str, args: I, on_line: F) -> AgentResult<i32>
where
    S: AsRef<OsStr>,
    I: IntoIterator<Item = S>,
    F: FnMut(&str) + Send,
{
    run_streaming_envs(cmd, args, Vec::<(&str, &str)>::new(), on_line).await
}

pub async fn run_streaming_envs<S, I, E, K, V, F>(
    cmd: &str,
    args: I,
    envs: E,
    mut on_line: F,
) -> AgentResult<i32>
where
    S: AsRef<OsStr>,
    I: IntoIterator<Item = S>,
    E: IntoIterator<Item = (K, V)>,
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
    F: FnMut(&str) + Send,
{
    let mut command = command(cmd, args);
    for (k, v) in envs {
        command.env(k, v);
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(AgentError::Io)?;
    let status = stream_child(&mut child, &mut on_line).await?;
    Ok(status.code().unwrap_or(-1))
}

/// Hand the child's stdout and stderr to `on_line` a line at a time, until
/// both end or DRAIN_AFTER_EXIT after it exits.
async fn stream_child<F>(child: &mut Child, on_line: &mut F) -> AgentResult<ExitStatus>
where
    F: FnMut(&str) + Send,
{
    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");

    // Lines are split on bytes and decoded lossily. `lines()` fails on the
    // first byte that is not UTF-8, which ended the run (and killed the
    // child) of any build that printed one.
    let mut out_reader = BufReader::new(stdout);
    let mut err_reader = BufReader::new(stderr);
    let mut out_buf: Vec<u8> = Vec::new();
    let mut err_buf: Vec<u8> = Vec::new();

    // Each stream is read until its own EOF. The old loop kept polling a
    // finished stderr (busy-spinning a core) and waited for stdout EOF even
    // after the command had exited.
    let mut out_done = false;
    let mut err_done = false;
    let mut exit: Option<ExitStatus> = None;
    let mut drain_until: Option<tokio::time::Instant> = None;
    while !(out_done && err_done) {
        let deadline = drain_until.unwrap_or_else(tokio::time::Instant::now);
        tokio::select! {
            // read_until keeps what it has read in the buffer when another
            // branch wins, so nothing is lost between iterations.
            n = out_reader.read_until(b'\n', &mut out_buf), if !out_done => match n {
                Ok(0) => out_done = true,
                Ok(_) => on_line(&take_line(&mut out_buf)),
                Err(e) => return Err(AgentError::Io(e)),
            },
            n = err_reader.read_until(b'\n', &mut err_buf), if !err_done => match n {
                Ok(0) => err_done = true,
                Ok(_) => on_line(&take_line(&mut err_buf)),
                Err(e) => return Err(AgentError::Io(e)),
            },
            s = child.wait(), if exit.is_none() => {
                exit = Some(s.map_err(AgentError::Io)?);
                drain_until = Some(tokio::time::Instant::now() + DRAIN_AFTER_EXIT);
            },
            _ = tokio::time::sleep_until(deadline), if drain_until.is_some() => break,
        }
    }

    // A last line with no newline, ended by EOF or by the drain deadline.
    for buf in [&mut out_buf, &mut err_buf] {
        if !buf.is_empty() {
            on_line(&take_line(buf));
        }
    }

    match exit {
        Some(s) => Ok(s),
        None => child.wait().await.map_err(AgentError::Io),
    }
}

/// How a `run_streaming_limited` command ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Ended {
    /// On its own, with this exit code (-1: a signal ended it).
    Exited(i32),
    /// It was still running at the limit and was stopped.
    TimedOut,
}

/// How long a process group gets between SIGTERM and SIGKILL.
const GROUP_KILL_GRACE: Duration = Duration::from_secs(5);

/// The process group of a command started by `run_streaming_limited`. Dropped
/// while the command may still be running (its caller gave up, or reading its
/// output failed), the whole group gets SIGTERM, and SIGKILL after `grace`.
///
/// kill_on_drop reaches the one process the agent started. For a build that
/// is `sudo`, and SIGKILL is the one signal sudo cannot pass on: npm and
/// everything it had started ran on with nobody watching. The terminal's
/// ProcessGroup (terminal.rs) is the same idea; this one sends SIGTERM first
/// so npm can stop its own children.
struct ProcessGroup {
    pgid: Option<u32>,
    grace: Duration,
}

impl ProcessGroup {
    fn signal(&self, kill: bool) {
        if let Some(pgid) = self.pgid {
            signal_group(pgid, kill);
        }
    }

    /// The command was reaped: there is nothing left to stop.
    fn disarm(&mut self) {
        self.pgid = None;
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        let Some(pgid) = self.pgid.take() else {
            return;
        };
        signal_group(pgid, false);
        // A plain thread: this also runs while the runtime is shutting down.
        let grace = self.grace;
        let later = std::thread::Builder::new().spawn(move || {
            std::thread::sleep(grace);
            signal_group(pgid, true);
        });
        if later.is_err() {
            signal_group(pgid, true);
        }
    }
}

#[cfg(unix)]
fn signal_group(pgid: u32, kill: bool) {
    use nix::sys::signal::{killpg, Signal};
    use nix::unistd::Pid;
    let signal = if kill { Signal::SIGKILL } else { Signal::SIGTERM };
    // ESRCH: every process of the group has already exited.
    let _ = killpg(Pid::from_raw(pgid as i32), signal);
}

#[cfg(not(unix))]
fn signal_group(_pgid: u32, _kill: bool) {}

/// `run_streaming_envs` for a command that must not run forever and whose
/// children must not outlive it. It runs in a process group of its own; one
/// still running after `limit` is stopped, the whole group, with SIGTERM and
/// then SIGKILL, and so is one whose returned future is dropped. A command
/// that ends on its own is left as `run_streaming_envs` leaves it.
pub async fn run_streaming_limited<S, I, E, K, V, F>(
    cmd: &str,
    args: I,
    envs: E,
    limit: Duration,
    on_line: F,
) -> AgentResult<Ended>
where
    S: AsRef<OsStr>,
    I: IntoIterator<Item = S>,
    E: IntoIterator<Item = (K, V)>,
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
    F: FnMut(&str) + Send,
{
    run_streaming_group(cmd, args, envs, limit, GROUP_KILL_GRACE, on_line).await
}

async fn run_streaming_group<S, I, E, K, V, F>(
    cmd: &str,
    args: I,
    envs: E,
    limit: Duration,
    grace: Duration,
    mut on_line: F,
) -> AgentResult<Ended>
where
    S: AsRef<OsStr>,
    I: IntoIterator<Item = S>,
    E: IntoIterator<Item = (K, V)>,
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
    F: FnMut(&str) + Send,
{
    let mut command = command(cmd, args);
    for (k, v) in envs {
        command.env(k, v);
    }
    // Group id = the child's pid, so one signal reaches everything it starts.
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(AgentError::Io)?;
    let mut group = ProcessGroup { pgid: child.id(), grace };

    {
        let streamed = stream_child(&mut child, &mut on_line);
        tokio::pin!(streamed);
        if let Ok(status) = tokio::time::timeout(limit, &mut streamed).await {
            // An error leaves `group` armed, and its drop stops the command.
            let status = status?;
            group.disarm();
            return Ok(Ended::Exited(status.code().unwrap_or(-1)));
        }
        group.signal(false);
        // What it prints while it shuts down still reaches the log.
        let _ = tokio::time::timeout(grace, &mut streamed).await;
    }
    // Whatever ignored SIGTERM. The child is not reaped before this, or was
    // reaped just now, so its pid (the group id) has not been handed out again.
    group.signal(true);
    // By pid as well, should it have left the group.
    let _ = child.kill().await;
    group.disarm();
    Ok(Ended::TimedOut)
}

/// The line in `buf` without its `\n` or `\r\n`, with bytes that are not
/// UTF-8 replaced. Leaves `buf` empty.
fn take_line(buf: &mut Vec<u8>) -> String {
    if buf.last() == Some(&b'\n') {
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
    }
    let line = String::from_utf8_lossy(buf).into_owned();
    buf.clear();
    line
}

#[cfg(all(test, unix))]
mod tests {
    use super::{run, run_envs, run_streaming, run_streaming_group, run_streaming_limited, run_with_input, Ended};
    use std::time::{Duration, Instant};

    #[tokio::test]
    async fn run_returns_when_a_background_child_keeps_the_pipes() {
        let started = Instant::now();
        let out = run("sh", ["-c", "sleep 30 & echo done"]).await.unwrap();
        assert_eq!(out.stdout.trim(), "done");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn streaming_returns_when_a_background_child_keeps_the_pipes() {
        let started = Instant::now();
        let mut lines = Vec::new();
        let code = run_streaming("sh", ["-c", "echo one; echo two >&2; sleep 30 &"], |l| {
            lines.push(l.to_string())
        })
        .await
        .unwrap();
        assert_eq!(code, 0);
        assert!(lines.contains(&"one".to_string()) && lines.contains(&"two".to_string()));
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn streaming_survives_output_that_is_not_utf8() {
        let mut lines = Vec::new();
        let code = run_streaming(
            "sh",
            ["-c", r"printf 'caf\351\r\n'; printf 'ok\n'; printf 'tail' >&2"],
            |l| lines.push(l.to_string()),
        )
        .await
        .unwrap();
        assert_eq!(code, 0);
        assert!(lines.contains(&"caf\u{fffd}".to_string()), "{lines:?}");
        assert!(lines.contains(&"ok".to_string()) && lines.contains(&"tail".to_string()));
    }

    #[tokio::test]
    async fn children_do_not_inherit_the_agent_secrets() {
        std::env::set_var("JWT_SECRET", "panel-secret");
        std::env::set_var("RESTIC_PASSWORD", "inherited");
        let show = ["-c", "echo ${JWT_SECRET:-none} ${RESTIC_PASSWORD:-none}"];
        let out = run("sh", show).await.unwrap();
        assert_eq!(out.stdout.trim(), "none none");
        // One the caller passes explicitly still arrives.
        let out = run_envs("sh", show, [("RESTIC_PASSWORD", "explicit")]).await.unwrap();
        assert_eq!(out.stdout.trim(), "none explicit");
    }

    /// Whether `pid` is still running (a zombie nobody reaped is not).
    fn running(pid: &str) -> bool {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            // "<pid> (<comm>) <state> ...": the state follows the last ')'.
            Ok(stat) => !stat.rsplit(')').next().unwrap_or("").trim_start().starts_with('Z'),
            Err(_) => false,
        }
    }

    async fn gone(pid: &str) -> bool {
        for _ in 0..50 {
            if !running(pid) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        false
    }

    const NO_ENV: [(&str, &str); 0] = [];

    #[tokio::test]
    async fn a_command_past_its_limit_is_stopped_with_its_children() {
        // sh starts a child and waits for it, as sudo does with npm, and npm
        // with a build that watches for changes.
        let mut lines = Vec::new();
        let started = Instant::now();
        let ended = run_streaming_limited(
            "sh",
            ["-c", "sleep 300 & echo $!; wait"],
            NO_ENV,
            Duration::from_millis(400),
            |l| lines.push(l.to_string()),
        )
        .await
        .unwrap();
        assert_eq!(ended, Ended::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(20));
        assert!(gone(&lines[0]).await, "the child {} outlived the command", lines[0]);
    }

    #[tokio::test]
    async fn children_that_ignore_sigterm_are_killed() {
        let mut lines = Vec::new();
        let ended = run_streaming_group(
            "sh",
            ["-c", "trap '' TERM; sleep 300 & echo $!; wait; sleep 300"],
            NO_ENV,
            Duration::from_millis(400),
            Duration::from_millis(300),
            |l| lines.push(l.to_string()),
        )
        .await
        .unwrap();
        assert_eq!(ended, Ended::TimedOut);
        assert!(gone(&lines[0]).await, "the child {} outlived the command", lines[0]);
    }

    #[tokio::test]
    async fn dropping_a_limited_command_stops_its_children() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let run = tokio::spawn(async move {
            run_streaming_limited(
                "sh",
                ["-c", "sleep 300 & echo $!; wait"],
                NO_ENV,
                Duration::from_secs(300),
                move |l| {
                    let _ = tx.send(l.to_string());
                },
            )
            .await
        });
        let pid = rx.recv().await.unwrap();
        assert!(running(&pid));
        run.abort();
        assert!(gone(&pid).await, "the child {pid} outlived the dropped command");
    }

    #[tokio::test]
    async fn a_limited_command_that_ends_in_time_reports_its_exit_code() {
        let mut lines = Vec::new();
        let ended = run_streaming_limited(
            "sh",
            ["-c", "echo $WHO; echo err >&2; exit 7"],
            [("WHO", "builder")],
            Duration::from_secs(30),
            |l| lines.push(l.to_string()),
        )
        .await
        .unwrap();
        assert_eq!(ended, Ended::Exited(7));
        assert!(lines.contains(&"builder".to_string()) && lines.contains(&"err".to_string()));
    }

    #[tokio::test]
    async fn input_larger_than_a_pipe_does_not_deadlock_with_the_output() {
        // cat fills its stdout pipe (64 KiB) long before it has read 1 MiB.
        let input = "0123456789abcdef".repeat(64 * 1024);
        let out = tokio::time::timeout(Duration::from_secs(20), run_with_input("cat", [""; 0], &input))
            .await
            .expect("run_with_input hung")
            .unwrap();
        assert_eq!(out.stdout.len(), input.len());
        assert_eq!(out.stdout, input);
    }

    #[tokio::test]
    async fn a_failed_command_with_unread_input_reports_its_own_error() {
        let input = "x".repeat(1024 * 1024);
        let err = run_with_input("sh", ["-c", "echo refused >&2; exit 4"], &input).await.unwrap_err();
        assert!(err.to_string().contains("refused"), "{err}");
        // One that succeeds without reading it all is still an error, as before.
        assert!(run_with_input("true", [""; 0], &input).await.is_err());
        // And a small input it never reads is fine, as before.
        let out = run_with_input("sh", ["-c", "sleep 0.3; echo ok"], "tiny").await.unwrap();
        assert_eq!(out.stdout.trim(), "ok");
    }

    #[tokio::test]
    async fn run_reports_failure_with_stderr() {
        let err = run("sh", ["-c", "echo boom >&2; exit 3"]).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("boom") || msg.contains('3'), "{msg}");
    }
}
