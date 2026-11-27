use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use tokio::net::lookup_host;

pub fn parse_ipv4(s: &str) -> Option<String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => Some(v4.to_string()),
        _ => None,
    }
}

pub fn parse_ipv6(s: &str) -> Option<String> {
    let trimmed = s.trim().trim_start_matches('[').trim_end_matches(']');
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.parse::<IpAddr>() {
        Ok(IpAddr::V6(v6)) => Some(v6.to_string()),
        _ => None,
    }
}

/// This server's IPv4, for DNS records, SPF and the mail checks.
///
/// A caller-supplied address wins. Otherwise (`Ipv4Facts::address`):
///   1. An address the panel hostname resolves to here that is on one of this
///      server's interfaces. Only public answers count: Debian maps the
///      machine's own name to 127.0.1.1 in /etc/hosts (Settings > Server
///      hostname writes the same line), and the lookup reads that file first.
///      When the panel hostname was also the server's hostname, new records
///      were written with 127.0.1.1.
///   2. The hostname resolves to an address that is on no interface. When the
///      server has a public address of its own, that one is used: the name is
///      behind a proxy or CDN, or still points at the server it is moving
///      from, and its address went into every zone's A records and SPF `ip4:`
///      all the same, so mail from this server failed SPF. When the server
///      has private addresses only, it is behind NAT and the hostname's
///      address is its public one: that is kept.
///   3. The hostname does not resolve: the server's own public address, else
///      its first private one, as install.sh finds them.
///
/// "Its own public address" is the one outgoing connections leave from when
/// that is public, else the first public one on an interface.
///
/// The certificate preflight accepts more than this one address
/// (`Ipv4Facts::reaching`).
pub async fn resolve_ipv4(caller_supplied: Option<&str>, panel_host: &str) -> Option<String> {
    // Loopback or link-local is never what a caller meant for public DNS; a
    // private address may be (a LAN setup), so that stays.
    if let Some(ip) = caller_supplied
        .and_then(|s| s.trim().parse::<Ipv4Addr>().ok())
        .filter(|ip| !ip.is_loopback() && !ip.is_unspecified() && !ip.is_link_local())
    {
        return Some(ip.to_string());
    }
    ipv4_facts(panel_host).await.address()
}

/// What is known about this server's IPv4, for DNS records and outgoing mail.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Ipv4Facts {
    /// The panel hostname the A was looked up for, or the address that
    /// stands in for one.
    pub host: String,
    /// The public addresses it resolves to on this server; the address
    /// itself when no hostname is set.
    pub named: Vec<String>,
    /// Every IPv4 address on this server's interfaces with the device it is
    /// on, loopback aside.
    pub interfaces: Vec<(String, String)>,
    /// The address outgoing connections leave from when a program does not
    /// ask for one.
    pub default_source: Option<String>,
}

pub(crate) fn is_public_v4(ip: &str) -> bool {
    ip.parse::<Ipv4Addr>().is_ok_and(|ip| !is_private_v4(&ip))
}

impl Ipv4Facts {
    pub fn on_interface(&self, ip: &str) -> bool {
        self.interfaces.iter().any(|(_, a)| a == ip)
    }

    /// The device an address is configured on.
    pub fn device_of(&self, ip: &str) -> Option<&str> {
        self.interfaces
            .iter()
            .find(|(_, a)| a == ip)
            .map(|(dev, _)| dev.as_str())
    }

    /// The server's own public address: the one outgoing connections leave
    /// from when that is public, else the first public one on an interface.
    fn own_public(&self) -> Option<&str> {
        self.default_source
            .as_deref()
            .filter(|s| is_public_v4(s) && self.on_interface(s))
            .or_else(|| {
                self.interfaces
                    .iter()
                    .map(|(_, a)| a.as_str())
                    .find(|a| is_public_v4(a))
            })
    }

    /// This server's IPv4 by the rule at `resolve_ipv4`.
    pub fn address(&self) -> Option<String> {
        if let Some(mine) = self.named.iter().find(|a| self.on_interface(a)) {
            return Some(mine.clone());
        }
        match (self.own_public(), self.named.first()) {
            (Some(own), _) => Some(own.to_string()),
            (None, Some(named)) => Some(named.clone()),
            (None, None) => self.interfaces.first().map(|(_, a)| a.clone()),
        }
    }

    /// The address the panel hostname resolves to, when that is not this
    /// server: a proxy in front of the panel, or the server the name used to
    /// point at.
    pub fn foreign(&self) -> Option<&str> {
        if self.named.iter().any(|a| self.on_interface(a)) {
            return None;
        }
        self.own_public()?;
        self.named.first().map(String::as_str)
    }

    /// Every IPv4 address a name may carry and still reach this server: the
    /// ones on its interfaces (nginx listens on all of them) and the ones
    /// the panel hostname resolves to. The panel itself is reached through
    /// those, whatever stands in front of it, and a name that pointed there
    /// passed the certificate preflight before `address` told the two apart.
    pub fn reaching(&self) -> Vec<String> {
        let mut own: Vec<String> = Vec::new();
        let all = self
            .address()
            .into_iter()
            .chain(self.interfaces.iter().map(|(_, a)| a.clone()))
            .chain(self.named.iter().cloned());
        for ip in all {
            if !own.contains(&ip) {
                own.push(ip);
            }
        }
        own
    }
}

/// `config_host` is the panel hostname the agent started with; the one .env
/// has now is used (`panel_host_now`). After Settings > Panel hostname the
/// old name was looked up until the agent restarted, and once that name was
/// gone a server behind NAT wrote its private address into new records.
pub async fn ipv4_facts(config_host: &str) -> Ipv4Facts {
    let host = panel_host_now(config_host).await;
    let named = match parse_ipv4(&host) {
        Some(ip) => vec![ip],
        // The system resolver can take its full timeout when DNS is down.
        None => tokio::time::timeout(
            std::time::Duration::from_secs(5),
            lookup_host(format!("{host}:0")),
        )
        .await
        .ok()
        .and_then(Result::ok)
        .map(|addrs| public_ipv4s(addrs.map(|a| a.ip())))
        .unwrap_or_default(),
    };
    Ipv4Facts {
        host,
        named,
        interfaces: ipv4_devices().await,
        default_source: default_source_ipv4().await,
    }
}

/// This server's IPv6: caller-supplied, literal, or the panel hostname as it
/// resolves here, never a loopback or link-local answer. No interface
/// fallback: an AAAA is only written for an address someone published.
pub async fn resolve_ipv6(caller_supplied: Option<&str>, panel_host: &str) -> Option<String> {
    if let Some(ip) = caller_supplied
        .and_then(|s| s.trim().trim_start_matches('[').trim_end_matches(']').parse::<Ipv6Addr>().ok())
        .filter(usable_ipv6)
    {
        return Some(ip.to_string());
    }
    if let Some(ip) = parse_ipv6(panel_host) {
        return Some(ip);
    }
    match lookup_host(format!("{panel_host}:0")).await {
        Ok(iter) => iter
            .filter_map(|addr| match addr.ip() {
                IpAddr::V6(v6) if usable_ipv6(&v6) => Some(v6.to_string()),
                _ => None,
            })
            .next(),
        Err(_) => None,
    }
}

fn usable_ipv6(ip: &Ipv6Addr) -> bool {
    // fe80::/10 is link-local.
    !ip.is_loopback() && !ip.is_unspecified() && (ip.segments()[0] & 0xffc0) != 0xfe80
}

/// The panel hostname as .env has it now. The agent's config is read once at
/// start, so after Settings > Panel hostname it names the old host until the
/// agent restarts: an AAAA added for the new name was not seen until then.
pub async fn panel_host_now(config_host: &str) -> String {
    let live = crate::handlers::system::current_panel_host().await;
    if live.is_empty() {
        config_host.to_string()
    } else {
        live
    }
}

/// Where the server's IPv6 address in effect comes from (`Ipv6Facts::in_effect`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Ipv6Source {
    /// The operator chose it (`ipv6.address`).
    Setting,
    /// The panel hostname's AAAA, which is on an interface here.
    Hostname,
    /// The only global address on the interfaces.
    Interface,
    /// There is none.
    None,
}

/// What is known about this server's IPv6, for DNS records and outgoing mail.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Ipv6Facts {
    /// The panel hostname the AAAA was looked up for.
    pub host: String,
    /// Its AAAA as this server resolves it.
    pub published: Option<String>,
    /// The global IPv6 addresses configured on this server's interfaces.
    pub on_interfaces: Vec<String>,
    /// `ipv6.address` as stored: empty for automatic.
    pub setting: String,
}

impl Ipv6Facts {
    /// The published address, when it really is an address of this server.
    pub fn usable(&self) -> Option<String> {
        self.published
            .as_ref()
            .filter(|p| self.on_interfaces.contains(p))
            .cloned()
    }

    /// The address the operator chose, whether or not it is still on an
    /// interface; None for automatic.
    pub fn chosen(&self) -> Option<String> {
        parse_ipv6(&self.setting)
    }

    /// The server's IPv6 address for the records the panel writes (AAAA, SPF
    /// `ip6:`), and where it comes from:
    ///   1. The address the operator chose, while it is on an interface. One
    ///      that has left the server gives none: nothing is published for an
    ///      address that is not here.
    ///   2. Automatic: the panel hostname's AAAA when that is on an interface.
    ///      The AAAA alone is not enough: a panel hostname behind a proxy, or
    ///      one that still points at the server it is being moved from,
    ///      resolves to an address that is not this server, and it was
    ///      written into every zone's AAAA and SPF records all the same.
    ///   3. Automatic: the only global address on the interfaces. Private
    ///      ones (fc00::/7) do not count; with several, the operator picks.
    pub fn in_effect(&self) -> (Option<String>, Ipv6Source) {
        if let Some(chosen) = self.chosen() {
            return if self.on_interfaces.contains(&chosen) {
                (Some(chosen), Ipv6Source::Setting)
            } else {
                (None, Ipv6Source::None)
            };
        }
        if let Some(named) = self.usable() {
            return (Some(named), Ipv6Source::Hostname);
        }
        let mut public = self.on_interfaces.iter().filter(|a| !is_ula(a));
        match (public.next(), public.next()) {
            (Some(only), None) => (Some(only.clone()), Ipv6Source::Interface),
            _ => (None, Ipv6Source::None),
        }
    }

    pub fn address(&self) -> Option<String> {
        self.in_effect().0
    }

    /// Every IPv6 address an AAAA record or SPF `ip6:` term may name and be
    /// this server's: the ones on the interfaces, and the one chosen also
    /// after it left them, so that records still naming it can be moved.
    pub fn own(&self) -> Vec<String> {
        let mut own = self.on_interfaces.clone();
        if let Some(chosen) = self.chosen() {
            if !own.contains(&chosen) {
                own.push(chosen);
            }
        }
        own
    }
}

/// fc00::/7: unique local addresses, not reachable from the internet.
pub fn is_ula(addr: &str) -> bool {
    addr.parse::<Ipv6Addr>()
        .is_ok_and(|ip| (ip.segments()[0] & 0xfe00) == 0xfc00)
}

/// `ipv6.address` from panel.db: the address the operator chose, empty for
/// automatic (also when the panel has not created its settings yet).
pub fn ipv6_setting(panel_db: &crate::db::PanelDb) -> String {
    panel_db
        .with(|conn| {
            conn.query_row(
                "SELECT value FROM settings WHERE key = 'ipv6.address'",
                [],
                |row| row.get::<_, String>(0),
            )
        })
        .map(|v| v.trim().to_string())
        .unwrap_or_default()
}

/// `ipv6.new_domains`: whether a new zone gets IPv6 when nobody says.
/// Off unless stored as on.
pub fn ipv6_new_domains(panel_db: &crate::db::PanelDb) -> bool {
    panel_db
        .with(|conn| {
            conn.query_row(
                "SELECT value FROM settings WHERE key = 'ipv6.new_domains'",
                [],
                |row| row.get::<_, String>(0),
            )
        })
        .is_ok_and(|v| v.trim() == "on")
}

pub async fn ipv6_facts(panel_db: &crate::db::PanelDb, config_host: &str) -> Ipv6Facts {
    let host = panel_host_now(config_host).await;
    // The system resolver can take its full timeout when DNS is down.
    let published = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        resolve_ipv6(None, &host),
    )
    .await
    .unwrap_or(None);
    Ipv6Facts {
        host,
        published,
        on_interfaces: ipv6s_on_interfaces().await,
        setting: ipv6_setting(panel_db),
    }
}

/// This server's IPv6 for the records the panel writes (AAAA, SPF `ip6:`):
/// `Ipv6Facts::in_effect` has the rule. Outgoing mail has a stricter one of
/// its own (`mail_ip::mail_ipv6`).
pub async fn server_ipv6(panel_db: &crate::db::PanelDb, config_host: &str) -> Option<String> {
    ipv6_facts(panel_db, config_host).await.address()
}

/// The global IPv6 addresses on this server's interfaces.
pub async fn ipv6s_on_interfaces() -> Vec<String> {
    ipv6_devices().await.into_iter().map(|(_, ip)| ip).collect()
}

/// The global IPv6 addresses on this server's interfaces with the device
/// each is on.
pub async fn ipv6_devices() -> Vec<(String, String)> {
    match crate::exec::run("ip", ["-6", "-o", "addr", "show", "scope", "global"]).await {
        Ok(out) => parse_interface_ipv6_devices(&out.stdout),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
fn parse_interface_ipv6s(ip_output: &str) -> Vec<String> {
    parse_interface_ipv6_devices(ip_output)
        .into_iter()
        .map(|(_, ip)| ip)
        .collect()
}

/// `ip -6 -o addr show scope global` lines look like
/// `2: eth0    inet6 2001:db8::1/64 scope global dynamic mngtmpaddr \  valid_lft ...`.
/// Left out: addresses still in duplicate detection or that failed it, and
/// temporary (privacy) or deprecated ones, which go away on their own.
fn parse_interface_ipv6_devices(ip_output: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in ip_output.lines() {
        let mut it = line.split_whitespace();
        let Some(dev) = it.nth(1).filter(|dev| *dev != "lo") else {
            continue;
        };
        let mut it = it.skip_while(|t| *t != "inet6");
        let Some(ip) = it
            .nth(1)
            .and_then(|cidr| cidr.split('/').next())
            .and_then(|s| s.parse::<Ipv6Addr>().ok())
            .filter(usable_ipv6)
        else {
            continue;
        };
        let transient = it
            .take_while(|t| !t.starts_with('\\'))
            .any(|flag| matches!(flag, "tentative" | "dadfailed" | "temporary" | "deprecated"));
        let ip = ip.to_string();
        if !transient && !out.iter().any(|(_, a)| *a == ip) {
            out.push((dev.to_string(), ip));
        }
    }
    out
}

/// Every IPv4 address on this server's interfaces with the device it is on,
/// loopback interface aside.
async fn ipv4_devices() -> Vec<(String, String)> {
    match crate::exec::run("ip", ["-4", "-o", "addr", "show", "scope", "global"]).await {
        Ok(out) => parse_interface_ipv4s(&out.stdout),
        Err(_) => Vec::new(),
    }
}

/// `ip -4 -o addr show scope global` lines look like
/// `2: eth0    inet 1.2.3.4/24 brd ... scope global eth0\ ...`. The device is
/// the second field, also for an address with a label of its own (`eth0:1`).
fn parse_interface_ipv4s(ip_output: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in ip_output.lines() {
        let mut it = line.split_whitespace();
        let Some(dev) = it.nth(1).filter(|dev| *dev != "lo") else {
            continue;
        };
        if let Some(ip) = it
            .skip_while(|t| *t != "inet")
            .nth(1)
            .and_then(|cidr| cidr.split('/').next())
            .and_then(|s| s.parse::<Ipv4Addr>().ok())
        {
            let ip = ip.to_string();
            if !out.iter().any(|(_, a)| *a == ip) {
                out.push((dev.to_string(), ip));
            }
        }
    }
    out
}

/// The word after `key` in `ip route get` output.
fn route_field(output: &str, key: &str) -> Option<String> {
    let mut it = output.split_whitespace().skip_while(|t| *t != key);
    it.nth(1).map(str::to_string)
}

/// The IPv4 address the kernel sends from when a program does not ask for
/// one. `ip -4 route get 1.1.1.1` prints
/// `1.1.1.1 via 172.31.1.1 dev eth0 src 203.0.113.5 uid 0`; it only asks the
/// routing table, nothing is sent.
pub async fn default_source_ipv4() -> Option<String> {
    let out = crate::exec::run("ip", ["-4", "route", "get", "1.1.1.1"])
        .await
        .ok()?;
    route_field(&out.stdout, "src").and_then(|s| parse_ipv4(&s))
}

/// The device packets sent from `addr` leave through.
/// `ip -4 route get 1.1.1.1 from 203.0.113.9` prints
/// `1.1.1.1 from 203.0.113.9 via 172.31.1.1 dev eth0 uid 0`, following the
/// routing rules for that source address. None when the kernel has no such
/// route (the address is not on this server, or there is no default route).
pub async fn route_dev_from(addr: &str) -> Option<String> {
    let addr = parse_ipv4(addr)?;
    let out = crate::exec::run("ip", ["-4", "route", "get", "1.1.1.1", "from", addr.as_str()])
        .await
        .ok()?;
    route_field(&out.stdout, "dev")
}

/// Loopback, private (RFC 1918), link-local, carrier-grade NAT and 0/8: none
/// of these reach this server from the internet.
pub fn is_private_v4(ip: &Ipv4Addr) -> bool {
    let oct = ip.octets();
    matches!(oct[0], 0 | 10 | 127)
        || (oct[0] == 172 && (16..=31).contains(&oct[1]))
        || (oct[0] == 192 && oct[1] == 168)
        || (oct[0] == 169 && oct[1] == 254)
        || (oct[0] == 100 && (64..=127).contains(&oct[1]))
}

/// The public IPv4 addresses among a lookup's answers, in their order.
fn public_ipv4s(addrs: impl Iterator<Item = IpAddr>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for addr in addrs {
        if let IpAddr::V4(v4) = addr {
            let ip = v4.to_string();
            if !is_private_v4(&v4) && !out.contains(&ip) {
                out.push(ip);
            }
        }
    }
    out
}

/// The server's public IPv4 from its interfaces, as install.sh detects it:
/// the first public global-scope address, else the first private one (a NAT'd
/// box), never one on the loopback interface.
pub async fn ipv4_on_interfaces() -> Option<String> {
    let out = crate::exec::run("ip", ["-4", "-o", "addr", "show", "scope", "global"])
        .await
        .ok()?;
    pick_interface_ipv4(&out.stdout)
}

/// `ip -4 -o addr show scope global` lines look like
/// `2: eth0    inet 1.2.3.4/24 brd ... scope global eth0\ ...`.
fn pick_interface_ipv4(ip_output: &str) -> Option<String> {
    let mut fallback: Option<Ipv4Addr> = None;
    for line in ip_output.lines() {
        let mut it = line.split_whitespace();
        // WSL puts a "global" address on lo; nothing reaches it from outside.
        if it.nth(1) == Some("lo") {
            continue;
        }
        let Some(ip) = it
            .skip_while(|t| *t != "inet")
            .nth(1)
            .and_then(|cidr| cidr.split('/').next())
            .and_then(|s| s.parse::<Ipv4Addr>().ok())
        else {
            continue;
        };
        if !is_private_v4(&ip) {
            return Some(ip.to_string());
        }
        fallback.get_or_insert(ip);
    }
    fallback.map(|ip| ip.to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        is_ula, parse_interface_ipv4s, parse_interface_ipv6_devices, parse_interface_ipv6s,
        pick_interface_ipv4, public_ipv4s, resolve_ipv4, resolve_ipv6, route_field, Ipv4Facts,
        Ipv6Facts,
    };
    use std::net::IpAddr;

    /// A server as its interfaces and routing show it. `named` is what the
    /// panel hostname resolves to; the first interface address is the one
    /// outgoing connections leave from.
    fn v4(named: &[&str], interfaces: &[(&str, &str)]) -> Ipv4Facts {
        Ipv4Facts {
            host: "panel.example.com".into(),
            named: named.iter().map(|s| s.to_string()).collect(),
            interfaces: interfaces
                .iter()
                .map(|(dev, ip)| (dev.to_string(), ip.to_string()))
                .collect(),
            default_source: interfaces.first().map(|(_, ip)| ip.to_string()),
        }
    }

    #[test]
    fn the_hostnames_address_is_this_servers_when_it_is_on_an_interface() {
        // The usual VPS.
        let f = v4(&["198.51.100.127"], &[("eth0", "198.51.100.127")]);
        assert_eq!(f.address().as_deref(), Some("198.51.100.127"));
        assert_eq!(f.foreign(), None);
        // Two public addresses, the panel hostname on the second: the name
        // decides, not the address connections leave from.
        let f = v4(
            &["203.0.113.9"],
            &[("eth0", "198.51.100.127"), ("eth0", "203.0.113.9")],
        );
        assert_eq!(f.address().as_deref(), Some("203.0.113.9"));
        assert_eq!(f.foreign(), None);
        // A name with two answers, one of them here.
        let f = v4(&["192.0.2.50", "198.51.100.127"], &[("eth0", "198.51.100.127")]);
        assert_eq!(f.address().as_deref(), Some("198.51.100.127"));
        assert_eq!(f.foreign(), None);
        // A LAN install named by its private address.
        let f = v4(&["192.168.1.10"], &[("eth0", "192.168.1.10")]);
        assert_eq!(f.address().as_deref(), Some("192.168.1.10"));
    }

    #[test]
    fn a_hostname_pointing_elsewhere_does_not_name_a_server_with_a_public_address() {
        // The panel behind a CDN, or a name that still points at the server
        // it is being moved from.
        let f = v4(&["104.16.1.1"], &[("eth0", "10.0.0.5"), ("eth0", "198.51.100.127")]);
        assert_eq!(f.address().as_deref(), Some("198.51.100.127"));
        assert_eq!(f.foreign(), Some("104.16.1.1"));
        // The name's address still passes the certificate preflight.
        assert_eq!(f.reaching(), vec!["198.51.100.127", "10.0.0.5", "104.16.1.1"]);

        // Of several public addresses, the one connections leave from.
        let mut f = v4(
            &["104.16.1.1"],
            &[("eth0", "198.51.100.127"), ("eth1", "203.0.113.9")],
        );
        f.default_source = Some("203.0.113.9".into());
        assert_eq!(f.address().as_deref(), Some("203.0.113.9"));
        // Leaving through a private address (a VPN, a NAT gateway) says
        // nothing: the first public one on an interface.
        f.default_source = Some("10.8.0.2".into());
        assert_eq!(f.address().as_deref(), Some("198.51.100.127"));
        f.default_source = None;
        assert_eq!(f.address().as_deref(), Some("198.51.100.127"));
    }

    #[test]
    fn behind_nat_the_hostnames_address_is_the_servers_public_one() {
        let f = v4(&["198.51.100.127"], &[("ens5", "172.31.20.4")]);
        assert_eq!(f.address().as_deref(), Some("198.51.100.127"));
        assert_eq!(f.foreign(), None);
        assert_eq!(f.reaching(), vec!["198.51.100.127", "172.31.20.4"]);
        // The interfaces could not be read: as before, the name is believed.
        let f = v4(&["198.51.100.127"], &[]);
        assert_eq!(f.address().as_deref(), Some("198.51.100.127"));
    }

    #[test]
    fn without_a_resolving_hostname_the_interfaces_decide() {
        let f = v4(&[], &[("eth0", "10.0.0.5"), ("eth0", "198.51.100.127")]);
        assert_eq!(f.address().as_deref(), Some("198.51.100.127"));
        let f = v4(&[], &[("eth0", "10.0.0.5"), ("docker0", "172.17.0.1")]);
        assert_eq!(f.address().as_deref(), Some("10.0.0.5"));
        assert_eq!(f.foreign(), None);
        assert_eq!(v4(&[], &[]).address(), None);
    }

    #[test]
    fn ip_route_get_names_the_source_address_and_the_device() {
        let plain = "1.1.1.1 via 172.31.1.1 dev eth0 src 203.0.113.5 uid 0 \n    cache \n";
        assert_eq!(route_field(plain, "src").as_deref(), Some("203.0.113.5"));
        assert_eq!(route_field(plain, "dev").as_deref(), Some("eth0"));
        let from = "1.1.1.1 from 203.0.113.9 via 203.0.113.1 dev eth1 table 100 uid 0 \n    cache \n";
        assert_eq!(route_field(from, "dev").as_deref(), Some("eth1"));
        assert_eq!(route_field(from, "src"), None);
        assert_eq!(route_field("RTNETLINK answers: Network is unreachable", "src"), None);
        assert_eq!(route_field("", "dev"), None);
    }

    #[test]
    fn interface_ipv6s_leave_out_addresses_that_come_and_go() {
        let out = "1: lo    inet6 2001:db8::99/128 scope global \\       valid_lft forever preferred_lft forever\n\
                   2: eth0    inet6 2001:db8:1::1/64 scope global \\       valid_lft forever preferred_lft forever\n\
                   2: eth0    inet6 2001:db8:1:0:5054:ff:fe12:3456/64 scope global dynamic mngtmpaddr noprefixroute \\       valid_lft 86395sec preferred_lft 14395sec\n\
                   2: eth0    inet6 2001:db8:1:0:a1b2:c3d4:e5f6:789/64 scope global temporary dynamic \\       valid_lft 600sec preferred_lft 300sec\n\
                   2: eth0    inet6 2001:db8:1::dead/64 scope global tentative \\       valid_lft forever preferred_lft forever\n\
                   2: eth0    inet6 2001:db8:1::beef/64 scope global deprecated \\       valid_lft 100sec preferred_lft 0sec\n\
                   2: eth0    inet6 2001:DB8:1::1/64 scope global \\       valid_lft forever preferred_lft forever\n";
        assert_eq!(
            parse_interface_ipv6s(out),
            vec!["2001:db8:1::1".to_string(), "2001:db8:1:0:5054:ff:fe12:3456".to_string()]
        );
        assert!(parse_interface_ipv6s("").is_empty());
    }

    #[test]
    fn interface_ipv4s_list_every_address_but_lo() {
        let out = "1: lo    inet 10.255.255.254/32 brd 10.255.255.254 scope global lo\n\
                   2: eth0    inet 198.51.100.127/24 brd 198.51.100.255 scope global eth0\n\
                   2: eth0    inet 203.0.113.9/32 scope global secondary eth0:1\n\
                   3: ens10    inet 10.0.0.5/32 brd 10.0.0.5 scope global dynamic ens10\\       valid_lft 80000sec preferred_lft 80000sec\n\
                   2: eth0    inet 198.51.100.127/24 brd 198.51.100.255 scope global eth0\n";
        assert_eq!(
            parse_interface_ipv4s(out),
            vec![
                ("eth0".to_string(), "198.51.100.127".to_string()),
                ("eth0".to_string(), "203.0.113.9".to_string()),
                ("ens10".to_string(), "10.0.0.5".to_string()),
            ]
        );
        assert!(parse_interface_ipv4s("").is_empty());
    }

    #[test]
    fn a_published_ipv6_counts_only_when_it_is_on_an_interface() {
        let mut facts = Ipv6Facts {
            host: "panel.example.com".into(),
            published: Some("2001:db8:1::1".into()),
            on_interfaces: vec!["2001:db8:1::1".into(), "2001:db8:1::2".into()],
            setting: String::new(),
        };
        assert_eq!(facts.usable().as_deref(), Some("2001:db8:1::1"));
        // The panel hostname behind a proxy, or still pointing at another server.
        facts.published = Some("2606:4700::6810:1".into());
        assert_eq!(facts.usable(), None);
        // An address on the server that nobody published.
        facts.published = None;
        assert_eq!(facts.usable(), None);
    }

    fn v6(published: Option<&str>, on: &[&str], setting: &str) -> Ipv6Facts {
        Ipv6Facts {
            host: "panel.example.com".into(),
            published: published.map(str::to_string),
            on_interfaces: on.iter().map(|s| s.to_string()).collect(),
            setting: setting.into(),
        }
    }

    #[test]
    fn the_address_in_effect_is_the_choice_else_the_hostnames_else_the_only_one() {
        use super::Ipv6Source as S;
        let two = ["2001:db8:1::1", "2001:db8:1::2"];
        // The operator's choice wins, whatever the panel hostname says.
        let f = v6(Some("2001:db8:1::1"), &two, "2001:DB8:1:0::2");
        assert_eq!(f.in_effect(), (Some("2001:db8:1::2".into()), S::Setting));
        // A choice that has left the interfaces gives none, and stays ours.
        let f = v6(Some("2001:db8:1::1"), &two, "2001:db8:9::9");
        assert_eq!(f.in_effect(), (None, S::None));
        assert_eq!(f.own(), vec!["2001:db8:1::1", "2001:db8:1::2", "2001:db8:9::9"]);
        // Automatic: today's rule first.
        let f = v6(Some("2001:db8:1::2"), &two, "");
        assert_eq!(f.in_effect(), (Some("2001:db8:1::2".into()), S::Hostname));
        // The hostname's AAAA is another machine's: with two candidates, none.
        let f = v6(Some("2606:4700::1"), &two, "");
        assert_eq!(f.in_effect(), (None, S::None));
        assert_eq!(f.own(), vec!["2001:db8:1::1", "2001:db8:1::2"]);
        // With one global address that one, a private one next to it aside.
        let f = v6(Some("2606:4700::1"), &["fd00::5", "2001:db8:1::1"], "");
        assert_eq!(f.in_effect(), (Some("2001:db8:1::1".into()), S::Interface));
        let f = v6(None, &["2001:db8:1::1"], "");
        assert_eq!(f.address().as_deref(), Some("2001:db8:1::1"));
        assert_eq!(f.in_effect().1, S::Interface);
        // No address, or only private ones.
        assert_eq!(v6(None, &[], "").in_effect(), (None, S::None));
        assert_eq!(v6(None, &["fd00::5"], "").in_effect(), (None, S::None));
        // A private address can still be chosen (a LAN setup).
        assert_eq!(v6(None, &["fd00::5"], "fd00::5").in_effect(), (Some("fd00::5".into()), S::Setting));
        // Something unreadable stored is automatic.
        assert_eq!(v6(None, &["2001:db8:1::1"], "nonsense").in_effect().1, S::Interface);
        assert!(is_ula("fd12::1") && is_ula("fc00::1") && !is_ula("2001:db8::1") && !is_ula("x"));
    }

    #[test]
    fn interface_ipv6s_come_with_their_device() {
        let out = "2: eth0    inet6 2001:db8:1::1/64 scope global \\       valid_lft forever preferred_lft forever\n\
                   3: ens10    inet6 fd00::5/64 scope global \\       valid_lft forever preferred_lft forever\n\
                   1: lo    inet6 2001:db8::99/128 scope global \\       valid_lft forever preferred_lft forever\n\
                   2: eth0    inet6 2001:db8:1::9/64 scope global temporary dynamic \\       valid_lft 600sec preferred_lft 300sec\n";
        assert_eq!(
            parse_interface_ipv6_devices(out),
            vec![
                ("eth0".to_string(), "2001:db8:1::1".to_string()),
                ("ens10".to_string(), "fd00::5".to_string()),
            ]
        );
    }

    fn ips(list: &[&str]) -> Vec<IpAddr> {
        list.iter().map(|s| s.parse().unwrap()).collect()
    }

    #[test]
    fn a_lookup_answer_counts_only_when_public() {
        assert!(public_ipv4s(ips(&["127.0.1.1"]).into_iter()).is_empty());
        assert!(public_ipv4s(ips(&["10.0.0.5", "::1"]).into_iter()).is_empty());
        assert_eq!(
            public_ipv4s(
                ips(&["127.0.1.1", "198.51.100.127", "2001:db8::1", "203.0.113.9", "198.51.100.127"])
                    .into_iter()
            ),
            vec!["198.51.100.127", "203.0.113.9"]
        );
    }

    #[test]
    fn interfaces_give_the_public_address_else_the_first_private_never_lo() {
        let vps = "1: lo    inet 127.0.0.1/8 scope host lo\n\
                   2: eth0    inet 198.51.100.127/24 brd 198.51.100.255 scope global eth0\n";
        assert_eq!(pick_interface_ipv4(vps).as_deref(), Some("198.51.100.127"));
        let private_first = "2: ens3    inet 10.0.0.5/24 brd 10.0.0.255 scope global ens3\n\
                             3: ens4    inet 203.0.113.9/24 brd 203.0.113.255 scope global ens4\n";
        assert_eq!(pick_interface_ipv4(private_first).as_deref(), Some("203.0.113.9"));
        let wsl = "1: lo    inet 10.255.255.254/32 brd 10.255.255.254 scope global lo\n\
                   2: eth0    inet 172.20.130.192/20 brd 172.20.143.255 scope global eth0\n";
        assert_eq!(pick_interface_ipv4(wsl).as_deref(), Some("172.20.130.192"));
        assert_eq!(pick_interface_ipv4(""), None);
    }

    #[tokio::test]
    async fn a_loopback_seed_is_ignored_a_private_one_kept() {
        // Not the seed: whatever the machine running this has instead.
        assert_ne!(
            resolve_ipv4(Some("127.0.1.1"), "198.51.100.127").await.as_deref(),
            Some("127.0.1.1")
        );
        assert_eq!(resolve_ipv4(Some("10.1.2.3"), "x").await.as_deref(), Some("10.1.2.3"));
        assert_eq!(resolve_ipv6(Some("::1"), "2001:db8::7").await.as_deref(), Some("2001:db8::7"));
    }

    /// The real resolver on a box whose own name is 127.0.1.1 in /etc/hosts
    /// (Debian, WSL): run with --ignored there.
    #[tokio::test]
    #[ignore]
    async fn the_own_hostname_never_resolves_to_loopback() {
        let out = std::process::Command::new("hostname").arg("-f").output().unwrap();
        let fqdn = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let got = resolve_ipv4(None, &fqdn).await;
        println!("{fqdn} -> {got:?}");
        assert!(got.is_some_and(|ip| !ip.starts_with("127.")));
    }
}
