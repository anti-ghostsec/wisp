//! Privacy rules that don't depend on the engine: HTTPS-only upgrades and
//! their exceptions, and tracking parameters taken off addresses.

use crate::config::Config;

/// Query parameters that exist only to track people across sites, in lower
/// case (names are compared without regard to case).
const TRACKING_PARAMS: &[&str] = &[
    "fbclid", "gclid", "gclsrc", "dclid", "gbraid", "wbraid", "gad_source", "gad_campaignid", "srsltid", "msclkid",
    "mc_cid", "mc_eid", "yclid", "ysclid", "igshid", "igsh", "twclid", "ttclid", "li_fat_id", "epik", "_hsenc",
    "_hsmi", "__hstc", "__hssc", "__hsfp", "hsctatracking", "mkt_tok", "oly_anon_id", "oly_enc_id", "vero_id",
    "vero_conv", "rb_clickid", "s_cid", "ref_src", "spm", "scm", "irclickid", "wickedid", "_openstat", "_branch_match_id",
    "_branch_referrer", "ss_email_id", "ml_subscriber", "ml_subscriber_hash", "sms_click", "sms_source", "sms_uph",
    "dm_i", "__s", "_kx", "trk_contact", "trk_msg", "trk_module", "trk_sid",
];
const TRACKING_PREFIXES: &[&str] = &["utm_", "pk_", "hsa_", "mtm_", "piwik_", "matomo_"];

/// Split a URL into (scheme, host) without pulling in a URL crate.
pub fn scheme_and_host(uri: &str) -> Option<(&str, &str)> {
    let (scheme, rest) = uri.split_once("://")?;
    Some((scheme, split_host_port(authority(rest)).0))
}

/// The part between `://` and the path, without any `user:password@`.
pub(crate) fn authority(rest: &str) -> &str {
    // A backslash ends the host too, as browsers read http(s) addresses.
    let authority = rest.split(['/', '\\', '?', '#']).next().unwrap_or("");
    authority.rsplit('@').next().unwrap_or(authority)
}

/// "host:port" or "[v6]:port" into the host (no brackets) and the port, if
/// one is given.
pub(crate) fn split_host_port(host_port: &str) -> (&str, Option<&str>) {
    if let Some(rest) = host_port.strip_prefix('[') {
        return match rest.split_once(']') {
            Some((host, after)) => (host, after.strip_prefix(':').filter(|p| !p.is_empty())),
            None => (rest, None),
        };
    }
    match host_port.split_once(':') {
        Some((host, port)) => (host, Some(port).filter(|p| !p.is_empty())),
        None => (host_port, None),
    }
}

/// What an exception to HTTPS-only is kept under: the host and the port of
/// an http:// address ("example.com:80", "[2001:db8::1]:8080"), so that
/// allowing one server on a machine doesn't allow the others.
pub fn http_site(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("http://")?;
    let (host, port) = split_host_port(authority(rest));
    if host.is_empty() {
        return None;
    }
    let host = host.to_ascii_lowercase();
    let port = port.unwrap_or("80");
    Some(if host.contains(':') { format!("[{host}]:{port}") } else { format!("{host}:{port}") })
}

/// Hosts that never get HTTPS: this computer, the local network, and names
/// that only exist on one (they can't have a certificate anyone trusts).
/// A public address given as numbers is upgraded like any other site.
pub fn is_local_host(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    let host = host.as_str();
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return is_local_ip(ip);
    }
    host == "localhost" || [".localhost", ".local", ".onion", ".test"].iter().any(|s| host.ends_with(s))
}

/// Loopback, private (10/8, 172.16/12, 192.168/16, fc00::/7) and link-local
/// (169.254/16, fe80::/10) addresses.
fn is_local_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        std::net::IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            v6.is_loopback() || first & 0xfe00 == 0xfc00 || first & 0xffc0 == 0xfe80
        }
    }
}

/// The start of a rule's pattern: http:// (or ws://, `{}`), with or
/// without `user@`.
const HTTP: &str = "^{}://([^/@]*@)?";
/// The end of a host in a rule's pattern: a port if there is one, then the
/// path, which WebKit always writes (at least "/").
const HOST_END: &str = "(:[0-9]+)?/";

/// The hosts of `is_local_host`, as patterns for WebKit's content filter
/// (which has no "or"). Addresses are matched as WebKit writes them: four
/// numbers, or lower-case IPv6 in brackets. Each is followed by `HOST_END`,
/// so that a user name that looks like a local host
/// (`http://127.0.0.1:x@tracker.example/`) doesn't count as one.
const LOCAL_HOST_PATTERNS: &[&str] = &[
    r"([^/:@]*\.)?localhost",
    r"[^/:@]*\.local",
    r"[^/:@]*\.onion",
    r"[^/:@]*\.test",
    r"127\.[0-9.]+",
    r"10\.[0-9.]+",
    r"192\.168\.[0-9.]+",
    r"169\.254\.[0-9.]+",
    r"172\.1[6-9]\.[0-9.]+",
    r"172\.2[0-9]\.[0-9.]+",
    r"172\.3[01]\.[0-9.]+",
    r"\[::1\]",
    r"\[f[cd][0-9a-f][0-9a-f]:[0-9a-f:.]*\]",
    r"\[fe[89ab][0-9a-f]:[0-9a-f:.]*\]",
];

/// A content filter that turns http:// into https:// for pages, frames and
/// everything they load (pictures, scripts, requests from scripts, ...)
/// inside the web process, before anything is sent. The upgrade done when a
/// load starts (`rewrite_url`) comes too late for that: by then the request
/// may be on its way. `allowed` are the `http_site`s let through as they are.
/// WebSockets the same way: ws:// becomes wss://.
pub fn https_filter(allowed: &[String]) -> String {
    const SCHEMES: [&str; 2] = ["http", "ws"];
    let mut rules: Vec<serde_json::Value> = SCHEMES
        .iter()
        .map(|scheme| {
            serde_json::json!({
                "trigger": { "url-filter": format!("^{scheme}://") },
                "action": { "type": "make-https" },
            })
        })
        .collect();
    let mut skip = |pattern: String| {
        rules.push(serde_json::json!({
            "trigger": { "url-filter": pattern },
            "action": { "type": "ignore-previous-rules" },
        }));
    };
    for scheme in SCHEMES {
        let start = HTTP.replace("{}", scheme);
        for host in LOCAL_HOST_PATTERNS {
            skip(format!("{start}{host}{HOST_END}"));
        }
        for site in allowed {
            let Some((host, port)) = site.rsplit_once(':') else { continue };
            let host = filter_escape(host);
            skip(if port == "80" { format!("{start}{host}(:80)?/") } else { format!("{start}{host}:{port}/") });
        }
    }
    serde_json::Value::Array(rules).to_string()
}

/// Wisp's own content filter: the HTTPS upgrade (with `https_only`), and
/// no `<a ping>`, a request to a tracker each time a link is clicked (which
/// WebKit no longer has a setting for; beacons are sent the same way and go
/// too).
///
/// With `block_onion`, nothing goes to a .onion address: without a Tor
/// proxy the name would only be asked of the system's DNS servers, which
/// tells them what you were after.
pub fn own_filter(https_only: bool, allowed: &[String], block_onion: bool) -> String {
    let mut rules: Vec<serde_json::Value> =
        if https_only { serde_json::from_str(&https_filter(allowed)).unwrap_or_default() } else { Vec::new() };
    // After the HTTPS rules, whose exceptions (.onion among them) would
    // undo it.
    if block_onion {
        rules.push(serde_json::json!({
            "trigger": { "url-filter": ONION },
            "action": { "type": "block" },
        }));
    }
    // Last: an exception to HTTPS-only mustn't let a ping through.
    rules.push(serde_json::json!({
        "trigger": { "url-filter": ".*", "resource-type": ["ping"] },
        "action": { "type": "block" },
    }));
    serde_json::Value::Array(rules).to_string()
}

/// Any address at a .onion host, as a content filter's pattern.
const ONION: &str = r"^[a-z][a-z0-9+.-]*://([^/@]*@)?[^/:@]*\.onion\.?(:[0-9]+)?/";

/// A host as a literal in a content filter's pattern.
fn filter_escape(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if !c.is_ascii_alphanumeric() && c != '-' && c != ':' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn is_tracking_param(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    TRACKING_PARAMS.contains(&name.as_str()) || TRACKING_PREFIXES.iter().any(|p| name.starts_with(p))
}

/// `uri` without its tracking parameters, if it had any.
pub fn strip_params(uri: &str) -> Option<String> {
    let (before_frag, frag) = match uri.split_once('#') {
        Some((a, b)) => (a, Some(b)),
        None => (uri, None),
    };
    let (base, query) = before_frag.split_once('?')?;
    let kept: Vec<&str> = query
        .split('&')
        .filter(|kv| !kv.is_empty() && !is_tracking_param(kv.split('=').next().unwrap_or("")))
        .collect();
    if kept.len() == query.split('&').filter(|kv| !kv.is_empty()).count() {
        return None;
    }
    let mut out = base.to_string();
    if !kept.is_empty() {
        out.push('?');
        out.push_str(&kept.join("&"));
    }
    if let Some(f) = frag {
        out.push('#');
        out.push_str(f);
    }
    Some(out)
}

/// Returns a cleaner URL to load instead of `uri`, if there is one.
/// `http_allowed(site)` lets the user opt out of HTTPS-only for one
/// `http_site` (a host and port).
pub fn rewrite_url(uri: &str, cfg: &Config, http_allowed: impl Fn(&str) -> bool) -> Option<String> {
    let mut out: Option<String> = None;
    if cfg.strip_tracking_params {
        out = strip_params(uri);
    }
    if cfg.https_only {
        let current = out.as_deref().unwrap_or(uri);
        if let Some(("http", host)) = scheme_and_host(current)
            && !is_local_host(host) && !http_site(current).is_some_and(|site| http_allowed(&site)) {
                out = Some(format!("https://{}", &current["http://".len()..]));
            }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_tracking_params() {
        assert_eq!(
            strip_params("https://a.com/x?utm_source=t&id=3&fbclid=9#top").as_deref(),
            Some("https://a.com/x?id=3#top")
        );
        assert_eq!(strip_params("https://a.com/x?utm_source=t").as_deref(), Some("https://a.com/x"));
        assert_eq!(strip_params("https://a.com/x?id=3"), None);
        // Names are compared without regard to case.
        assert_eq!(strip_params("https://a.com/x?UTM_Source=t&FBCLID=1&Id=3").as_deref(), Some("https://a.com/x?Id=3"));
        assert_eq!(strip_params("https://a.com/x?mtm_campaign=a&gad_source=1").as_deref(), Some("https://a.com/x"));
    }

    #[test]
    fn a_user_name_is_not_the_host() {
        let cfg = Config::default();
        let no = |_: &str| false;
        assert_eq!(scheme_and_host("http://127.0.0.1:x@tracker.com/"), Some(("http", "tracker.com")));
        assert_eq!(scheme_and_host("http://localhost@tracker.com/a@b"), Some(("http", "tracker.com")));
        assert_eq!(scheme_and_host("http://127.0.0.1\\@tracker.com/"), Some(("http", "127.0.0.1")));
        assert_eq!(
            rewrite_url("http://127.0.0.1:x@tracker.com/p", &cfg, no).as_deref(),
            Some("https://127.0.0.1:x@tracker.com/p")
        );
        assert_eq!(rewrite_url("http://a.local@tracker.com/", &cfg, no).as_deref(), Some("https://a.local@tracker.com/"));
        assert_eq!(http_site("http://localhost:80@tracker.com/").as_deref(), Some("tracker.com:80"));
        assert_eq!(rewrite_url("http://u:p@127.0.0.1:8000/", &cfg, no), None);
    }

    #[test]
    fn upgrades_http() {
        let cfg = Config::default();
        let no = |_: &str| false;
        assert_eq!(rewrite_url("http://a.com/p", &cfg, no).as_deref(), Some("https://a.com/p"));
        assert_eq!(rewrite_url("http://localhost:8000/", &cfg, no), None);
        assert_eq!(rewrite_url("http://192.168.1.1/", &cfg, no), None);
        assert_eq!(rewrite_url("http://a.com/", &cfg, |site| site == "a.com:80"), None);
        assert_eq!(rewrite_url("https://a.com/", &cfg, no), None);
        // A public address given as numbers is a site like any other.
        assert_eq!(rewrite_url("http://93.184.216.34/p", &cfg, no).as_deref(), Some("https://93.184.216.34/p"));
        assert_eq!(rewrite_url("http://[2001:db8::1]:8080/", &cfg, no).as_deref(), Some("https://[2001:db8::1]:8080/"));
    }

    #[test]
    fn http_exceptions_are_for_one_host_and_port() {
        let cfg = Config::default();
        let allowed = |site: &str| site == "a.com:8080";
        assert_eq!(rewrite_url("http://a.com:8080/x", &cfg, allowed), None);
        assert_eq!(rewrite_url("http://a.com/x", &cfg, allowed).as_deref(), Some("https://a.com/x"));
        assert_eq!(rewrite_url("http://a.com:81/x", &cfg, allowed).as_deref(), Some("https://a.com:81/x"));
        assert_eq!(rewrite_url("http://b.a.com:8080/x", &cfg, allowed).as_deref(), Some("https://b.a.com:8080/x"));
        assert_eq!(http_site("http://A.com/x").as_deref(), Some("a.com:80"));
        assert_eq!(http_site("http://u:p@a.com:8080/x?y").as_deref(), Some("a.com:8080"));
        assert_eq!(http_site("http://[2001:db8::1]/").as_deref(), Some("[2001:db8::1]:80"));
        assert_eq!(http_site("http://[2001:db8::1]:81/").as_deref(), Some("[2001:db8::1]:81"));
        assert_eq!(http_site("https://a.com/"), None);
        assert_eq!(http_site("http:///x"), None);
    }

    #[test]
    fn only_local_hosts_stay_on_http() {
        for local in [
            "localhost", "LocalHost", "app.localhost", "printer.local", "abc.onion", "dev.test",
            "127.0.0.1", "127.8.9.10", "10.0.0.1", "10.255.255.255", "172.16.0.1", "172.31.255.255",
            "192.168.0.1", "192.168.255.255", "169.254.1.1", "::1", "fc00::1", "fd12:3456::1",
            "fe80::1", "febf::1",
        ] {
            assert!(is_local_host(local), "{local} is local");
        }
        for public in [
            "example.com", "localhost.example.com", "notlocal", "mylocal", "test", "8.8.8.8", "1.1.1.1",
            "9.255.255.255", "11.0.0.1", "172.15.255.255", "172.32.0.1", "192.167.1.1", "192.169.1.1",
            "169.253.1.1", "126.255.255.255", "128.0.0.1", "100.64.0.1", "0.0.0.0", "2001:db8::1",
            "2606:4700::1111", "fec0::1", "fe7f::1", "fbff::1", "fe00::1", "::ffff:127.0.0.1", "::",
        ] {
            assert!(!is_local_host(public), "{public} is not local");
        }
    }

    #[test]
    fn https_filter_upgrades_all_but_local_and_allowed() {
        let rules: serde_json::Value =
            serde_json::from_str(&https_filter(&["a.com:80".into(), "b-c.org:8080".into(), "[2001:db8::1]:80".into()])).unwrap();
        let rules = rules.as_array().unwrap();
        assert_eq!(rules[0]["action"]["type"], "make-https");
        assert_eq!(rules[0]["trigger"]["url-filter"], "^http://");
        assert_eq!(rules[1]["action"]["type"], "make-https");
        assert_eq!(rules[1]["trigger"]["url-filter"], "^ws://");
        // Not only pages: pictures, scripts and requests too.
        assert!(rules[0]["trigger"].get("resource-type").is_none());
        // Everything after the first two rules is an exception to them.
        assert!(rules[2..].iter().all(|r| r["action"]["type"] == "ignore-previous-rules"));
        let patterns: Vec<&str> = rules[2..].iter().map(|r| r["trigger"]["url-filter"].as_str().unwrap()).collect();
        assert_eq!(patterns.len(), 2 * (LOCAL_HOST_PATTERNS.len() + 3));
        assert!(patterns.contains(&r"^ws://([^/@]*@)?([^/:@]*\.)?localhost(:[0-9]+)?/"));
        assert!(patterns.contains(&r"^ws://([^/@]*@)?a\.com(:80)?/"));
        assert!(patterns.contains(&r"^http://([^/@]*@)?([^/:@]*\.)?localhost(:[0-9]+)?/"));
        assert!(patterns.contains(&r"^http://([^/@]*@)?192\.168\.[0-9.]+(:[0-9]+)?/"));
        assert!(patterns.contains(&r"^http://([^/@]*@)?a\.com(:80)?/"));
        assert!(patterns.contains(&r"^http://([^/@]*@)?b-c\.org:8080/"));
        assert!(patterns.contains(&r"^http://([^/@]*@)?\[2001:db8::1\](:80)?/"));
    }

    #[test]
    fn own_filter_blocks_pings_last() {
        for https_only in [true, false] {
            let rules: serde_json::Value = serde_json::from_str(&own_filter(https_only, &["a.com:80".into()], false)).unwrap();
            let rules = rules.as_array().unwrap();
            let last = rules.last().unwrap();
            assert_eq!(last["action"]["type"], "block");
            assert_eq!(last["trigger"]["resource-type"], serde_json::json!(["ping"]));
            assert_eq!(rules.iter().any(|r| r["action"]["type"] == "make-https"), https_only);
        }
    }

    #[test]
    fn onion_addresses_are_blocked_without_tor() {
        let onion = regex::Regex::new(ONION).unwrap();
        for url in ["http://abc.onion/", "https://x.abc.onion:8080/a", "ws://u@abc.onion/", "https://abc.onion./"] {
            assert!(onion.is_match(url), "{url}");
        }
        for url in ["https://onion.com/", "https://a.onion.com/", "https://x.com/?u=http://a.onion/", "https://a.onion@x.com/"] {
            assert!(!onion.is_match(url), "{url}");
        }
        let rules: serde_json::Value = serde_json::from_str(&own_filter(true, &[], true)).unwrap();
        let rules = rules.as_array().unwrap();
        let at = rules.iter().position(|r| r["trigger"]["url-filter"] == ONION).unwrap();
        // After every exception that would undo it.
        assert!(rules[at..].iter().all(|r| r["action"]["type"] == "block"));
        let none: serde_json::Value = serde_json::from_str(&own_filter(true, &[], false)).unwrap();
        assert!(none.as_array().unwrap().iter().all(|r| r["trigger"]["url-filter"] != ONION));
    }

    /// The filter's exceptions, run as the regular expressions they are:
    /// a local-looking user name never lets another host through.
    #[test]
    fn https_filter_exceptions_match_the_host_only() {
        let rules: serde_json::Value = serde_json::from_str(&https_filter(&["a.com:80".into()])).unwrap();
        let skips: Vec<regex::Regex> = rules.as_array().unwrap()[2..]
            .iter()
            .map(|r| regex::Regex::new(r["trigger"]["url-filter"].as_str().unwrap()).unwrap())
            .collect();
        let skipped = |url: &str| skips.iter().any(|r| r.is_match(url));
        for local in [
            "http://localhost/", "http://localhost:8000/x", "http://app.localhost/", "http://127.0.0.1/",
            "http://127.0.0.1:3000/a?b", "http://192.168.1.1/", "http://u:p@10.0.0.1:81/", "http://[::1]:8080/",
            "http://printer.local/", "http://a.com/", "http://a.com:80/x", "http://u@a.com/",
        ] {
            assert!(skipped(local), "{local} stays on http");
        }
        for upgraded in [
            "http://127.0.0.1:x@tracker.com/", "http://127.0.0.1:80@tracker.com/", "http://localhost@tracker.com/",
            "http://localhost:1@tracker.com/", "http://a.local@tracker.com/", "http://192.168.1.1.tracker.com/",
            "http://localhost.tracker.com/", "http://a.com.tracker.com/", "http://a.com:80@tracker.com/",
            "http://a.com:8080/", "http://tracker.com/?u=http://localhost/",
        ] {
            assert!(!skipped(upgraded), "{upgraded} is upgraded");
        }
    }

    #[test]
    fn parses_hosts() {
        assert_eq!(scheme_and_host("https://u:p@ex.com:81/a"), Some(("https", "ex.com")));
        assert_eq!(scheme_and_host("http://[::1]:80/"), Some(("http", "::1")));
        assert_eq!(scheme_and_host("http://[::1]/"), Some(("http", "::1")));
        assert_eq!(scheme_and_host("http://ex.com"), Some(("http", "ex.com")));
    }
}
