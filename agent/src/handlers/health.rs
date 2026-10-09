use axum::Json;
use serde::Serialize;

#[derive(Serialize)]
pub struct Health {
    pub ok: bool,
    pub version: &'static str,
    pub now: chrono::DateTime<chrono::Utc>,
}

pub async fn get() -> Json<Health> {
    Json(Health {
        ok: true,
        version: env!("CARGO_PKG_VERSION"),
        now: chrono::Utc::now(),
    })
}
