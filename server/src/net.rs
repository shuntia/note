use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};

/// Addresses the server must never be talked into fetching: anything that would
/// reach the host, the LAN, the carrier-grade NAT space Tailscale and ISPs use,
/// or a cloud metadata service.
fn blocked_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || a == 0
        || (a == 100 && (64..128).contains(&b))
}

fn blocked_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return blocked_v4(v4);
    }
    let first = ip.segments()[0];
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || (first & 0xfe00) == 0xfc00
        || (first & 0xffc0) == 0xfe80
}

pub fn blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => blocked_v4(v4),
        IpAddr::V6(v6) => blocked_v6(v6),
    }
}

/// The host of an `https://` URL, without userinfo or port. `None` for anything
/// this code will not vouch for, userinfo included: `https://push.example@10.0.0.1/`
/// reaches the address after the `@`, not the name before it.
fn https_host(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let host = match authority.strip_prefix('[') {
        Some(after) => after.split(']').next()?.to_string(),
        None => authority.split(':').next()?.to_string(),
    };
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// Whether a push endpoint may be stored: HTTPS, and a host that resolves
/// wholly outside the ranges above. A name that will not resolve is refused
/// too — an endpoint the server cannot reach is worth nothing, and resolving it
/// later could answer with an address this check never saw.
///
/// Blocking on DNS: call it off the async executor.
pub fn push_endpoint_ok(endpoint: &str) -> Result<(), &'static str> {
    let Some(host) = https_host(endpoint) else {
        return Err("endpoint must be an https:// URL with a plain host");
    };
    if host == "localhost" || host.ends_with(".localhost") {
        return Err("endpoint must not point at this server");
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return if blocked_ip(ip) {
            Err("endpoint must not point into a private network")
        } else {
            Ok(())
        };
    }
    let addrs = (host.as_str(), 443u16)
        .to_socket_addrs()
        .map_err(|_| "endpoint host does not resolve")?;
    let mut any = false;
    for addr in addrs {
        any = true;
        if blocked_ip(addr.ip()) {
            return Err("endpoint must not point into a private network");
        }
    }
    if any {
        Ok(())
    } else {
        Err("endpoint host does not resolve")
    }
}

/// Provenance for routes a foreign page must not be able to start: a browser
/// labels the request, and a non-browser client sends no label at all.
pub fn fetch_site_ok(headers: &axum::http::HeaderMap) -> bool {
    match headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        None => true,
        Some(v) => matches!(v, "same-origin" | "none"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_public_https_endpoint_is_accepted() {
        assert!(push_endpoint_ok("https://203.0.113.9/send/abc").is_ok());
        assert!(push_endpoint_ok("https://[2001:db8::1]:8443/send/abc").is_ok());
    }

    #[test]
    fn private_and_local_endpoints_are_refused() {
        for bad in [
            "http://203.0.113.9/send",
            "ftp://203.0.113.9/send",
            "https://",
            "https://127.0.0.1/send",
            "https://127.9.9.9/send",
            "https://[::1]/send",
            "https://[::ffff:127.0.0.1]/send",
            "https://10.0.0.1/send",
            "https://172.31.255.254/send",
            "https://192.168.1.1/send",
            "https://169.254.169.254/send",
            "https://100.64.0.1/send",
            "https://100.127.255.255/send",
            "https://0.0.0.0/send",
            "https://255.255.255.255/send",
            "https://[fd12::3]/send",
            "https://[fe80::1]/send",
            "https://localhost/send",
            "https://note.localhost/send",
            "https://push.example@10.0.0.1/send",
        ] {
            assert!(push_endpoint_ok(bad).is_err(), "accepted {bad}");
        }
    }

    #[test]
    fn cgnat_neighbours_outside_the_range_stay_reachable() {
        assert!(push_endpoint_ok("https://100.63.255.255/send").is_ok());
        assert!(push_endpoint_ok("https://100.128.0.1/send").is_ok());
    }

    #[test]
    fn an_unresolvable_host_is_refused() {
        assert!(push_endpoint_ok("https://nothing.invalid/send").is_err());
    }

    #[test]
    fn fetch_site_rejects_foreign_and_sibling_origins() {
        let mut h = axum::http::HeaderMap::new();
        assert!(fetch_site_ok(&h));
        for ok in ["same-origin", "none"] {
            h.insert("sec-fetch-site", ok.parse().unwrap());
            assert!(fetch_site_ok(&h), "{ok}");
        }
        for bad in ["cross-site", "same-site"] {
            h.insert("sec-fetch-site", bad.parse().unwrap());
            assert!(!fetch_site_ok(&h), "{bad}");
        }
    }
}
