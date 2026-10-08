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

/// A browser's `Origin` must be one of `allowed` or name the host the request
/// was sent to; a non-browser client sends none.
pub fn origin_ok(headers: &axum::http::HeaderMap, allowed: &[String]) -> bool {
    let Some(origin) = headers.get(axum::http::header::ORIGIN) else { return true };
    let Ok(origin) = origin.to_str() else { return false };
    let origin = origin.trim_end_matches('/').to_ascii_lowercase();
    if allowed.contains(&origin) {
        return true;
    }
    let host = headers.get(axum::http::header::HOST).and_then(|h| h.to_str().ok()).map(str::to_ascii_lowercase);
    match (origin.split_once("://"), host) {
        (Some(("http" | "https", authority)), Some(host)) => authority == host,
        _ => false,
    }
}

/// The origins Note's own pages are served from: `public_base_url`'s, and the
/// listening address's (with its loopback names when it listens on loopback or
/// on every address).
pub fn page_origins(public_base_url: &str, bind_addr: Option<&str>) -> Vec<String> {
    let mut out = Vec::new();
    let base = public_base_url.to_ascii_lowercase();
    if let Some((scheme, rest)) = base.split_once("://") {
        let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
        out.push(format!("{scheme}://{host}"));
    }
    if let Some(addr) = bind_addr.and_then(|a| a.parse::<std::net::SocketAddr>().ok()) {
        let port = addr.port();
        if addr.ip().is_loopback() || addr.ip().is_unspecified() {
            out.extend([format!("http://127.0.0.1:{port}"), format!("http://localhost:{port}"), format!("http://[::1]:{port}")]);
        } else if port == 80 {
            out.push(format!("http://{}", std::net::SocketAddr::new(addr.ip(), 0).to_string().trim_end_matches(":0")));
        } else {
            out.push(format!("http://{addr}"));
        }
    }
    out
}

/// The address a per-client limiter keys on: Cloudflare's header behind the
/// tunnel, the first forwarded hop otherwise, and one shared bucket when the
/// request came straight to the socket.
pub fn client_key(headers: &axum::http::HeaderMap) -> String {
    let pick = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    pick("cf-connecting-ip").or_else(|| pick("x-forwarded-for")).unwrap_or_else(|| "local".to_string())
}

/// Where Cloudflare's visitor location headers place a request; every field is
/// absent when the headers are.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Place {
    pub city: Option<String>,
    pub country: Option<String>,
    pub coords: Option<(f64, f64)>,
}

pub fn place(headers: &axum::http::HeaderMap) -> Place {
    let text = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty() && v != "XX" && v != "T1")
    };
    let num = |name: &str| text(name).and_then(|v| v.parse::<f64>().ok()).filter(|v| v.is_finite());
    let coords = match (num("cf-iplatitude"), num("cf-iplongitude")) {
        (Some(lat), Some(lon)) if lat.abs() <= 90.0 && lon.abs() <= 180.0 => Some((lat, lon)),
        _ => None,
    };
    Place { city: text("cf-ipcity"), country: text("cf-ipcountry"), coords }
}

/// Great-circle distance in kilometres between two (latitude, longitude) points.
pub fn km(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (la1, lo1, la2, lo2) = (a.0.to_radians(), a.1.to_radians(), b.0.to_radians(), b.1.to_radians());
    let h = ((la2 - la1) / 2.0).sin().powi(2) + la1.cos() * la2.cos() * ((lo2 - lo1) / 2.0).sin().powi(2);
    6371.0 * 2.0 * h.sqrt().min(1.0).asin()
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

    #[test]
    fn an_origin_must_be_one_of_notes_own() {
        let allowed = page_origins("https://Note.example.net/", Some("127.0.0.1:3271"));
        let mut h = axum::http::HeaderMap::new();
        assert!(origin_ok(&h, &allowed), "no Origin: a non-browser client");
        for ok in ["https://note.example.net", "http://localhost:3271", "http://127.0.0.1:3271"] {
            h.insert("origin", ok.parse().unwrap());
            assert!(origin_ok(&h, &allowed), "{ok}");
        }
        for bad in ["https://evil.example", "http://note.example.net", "https://note.example.net.evil.example", "null"] {
            h.insert("origin", bad.parse().unwrap());
            assert!(!origin_ok(&h, &allowed), "{bad}");
        }
    }

    #[test]
    fn an_origin_naming_the_requested_host_is_same_origin() {
        let allowed = page_origins("https://note.example.net", Some("0.0.0.0:3271"));
        let mut h = axum::http::HeaderMap::new();
        h.insert("host", "100.64.0.9:3271".parse().unwrap());
        h.insert("origin", "http://100.64.0.9:3271".parse().unwrap());
        assert!(origin_ok(&h, &allowed));
        h.insert("host", "laptop.tail.ts.net".parse().unwrap());
        h.insert("origin", "https://laptop.tail.ts.net".parse().unwrap());
        assert!(origin_ok(&h, &allowed));
        for bad in ["https://evil.example", "https://laptop.tail.ts.net:444", "null", "file://laptop.tail.ts.net"] {
            h.insert("origin", bad.parse().unwrap());
            assert!(!origin_ok(&h, &allowed), "{bad}");
        }
    }

    #[test]
    fn a_bind_on_a_named_address_allows_that_address_alone() {
        assert_eq!(
            page_origins("https://n.example/app", Some("192.168.1.4:8080")),
            ["https://n.example", "http://192.168.1.4:8080"]
        );
        assert_eq!(page_origins("https://n.example", Some("[fd00::4]:80")), ["https://n.example", "http://[fd00::4]"]);
        assert_eq!(page_origins("https://n.example", None), ["https://n.example"]);
    }

    #[test]
    fn client_key_prefers_cloudflare_then_forwarded_then_local() {
        let mut h = axum::http::HeaderMap::new();
        assert_eq!(client_key(&h), "local");
        h.insert("x-forwarded-for", "10.0.0.7, 172.16.0.1".parse().unwrap());
        assert_eq!(client_key(&h), "10.0.0.7");
        h.insert("cf-connecting-ip", "203.0.113.9".parse().unwrap());
        assert_eq!(client_key(&h), "203.0.113.9");
    }

    #[test]
    fn place_reads_cloudflare_location_and_drops_the_unknowns() {
        let mut h = axum::http::HeaderMap::new();
        assert_eq!(place(&h), Place::default());
        h.insert("cf-ipcountry", "XX".parse().unwrap());
        h.insert("cf-iplatitude", "north".parse().unwrap());
        h.insert("cf-iplongitude", "-122.3".parse().unwrap());
        assert_eq!(place(&h), Place::default());
        h.insert("cf-ipcountry", "US".parse().unwrap());
        h.insert("cf-ipcity", "Seattle".parse().unwrap());
        h.insert("cf-iplatitude", "47.6".parse().unwrap());
        let p = place(&h);
        assert_eq!(p.city.as_deref(), Some("Seattle"));
        assert_eq!(p.country.as_deref(), Some("US"));
        assert_eq!(p.coords, Some((47.6, -122.3)));
    }

    #[test]
    fn km_measures_the_great_circle() {
        assert!(km((47.6, -122.3), (47.6, -122.3)).abs() < 1e-9);
        let seattle_portland = km((47.6062, -122.3321), (45.5152, -122.6784));
        assert!((seattle_portland - 234.0).abs() < 3.0, "{seattle_portland}");
        let london_tokyo = km((51.5074, -0.1278), (35.6762, 139.6503));
        assert!((london_tokyo - 9560.0).abs() < 30.0, "{london_tokyo}");
    }
}
