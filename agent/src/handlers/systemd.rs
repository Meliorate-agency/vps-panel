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
    let mut rows = parse_list_units(&out.stdout);

    // list-units only has the units systemd keeps in memory, and it drops a
    // service that is stopped and disabled: one stopped from the panel (an
    // app, fail2ban from the Firewall page) left this list, and with it the
    // only place to start it again and the Logs page's unit menu. The unit
    // files say which services are installed and disabled.
    let files = exec::run(
        "systemctl",
        ["list-unit-files", "--type=service", "--state=disabled", "--no-pager", "--no-legend", "--plain"],
    )
    .await
    .map(|o| o.stdout)
    .unwrap_or_default();
    let missing = disabled_units_not_listed(&files, &rows);
    if !missing.is_empty() {
        // `show` loads each unit for the answer: a stopped one reads
        // "loaded inactive dead", as it did in list-units while it was enabled.
        let mut args = vec!["show", "--no-pager", "-p", "Id,LoadState,ActiveState,SubState,Description", "--"];
        args.extend(missing.iter().map(String::as_str));
        let shown = exec::run("systemctl", args).await.map(|o| o.stdout).unwrap_or_default();
        rows.extend(rows_from_show(&shown, &missing));
        // The order list-units gives.
        rows.sort_by_key(|r| r.unit.to_lowercase());
    }
    Ok(Json(rows))
}

/// Rows of `systemctl list-units --plain --no-legend`.
fn parse_list_units(out: &str) -> Vec<ServiceRow> {
    let mut rows = Vec::new();
    for line in out.lines() {
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
    rows
}

/// The services in `systemctl list-unit-files --state=disabled` output that
/// `rows` does not have. Templates (`getty@.service`) are left out: they are
/// not units and cannot be started.
fn disabled_units_not_listed(unit_files: &str, rows: &[ServiceRow]) -> Vec<String> {
    let listed: std::collections::HashSet<&str> = rows.iter().map(|r| r.unit.as_str()).collect();
    let mut missing: Vec<String> = Vec::new();
    for line in unit_files.lines() {
        let mut parts = line.split_whitespace();
        let (Some(unit), Some("disabled")) = (parts.next(), parts.next()) else {
            continue;
        };
        if !unit.ends_with(".service") || unit.ends_with("@.service") || !is_safe_unit_name(unit) {
            continue;
        }
        if !listed.contains(unit) && !missing.iter().any(|m| m == unit) {
            missing.push(unit.to_string());
        }
    }
    missing
}

/// One row per unit in `wanted`, from the blocks `systemctl show` prints
/// (blank-line separated `Key=Value` lines). A unit it did not answer for is
/// listed as stopped.
fn rows_from_show(shown: &str, wanted: &[String]) -> Vec<ServiceRow> {
    let mut by_id: std::collections::HashMap<String, ServiceRow> = std::collections::HashMap::new();
    for block in shown.split("\n\n") {
        let field = |key: &str| {
            block
                .lines()
                .find_map(|l| l.strip_prefix(key))
                .map(|v| v.trim().to_string())
                .unwrap_or_default()
        };
        let unit = field("Id=");
        if unit.is_empty() {
            continue;
        }
        by_id.insert(
            unit.clone(),
            ServiceRow {
                unit,
                load: field("LoadState="),
                active: field("ActiveState="),
                sub: field("SubState="),
                description: field("Description="),
            },
        );
    }
    wanted
        .iter()
        .map(|unit| {
            by_id.remove(unit).unwrap_or_else(|| ServiceRow {
                unit: unit.clone(),
                load: "loaded".into(),
                active: "inactive".into(),
                sub: "dead".into(),
                description: String::new(),
            })
        })
        .collect()
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
    // `--`: a name may start with `-` (`-.mount` is a real unit), and without
    // it `--help` was read as an option and reported as a successful action.
    exec::run("systemctl", [action.as_str(), "--", unit.as_str()]).await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    const LIST_UNITS: &str = "\
cron.service                 loaded    active   running Regular background program processing daemon
fail2ban.service             not-found inactive dead    fail2ban.service
nginx.service                loaded    active   running A high performance web server and a reverse proxy server
";

    #[test]
    fn a_stopped_and_disabled_service_stays_in_the_list() {
        let rows = parse_list_units(LIST_UNITS);
        assert_eq!(rows.len(), 3);
        assert_eq!((rows[2].unit.as_str(), rows[2].sub.as_str()), ("nginx.service", "running"));
        assert_eq!(rows[2].description, "A high performance web server and a reverse proxy server");

        // list-unit-files --state=disabled: unit, state, preset.
        let files = "\
console-getty.service        disabled disabled
nginx.service                disabled enabled
redis-server.service         disabled enabled
serial-getty@.service        disabled enabled
vps-app-shop.service         disabled enabled
ssh.socket                   disabled enabled
evil;rm.service              disabled enabled
";
        let missing = disabled_units_not_listed(files, &rows);
        // Not the one already listed, not the template, only services.
        assert_eq!(missing, ["console-getty.service", "redis-server.service", "vps-app-shop.service"]);

        // systemctl show prints the properties in its own order.
        let shown = "\
Id=console-getty.service
Description=Console Getty
LoadState=loaded
ActiveState=inactive
SubState=dead

Id=redis-server.service
Description=Advanced key-value store
LoadState=loaded
ActiveState=inactive
SubState=dead
";
        let added = rows_from_show(shown, &missing);
        assert_eq!(added.len(), 3);
        assert_eq!(added[1].unit, "redis-server.service");
        assert_eq!(added[1].description, "Advanced key-value store");
        assert_eq!((added[1].load.as_str(), added[1].active.as_str(), added[1].sub.as_str()), ("loaded", "inactive", "dead"));
        // One `show` had no answer for is still there to be started.
        assert_eq!((added[2].unit.as_str(), added[2].sub.as_str()), ("vps-app-shop.service", "dead"));
    }

    #[test]
    fn nothing_is_added_when_every_disabled_service_is_loaded() {
        let rows = parse_list_units(LIST_UNITS);
        assert!(disabled_units_not_listed("nginx.service disabled enabled\n", &rows).is_empty());
        assert!(disabled_units_not_listed("", &rows).is_empty());
    }
}
