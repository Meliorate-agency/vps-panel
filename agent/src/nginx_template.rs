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
    pub cert_path: &'a str,
    pub key_path: &'a str,
    pub acme_webroot: &'a str,
}
