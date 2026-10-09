use crate::error::{AgentError, AgentResult};

pub fn validate_name(name: &str) -> AgentResult<()> {
    let re = regex::Regex::new(r"^[a-z0-9][a-z0-9-]{0,30}[a-z0-9]$").unwrap();
    if !re.is_match(name) {
        return Err(AgentError::Invalid(
            "name must be lowercase alphanumeric with hyphens, 2-32 chars".into(),
        ));
    }
    Ok(())
}

pub fn is_safe_subdomain(s: &str) -> bool {
    let re = regex::Regex::new(
        r"^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?(\.[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)+$",
    )
    .unwrap();
    re.is_match(s)
}

/// A folder a static site may serve from. The agent creates it and runs
/// `chown -R www-data:www-data` on it, so it must be a folder of its own
/// under /var/www or /srv/drive: any absolute path was accepted, and a folder
/// site on `/etc` or `/` would have handed the system to www-data.
pub fn is_safe_folder_path(p: &str) -> bool {
    if p.is_empty() || p.len() > 256 {
        return false;
    }
    let Some(rest) = p.strip_prefix("/var/www/").or_else(|| p.strip_prefix("/srv/drive/")) else {
        return false;
    };
    let rest = rest.trim_end_matches('/');
    if rest.is_empty() || rest.split('/').any(|seg| seg.is_empty() || seg == "." || seg == "..") {
        return false;
    }
    // The ACME webroot lives under /var/www too; a site must not take it.
    if p.trim_end_matches('/') == crate::nginx_template::ACME_WEBROOT
        || p.starts_with(&format!("{}/", crate::nginx_template::ACME_WEBROOT))
    {
        return false;
    }
    p.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-'))
}

pub fn validate_acme_email(email: &str) -> AgentResult<()> {
    let (local, domain) = email.split_once('@').ok_or_else(|| {
        AgentError::Invalid(format!(
            "ACME_EMAIL '{email}' is invalid: missing '@'. Set ACME_EMAIL in /etc/vps-panel/.env to a real address (e.g. you@yourdomain.com)."
        ))
    })?;
    if local.is_empty() {
        return Err(AgentError::Invalid(format!(
            "ACME_EMAIL '{email}' is invalid: empty local part."
        )));
    }
    if !domain.contains('.') {
        return Err(AgentError::Invalid(format!(
            "ACME_EMAIL '{email}' is invalid: domain '{domain}' has no dot. Let's Encrypt rejects single-label domains. Set ACME_EMAIL in /etc/vps-panel/.env to a real address (e.g. you@yourdomain.com), then `systemctl restart vps-panel-agent`."
        )));
    }
    if email.chars().any(char::is_whitespace) {
        return Err(AgentError::Invalid(format!(
            "ACME_EMAIL '{email}' is invalid: contains whitespace."
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::is_safe_folder_path;

    #[test]
    fn folder_sites_only_under_var_www_or_drive() {
        assert!(is_safe_folder_path("/var/www/example.com"));
        assert!(is_safe_folder_path("/var/www/example.com/public/"));
        assert!(is_safe_folder_path("/srv/drive/sites/blog"));
        for bad in [
            "/", "/etc", "/var/www", "/var/www/", "/var/www/../etc", "/var/www//x",
            "/srv/apps/app", "/var/lib/vps-panel", "/var/www/vps-panel-acme",
            "/var/www/vps-panel-acme/x", "/var/www/a b", "var/www/x",
        ] {
            assert!(!is_safe_folder_path(bad), "{bad} should be refused");
        }
    }
}
