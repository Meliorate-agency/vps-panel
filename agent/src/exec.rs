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

fn command<S, I>(cmd: &str, args: I) -> Command
where
    S: AsRef<OsStr>,
    I: IntoIterator<Item = S>,
{
    let mut command = Command::new(cmd);
    command.args(args);
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

    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        stdin
            .write_all(stdin_data.as_bytes())
            .await
            .map_err(AgentError::Io)?;
    }

    collect(cmd, child).await
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

    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");

    let mut out_reader = BufReader::new(stdout).lines();
    let mut err_reader = BufReader::new(stderr).lines();

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
            line = out_reader.next_line(), if !out_done => match line {
                Ok(Some(l)) => on_line(&l),
                Ok(None) => out_done = true,
                Err(e) => return Err(AgentError::Io(e)),
            },
            line = err_reader.next_line(), if !err_done => match line {
                Ok(Some(l)) => on_line(&l),
                Ok(None) => err_done = true,
                Err(e) => return Err(AgentError::Io(e)),
            },
            s = child.wait(), if exit.is_none() => {
                exit = Some(s.map_err(AgentError::Io)?);
                drain_until = Some(tokio::time::Instant::now() + DRAIN_AFTER_EXIT);
            },
            _ = tokio::time::sleep_until(deadline), if drain_until.is_some() => break,
        }
    }

    let status = match exit {
        Some(s) => s,
        None => child.wait().await.map_err(AgentError::Io)?,
    };
    Ok(status.code().unwrap_or(-1))
}

#[cfg(all(test, unix))]
mod tests {
    use super::{run, run_streaming};
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
    async fn run_reports_failure_with_stderr() {
        let err = run("sh", ["-c", "echo boom >&2; exit 3"]).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("boom") || msg.contains('3'), "{msg}");
    }
}
