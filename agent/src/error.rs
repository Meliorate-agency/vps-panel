use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use thiserror::Error;

pub type AgentResult<T> = Result<T, AgentError>;

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("unauthorized")]
    Unauthorized,

    #[allow(dead_code)] // kept available for future 403-class handlers
    #[error("forbidden")]
    Forbidden,

    #[error("not found")]
    NotFound,

    #[error("conflict: {0}")]
    Conflict(String),

    #[error("invalid input: {0}")]
    Invalid(String),

    #[error("subprocess `{cmd}` failed (exit {code}): {stderr}")]
    Subprocess {
        cmd: String,
        code: i32,
        stderr: String,
    },

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("mariadb: {0}")]
    MariaDb(#[from] mysql_async::Error),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
    code: &'static str,
}

impl IntoResponse for AgentError {
    fn into_response(self) -> Response {
        let (status, code) = match &self {
            AgentError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            AgentError::Forbidden => (StatusCode::FORBIDDEN, "forbidden"),
            AgentError::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            AgentError::Conflict(_) => (StatusCode::CONFLICT, "conflict"),
            AgentError::Invalid(_) => (StatusCode::BAD_REQUEST, "invalid"),
            AgentError::Subprocess { .. } => (StatusCode::INTERNAL_SERVER_ERROR, "subprocess"),
            AgentError::Io(_) => (StatusCode::INTERNAL_SERVER_ERROR, "io"),
            AgentError::Sqlite(_) => (StatusCode::INTERNAL_SERVER_ERROR, "sqlite"),
            AgentError::MariaDb(_) => (StatusCode::INTERNAL_SERVER_ERROR, "mariadb"),
            AgentError::Other(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        };
        if status.is_server_error() {
            tracing::error!("{self}");
        }
        (
            status,
            Json(ErrorBody {
                error: self.to_string(),
                code,
            }),
        )
            .into_response()
    }
}
