use crate::error::AgentError;
use crate::exec;
use crate::fs_atomic;
use crate::ipc::OkResponse;
use axum::Json;
use std::path::{Path, PathBuf};

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

// ---------------------------------------------------------------------------
// Access-log privacy (Settings > Privacy). privacy.rs reads the settings and
// reports; the nginx side of it is here.
// ---------------------------------------------------------------------------

/// nginx's main file. No vhost the panel writes sets `access_log`, so every
/// site, app and the panel log through the one line of its http block,
/// `access_log /var/log/nginx/access.log;`. That line comes BEFORE the
/// include of conf.d, and access_log directives of one level add up: a
/// second one in a drop-in would log every request twice, once in full. So
/// while a privacy setting is on, the line is commented out with CONF_MARK
/// and LOG_PRIVACY_CONF declares the log again in its own format; with the
/// settings off the mark is taken out and the file is byte for byte what it
/// was.
pub const NGINX_CONF: &str = "/etc/nginx/nginx.conf";
pub const LOG_PRIVACY_CONF: &str = "/etc/nginx/conf.d/vps-panel-privacy.conf";
pub const LOG_FORMAT_NAME: &str = "vps_panel_privacy";
const CONF_MARK: &str = "#vps-panel-privacy: ";
/// Where nginx logs when no access_log is declared at all (Ubuntu's build).
const DEFAULT_ACCESS_LOG: &str = "/var/log/nginx/access.log";

/// `$remote_addr` to its masked form: the (regex, value) pairs of an nginx
/// map, tried in this order; no match gives ADDR_MAP_DEFAULT, so a value
/// nginx could not have printed for an address is never logged as it is.
/// The result is what privacy::mask_ip gives for the same address (a test
/// holds the two together): IPv4 with the last octet zeroed, IPv6 cut to its
/// first three groups and written the short way (`2001:db8::1` becomes
/// `2001:db8::`, not `2001:db8:0::`). nginx prints an IPv6 address in lower
/// case with its longest run of zero groups as `::`, which is what the
/// patterns rely on.
pub(crate) const ADDR_MAP: &[(&str, &str)] = &[
    (r"^(?<vps_panel_a>[0-9]+\.[0-9]+\.[0-9]+)\.[0-9]+$", "${vps_panel_a}.0"),
    // `::` stands for the first two groups only when six follow it. Every
    // other address that starts with `::` has three zero groups in front,
    // `::ffff:203.0.113.87` (IPv4 on a dual-stack socket) included.
    (
        r"^::(?<vps_panel_a>[0-9a-f]+):[0-9a-f]+:[0-9a-f]+:[0-9a-f]+:[0-9a-f]+:[0-9a-f]+$",
        "0:0:${vps_panel_a}::",
    ),
    (r"^::", "::"),
    (r"^(?<vps_panel_a>[0-9a-f]+)::", "${vps_panel_a}::"),
    (r"^(?<vps_panel_a>[0-9a-f]+:[0-9a-f]+)::", "${vps_panel_a}::"),
    // All eight groups written out, or the `::` further back: zero groups
    // at the end of the first three join the `::` of the masked form.
    (r"^0:0:0:", "::"),
    (r"^(?<vps_panel_a>[0-9a-f]+):0:0:", "${vps_panel_a}::"),
    (r"^(?<vps_panel_a>[0-9a-f]+:[0-9a-f]+):0:", "${vps_panel_a}::"),
    (r"^(?<vps_panel_a>[0-9a-f]+:[0-9a-f]+:[0-9a-f]+):", "${vps_panel_a}::"),
];
pub(crate) const ADDR_MAP_DEFAULT: &str = "0.0.0.0";

/// `$request` ("GET /path?query HTTP/1.1") without its query string:
/// everything from the first `?` up to the protocol goes, also when a
/// request nginx refused had spaces in it. A request line without `?` is
/// logged as it is.
pub(crate) const REQUEST_MAP: (&str, &str) = (
    r"^(?<vps_panel_r>[^?]*)\?.*?(?<vps_panel_p> HTTP/[0-9.]+)?$",
    "${vps_panel_r}${vps_panel_p}",
);
/// `$http_referer` up to its first `?`.
pub(crate) const REFERER_MAP: (&str, &str) = (r"^(?<vps_panel_f>[^?]*)\?", "${vps_panel_f}");

/// LOG_PRIVACY_CONF's content. `logs` are the access_log directives taken
/// over from nginx.conf, each as "path" or "path extra parameters".
pub fn render_log_privacy(mask: bool, strip: bool, logs: &[String]) -> String {
    let what = match (mask, strip) {
        (true, true) => "visitor addresses masked and query strings left out",
        (true, false) => "visitor addresses masked",
        _ => "query strings left out",
    };
    let mut out = format!(
        "# Managed by vps-panel (Settings > Privacy; agent/src/handlers/nginx.rs).\n\
         # The access log in the combined format's shape, with {what}.\n\
         # While this file is here, the access_log line of {NGINX_CONF} is\n\
         # commented out with \"{}\"; turning the settings off puts it back\n\
         # and removes this file.\n",
        CONF_MARK.trim_end()
    );
    if mask {
        out.push_str(&format!(
            "map $remote_addr $vps_panel_log_addr {{\n    default {ADDR_MAP_DEFAULT};\n"
        ));
        for (regex, value) in ADDR_MAP {
            out.push_str(&format!("    \"~{regex}\" \"{value}\";\n"));
        }
        out.push_str("}\n");
    }
    if strip {
        out.push_str(&format!(
            "map $request $vps_panel_log_request {{\n    default $request;\n    \"~{}\" \"{}\";\n}}\n",
            REQUEST_MAP.0, REQUEST_MAP.1
        ));
        // The combined format prints "-" for a request without a referrer.
        out.push_str(&format!(
            "map $http_referer $vps_panel_log_referer {{\n    default $http_referer;\n    \"\" \"-\";\n    \"~{}\" \"{}\";\n}}\n",
            REFERER_MAP.0, REFERER_MAP.1
        ));
    }
    let addr = if mask { "$vps_panel_log_addr" } else { "$remote_addr" };
    let (request, referer) = if strip {
        ("$vps_panel_log_request", "$vps_panel_log_referer")
    } else {
        ("$request", "$http_referer")
    };
    let pad = " ".repeat("log_format ".len() + LOG_FORMAT_NAME.len() + 1);
    out.push_str(&format!(
        "log_format {LOG_FORMAT_NAME} '{addr} - $remote_user [$time_local] '\n\
         {pad}'\"{request}\" $status $body_bytes_sent '\n\
         {pad}'\"{referer}\" \"$http_user_agent\"';\n"
    ));
    for log in logs {
        let (path, extra) = log.split_once(' ').unwrap_or((log.as_str(), ""));
        let sep = if extra.is_empty() { "" } else { " " };
        out.push_str(&format!("access_log {path} {LOG_FORMAT_NAME}{sep}{extra};\n"));
    }
    out
}

/// One directive of an nginx configuration file.
#[derive(Debug, Clone, PartialEq)]
pub struct Directive {
    /// Its words, quotes taken off.
    pub words: Vec<String>,
    /// Line of its first word and of its `;` or `{`, counted from 1.
    pub line: usize,
    pub end_line: usize,
    /// The blocks it sits in, outermost first (`["http", "server"]`).
    pub context: Vec<String>,
}

/// The directives of `text` in file order: enough of nginx's syntax
/// (comments, quotes, blocks) to find a directive and know where it sits.
pub fn directives(text: &str) -> Vec<Directive> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut stack: Vec<String> = Vec::new();
    let mut words: Vec<String> = Vec::new();
    let (mut line, mut first_line) = (1usize, 1usize);
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            b'\n' => {
                line += 1;
                i += 1;
            }
            c if c.is_ascii_whitespace() => i += 1,
            b'#' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b';' | b'{' => {
                if !words.is_empty() {
                    out.push(Directive {
                        words: words.clone(),
                        line: first_line,
                        end_line: line,
                        context: stack.clone(),
                    });
                }
                if c == b'{' {
                    stack.push(words.first().cloned().unwrap_or_default());
                }
                words.clear();
                i += 1;
            }
            b'}' => {
                stack.pop();
                words.clear();
                i += 1;
            }
            _ => {
                if words.is_empty() {
                    first_line = line;
                }
                let mut word: Vec<u8> = Vec::new();
                if c == b'"' || c == b'\'' {
                    i += 1;
                    while i < bytes.len() && bytes[i] != c {
                        if bytes[i] == b'\\' && i + 1 < bytes.len() {
                            word.push(bytes[i]);
                            i += 1;
                        }
                        if bytes[i] == b'\n' {
                            line += 1;
                        }
                        word.push(bytes[i]);
                        i += 1;
                    }
                    i += 1;
                } else {
                    while i < bytes.len() {
                        let b = bytes[i];
                        // `${name}` is a variable, not a block.
                        let variable = b == b'{' && word.last() == Some(&b'$');
                        if b.is_ascii_whitespace() || b == b';' || (b == b'{' && !variable) {
                            break;
                        }
                        if b == b'\\' && i + 1 < bytes.len() {
                            word.push(b);
                            i += 1;
                        }
                        word.push(bytes[i]);
                        i += 1;
                    }
                }
                words.push(String::from_utf8_lossy(&word).into_owned());
            }
        }
    }
    out
}

/// A word that can be copied into our own file as it is.
fn plain_word(w: &str) -> bool {
    !w.is_empty() && w.chars().all(|c| c.is_ascii_alphanumeric() || "_./:=@,+-".contains(c))
}

/// nginx.conf with the marks taken out: the operator's own file.
fn unmark(conf: &str) -> String {
    conf.split_inclusive('\n')
        .map(|l| {
            let indent = l.len() - l.trim_start().len();
            match l[indent..].strip_prefix(CONF_MARK) {
                Some(rest) => format!("{}{rest}", &l[..indent]),
                None => l.to_string(),
            }
        })
        .collect()
}

/// What nginx.conf and LOG_PRIVACY_CONF should hold for the two settings.
#[derive(Debug, PartialEq)]
pub struct LogPlan {
    pub conf: String,
    /// None: the file should not exist.
    pub dropin: Option<String>,
}

/// Works from the unmarked file, so it gives the same answer whatever state
/// the last run left. An http-level `access_log <path>;` (or `... combined`,
/// with or without buffer parameters) on a line of its own is taken over.
/// One in the operator's own format, or `access_log off;`, is theirs: it
/// stays, and log_state() reports it.
pub fn plan_log_privacy(conf: &str, mask: bool, strip: bool) -> LogPlan {
    let original = unmark(conf);
    if !mask && !strip {
        return LogPlan { conf: original, dropin: None };
    }
    let all = directives(&original);
    let http_level: Vec<&Directive> = all
        .iter()
        .filter(|d| d.context == ["http"] && d.words[0] == "access_log")
        .collect();
    let off = http_level.iter().any(|d| d.words.get(1).map(String::as_str) == Some("off"));
    let lines: Vec<&str> = original.split_inclusive('\n').collect();
    // (index of the line, what goes after `access_log` in our file)
    let mut taken: Vec<(usize, String)> = Vec::new();
    if !off {
        for d in &http_level {
            let format = d.words.get(2).map(String::as_str);
            // Nothing else on the line but a comment after the `;`.
            let alone = d.line == d.end_line
                && lines.get(d.line - 1).is_some_and(|l| {
                    let after = l.split_once(';').map(|(_, a)| a.trim()).unwrap_or("x");
                    l.trim_start().starts_with("access_log") && (after.is_empty() || after.starts_with('#'))
                });
            if alone && matches!(format, None | Some("combined")) && d.words[1..].iter().all(|w| plain_word(w)) {
                let mut log = d.words[1].clone();
                for extra in d.words.iter().skip(3) {
                    log.push(' ');
                    log.push_str(extra);
                }
                taken.push((d.line - 1, log));
            }
        }
    }
    // The same log declared twice is still written once in our file.
    let mut logs: Vec<String> = Vec::new();
    for (_, log) in &taken {
        if !logs.contains(log) {
            logs.push(log.clone());
        }
    }
    // No access_log at all is nginx's built-in one, in the combined format.
    if http_level.is_empty() && all.iter().any(|d| d.context.is_empty() && d.words[0] == "http") {
        logs.push(DEFAULT_ACCESS_LOG.to_string());
    }
    let conf: String = lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            if taken.iter().any(|(n, _)| *n == i) {
                let indent = l.len() - l.trim_start().len();
                format!("{}{CONF_MARK}{}", &l[..indent], &l[indent..])
            } else {
                l.to_string()
            }
        })
        .collect();
    LogPlan { conf, dropin: Some(render_log_privacy(mask, strip, &logs)) }
}

/// An access_log directive and where it is.
#[derive(Debug, Clone, PartialEq)]
pub struct AccessLogAt {
    pub file: String,
    pub line: usize,
    /// Its log format, when it names one other than nginx's `combined`.
    pub format: Option<String>,
}

/// What the configuration nginx reads (`nginx -T`) does with access logs.
#[derive(Debug, Default, PartialEq)]
pub struct LogState {
    /// LOG_PRIVACY_CONF is part of the configuration.
    pub dropin_loaded: bool,
    /// What our log_format in it does.
    pub masks: bool,
    pub strips: bool,
    /// http level: `access_log off;`, logs in our format, logs in another.
    pub http_off: bool,
    pub http_ours: usize,
    pub http_full: Vec<AccessLogAt>,
    /// No access_log at http level at all: nginx's built-in combined log.
    pub http_implicit: bool,
    /// A server or location with an access_log of its own that is neither
    /// off nor in our format: hand-written, and not ours to rewrite.
    pub elsewhere: Vec<AccessLogAt>,
}

pub fn log_state(dump: &str) -> LogState {
    let mut state = LogState::default();
    // `nginx -T` prints every file it read under such a line, the main one first.
    let mut sections: Vec<(String, String)> = Vec::new();
    for line in dump.split_inclusive('\n') {
        let header = line
            .trim_end()
            .strip_prefix("# configuration file ")
            .and_then(|r| r.strip_suffix(':'));
        match (header, sections.last_mut()) {
            (Some(path), _) => sections.push((path.to_string(), String::new())),
            (None, Some((_, body))) => body.push_str(line),
            (None, None) => {}
        }
    }
    for (index, (file, body)) in sections.iter().enumerate() {
        let ours = file == LOG_PRIVACY_CONF;
        state.dropin_loaded |= ours;
        for d in directives(body) {
            let outer = d.context.first().map(String::as_str);
            if ours && d.words[0] == "log_format" && d.words.get(1).map(String::as_str) == Some(LOG_FORMAT_NAME) {
                let format = d.words[2..].concat();
                state.masks = format.contains("$vps_panel_log_addr");
                state.strips =
                    format.contains("$vps_panel_log_request") && format.contains("$vps_panel_log_referer");
            }
            if d.words[0] != "access_log" || matches!(outer, Some("stream" | "mail" | "events")) {
                continue;
            }
            let off = d.words.get(1).map(String::as_str) == Some("off");
            let our_format = d.words.get(2).map(String::as_str) == Some(LOG_FORMAT_NAME);
            let format = d.words.get(2).filter(|f| *f != "combined").cloned();
            let at = AccessLogAt { file: file.clone(), line: d.line, format };
            // Another included file does not say which block it is read in.
            let http_level = if index == 0 { d.context == ["http"] } else { ours && d.context.is_empty() };
            if http_level {
                match (off, our_format) {
                    (true, _) => state.http_off = true,
                    (false, true) => state.http_ours += 1,
                    (false, false) => state.http_full.push(at),
                }
            } else if !off && !our_format {
                state.elsewhere.push(at);
            }
        }
    }
    state.http_implicit = !state.http_off && state.http_ours == 0 && state.http_full.is_empty();
    state
}

/// The configuration as nginx reads it, every included file under its path.
/// Fails when the configuration does not pass nginx's own test.
pub async fn dump_config() -> Result<String, AgentError> {
    exec::run("nginx", ["-T"]).await.map(|o| o.stdout)
}

/// A file as it was, its modification time included: the status tells from
/// that time whether nginx has reloaded since its configuration changed, and
/// a change that was taken back is none.
async fn put_back(path: &Path, body: &str, mode: u32, modified: Option<std::time::SystemTime>) {
    if fs_atomic::write(path, body.as_bytes(), mode).await.is_err() {
        return;
    }
    if let Some(time) = modified {
        let path = path.to_path_buf();
        let _ = tokio::task::spawn_blocking(move || {
            std::fs::File::options().write(true).open(path).and_then(|f| f.set_modified(time))
        })
        .await;
    }
}

/// Whether apply_log_privacy() would change a file: the settings are not, or
/// no longer, what is on disk.
pub async fn log_privacy_pending(mask: bool, strip: bool) -> bool {
    let Ok(conf) = tokio::fs::read_to_string(NGINX_CONF).await else {
        return false;
    };
    let dropin = tokio::fs::read_to_string(LOG_PRIVACY_CONF).await.ok();
    let plan = plan_log_privacy(&conf, mask, strip);
    plan.conf != conf || plan.dropin != dropin
}

/// Bring the access-log format to the two settings: write, test, reload.
/// On an error everything written is put back as it was, except after a
/// failed reload: the configuration is tested by then, and stays for the
/// next one. Ok(false) when the files already were what they should be, and
/// nothing was touched.
pub async fn apply_log_privacy(mask: bool, strip: bool) -> Result<bool, String> {
    let dropin = Path::new(LOG_PRIVACY_CONF);
    let dropin_before = tokio::fs::read_to_string(dropin).await.ok();
    // nginx.conf may be a link; the write goes to the file it names.
    let conf_path = tokio::fs::canonicalize(NGINX_CONF)
        .await
        .unwrap_or_else(|_| PathBuf::from(NGINX_CONF));
    let conf_before = match tokio::fs::read_to_string(&conf_path).await {
        Ok(s) => s,
        // No nginx: nothing to set up and nothing of ours to take away.
        Err(_) if !mask && !strip && dropin_before.is_none() => return Ok(false),
        Err(e) => return Err(format!("{NGINX_CONF} could not be read: {e}")),
    };
    let plan = plan_log_privacy(&conf_before, mask, strip);
    if plan.conf == conf_before && plan.dropin == dropin_before {
        return Ok(false);
    }
    let (mode, conf_time) = {
        use std::os::unix::fs::PermissionsExt;
        match tokio::fs::metadata(&conf_path).await {
            Ok(m) => (m.permissions().mode() & 0o7777, m.modified().ok()),
            Err(_) => (0o644, None),
        }
    };
    let dropin_time = tokio::fs::metadata(dropin).await.ok().and_then(|m| m.modified().ok());

    let written: Result<(), String> = async {
        let write_conf = async {
            if plan.conf != conf_before {
                fs_atomic::write(&conf_path, plan.conf.as_bytes(), mode).await?;
            }
            Ok::<(), AgentError>(())
        };
        // Our file first when turning on, nginx.conf first when turning off:
        // in between the configuration is valid (it logs twice at worst), so
        // a `nginx -t` another handler runs meanwhile does not fail on it.
        match &plan.dropin {
            Some(body) => {
                fs_atomic::write(dropin, body.as_bytes(), 0o644).await.map_err(|e| e.to_string())?;
                write_conf.await.map_err(|e| e.to_string())?;
            }
            None => {
                write_conf.await.map_err(|e| e.to_string())?;
                fs_atomic::remove_if_exists(dropin).await.map_err(|e| e.to_string())?;
            }
        }
        let dump = dump_config()
            .await
            .map_err(|e| format!("nginx -t did not accept the change: {e}"))?;
        if plan.dropin.is_some() && !log_state(&dump).dropin_loaded {
            return Err(format!(
                "{NGINX_CONF} does not include /etc/nginx/conf.d/*.conf, so nginx would not read the log format"
            ));
        }
        Ok(())
    }
    .await;
    if let Err(e) = written {
        if plan.conf != conf_before {
            put_back(&conf_path, &conf_before, mode, conf_time).await;
        }
        match &dropin_before {
            Some(body) => put_back(dropin, body, 0o644, dropin_time).await,
            None => {
                let _ = fs_atomic::remove_if_exists(dropin).await;
            }
        }
        let e = e.trim_end().trim_end_matches('.');
        return Err(format!("{e}. nginx's configuration was put back as it was."));
    }
    exec::run("systemctl", ["reload", "nginx"]).await.map_err(|e| {
        format!(
            "The log format is saved and passes nginx -t, but nginx could not be reloaded ({e}); \
             it applies from the next reload."
        )
    })?;
    tracing::info!(mask, strip, "nginx access-log format updated");
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::privacy::mask_ip;
    use std::net::Ipv6Addr;

    /// Ubuntu 24.04's nginx.conf, shortened between the parts that matter;
    /// tabs as in the package.
    const STOCK: &str = "user www-data;\nworker_processes auto;\npid /run/nginx.pid;\nerror_log /var/log/nginx/error.log;\ninclude /etc/nginx/modules-enabled/*.conf;\n\nevents {\n\tworker_connections 768;\n\t# multi_accept on;\n}\n\nhttp {\n\n\t##\n\t# Basic Settings\n\t##\n\n\tsendfile on;\n\t# server_names_hash_bucket_size 64;\n\n\t##\n\t# Logging Settings\n\t##\n\n\taccess_log /var/log/nginx/access.log;\n\n\t##\n\t# Gzip Settings\n\t##\n\n\tgzip on;\n\n\tinclude /etc/nginx/conf.d/*.conf;\n\tinclude /etc/nginx/sites-enabled/*;\n}\n\n\n#mail {\n#\tserver {\n#\t\tlisten     localhost:110;\n#\t}\n#}\n";
    const STOCK_LINE: &str = "\taccess_log /var/log/nginx/access.log;\n";
    const MARKED_LINE: &str = "\t#vps-panel-privacy: access_log /var/log/nginx/access.log;\n";

    fn with_line(line: &str) -> String {
        STOCK.replace(STOCK_LINE, line)
    }

    #[test]
    fn directives_know_their_block_and_their_line() {
        let text = "# a comment; with { braces }\nhttp {\n    log_format main '$remote_addr; {x}'\n        \"$request\";\n    server {\n        set $a \"${b}c\"; listen 80; # two on a line\n        location ~ \\.php$ { return 404; }\n    }\n}\nstream { access_log off; }\n";
        let all = directives(text);
        let find = |name: &str| all.iter().find(|d| d.words[0] == name).unwrap();
        assert_eq!(find("http").context, Vec::<String>::new());
        let format = find("log_format");
        assert_eq!(format.words, ["log_format", "main", "$remote_addr; {x}", "$request"]);
        assert_eq!((format.line, format.end_line), (3, 4));
        assert_eq!(format.context, ["http"]);
        assert_eq!(find("set").words, ["set", "$a", "${b}c"]);
        assert_eq!(find("listen").line, 6);
        assert_eq!(find("return").context, ["http", "server", "location"]);
        assert_eq!(find("access_log").context, ["stream"]);
    }

    #[test]
    fn default_settings_leave_nginx_as_it_is() {
        assert_eq!(
            plan_log_privacy(STOCK, false, false),
            LogPlan { conf: STOCK.to_string(), dropin: None }
        );
    }

    #[test]
    fn the_stock_access_log_is_taken_over_and_put_back_byte_for_byte() {
        let on = plan_log_privacy(STOCK, true, true);
        assert_eq!(on.conf, with_line(MARKED_LINE));
        let dropin = on.dropin.unwrap();
        assert!(dropin.ends_with("access_log /var/log/nginx/access.log vps_panel_privacy;\n"), "{dropin}");
        // The format is declared before the access_log that names it.
        assert!(dropin.find("log_format vps_panel_privacy").unwrap() < dropin.find("access_log /var").unwrap());

        // Applying again, or another combination, starts from the same file.
        assert_eq!(plan_log_privacy(&on.conf, true, true).conf, on.conf);
        let mask_only = plan_log_privacy(&on.conf, true, false);
        assert_eq!(mask_only.conf, on.conf);
        let dropin = mask_only.dropin.unwrap();
        assert!(dropin.contains("'$vps_panel_log_addr - $remote_user [$time_local] '"));
        assert!(dropin.contains("'\"$request\" $status $body_bytes_sent '"));
        assert!(dropin.contains("'\"$http_referer\" \"$http_user_agent\"';"));
        assert!(!dropin.contains("map $request"));
        let strip_only = plan_log_privacy(&on.conf, false, true).dropin.unwrap();
        assert!(strip_only.contains("'$remote_addr - $remote_user [$time_local] '"));
        assert!(!strip_only.contains("map $remote_addr"));

        // Off: the operator's file, to the byte, and no file of ours.
        assert_eq!(
            plan_log_privacy(&on.conf, false, false),
            LogPlan { conf: STOCK.to_string(), dropin: None }
        );
    }

    #[test]
    fn the_file_written_for_masked_addresses_without_query_strings() {
        let expected = r##"# Managed by vps-panel (Settings > Privacy; agent/src/handlers/nginx.rs).
# The access log in the combined format's shape, with visitor addresses masked and query strings left out.
# While this file is here, the access_log line of /etc/nginx/nginx.conf is
# commented out with "#vps-panel-privacy:"; turning the settings off puts it back
# and removes this file.
map $remote_addr $vps_panel_log_addr {
    default 0.0.0.0;
    "~^(?<vps_panel_a>[0-9]+\.[0-9]+\.[0-9]+)\.[0-9]+$" "${vps_panel_a}.0";
    "~^::(?<vps_panel_a>[0-9a-f]+):[0-9a-f]+:[0-9a-f]+:[0-9a-f]+:[0-9a-f]+:[0-9a-f]+$" "0:0:${vps_panel_a}::";
    "~^::" "::";
    "~^(?<vps_panel_a>[0-9a-f]+)::" "${vps_panel_a}::";
    "~^(?<vps_panel_a>[0-9a-f]+:[0-9a-f]+)::" "${vps_panel_a}::";
    "~^0:0:0:" "::";
    "~^(?<vps_panel_a>[0-9a-f]+):0:0:" "${vps_panel_a}::";
    "~^(?<vps_panel_a>[0-9a-f]+:[0-9a-f]+):0:" "${vps_panel_a}::";
    "~^(?<vps_panel_a>[0-9a-f]+:[0-9a-f]+:[0-9a-f]+):" "${vps_panel_a}::";
}
map $request $vps_panel_log_request {
    default $request;
    "~^(?<vps_panel_r>[^?]*)\?.*?(?<vps_panel_p> HTTP/[0-9.]+)?$" "${vps_panel_r}${vps_panel_p}";
}
map $http_referer $vps_panel_log_referer {
    default $http_referer;
    "" "-";
    "~^(?<vps_panel_f>[^?]*)\?" "${vps_panel_f}";
}
log_format vps_panel_privacy '$vps_panel_log_addr - $remote_user [$time_local] '
                             '"$vps_panel_log_request" $status $body_bytes_sent '
                             '"$vps_panel_log_referer" "$http_user_agent"';
access_log /var/log/nginx/access.log vps_panel_privacy;
"##;
        assert_eq!(render_log_privacy(true, true, &["/var/log/nginx/access.log".to_string()]), expected);
    }

    #[test]
    fn an_access_log_the_operator_wrote_their_own_way_stays() {
        // Their own format, logging switched off, or a line shared with
        // something else: nginx.conf is not touched, and our file only
        // declares the format.
        for line in [
            "\taccess_log /var/log/nginx/access.log main;\n",
            "\taccess_log off;\n",
            "\taccess_log /var/log/nginx/access.log; gzip_vary on;\n",
            "\taccess_log\n\t\t/var/log/nginx/access.log;\n",
            "\taccess_log \"/var/log/nginx/my log.log\";\n",
        ] {
            let conf = with_line(line);
            let plan = plan_log_privacy(&conf, true, true);
            assert_eq!(plan.conf, conf, "{line}");
            assert!(!plan.dropin.unwrap().contains("\naccess_log "), "{line}");
        }
        // Buffer parameters and a comment after it are no obstacle.
        let conf = with_line("\taccess_log /var/log/nginx/access.log combined buffer=32k flush=5s; # ours\n");
        let plan = plan_log_privacy(&conf, true, false);
        assert!(plan.conf.contains("\t#vps-panel-privacy: access_log /var/log/nginx/access.log combined buffer=32k flush=5s; # ours\n"));
        assert!(plan
            .dropin
            .unwrap()
            .ends_with("access_log /var/log/nginx/access.log vps_panel_privacy buffer=32k flush=5s;\n"));
        assert_eq!(plan_log_privacy(&plan.conf, false, false).conf, conf);
        // The stock line a second time (added by hand while ours was on):
        // both are commented out, the log is declared once.
        let conf = with_line(&format!("{MARKED_LINE}{STOCK_LINE}"));
        let plan = plan_log_privacy(&conf, true, true);
        assert_eq!(plan.conf, with_line(&format!("{MARKED_LINE}{MARKED_LINE}")));
        assert_eq!(plan.dropin.unwrap().matches("\naccess_log ").count(), 1);
        // An access_log in a server block of nginx.conf is not the http one.
        let conf = STOCK.replace("\tgzip on;\n", "\tserver {\n\t\taccess_log /var/log/nginx/own.log;\n\t}\n");
        let plan = plan_log_privacy(&conf, true, true);
        assert!(plan.conf.contains("\t\taccess_log /var/log/nginx/own.log;\n"));
        assert!(plan.conf.contains(MARKED_LINE));
    }

    #[test]
    fn no_access_log_line_at_all_is_nginxs_built_in_one() {
        let conf = with_line("");
        let plan = plan_log_privacy(&conf, true, true);
        assert_eq!(plan.conf, conf);
        assert!(plan.dropin.unwrap().ends_with("access_log /var/log/nginx/access.log vps_panel_privacy;\n"));
    }

    fn dump(files: &[(&str, &str)]) -> String {
        files
            .iter()
            .map(|(path, body)| format!("# configuration file {path}:\n{body}\n"))
            .collect()
    }

    #[test]
    fn log_state_reads_what_nginx_dumps() {
        // Untouched.
        let state = log_state(&dump(&[(NGINX_CONF, STOCK), ("/etc/nginx/mime.types", "types {\n    text/html html;\n}\n")]));
        assert_eq!(
            state,
            LogState {
                http_full: vec![AccessLogAt { file: NGINX_CONF.into(), line: 25, format: None }],
                ..LogState::default()
            }
        );

        // On, with a hand-written vhost that logs by itself and one that does not.
        let on = plan_log_privacy(STOCK, true, false);
        let own = "server {\n    listen 80;\n    access_log /var/log/nginx/shop.log;\n    location /health { access_log off; }\n}\n";
        let fine = "server {\n    listen 80;\n    access_log /var/log/nginx/blog.log vps_panel_privacy;\n}\n";
        let stream = "stream {\n    access_log /var/log/nginx/stream.log basic;\n}\n";
        let state = log_state(&dump(&[
            (NGINX_CONF, &on.conf),
            (LOG_PRIVACY_CONF, on.dropin.as_deref().unwrap()),
            ("/etc/nginx/sites-enabled/shop", own),
            ("/etc/nginx/sites-enabled/blog", fine),
            ("/etc/nginx/extra/stream.conf", stream),
        ]));
        assert_eq!(
            state,
            LogState {
                dropin_loaded: true,
                masks: true,
                strips: false,
                http_ours: 1,
                elsewhere: vec![AccessLogAt { file: "/etc/nginx/sites-enabled/shop".into(), line: 3, format: None }],
                ..LogState::default()
            }
        );

        // The package put its own nginx.conf back: both log now.
        let state = log_state(&dump(&[(NGINX_CONF, STOCK), (LOG_PRIVACY_CONF, on.dropin.as_deref().unwrap())]));
        assert_eq!((state.http_ours, state.http_full.len(), state.http_implicit), (1, 1, false));

        // The operator's own format is named.
        let own = log_state(&dump(&[(NGINX_CONF, &with_line("\taccess_log /var/log/nginx/access.log main buffer=8k;\n"))]));
        assert_eq!(own.http_full[0].format.as_deref(), Some("main"));
        let combined = log_state(&dump(&[(NGINX_CONF, &with_line("\taccess_log /var/log/nginx/access.log combined;\n"))]));
        assert_eq!(combined.http_full[0].format, None);

        // No access_log anywhere, and logging switched off.
        assert!(log_state(&dump(&[(NGINX_CONF, &with_line(""))])).http_implicit);
        let off = log_state(&dump(&[(NGINX_CONF, &with_line("\taccess_log off;\n"))]));
        assert!(off.http_off && !off.http_implicit);
    }

    /// What nginx does with a map: the first pattern that matches gives the
    /// value, with the named groups filled in.
    fn nginx_map(table: &[(&str, &str)], default: &str, input: &str) -> String {
        for (pattern, value) in table {
            if let Some(found) = regex::Regex::new(pattern).unwrap().captures(input) {
                let mut out = String::new();
                found.expand(value, &mut out);
                return out;
            }
        }
        default.to_string()
    }

    /// `$remote_addr` for an IPv6 client, as nginx's ngx_inet6_ntop() prints
    /// it: lower case, the first longest run of two or more zero groups as
    /// `::`, the last 32 bits dotted for IPv4-mapped and -compatible ones.
    fn nginx_text(a: Ipv6Addr) -> String {
        let p = a.octets();
        let (mut zero, mut max, mut n, mut zero_start) = (usize::MAX, 1usize, 0usize, usize::MAX);
        for i in (0..16).step_by(2) {
            if p[i] != 0 || p[i + 1] != 0 {
                if max < n {
                    zero = zero_start;
                    max = n;
                }
                n = 0;
                zero_start = usize::MAX;
                continue;
            }
            if n == 0 {
                zero_start = i;
            }
            n += 1;
        }
        if max < n {
            zero = zero_start;
            max = n;
        }
        let mut out = String::new();
        let mut end = 16;
        if zero == 0 {
            if (max == 5 && p[10] == 0xff && p[11] == 0xff) || max == 6 || (max == 7 && p[14] != 0 && p[15] != 1) {
                end = 12;
            }
            out.push(':');
        }
        let mut i = 0;
        while i < end {
            if i == zero {
                out.push(':');
                i += max * 2;
                continue;
            }
            out.push_str(&format!("{:x}", u16::from(p[i]) * 256 + u16::from(p[i + 1])));
            if i < 14 {
                out.push(':');
            }
            i += 2;
        }
        if end == 12 {
            out.push_str(&format!("{}.{}.{}.{}", p[12], p[13], p[14], p[15]));
        }
        out
    }

    fn masked_by_nginx(addr: &str) -> String {
        nginx_map(ADDR_MAP, ADDR_MAP_DEFAULT, addr)
    }

    #[test]
    fn nginx_masks_an_address_to_the_same_form_as_mask_ip() {
        // Lines a real nginx 1.24 logged for these addresses (the test
        // `a_real_nginx_...` below reads them back from one).
        for (addr, printed) in [
            ("0:0:5:0:0:0:0:1", "0:0:5::1"),
            ("0:0:5:6:7:8:9:a", "::5:6:7:8:9:a"),
            ("2001:0:0:1:0:0:0:1", "2001:0:0:1::1"),
            ("::ffff:203.0.113.87", "::ffff:203.0.113.87"),
            ("::1.2.3.4", "::1.2.3.4"),
            ("5:0:7:1:2:3:4:5", "5:0:7:1:2:3:4:5"),
        ] {
            assert_eq!(nginx_text(addr.parse().unwrap()), printed);
        }

        for (addr, masked) in [
            ("203.0.113.87", "203.0.113.0"),
            ("2001:db8:85a3:8d3::7", "2001:db8:85a3::"),
            ("2001:db8::1", "2001:db8::"),
            ("2001:0:0:1::5", "2001::"),
            ("::1", "::"),
            ("::ffff:203.0.113.87", "::"),
            ("::1.2.3.4", "::"),
            ("fe80::1", "fe80::"),
            ("0:0:5::1", "0:0:5::"),
            ("::5:6:7:8:9:a", "0:0:5::"),
            ("5:0:7:1:2:3:4:5", "5:0:7::"),
            ("0:5::1", "0:5::"),
        ] {
            assert_eq!(masked_by_nginx(addr), masked, "{addr}");
            assert_eq!(mask_ip(addr).as_deref(), Some(masked), "{addr}");
        }

        // Every pattern of zero and non-zero groups, with values that also
        // give the dotted forms.
        for values in [[0x2001u16, 0xdb8, 0x85a3, 0x8d3, 0x1, 0xffff, 0xcb00, 0x7157], [0xa, 0xb, 0xc, 0xd, 0xe, 0xf, 0x10, 0x1]] {
            for zeros in 0u16..256 {
                let mut groups = values;
                for (i, g) in groups.iter_mut().enumerate() {
                    if zeros & (1 << i) != 0 {
                        *g = 0;
                    }
                }
                let addr = Ipv6Addr::from(groups);
                let printed = nginx_text(addr);
                assert_eq!(printed.parse::<Ipv6Addr>().unwrap(), addr, "{printed}");
                assert_eq!(Some(masked_by_nginx(&printed)), mask_ip(&printed), "{printed}");
            }
        }
    }

    #[test]
    fn what_is_not_an_address_is_never_logged_as_it_is() {
        for odd in ["unix:", "", "-", "203.0.113", "203.0.113.87.5", "not an address", "1.2.3.4, 5.6.7.8", "gggg:1:2:3::"] {
            assert_eq!(masked_by_nginx(odd), "0.0.0.0", "{odd}");
        }
        // nginx never prints a zone; if it did, the zone would go as well.
        assert_eq!(masked_by_nginx("fe80::1%eth0"), "fe80::");
        // Text that starts like an address is cut to what a masked one keeps.
        assert_eq!(masked_by_nginx("2001:db8:1:2:3:4:5:6 extra"), "2001:db8:1::");
    }

    #[test]
    fn query_strings_leave_the_request_and_the_referrer() {
        let request = |line: &str| nginx_map(&[REQUEST_MAP], line, line);
        for (line, logged) in [
            ("GET /p?a=1&b=2 HTTP/1.1", "GET /p HTTP/1.1"),
            ("GET /plain HTTP/1.1", "GET /plain HTTP/1.1"),
            ("POST /login?next=/x?y HTTP/2.0", "POST /login HTTP/2.0"),
            // Refused by nginx, logged all the same.
            ("GET /bad?x=1 y=2 HTTP/1.1", "GET /bad HTTP/1.1"),
            ("GET /old?x=1", "GET /old"),
            ("GET /? HTTP/1.1", "GET / HTTP/1.1"),
            ("\\x16\\x03\\x01?garbage", "\\x16\\x03\\x01"),
            ("", ""),
        ] {
            assert_eq!(request(line), logged, "{line}");
        }
        let referer = |value: &str| nginx_map(&[REFERER_MAP], value, value);
        assert_eq!(referer("https://ref.example/p?q=secret&r=1"), "https://ref.example/p");
        assert_eq!(referer("https://ref.example/?"), "https://ref.example/");
        assert_eq!(referer("https://ref.example/p"), "https://ref.example/p");
    }

    fn http(port: u16, path: &str, headers: &[(&str, &str)]) {
        use std::io::{Read, Write};
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let mut request = format!("GET {path} HTTP/1.1\r\nHost: privacy.test\r\nConnection: close\r\n");
        for (name, value) in headers {
            request.push_str(&format!("{name}: {value}\r\n"));
        }
        request.push_str("\r\n");
        stream.write_all(request.as_bytes()).unwrap();
        let mut answer = String::new();
        let _ = stream.read_to_string(&mut answer);
        assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    }

    /// The lines the access log gained since it was `before` bytes long.
    fn new_lines(before: u64, expected: usize) -> Vec<String> {
        let read = || {
            let all = std::fs::read("/var/log/nginx/access.log").unwrap_or_default();
            String::from_utf8_lossy(&all[before as usize..]).into_owned()
        };
        for _ in 0..100 {
            let text = read();
            if text.lines().count() >= expected && text.ends_with('\n') {
                // A request logged twice would show up a moment later.
                std::thread::sleep(std::time::Duration::from_millis(200));
                return read().lines().map(str::to_string).collect();
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("the access log did not get {expected} new lines");
    }

    fn log_len() -> u64 {
        std::fs::metadata("/var/log/nginx/access.log").map(|m| m.len()).unwrap_or(0)
    }

    /// Apply, then give nginx the moment it takes to start the workers that
    /// log the new way.
    async fn apply_and_settle(mask: bool, strip: bool) {
        apply_log_privacy(mask, strip).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(700)).await;
    }

    /// A log line with the time taken out.
    fn timeless(line: &str) -> String {
        let (start, rest) = line.split_once('[').unwrap();
        format!("{start}[]{}", rest.split_once(']').unwrap().1)
    }

    /// Run on a disposable machine, by itself (the privacy tests that are
    /// ignored share the one nginx): `cargo test -- --ignored a_real_nginx`.
    /// It rewrites /etc/nginx/nginx.conf and reloads nginx several times, and
    /// puts everything back. In a container without systemd, put a
    /// `systemctl` on PATH that runs `nginx -s reload` for `reload nginx`.
    #[tokio::test]
    #[ignore = "root only: needs a running nginx with the stock nginx.conf, and changes its configuration for a moment (cargo test -- --ignored)"]
    async fn a_real_nginx_logs_masked_addresses_and_no_query_strings_and_is_put_back() {
        const VHOST: &str = "/etc/nginx/conf.d/zz-vps-panel-privacy-test.conf";
        const BROKEN: &str = "/etc/nginx/conf.d/zz-vps-panel-privacy-broken.conf";
        const PORT: u16 = 18088;
        // A vhost someone wrote by hand, with a log of its own.
        const OWN_PORT: u16 = 18089;
        const OWN_LOG: &str = "/var/log/nginx/vps-panel-privacy-test-own.log";
        let original = std::fs::read(NGINX_CONF).unwrap();
        assert!(!Path::new(LOG_PRIVACY_CONF).exists(), "start from an untouched nginx");
        // $remote_addr comes from a header here, so IPv6 clients can be
        // tried on a machine without IPv6.
        std::fs::write(
            VHOST,
            format!(
                "server {{\n    listen 127.0.0.1:{PORT};\n    set_real_ip_from 127.0.0.1;\n    real_ip_header X-Test-Addr;\n    location / {{ return 200; }}\n}}\n\
                 server {{\n    listen 127.0.0.1:{OWN_PORT};\n    access_log {OWN_LOG};\n    location / {{ return 200; }}\n}}\n"
            ),
        )
        .unwrap();
        let _ = reload().await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(700)).await;
        let query = "/p?a=1&b=2";
        let headers = [("Referer", "https://ref.example/p?q=secret"), ("User-Agent", "privacy-test")];

        // Off: nginx's own format, and nothing of ours on disk.
        apply_and_settle(false, false).await;
        assert_eq!(std::fs::read(NGINX_CONF).unwrap(), original);
        let before = log_len();
        http(PORT, query, &headers);
        let stock_line = new_lines(before, 1).remove(0);
        assert_eq!(
            timeless(&stock_line),
            "127.0.0.1 - - [] \"GET /p?a=1&b=2 HTTP/1.1\" 200 0 \"https://ref.example/p?q=secret\" \"privacy-test\""
        );

        // Masked and stripped: each request is logged once, in that form.
        apply_and_settle(true, true).await;
        assert!(std::fs::read_to_string(NGINX_CONF).unwrap().contains(MARKED_LINE));
        let state = log_state(&dump_config().await.unwrap());
        assert!(state.dropin_loaded && state.masks && state.strips && state.http_full.is_empty());
        // The hand-written vhost is found, not rewritten: it logs in full.
        assert_eq!(state.elsewhere, [AccessLogAt { file: VHOST.into(), line: 9, format: None }]);
        http(OWN_PORT, query, &headers);
        std::thread::sleep(std::time::Duration::from_millis(300));
        let own = std::fs::read_to_string(OWN_LOG).unwrap();
        assert!(own.starts_with("127.0.0.1 - - [") && own.contains("GET /p?a=1&b=2 HTTP/1.1"), "{own}");
        let before = log_len();
        http(PORT, query, &headers);
        http(PORT, "/plain", &[("User-Agent", "privacy-test")]);
        let lines = new_lines(before, 2);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(
            timeless(&lines[0]),
            "127.0.0.0 - - [] \"GET /p HTTP/1.1\" 200 0 \"https://ref.example/p\" \"privacy-test\""
        );
        assert_eq!(timeless(&lines[1]), "127.0.0.0 - - [] \"GET /plain HTTP/1.1\" 200 0 \"-\" \"privacy-test\"");

        // Every shape of IPv6 address, and IPv4, against mask_ip.
        let mut sent: Vec<String> = vec!["203.0.113.87".into(), "::ffff:203.0.113.87".into(), "::1".into()];
        for zeros in 0u16..256 {
            let mut groups = [0x2001u16, 0xdb8, 0x85a3, 0x8d3, 0x1, 0xffff, 0xcb00, 0x7157];
            for (i, g) in groups.iter_mut().enumerate() {
                if zeros & (1 << i) != 0 {
                    *g = 0;
                }
            }
            sent.push(Ipv6Addr::from(groups).to_string());
        }
        let before = log_len();
        for addr in &sent {
            http(PORT, "/a?secret=1", &[("X-Test-Addr", addr.as_str())]);
        }
        let lines = new_lines(before, sent.len());
        assert_eq!(lines.len(), sent.len());
        for (addr, line) in sent.iter().zip(&lines) {
            let masked = mask_ip(addr).unwrap();
            assert!(line.starts_with(&format!("{masked} - - [")), "{addr}: {line}");
            assert!(!line.contains('?') && !line.contains("secret"), "{line}");
        }

        // One setting at a time.
        apply_and_settle(true, false).await;
        let before = log_len();
        http(PORT, query, &headers);
        assert_eq!(
            timeless(&new_lines(before, 1)[0]),
            "127.0.0.0 - - [] \"GET /p?a=1&b=2 HTTP/1.1\" 200 0 \"https://ref.example/p?q=secret\" \"privacy-test\""
        );
        apply_and_settle(false, true).await;
        let before = log_len();
        http(PORT, query, &headers);
        assert_eq!(
            timeless(&new_lines(before, 1)[0]),
            "127.0.0.1 - - [] \"GET /p HTTP/1.1\" 200 0 \"https://ref.example/p\" \"privacy-test\""
        );

        // A configuration nginx refuses: both files are put back as they
        // were, to the modification time.
        let conf_on = std::fs::read(NGINX_CONF).unwrap();
        let dropin_on = std::fs::read(LOG_PRIVACY_CONF).unwrap();
        let times = || [NGINX_CONF, LOG_PRIVACY_CONF].map(|p| std::fs::metadata(p).unwrap().modified().unwrap());
        let times_on = times();
        std::fs::write(BROKEN, "no_such_directive on;\n").unwrap();
        for (mask, strip) in [(false, false), (true, true)] {
            let refused = apply_log_privacy(mask, strip).await.unwrap_err();
            assert!(refused.contains("put back"), "{refused}");
            assert_eq!(std::fs::read(NGINX_CONF).unwrap(), conf_on);
            assert_eq!(std::fs::read(LOG_PRIVACY_CONF).unwrap(), dropin_on);
            assert_eq!(times(), times_on);
        }
        std::fs::remove_file(BROKEN).unwrap();

        // Off again: the stock file to the byte, and the stock line format.
        apply_and_settle(false, false).await;
        assert_eq!(std::fs::read(NGINX_CONF).unwrap(), original);
        assert!(!Path::new(LOG_PRIVACY_CONF).exists());
        let before = log_len();
        http(PORT, query, &headers);
        assert_eq!(timeless(&new_lines(before, 1)[0]), timeless(&stock_line));

        std::fs::remove_file(VHOST).unwrap();
        let _ = std::fs::remove_file(OWN_LOG);
        let _ = reload().await.unwrap();
    }
}
