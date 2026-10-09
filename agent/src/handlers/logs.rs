use crate::error::AgentError;
use axum::{
    extract::{Path, Query},
    response::sse::{Event, KeepAlive, Sse},
};
use futures::stream::Stream;
use serde::Deserialize;
use std::convert::Infallible;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

#[derive(Deserialize)]
pub struct LogQuery {
    #[serde(default = "default_lines")]
    pub lines: u32,
}

fn default_lines() -> u32 {
    200
}

pub async fn stream(
    Path(unit): Path<String>,
    Query(q): Query<LogQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, AgentError> {
    if !crate::handlers::systemd::is_safe_unit_name(&unit) {
        return Err(AgentError::Invalid("invalid unit name".into()));
    }
    let lines = q.lines.min(2000).to_string();

    let mut child = Command::new("journalctl")
        .args([
            "-u",
            &unit,
            "-n",
            &lines,
            "-f",
            "--no-pager",
            "--output=short-iso",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // When the viewer disconnects the SSE stream is dropped before the
        // loop below ends; without this the journalctl -f child lived on
        // until the unit happened to log again.
        .kill_on_drop(true)
        .spawn()
        .map_err(AgentError::Io)?;

    let stdout = child.stdout.take().expect("stdout piped");
    let stream = async_stream::stream! {
        let mut reader = BufReader::new(stdout).lines();
        loop {
            match reader.next_line().await {
                Ok(Some(line)) => {
                    yield Ok(Event::default().event("line").data(line));
                }
                Ok(None) => break,
                Err(_) => break,
            }
        }
        let _ = child.kill().await;
    };

    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}
