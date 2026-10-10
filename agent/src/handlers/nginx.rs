use crate::error::AgentError;
use crate::exec;
use crate::fs_atomic;
use crate::ipc::OkResponse;
use axum::Json;
use std::path::Path;

/// http-level settings for every vhost, pulled in by the stock nginx.conf's
/// `include /etc/nginx/conf.d/*.conf;`. nginx sizes server_names_hash_bucket_size
/// to the CPU cache line (64), which fits server names up to 46 characters once
/// several vhosts share a port: a longer one failed `nginx -t` ("could not
/// build server_names_hash") and its site was rolled back. 128 fits up to 110.
pub const HTTP_DEFAULTS_CONF: &str = "/etc/nginx/conf.d/vps-panel.conf";
pub const HTTP_DEFAULTS_BODY: &str = "# Managed by vps-panel (agent/src/handlers/nginx.rs).\n\
# Server names up to 110 characters; nginx's default fits 46.\n\
server_names_hash_bucket_size 128;\n";

/// Install the drop-in above. Runs at every agent start, so existing boxes pick
/// it up on their next update. No-op when the file is already current. Put back
/// as it was when `nginx -t` refuses it (e.g. the operator already set the
/// directive in nginx.conf, which makes ours a duplicate).
pub async fn ensure_http_defaults() {
    let path = Path::new(HTTP_DEFAULTS_CONF);
    let previous = tokio::fs::read_to_string(path).await.ok();
    if previous.as_deref() == Some(HTTP_DEFAULTS_BODY) {
        return;
    }
    if let Err(e) = fs_atomic::write(path, HTTP_DEFAULTS_BODY.as_bytes(), 0o644).await {
        tracing::warn!(error = %e, "could not write {HTTP_DEFAULTS_CONF}");
        return;
    }
    if let Err(e) = test_config().await {
        let _ = match previous {
            Some(body) => fs_atomic::write(path, body.as_bytes(), 0o644).await,
            None => fs_atomic::remove_if_exists(path).await,
        };
        tracing::warn!(error = %e, "nginx -t refused {HTTP_DEFAULTS_CONF}; left it out");
        return;
    }
    let _ = exec::run("systemctl", ["reload", "nginx"]).await;
    tracing::info!("installed {HTTP_DEFAULTS_CONF}");
}

pub async fn reload() -> Result<Json<OkResponse>, AgentError> {
    test_config().await?;
    exec::run("systemctl", ["reload", "nginx"]).await?;
    Ok(Json(OkResponse::default()))
}

pub async fn test_config() -> Result<(), AgentError> {
    exec::run("nginx", ["-t"]).await.map(|_| ())
}
