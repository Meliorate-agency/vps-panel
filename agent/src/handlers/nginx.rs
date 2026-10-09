use crate::error::AgentError;
use crate::exec;
use crate::ipc::OkResponse;
use axum::Json;

pub async fn reload() -> Result<Json<OkResponse>, AgentError> {
    test_config().await?;
    exec::run("systemctl", ["reload", "nginx"]).await?;
    Ok(Json(OkResponse::default()))
}

pub async fn test_config() -> Result<(), AgentError> {
    exec::run("nginx", ["-t"]).await.map(|_| ())
}
