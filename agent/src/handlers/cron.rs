use crate::error::{AgentError, AgentResult};
use crate::fs_atomic;
use crate::ipc::OkResponse;
use axum::{extract::Path, Json};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::fs;
use uuid::Uuid;

#[derive(Deserialize)]
pub struct CreateJob {
    pub schedule: String,
    pub user: String,
    pub command: String,
    pub label: Option<String>,
}

#[derive(Serialize)]
pub struct JobRow {
    pub id: String,
    pub schedule: String,
    pub user: String,
    pub command: String,
    pub label: String,
}

const CRON_DIR: &str = "/etc/cron.d";

pub async fn list() -> AgentResult<Json<Vec<JobRow>>> {
    let mut entries = fs::read_dir(CRON_DIR).await.map_err(AgentError::Io)?;
    let mut rows = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        let id = match name.strip_prefix("vps-panel-") {
            Some(id) => id.to_string(),
            None => continue,
        };
        let body = match fs::read_to_string(entry.path()).await {
            Ok(b) => b,
            Err(_) => continue,
        };
        let mut label = String::new();
        let mut job = None;
        for l in body.lines() {
            if let Some(rest) = l.strip_prefix("# label: ") {
                label = rest.to_string();
            } else if let Some(parsed) = parse_job_line(l) {
                job = Some(parsed);
                break;
            }
        }
        let Some((schedule, user, command)) = job else {
            continue;
        };
        rows.push(JobRow {
            id,
            schedule,
            user,
            command,
            label,
        });
    }
    Ok(Json(rows))
}

pub async fn create(Json(req): Json<CreateJob>) -> AgentResult<Json<JobRow>> {
    if !is_valid_cron_spec(&req.schedule) {
        return Err(AgentError::Invalid("invalid cron spec".into()));
    }
    if !is_safe_username(&req.user) {
        return Err(AgentError::Invalid("invalid user".into()));
    }
    // Verify the user actually exists in /etc/passwd. Without this, cron
    // silently skips the job at runtime ("CRON: cannot find user xyz" in
    // syslog) and the operator sees the schedule in the panel but the
    // command never runs.
    #[cfg(unix)]
    {
        use nix::unistd::User;
        let exists = User::from_name(&req.user)
            .map_err(|e| AgentError::Other(anyhow::anyhow!("getpwnam: {e}")))?
            .is_some();
        if !exists {
            return Err(AgentError::Invalid(format!(
                "user {:?} does not exist on this system",
                req.user
            )));
        }
    }
    // \r (not just \n) also ends a crontab line.
    if req.command.chars().any(|c| c.is_control()) {
        return Err(AgentError::Invalid("command must be a single line".into()));
    }
    if req.command.trim().is_empty() {
        return Err(AgentError::Invalid("command must not be empty".into()));
    }
    let id = Uuid::new_v4().to_string();
    let label = req.label.clone().unwrap_or_default();
    let schedule = req.schedule.split_whitespace().collect::<Vec<_>>().join(" ");
    let body = format!(
        "# managed-by: vps-panel\n# label: {}\nSHELL=/bin/sh\nPATH=/usr/local/sbin:/usr/local/bin:/sbin:/bin:/usr/sbin:/usr/bin\n{} {} {}\n",
        sanitize_label(&label),
        schedule,
        req.user,
        escape_percent(&req.command),
    );
    let path = cron_path(&id);
    fs_atomic::write(&path, body.as_bytes(), 0o644).await?;
    Ok(Json(JobRow {
        id,
        schedule,
        user: req.user,
        command: req.command,
        label,
    }))
}

pub async fn delete(Path(id): Path<String>) -> AgentResult<Json<OkResponse>> {
    if !is_safe_id(&id) {
        return Err(AgentError::Invalid("invalid id".into()));
    }
    fs_atomic::remove_if_exists(&cron_path(&id)).await?;
    Ok(Json(OkResponse::default()))
}

fn cron_path(id: &str) -> PathBuf {
    PathBuf::from(format!("{CRON_DIR}/vps-panel-{id}"))
}

fn is_safe_username(s: &str) -> bool {
    let re = regex::Regex::new(r"^[a-z_][a-z0-9_-]{0,30}$").unwrap();
    re.is_match(s)
}

fn is_safe_id(s: &str) -> bool {
    let re = regex::Regex::new(r"^[a-zA-Z0-9-]{1,64}$").unwrap();
    re.is_match(s)
}

fn is_valid_cron_spec(s: &str) -> bool {
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() != 5 {
        return false;
    }
    let allowed = regex::Regex::new(r"^[0-9*/,\-]+$").unwrap();
    parts.iter().all(|p| allowed.is_match(p))
}

/// Cron turns an unescaped `%` into a newline and feeds the rest to stdin,
/// so `date +%F` was silently cut short at the first `%`.
fn escape_percent(command: &str) -> String {
    command.replace('%', "\\%")
}

/// A job line of a /etc/cron.d file: five schedule fields, the user, then the
/// command (inner spacing kept, `\%` shown as `%`). None for comments, blank
/// lines and `NAME=value` environment lines: panel jobs start with
/// SHELL=/PATH= lines, and the list used to show "SHELL=/bin/sh" as the
/// schedule with an empty command.
fn parse_job_line(line: &str) -> Option<(String, String, String)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let first = trimmed.split_whitespace().next()?;
    if let Some((name, _)) = first.split_once('=') {
        if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return None;
        }
    }
    let mut rest = trimmed;
    let mut fields = Vec::with_capacity(6);
    for _ in 0..6 {
        let start = rest.find(|c: char| !c.is_whitespace())?;
        rest = &rest[start..];
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        fields.push(&rest[..end]);
        rest = &rest[end..];
    }
    let command = rest.trim_start().replace("\\%", "%");
    let schedule = fields[..5].join(" ");
    Some((schedule, fields[5].to_string(), command))
}

fn sanitize_label(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'))
        .take(80)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{escape_percent, parse_job_line};

    #[test]
    fn env_lines_and_comments_are_not_jobs() {
        assert!(parse_job_line("SHELL=/bin/sh").is_none());
        assert!(parse_job_line("PATH=/usr/local/sbin:/usr/bin").is_none());
        assert!(parse_job_line("# label: nightly").is_none());
        assert!(parse_job_line("   ").is_none());
    }

    #[test]
    fn job_line_keeps_command_spacing_and_unescapes_percent() {
        let (schedule, user, command) =
            parse_job_line("0  3 * * *   root  tar czf /b/$(date +\\%F).tgz  /srv").unwrap();
        assert_eq!(schedule, "0 3 * * *");
        assert_eq!(user, "root");
        assert_eq!(command, "tar czf /b/$(date +%F).tgz  /srv");
    }

    #[test]
    fn percent_is_escaped_for_cron() {
        assert_eq!(escape_percent("date +%F"), "date +\\%F");
    }
}
