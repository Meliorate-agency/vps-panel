//! Server snapshots over HTTP (Backups > Server snapshot): list, size
//! estimate, export, delete and the file itself. The work is in
//! crate::snapshot.

use crate::error::{AgentError, AgentResult};
use crate::ipc::{OkResponse, TaskAccepted};
use crate::snapshot::export::{self, Estimate, ExportReq};
use crate::snapshot::format::{self, Sidecar};
use crate::snapshot::http_range;
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method};
use axum::response::Response;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize)]
pub struct Item {
    #[serde(flatten)]
    pub sidecar: Sidecar,
    /// "local" (made here) or "incoming" (pulled from another server).
    pub location: &'static str,
}

#[derive(Serialize)]
pub struct ListResp {
    pub snapshots: Vec<Item>,
    pub free_bytes: Option<u64>,
}

const DIRS: [(&str, &str); 2] = [(format::SNAPSHOT_DIR, "local"), (format::INCOMING_DIR, "incoming")];

pub async fn list() -> AgentResult<Json<ListResp>> {
    let mut snapshots = Vec::new();
    for (dir, location) in DIRS {
        let Ok(mut rd) = tokio::fs::read_dir(dir).await else { continue };
        while let Ok(Some(e)) = rd.next_entry().await {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(stem) = format::stem_of(&name) else { continue };
            let file = e.path();
            let Ok(meta) = tokio::fs::metadata(&file).await else { continue };
            let sidecar = read_sidecar(dir, stem).await.unwrap_or_else(|| Sidecar {
                name: stem.to_string(),
                size: meta.len(),
                origin: location.to_string(),
                ..Default::default()
            });
            snapshots.push(Item { sidecar, location });
        }
    }
    snapshots.sort_by(|a, b| b.sidecar.created_at.cmp(&a.sidecar.created_at).then(b.sidecar.name.cmp(&a.sidecar.name)));
    let free_bytes = crate::handlers::files::available_bytes(std::path::Path::new(format::SNAPSHOT_DIR))
        .or_else(|| crate::handlers::files::available_bytes(std::path::Path::new("/var/backups")));
    Ok(Json(ListResp { snapshots, free_bytes }))
}

async fn read_sidecar(dir: &str, stem: &str) -> Option<Sidecar> {
    let text = tokio::fs::read(PathBuf::from(dir).join(format!("{stem}.json"))).await.ok()?;
    let s: Sidecar = serde_json::from_slice(&text).ok()?;
    (s.name == stem).then_some(s)
}

/// The snapshot file named by `stem`, wherever it is, and its folder.
async fn locate(stem: &str) -> AgentResult<(PathBuf, &'static str)> {
    if !format::is_valid_stem(stem) {
        return Err(AgentError::Invalid("not a snapshot name".into()));
    }
    for (dir, _) in DIRS {
        let path = PathBuf::from(dir).join(format::file_name(stem));
        if tokio::fs::symlink_metadata(&path).await.is_ok_and(|m| m.is_file()) {
            return Ok((path, dir));
        }
    }
    Err(AgentError::NotFound)
}

#[derive(Deserialize)]
pub struct EstimateQuery {
    #[serde(default)]
    pub drive: bool,
}

pub async fn estimate(State(state): State<AppState>, Query(q): Query<EstimateQuery>) -> Json<Estimate> {
    Json(export::estimate(&state, q.drive).await)
}

pub async fn create(State(state): State<AppState>, Json(req): Json<ExportReq>) -> AgentResult<Json<TaskAccepted>> {
    let task_id = export::start(state, req).await?;
    Ok(Json(TaskAccepted { task_id }))
}

pub async fn delete(State(state): State<AppState>, Path(stem): Path<String>) -> AgentResult<Json<OkResponse>> {
    if state.tasks.any_live_with_prefix("snapshots.") {
        return Err(AgentError::Conflict(
            "a server snapshot is being made, pulled or imported; delete it when that is done".into(),
        ));
    }
    let (path, dir) = locate(&stem).await?;
    tokio::fs::remove_file(&path).await?;
    let _ = tokio::fs::remove_file(PathBuf::from(dir).join(format!("{stem}.json"))).await;
    Ok(Json(OkResponse::default()))
}

pub async fn file(
    Path(stem): Path<String>,
    method: Method,
    headers: HeaderMap,
) -> AgentResult<Response> {
    let (path, dir) = locate(&stem).await?;
    let etag = match read_sidecar(dir, &stem).await {
        Some(s) if !s.sha256.is_empty() => format!("\"{}\"", s.sha256),
        _ => {
            use std::os::unix::fs::MetadataExt;
            let m = tokio::fs::metadata(&path).await?;
            format!("\"{}-{}\"", m.len(), m.mtime())
        }
    };
    http_range::serve(&path, &method, &headers, &etag, &format::file_name(&stem)).await
}
