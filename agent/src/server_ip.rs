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

/// This server's IPv4, for DNS records and the certificate preflight.
///
/// A caller-supplied or literal address wins. Next the panel hostname as this
/// server resolves it, but only a public answer: Debian maps the machine's own
/// name to 127.0.1.1 in /etc/hosts (Settings > Server hostname writes the same
/// line), and the lookup reads that file first. When the panel hostname was
/// also the server's hostname, new records were written with 127.0.1.1 and
/// the certificate preflight said the server was 127.0.1.1. Last the addresses
/// on the interfaces, as install.sh finds them.
pub async fn resolve_ipv4(caller_supplied: Option<&str>, panel_host: &str) -> Option<String> {
    // Loopback or link-local is never what a caller meant for public DNS; a
    // private address may be (a LAN setup), so that stays.
    if let Some(ip) = caller_supplied
        .and_then(|s| s.trim().parse::<Ipv4Addr>().ok())
        .filter(|ip| !ip.is_loopback() && !ip.is_unspecified() && !ip.is_link_local())
    {
        return Some(ip.to_string());
    }
    if let Some(ip) = parse_ipv4(panel_host) {
        return Some(ip);
    }
    if let Ok(addrs) = lookup_host(format!("{panel_host}:0")).await {
        if let Some(ip) = first_public_ipv4(addrs.map(|a| a.ip())) {
            return Some(ip.to_string());
        }
    }
    ipv4_on_interfaces().await
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

fn first_public_ipv4(addrs: impl Iterator<Item = IpAddr>) -> Option<Ipv4Addr> {
    addrs
        .filter_map(|a| match a {
            IpAddr::V4(v4) if !is_private_v4(&v4) => Some(v4),
            _ => None,
        })
        .next()
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
    use super::{first_public_ipv4, pick_interface_ipv4, resolve_ipv4, resolve_ipv6};
    use std::net::IpAddr;

    fn ips(list: &[&str]) -> Vec<IpAddr> {
        list.iter().map(|s| s.parse().unwrap()).collect()
    }

    #[test]
    fn a_lookup_answer_counts_only_when_public() {
        assert_eq!(first_public_ipv4(ips(&["127.0.1.1"]).into_iter()), None);
        assert_eq!(first_public_ipv4(ips(&["10.0.0.5", "::1"]).into_iter()), None);
        assert_eq!(
            first_public_ipv4(ips(&["127.0.1.1", "198.51.100.127"]).into_iter()).map(|ip| ip.to_string()),
            Some("198.51.100.127".into())
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
        assert_eq!(
            resolve_ipv4(Some("127.0.1.1"), "198.51.100.127").await.as_deref(),
            Some("198.51.100.127")
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
