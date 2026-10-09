use crate::error::AgentError;
use crate::exec;
use crate::ipc::OkResponse;
use axum::{extract::Path, Json};
use serde::Serialize;

#[derive(Serialize)]
pub struct ServiceRow {
    pub unit: String,
    pub load: String,
    pub active: String,
    pub sub: String,
    pub description: String,
}

pub async fn list_services() -> Result<Json<Vec<ServiceRow>>, AgentError> {
    let out = exec::run(
        "systemctl",
        ["list-units", "--type=service", "--all", "--no-pager", "--no-legend", "--plain"],
    )
    .await?;

    let mut rows = Vec::new();
    for line in out.stdout.lines() {
        let mut parts = line.split_whitespace();
        let unit = parts.next().unwrap_or("").to_string();
        let load = parts.next().unwrap_or("").to_string();
        let active = parts.next().unwrap_or("").to_string();
        let sub = parts.next().unwrap_or("").to_string();
        let description = parts.collect::<Vec<_>>().join(" ");
        if unit.is_empty() {
            continue;
        }
        rows.push(ServiceRow {
            unit,
            load,
            active,
            sub,
            description,
        });
    }
    Ok(Json(rows))
}

pub async fn service_action(
    Path((unit, action)): Path<(String, String)>,
) -> Result<Json<OkResponse>, AgentError> {
    let action = match action.as_str() {
        "start" | "stop" | "restart" | "reload" | "enable" | "disable" => action,
        other => return Err(AgentError::Invalid(format!("unsupported action: {other}"))),
    };
    if !is_safe_unit_name(&unit) {
        return Err(AgentError::Invalid("invalid unit name".into()));
    }
    exec::run("systemctl", [action.as_str(), unit.as_str()]).await?;
    Ok(Json(OkResponse::default()))
}

pub fn is_safe_unit_name(unit: &str) -> bool {
    !unit.is_empty()
        && unit.len() < 200
        && unit
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '@' | ':'))
        && !unit.contains("..")
}
