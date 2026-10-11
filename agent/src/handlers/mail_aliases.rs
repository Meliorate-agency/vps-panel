//! Forwarders: rows in `mail.virtual_aliases`, which Postfix's
//! `virtual_alias_maps` reads. `source` is a full address (a forwarder) or
//! `@domain` (the domain's catch-all: any address there that is neither a
//! mailbox nor a forwarder of its own). One row forwards to one destination, so
//! a source with three destinations has three rows.
//!
//! The map query (install.sh, update.sh::enable_forwarders) also returns a
//! mailbox's own address, so a forwarder on a mailbox keeps a copy there and a
//! catch-all never takes mail meant for a real mailbox.

use crate::error::{AgentError, AgentResult};
use crate::handlers::mail::{mail_conn, mailbox_parts};
use crate::ipc::OkResponse;
use crate::state::AppState;
use axum::{
    extract::{Path, State},
    Json,
};
use mysql_async::prelude::*;
use serde::{Deserialize, Serialize};

/// More than this many destinations on one source is a mailing list, not a
/// forwarder.
const MAX_DESTINATIONS: usize = 20;

#[derive(Serialize)]
pub struct AliasRow {
    pub id: u32,
    pub source: String,
    pub destination: String,
}

#[derive(Deserialize)]
pub struct CreateAlias {
    pub source: String,
    pub destinations: Vec<String>,
}

#[derive(Serialize)]
pub struct CreateAliasResp {
    /// Destinations added; ones already forwarded to are skipped.
    pub added: u32,
}

/// GET /mail/aliases
pub async fn list(State(state): State<AppState>) -> AgentResult<Json<Vec<AliasRow>>> {
    let mut conn = mail_conn(&state).await?;
    let rows: Vec<(u32, String, String)> = conn
        .query(
            "SELECT id, source, destination FROM virtual_aliases \
             ORDER BY SUBSTRING_INDEX(source, '@', -1), source, destination",
        )
        .await?;
    Ok(Json(
        rows.into_iter()
            .map(|(id, source, destination)| AliasRow { id, source, destination })
            .collect(),
    ))
}

/// Checked, lowercased source and its domain. `@example.com` is the catch-all.
fn alias_source(raw: &str) -> AgentResult<(String, String)> {
    let source = raw.trim().to_lowercase();
    if let Some(domain) = source.strip_prefix('@') {
        if !crate::validate::is_safe_subdomain(domain) {
            return Err(AgentError::Invalid("invalid domain".into()));
        }
        let domain = domain.to_string();
        return Ok((source, domain));
    }
    let (email, _local, domain) = mailbox_parts(&source)?;
    Ok((email, domain))
}

/// Checked, lowercased destinations: no blanks, no repeats, never the source.
fn alias_destinations(source: &str, raw: &[String]) -> AgentResult<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for d in raw.iter().map(|d| d.trim()).filter(|d| !d.is_empty()) {
        let (email, _, _) = mailbox_parts(d)
            .map_err(|_| AgentError::Invalid(format!("not an email address: {d}")))?;
        if email == source {
            return Err(AgentError::Invalid(format!("{email} cannot forward to itself")));
        }
        if !out.contains(&email) {
            out.push(email);
        }
    }
    if out.is_empty() {
        return Err(AgentError::Invalid("add an address to forward to".into()));
    }
    if out.len() > MAX_DESTINATIONS {
        return Err(AgentError::Invalid(format!(
            "at most {MAX_DESTINATIONS} addresses per forwarder"
        )));
    }
    Ok(out)
}

/// POST /mail/aliases
pub async fn create(
    State(state): State<AppState>,
    Json(req): Json<CreateAlias>,
) -> AgentResult<Json<CreateAliasResp>> {
    let (source, domain) = alias_source(&req.source)?;
    let destinations = alias_destinations(&source, &req.destinations)?;

    let mut conn = mail_conn(&state).await?;
    let domain_id: u32 = conn
        .exec_first("SELECT id FROM virtual_domains WHERE name = ?", (&domain,))
        .await?
        .ok_or_else(|| {
            AgentError::Invalid(format!(
                "{domain} does not receive mail here yet: create a mailbox on it first"
            ))
        })?;

    let mut added = 0;
    for destination in &destinations {
        let exists: Option<u32> = conn
            .exec_first(
                "SELECT id FROM virtual_aliases WHERE source = ? AND destination = ?",
                (&source, destination),
            )
            .await?;
        if exists.is_some() {
            continue;
        }
        conn.exec_drop(
            "INSERT INTO virtual_aliases (domain_id, source, destination) VALUES (?, ?, ?)",
            (domain_id, &source, destination),
        )
        .await?;
        added += 1;
    }
    Ok(Json(CreateAliasResp { added }))
}

/// DELETE /mail/aliases/{id}
pub async fn delete(
    State(state): State<AppState>,
    Path(id): Path<u32>,
) -> AgentResult<Json<OkResponse>> {
    let mut conn = mail_conn(&state).await?;
    conn.exec_drop("DELETE FROM virtual_aliases WHERE id = ?", (id,))
        .await?;
    if conn.affected_rows() == 0 {
        return Err(AgentError::NotFound);
    }
    Ok(Json(OkResponse::default()))
}

#[cfg(test)]
mod tests {
    use super::{alias_destinations, alias_source};

    #[test]
    fn sources_are_an_address_or_a_domain_catch_all() {
        assert_eq!(
            alias_source(" Sales@Example.com ").unwrap(),
            ("sales@example.com".into(), "example.com".into())
        );
        assert_eq!(
            alias_source("@Example.com").unwrap(),
            ("@example.com".into(), "example.com".into())
        );
        for bad in ["@", "@../etc", "sales", "sales@", "a b@example.com", "@exa mple.com"] {
            assert!(alias_source(bad).is_err(), "{bad} should be refused");
        }
    }

    #[test]
    fn destinations_are_deduplicated_and_never_the_source() {
        let got = alias_destinations(
            "sales@example.com",
            &["Jane@Example.com".into(), " ".into(), "jane@example.com".into(), "x@gmail.com".into()],
        )
        .unwrap();
        assert_eq!(got, vec!["jane@example.com".to_string(), "x@gmail.com".to_string()]);
        assert!(alias_destinations("sales@example.com", &["SALES@example.com".into()]).is_err());
        assert!(alias_destinations("sales@example.com", &[]).is_err());
        assert!(alias_destinations("sales@example.com", &["not-an-address".into()]).is_err());
        let many: Vec<String> = (0..21).map(|i| format!("u{i}@example.com")).collect();
        assert!(alias_destinations("sales@example.com", &many).is_err());
    }
}
