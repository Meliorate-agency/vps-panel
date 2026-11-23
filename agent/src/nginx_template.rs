use askama::Template;

/// HTTP-01 challenge files, served by nginx for every vhost. Outside
/// /var/lib/vps-panel on purpose: nginx (www-data) reaching the old path
/// needed the vps-panel group, and that group can read the agent token and
/// connect to the root agent's socket.
pub const ACME_WEBROOT: &str = "/var/www/vps-panel-acme";

/// My Networks: the `/api/peer/` location every server block that proxies
/// to the panel carries, so the other servers in a network get past
/// Settings > Panel access. Copies: scripts/nginx/panel.conf.template and
/// the update.sh backfill.
pub const PEER_API_LOCATION: &str = r#"    # My Networks: the other servers in a network reach this panel here
    # from any address, whatever Settings > Panel access allows. The panel
    # checks each request's signature. Only plain /api/peer/<name> paths
    # pass, so nothing else can be made to normalize into this location.
    location ^~ /api/peer/ {
        allow all;
        if ($request_uri !~ "^/api/peer/[a-z]+(\?[A-Za-z0-9=&_-]*)?$") {
            return 404;
        }
        client_max_body_size 64k;
        proxy_pass http://127.0.0.1:10000;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_http_version 1.1;
        proxy_read_timeout 60s;
    }

"#;

#[derive(Template)]
#[template(path = "app_vhost.conf", escape = "none")]
pub struct AppVhostTemplate<'a> {
    pub name: &'a str,
    pub server_name: &'a str,
    pub upstream_port: u16,
    pub ssl_enabled: bool,
    pub cert_path: &'a str,
    pub key_path: &'a str,
    pub acme_webroot: &'a str,
}

#[derive(Template)]
#[template(path = "panel_vhost.conf", escape = "none")]
pub struct PanelVhostTemplate<'a> {
    pub server_name: &'a str,
    /// Port 80 as nginx's default server (see system.rs::render_panel_vhost,
    /// the one place this is rendered).
    pub default_server: bool,
    pub ssl_enabled: bool,
    pub cert_path: &'a str,
    pub key_path: &'a str,
    pub acme_webroot: &'a str,
    /// PEER_API_LOCATION.
    pub peer_location: &'a str,
}

#[derive(Template)]
#[template(path = "site_vhost.conf", escape = "none")]
pub struct SiteVhostTemplate<'a> {
    pub domain: &'a str,
    pub mode: &'a str,
    pub upstream_port: Option<u16>,
    pub folder_path: Option<&'a str>,
    pub force_www_redirect: bool,
    pub ssl_enabled: bool,
    /// The certificate lists `www.<domain>`. Without it the www block is
    /// left out on 443: it would answer with a certificate for another name.
    pub www_in_cert: bool,
    pub cert_path: &'a str,
    pub key_path: &'a str,
    pub acme_webroot: &'a str,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site(force_www_redirect: bool, ssl_enabled: bool, www_in_cert: bool) -> String {
        SiteVhostTemplate {
            domain: "example.com",
            mode: "port",
            upstream_port: Some(3000),
            folder_path: None,
            force_www_redirect,
            ssl_enabled,
            www_in_cert,
            cert_path: "/c/fullchain.pem",
            key_path: "/c/privkey.pem",
            acme_webroot: ACME_WEBROOT,
        }
        .render()
        .unwrap()
    }

    /// HSTS is for the name that sent it. With includeSubDomains one HTTPS
    /// site on example.com made browsers refuse every HTTP-only site or app
    /// on a subdomain of it for two years.
    #[test]
    fn hsts_covers_the_vhosts_own_name_only() {
        let app = AppVhostTemplate {
            name: "shop",
            server_name: "shop.example.com",
            upstream_port: 3000,
            ssl_enabled: true,
            cert_path: "/c/fullchain.pem",
            key_path: "/c/privkey.pem",
            acme_webroot: ACME_WEBROOT,
        }
        .render()
        .unwrap();
        let panel = PanelVhostTemplate {
            server_name: "panel.example.com",
            default_server: true,
            ssl_enabled: true,
            cert_path: "/c/fullchain.pem",
            key_path: "/c/privkey.pem",
            acme_webroot: ACME_WEBROOT,
            peer_location: PEER_API_LOCATION,
        }
        .render()
        .unwrap();
        for (what, body) in [("site", site(true, true, true)), ("app", app), ("panel", panel)] {
            assert_eq!(
                body.matches("add_header Strict-Transport-Security \"max-age=63072000\" always;").count(),
                1,
                "{what}"
            );
            assert!(!body.contains("includeSubDomains"), "{what}");
        }
        assert!(!site(true, false, false).contains("Strict-Transport-Security"));
    }

    /// The www block answers on 443 only with a certificate that lists
    /// www.<domain>; port 80 for the name is there whenever the flag is.
    #[test]
    fn the_www_block_is_on_443_only_when_the_certificate_lists_www() {
        let www_443 = "listen 443 ssl http2;\n    listen [::]:443 ssl http2;\n    server_name www.example.com;";
        let www_80 = "listen 80;\n    listen [::]:80;\n    server_name www.example.com;";

        let covered = site(true, true, true);
        assert!(covered.contains(www_443));
        assert!(covered.contains(www_80));
        assert_eq!(covered.matches("ssl_certificate /c/fullchain.pem;").count(), 2);

        let not_covered = site(true, true, false);
        assert!(!not_covered.contains(www_443));
        assert!(not_covered.contains(www_80));
        assert!(not_covered.contains("return 301 https://example.com$request_uri;"));
        assert_eq!(not_covered.matches("ssl_certificate /c/fullchain.pem;").count(), 1);
        assert!(not_covered.contains("listen 443 ssl http2;\n    listen [::]:443 ssl http2;\n    server_name example.com;"));

        // No certificate in use at all: nothing on 443, whatever the flag says.
        let http_only = site(true, false, true);
        assert!(!http_only.contains("listen 443"));
        assert!(http_only.contains("return 301 http://example.com$request_uri;"));
        assert!(!site(false, true, true).contains("server_name www.example.com;"));
    }
}
