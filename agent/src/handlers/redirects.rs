//! Path redirects (301 / 302) on panel-owned vhosts.
//!
//! The panel's SQLite `redirects` table is the source of truth; this handler
//! is a pure renderer. `PUT /redirects/{domain}` receives the FULL rule set
//! for one domain and makes disk match it:
//!
//!   1. writes `/etc/nginx/vps-redirects/<domain>/redirects.conf` (or removes
//!      the file + directory when the set is empty),
//!   2. makes sure the domain's vhost carries
//!      `include /etc/nginx/vps-redirects/<domain>/*.conf;` inside each of its
//!      `server {}` blocks - the templates emit it, and vhosts rendered before
//!      this feature existed get the line injected after `server_name <domain>;`,
//!   3. `nginx -t` (rolling both files back on failure) + `systemctl reload nginx`.
//!
//! The include is a glob on purpose: nginx accepts a wildcard that matches
//! nothing, so a vhost can carry the line permanently while the per-domain
//! directory only exists for as long as rules do.
//!
//! Rendering rules:
//!   - exact  -> `location = /old { return 301 "https://target$is_args$args"; }`
//!   - prefix -> `location ~ ^/old(?:/(.*))?$ { return 301 "https://target/$1$is_args$args"; }`
//!               (keeps the remainder of the path; longest source first so a
//!               more specific prefix wins over a shorter one)
//!   - `$is_args$args` carries the request's query string over. A target with
//!     a query string of its own is sent as written.
//!   - a prefix rule on `/` (whole domain) excludes `/.well-known/acme-challenge/`
//!     so Let's Encrypt renewals keep working on vhosts whose ACME location
//!     predates the `^~` modifier.

use crate::error::{AgentError, AgentResult};
use crate::exec;
use crate::fs_atomic;
use crate::handlers::nginx as nginx_handler;
use crate::ipc::OkResponse;
use crate::state::AppState;
use crate::validate::is_safe_subdomain;
use axum::{
    extract::{Path, State},
    Json,
};
use serde::Deserialize;
use std::path::PathBuf;

pub const REDIRECTS_ROOT: &str = "/etc/nginx/vps-redirects";

#[derive(Clone, Deserialize)]
pub struct RedirectRule {
    pub id: i64,
    pub source_path: String,
    pub target_url: String,
    pub code: u16,
    pub match_mode: String,
}

#[derive(Deserialize)]
pub struct PutRedirects {
    #[serde(default)]
    pub rules: Vec<RedirectRule>,
}

fn domain_dir(domain: &str) -> PathBuf {
    PathBuf::from(format!("{REDIRECTS_ROOT}/{domain}"))
}

fn conf_path(domain: &str) -> PathBuf {
    domain_dir(domain).join("redirects.conf")
}

/// The line every panel-owned vhost carries inside its `server {}` blocks.
pub fn include_line(domain: &str) -> String {
    format!("include {REDIRECTS_ROOT}/{domain}/*.conf;")
}

pub fn validate_source_path(p: &str) -> AgentResult<()> {
    if !p.starts_with('/') {
        return Err(AgentError::Invalid("source path must start with /".into()));
    }
    if p.len() > 512 {
        return Err(AgentError::Invalid(
            "source path too long (max 512 chars)".into(),
        ));
    }
    // Strict charset: nothing that nginx or PCRE could interpret. `.` is the
    // only regex metacharacter allowed through and gets escaped at render.
    if !p
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '.' | '~' | '%' | '-'))
    {
        return Err(AgentError::Invalid(
            "source path may only contain letters, digits and / _ . ~ % -".into(),
        ));
    }
    if p.split('/').any(|seg| seg == "..") {
        return Err(AgentError::Invalid("source path may not contain ..".into()));
    }
    if p.starts_with("/.well-known") {
        return Err(AgentError::Invalid(
            "/.well-known is reserved for Let's Encrypt validation".into(),
        ));
    }
    Ok(())
}

pub fn validate_target_url(t: &str) -> AgentResult<()> {
    if !(t.starts_with("https://") || t.starts_with("http://") || t.starts_with('/')) {
        return Err(AgentError::Invalid(
            "target must start with https://, http:// or /".into(),
        ));
    }
    if t.len() > 2048 {
        return Err(AgentError::Invalid("target too long (max 2048 chars)".into()));
    }
    // The value is emitted inside double quotes. Reject anything that could
    // terminate the string, start a variable, or end the directive: `"`, `\`,
    // `$`, `;`, `{`, `}`, whitespace and control characters.
    if !t.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || matches!(
                c,
                '-' | '.'
                    | '_'
                    | '~'
                    | ':'
                    | '/'
                    | '?'
                    | '='
                    | '&'
                    | '%'
                    | '+'
                    | ','
                    | '@'
                    | '!'
                    | '*'
                    | '('
                    | ')'
                    | '['
                    | ']'
                    | '#'
            )
    }) {
        return Err(AgentError::Invalid(
            "target contains characters that are not allowed in a redirect (no spaces, quotes, $ ; { } \\ < >)".into(),
        ));
    }
    Ok(())
}

fn validate_code(code: u16) -> AgentResult<()> {
    if code == 301 || code == 302 {
        Ok(())
    } else {
        Err(AgentError::Invalid(format!(
            "code must be 301 or 302 (got {code})"
        )))
    }
}

/// Render the per-domain include file. Pure: validated input in, nginx
/// config text out.
pub fn render(domain: &str, rules: &[RedirectRule]) -> AgentResult<String> {
    let mut exact: Vec<&RedirectRule> = Vec::new();
    let mut prefix: Vec<&RedirectRule> = Vec::new();
    let mut seen: Vec<(String, String)> = Vec::new();
    for r in rules {
        validate_source_path(&r.source_path)?;
        validate_target_url(&r.target_url)?;
        validate_code(r.code)?;
        match r.match_mode.as_str() {
            "exact" => exact.push(r),
            "prefix" => prefix.push(r),
            other => {
                return Err(AgentError::Invalid(format!(
                    "match_mode must be 'exact' or 'prefix' (got '{other}')"
                )))
            }
        }
        if loops_back(domain, r) {
            return Err(AgentError::Invalid(format!(
                "the {} redirect for {} points back at itself ({}): every visit would loop",
                r.match_mode, r.source_path, r.target_url
            )));
        }
        // `/blog/` and `/blog` are one prefix, but two different exact
        // locations: both may have a rule.
        let source = match r.match_mode.as_str() {
            "prefix" => prefix_source(&r.source_path),
            _ => r.source_path.as_str(),
        };
        let key = (r.match_mode.clone(), source.to_string());
        if seen.contains(&key) {
            return Err(AgentError::Invalid(format!(
                "duplicate {} redirect for {}",
                r.match_mode, r.source_path
            )));
        }
        seen.push(key);
    }
    // nginx tests regex locations in file order and takes the first match,
    // so the longer (more specific) prefix has to come first.
    prefix.sort_by(|a, b| {
        b.source_path
            .len()
            .cmp(&a.source_path.len())
            .then_with(|| a.source_path.cmp(&b.source_path))
    });

    let mut out = String::new();
    out.push_str(&format!("# vps-redirects:{domain}\n"));
    out.push_str(
        "# Managed by vps-panel (Redirects page). Rebuilt on every change - do not edit by hand.\n",
    );
    for r in exact {
        out.push_str(&format!(
            "\n# id={} exact {} {} -> {}\n",
            r.id, r.code, r.source_path, r.target_url
        ));
        out.push_str(&format!(
            "location = {} {{ return {} \"{}\"; }}\n",
            r.source_path,
            r.code,
            with_request_query(&r.target_url)
        ));
    }
    for r in prefix {
        let src = prefix_source(&r.source_path);
        let dest = with_request_query(&format!("{}/$1", r.target_url.trim_end_matches('/')));
        out.push_str(&format!(
            "\n# id={} prefix {} {} -> {}\n",
            r.id, r.code, r.source_path, r.target_url
        ));
        if src.is_empty() {
            out.push_str(&format!(
                "location ~ ^/(?!\\.well-known/acme-challenge/)(.*)$ {{ return {} \"{dest}\"; }}\n",
                r.code
            ));
        } else {
            let esc = src.replace('.', "\\.");
            out.push_str(&format!(
                "location ~ ^{esc}(?:/(.*))?$ {{ return {} \"{dest}\"; }}\n",
                r.code
            ));
        }
    }
    Ok(out)
}

/// Where a rule sends the visitor, with the request's query string carried
/// over: `$is_args$args` is `?a=b`, or nothing for a request without one.
/// `/old?utm_source=x` used to arrive at the target without `utm_source=x`.
/// It goes before a `#fragment`. A destination that has a query string of
/// its own stays as written: nginx cannot merge the two.
fn with_request_query(dest: &str) -> String {
    let (base, fragment) = match dest.split_once('#') {
        Some((base, fragment)) => (base, Some(fragment)),
        None => (dest, None),
    };
    if base.contains('?') {
        return dest.to_string();
    }
    match fragment {
        Some(fragment) => format!("{base}$is_args$args#{fragment}"),
        None => format!("{base}$is_args$args"),
    }
}

/// `/blog/` and `/blog` are the same prefix; `/` becomes the empty string
/// (whole-domain rule).
fn prefix_source(p: &str) -> &str {
    p.trim_end_matches('/')
}

/// The path a target leads to on `domain` itself, without query or fragment;
/// None when it goes to another host.
fn same_site_path<'a>(domain: &str, target: &'a str) -> Option<&'a str> {
    let with_host = target
        .strip_prefix("https://")
        .or_else(|| target.strip_prefix("http://"))
        .or_else(|| target.strip_prefix("//"));
    let rest = match with_host {
        Some(after) => {
            let host_end = after.find(['/', '?', '#']).unwrap_or(after.len());
            if !after[..host_end].eq_ignore_ascii_case(domain) {
                return None;
            }
            &after[host_end..]
        }
        None => target,
    };
    let path = &rest[..rest.find(['?', '#']).unwrap_or(rest.len())];
    Some(if path.is_empty() { "/" } else { path })
}

/// A rule whose target is matched by the rule itself: `/` (prefix) to
/// `https://<this domain>`, or `/shop` (prefix) to `/shop/new`. nginx accepts
/// it and then answers every such request with a redirect to another one,
/// which takes the site (or that path) down.
fn loops_back(domain: &str, r: &RedirectRule) -> bool {
    let Some(path) = same_site_path(domain, &r.target_url) else {
        return false;
    };
    if r.match_mode == "exact" {
        return path == r.source_path;
    }
    let source = prefix_source(&r.source_path);
    let path = path.trim_end_matches('/');
    path == source || path.starts_with(&format!("{source}/"))
}

/// Insert the include line after every `server_name <domain>;` that isn't
/// already followed by it. Returns `None` when nothing had to change.
fn with_include(vhost: &str, domain: &str) -> Option<String> {
    let needle = format!("server_name {domain};");
    let include = include_line(domain);
    let lines: Vec<&str> = vhost.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len() + 4);
    let mut changed = false;
    for (i, line) in lines.iter().enumerate() {
        out.push((*line).to_string());
        if line.trim() != needle {
            continue;
        }
        let next = lines[i + 1..]
            .iter()
            .map(|l| l.trim())
            .find(|l| !l.is_empty());
        if next == Some(include.as_str()) {
            continue;
        }
        let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
        out.push(format!("{indent}{include}"));
        changed = true;
    }
    if !changed {
        return None;
    }
    let mut s = out.join("\n");
    if vhost.ends_with('\n') {
        s.push('\n');
    }
    Some(s)
}

/// The vhost that serves `domain`, if the panel owns it: `site-<domain>`
/// (Sites tab / routing slot) or `<domain>` carrying the `# vps-app:` marker.
/// External and panel-host vhosts are never touched.
async fn find_vhost(domain: &str) -> AgentResult<PathBuf> {
    let site = PathBuf::from(format!("/etc/nginx/sites-available/site-{domain}"));
    if tokio::fs::try_exists(&site).await.map_err(AgentError::Io)? {
        return Ok(site);
    }
    let app = PathBuf::from(format!("/etc/nginx/sites-available/{domain}"));
    if let Ok(body) = tokio::fs::read_to_string(&app).await {
        if body
            .lines()
            .next()
            .map(str::trim)
            .is_some_and(|l| l.starts_with("# vps-app:"))
        {
            return Ok(app);
        }
    }
    Err(AgentError::Conflict(format!(
        "{domain} has no panel-managed site or app vhost - redirects can only be added to domains the panel serves"
    )))
}

fn nginx_rejection(e: AgentError) -> AgentError {
    match e {
        AgentError::Subprocess { stderr, .. } => AgentError::Invalid(format!(
            "nginx rejected the redirect rules: {}",
            stderr.trim()
        )),
        other => other,
    }
}

/// Remove the per-domain include file and its directory. Best-effort on the
/// directory: if an operator dropped extra files in there, leave them.
pub async fn remove_domain_files(domain: &str) -> AgentResult<()> {
    fs_atomic::remove_if_exists(&conf_path(domain)).await?;
    let _ = tokio::fs::remove_dir(domain_dir(domain)).await;
    Ok(())
}

pub async fn put(
    State(state): State<AppState>,
    Path(domain): Path<String>,
    Json(req): Json<PutRedirects>,
) -> AgentResult<Json<OkResponse>> {
    if !is_safe_subdomain(&domain) {
        return Err(AgentError::Invalid("invalid domain".into()));
    }
    if !state.config.panel_host.is_empty() && domain == state.config.panel_host {
        return Err(AgentError::Conflict(
            "refusing to add redirects to the panel host vhost".into(),
        ));
    }

    if req.rules.is_empty() {
        // No vhost lookup: clearing the last rule must work even after the
        // site or app that carried it is gone.
        let conf = conf_path(&domain);
        let conf_before = tokio::fs::read(&conf).await.ok();
        remove_domain_files(&domain).await?;
        if let Err(e) = nginx_handler::test_config().await {
            // nginx was not reloaded and still serves the rules, and the
            // panel keeps its row on an error: put the file back so the two
            // stay in step.
            if let Some(prev) = conf_before {
                let _ = fs_atomic::write(&conf, &prev, 0o644).await;
            }
            return Err(e);
        }
        exec::run("systemctl", ["reload", "nginx"]).await?;
        return Ok(Json(OkResponse::default()));
    }

    let body = render(&domain, &req.rules)?;
    let vhost_path = find_vhost(&domain).await?;
    let vhost_before = tokio::fs::read_to_string(&vhost_path)
        .await
        .map_err(AgentError::Io)?;
    let vhost_after = with_include(&vhost_before, &domain);
    let include = include_line(&domain);
    if !vhost_after
        .as_deref()
        .unwrap_or(&vhost_before)
        .contains(&include)
    {
        return Err(AgentError::Conflict(format!(
            "{} has no `server_name {domain};` line to attach the redirects include to",
            vhost_path.display()
        )));
    }

    let conf = conf_path(&domain);
    let conf_before = tokio::fs::read_to_string(&conf).await.ok();
    fs_atomic::ensure_dir(&domain_dir(&domain), 0o755).await?;
    fs_atomic::write(&conf, body.as_bytes(), 0o644).await?;
    if let Some(ref v) = vhost_after {
        fs_atomic::write(&vhost_path, v.as_bytes(), 0o644).await?;
    }

    if let Err(e) = nginx_handler::test_config().await {
        // Back to the last known-good layout before surfacing the error, so
        // a bad rule never leaves nginx un-reloadable for the next change.
        match conf_before {
            Some(prev) => {
                let _ = fs_atomic::write(&conf, prev.as_bytes(), 0o644).await;
            }
            None => {
                let _ = remove_domain_files(&domain).await;
            }
        }
        if vhost_after.is_some() {
            let _ = fs_atomic::write(&vhost_path, vhost_before.as_bytes(), 0o644).await;
        }
        return Err(nginx_rejection(e));
    }
    exec::run("systemctl", ["reload", "nginx"]).await?;
    Ok(Json(OkResponse::default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(id: i64, src: &str, dst: &str, code: u16, mode: &str) -> RedirectRule {
        RedirectRule {
            id,
            source_path: src.into(),
            target_url: dst.into(),
            code,
            match_mode: mode.into(),
        }
    }

    #[test]
    fn renders_exact_and_prefix_rules() {
        let out = render(
            "example.com",
            &[
                rule(1, "/old", "https://new.example.com/page", 301, "exact"),
                rule(2, "/blog", "https://blog.example.com", 302, "prefix"),
                rule(3, "/blog/archive", "/archive", 301, "prefix"),
            ],
        )
        .unwrap();
        assert!(out.starts_with("# vps-redirects:example.com\n"));
        assert!(out.contains(
            "location = /old { return 301 \"https://new.example.com/page$is_args$args\"; }"
        ));
        // Longer prefix first, remainder of the path appended via $1.
        let archive = out
            .find("location ~ ^/blog/archive(?:/(.*))?$ { return 301 \"/archive/$1$is_args$args\"; }")
            .unwrap();
        let blog = out
            .find("location ~ ^/blog(?:/(.*))?$ { return 302 \"https://blog.example.com/$1$is_args$args\"; }")
            .unwrap();
        assert!(archive < blog);
    }

    #[test]
    fn whole_domain_prefix_keeps_acme_reachable() {
        let out = render(
            "example.com",
            &[rule(1, "/", "https://new.example.com", 301, "prefix")],
        )
        .unwrap();
        assert!(out.contains(
            "location ~ ^/(?!\\.well-known/acme-challenge/)(.*)$ { return 301 \"https://new.example.com/$1$is_args$args\"; }"
        ));
    }

    #[test]
    fn the_request_query_string_is_carried_over_unless_the_target_has_one() {
        assert_eq!(with_request_query("/new"), "/new$is_args$args");
        assert_eq!(with_request_query("https://a.example/x/$1"), "https://a.example/x/$1$is_args$args");
        // Before the fragment, where a query string belongs.
        assert_eq!(with_request_query("/new#top"), "/new$is_args$args#top");
        assert_eq!(with_request_query("/new#a?b"), "/new$is_args$args#a?b");
        // The target's own query string wins.
        assert_eq!(with_request_query("/new?lang=en"), "/new?lang=en");
        assert_eq!(with_request_query("/new?lang=en#top"), "/new?lang=en#top");
        assert_eq!(with_request_query("/search?q=/$1"), "/search?q=/$1");

        let out = render(
            "example.com",
            &[
                rule(1, "/old", "/new?lang=en", 301, "exact"),
                rule(2, "/doc", "https://docs.example.com/v2#intro", 302, "exact"),
                rule(3, "/shop", "https://shop.example.com/?ref=old", 301, "prefix"),
            ],
        )
        .unwrap();
        assert!(out.contains("location = /old { return 301 \"/new?lang=en\"; }"));
        assert!(out.contains(
            "location = /doc { return 302 \"https://docs.example.com/v2$is_args$args#intro\"; }"
        ));
        assert!(out.contains(
            "location ~ ^/shop(?:/(.*))?$ { return 301 \"https://shop.example.com/?ref=old/$1\"; }"
        ));
    }

    #[test]
    fn escapes_dots_in_prefix_sources() {
        let out = render("example.com", &[rule(1, "/v1.0", "/v2", 301, "prefix")]).unwrap();
        assert!(out.contains("location ~ ^/v1\\.0(?:/(.*))?$"));
    }

    #[test]
    fn rejects_bad_input() {
        assert!(render("example.com", &[rule(1, "old", "/new", 301, "exact")]).is_err());
        assert!(render("example.com", &[rule(1, "/old", "new", 301, "exact")]).is_err());
        assert!(render("example.com", &[rule(1, "/old", "/new", 307, "exact")]).is_err());
        assert!(render("example.com", &[rule(1, "/old", "/new", 301, "regex")]).is_err());
        assert!(render("example.com", &[rule(1, "/a b", "/new", 301, "exact")]).is_err());
        assert!(render("example.com", &[rule(1, "/old", "/new$x", 301, "exact")]).is_err());
        assert!(render("example.com", &[rule(1, "/old", "/new\"; }", 301, "exact")]).is_err());
        assert!(render(
            "example.com",
            &[rule(1, "/.well-known/x", "/new", 301, "exact")]
        )
        .is_err());
        assert!(render(
            "example.com",
            &[
                rule(1, "/a", "/b", 301, "exact"),
                rule(2, "/a", "/c", 302, "exact")
            ]
        )
        .is_err());
    }

    #[test]
    fn exact_rules_with_and_without_a_trailing_slash_are_two_rules() {
        let out = render(
            "example.com",
            &[
                rule(1, "/a", "/b", 301, "exact"),
                rule(2, "/a/", "/b", 301, "exact"),
            ],
        )
        .unwrap();
        assert!(out.contains("location = /a { return 301 \"/b$is_args$args\"; }"));
        assert!(out.contains("location = /a/ { return 301 \"/b$is_args$args\"; }"));
        // For a prefix they are the same rule.
        assert!(render(
            "example.com",
            &[
                rule(1, "/a", "/b", 301, "prefix"),
                rule(2, "/a/", "/c", 301, "prefix")
            ]
        )
        .is_err());
    }

    #[test]
    fn refuses_a_rule_that_redirects_to_itself() {
        for (src, dst, mode) in [
            ("/", "https://example.com", "prefix"),
            ("/", "http://Example.com/", "prefix"),
            ("/", "/new", "prefix"),
            ("/shop", "/shop/new", "prefix"),
            ("/shop/", "https://example.com/shop?x=1", "prefix"),
            ("/", "//example.com/x", "prefix"),
            ("/a", "/a", "exact"),
            ("/a", "https://example.com/a#top", "exact"),
            ("/", "https://example.com", "exact"),
        ] {
            assert!(
                render("example.com", &[rule(1, src, dst, 301, mode)]).is_err(),
                "{mode} {src} -> {dst} was accepted"
            );
        }
        for (src, dst, mode) in [
            ("/", "https://www.example.com", "prefix"),
            ("/", "https://example.com.au/", "prefix"),
            ("/", "//other.example.com/x", "prefix"),
            ("/shop", "/shop-new", "prefix"),
            ("/shop", "/", "prefix"),
            ("/a", "/a/", "exact"),
            ("/a", "/b", "exact"),
            ("/", "/home", "exact"),
        ] {
            assert!(
                render("example.com", &[rule(1, src, dst, 301, mode)]).is_ok(),
                "{mode} {src} -> {dst} was refused"
            );
        }
    }

    #[test]
    fn injects_include_after_each_server_name() {
        let vhost = "# vps-site:example.com\nserver {\n    listen 80;\n    server_name example.com;\n\n    location / {}\n}\nserver {\n    listen 443 ssl;\n    server_name example.com;\n    location / {}\n}\nserver {\n    server_name www.example.com;\n}\n";
        let out = with_include(vhost, "example.com").unwrap();
        let inc = "    include /etc/nginx/vps-redirects/example.com/*.conf;";
        assert_eq!(out.matches(inc).count(), 2);
        assert!(out.contains(&format!("    server_name example.com;\n{inc}\n")));
        assert!(!out.contains(&format!("server_name www.example.com;\n{inc}")));
        assert!(out.ends_with('\n'));
        // Idempotent: a vhost that already carries the lines is left alone.
        assert!(with_include(&out, "example.com").is_none());
    }
}
