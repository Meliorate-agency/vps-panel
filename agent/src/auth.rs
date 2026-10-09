use crate::error::AgentError;
use axum::{
    extract::State,
    http::{header, Request},
    middleware::Next,
    response::Response,
};
use subtle::ConstantTimeEq;

use crate::state::AppState;

/// Who is on the other end of the unix socket (SO_PEERCRED).
#[derive(Clone, Debug)]
pub struct PeerCred {
    pub uid: Option<u32>,
}

#[cfg(unix)]
impl axum::extract::connect_info::Connected<axum::serve::IncomingStream<'_, tokio::net::UnixListener>>
    for PeerCred
{
    fn connect_info(stream: axum::serve::IncomingStream<'_, tokio::net::UnixListener>) -> Self {
        PeerCred {
            uid: stream.io().peer_cred().ok().map(|c| c.uid()),
        }
    }
}

pub async fn bearer_auth(
    State(state): State<AppState>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, AgentError> {
    // Only root and the panel's own user may talk to the agent, whatever
    // else can open the socket or has read the token. www-data used to be in
    // the socket's group and could read the token.
    if let Some(panel_uid) = state.config.vps_panel_uid {
        let peer = req
            .extensions()
            .get::<axum::extract::ConnectInfo<PeerCred>>()
            .and_then(|c| c.0.uid);
        if !matches!(peer, Some(uid) if uid == 0 || uid == panel_uid) {
            tracing::warn!(?peer, "refused an agent connection from another user");
            return Err(AgentError::Forbidden);
        }
    }

    let header_value = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or(AgentError::Unauthorized)?;

    let token = header_value
        .strip_prefix("Bearer ")
        .ok_or(AgentError::Unauthorized)?;

    let expected = state.config.agent_token.as_bytes();
    let provided = token.as_bytes();
    if expected.len() != provided.len() || expected.ct_eq(provided).unwrap_u8() == 0 {
        return Err(AgentError::Unauthorized);
    }

    Ok(next.run(req).await)
}
