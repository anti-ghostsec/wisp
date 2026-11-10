//! Privacy and memory hardening for the WebKit objects shared by every tab.

use crate::config::{self, Config};
use webkit6::{
    CacheModel, CookieAcceptPolicy, HardwareAccelerationPolicy, MemoryPressureSettings,
    NetworkSession, Settings, WebContext,
};

/// Query parameters that exist only to track people across sites.
const TRACKING_PARAMS: &[&str] = &[
    "fbclid", "gclid", "gclsrc", "dclid", "gbraid", "wbraid", "msclkid", "mc_cid", "mc_eid",
    "yclid", "igshid", "twclid", "ttclid", "li_fat_id", "_hsenc", "_hsmi", "mkt_tok", "oly_anon_id",
    "oly_enc_id", "vero_id", "rb_clickid", "s_cid", "ref_src", "spm",
];
const TRACKING_PREFIXES: &[&str] = &["utm_", "pk_", "hsa_"];

fn memory_settings(cfg: &Config) -> Option<MemoryPressureSettings> {
    if cfg.memory_limit_mb == 0 {
        return None;
    }
    let mut s = MemoryPressureSettings::new();
    s.set_memory_limit(cfg.memory_limit_mb);
    // Start trimming caches early; never kill a process just for being big.
    s.set_conservative_threshold(0.33);
    s.set_strict_threshold(0.5);
    Some(s)
}

pub fn network_session(cfg: &Config) -> NetworkSession {
    if let Some(mut s) = memory_settings(cfg) {
        // Must happen before the first session is created.
        NetworkSession::set_memory_pressure_settings(&mut s);
    }
    let session = if cfg.persistent_profile {
        let data = config::data_dir().join("profile");
        let cache = config::cache_dir().join("profile");
        NetworkSession::new(data.to_str(), cache.to_str())
    } else {
        NetworkSession::new_ephemeral()
    };
    session.set_itp_enabled(cfg.tracking_prevention);
    session.set_persistent_credential_storage_enabled(false);
    if let Some(cookies) = session.cookie_manager() {
        cookies.set_accept_policy(if cfg.block_third_party_cookies {
            CookieAcceptPolicy::NoThirdParty
        } else {
            CookieAcceptPolicy::Always
        });
    }
    session
}

pub fn web_context(cfg: &Config) -> WebContext {
    let mut builder = WebContext::builder();
    if let Some(s) = memory_settings(cfg) {
        builder = builder.memory_pressure_settings(&s);
    }
    let ctx = builder.build();
    // DocumentViewer, the smallest, keeps no pictures in memory at all:
    // pages that redraw their lists (YouTube's thumbnails and avatars)
    // fetched every picture again each time, and they flashed or stayed
    // blank. DocumentBrowser still keeps them, in less memory than
    // WebBrowser.
    ctx.set_cache_model(if cfg.low_memory_cache {
        CacheModel::DocumentBrowser
    } else {
        CacheModel::WebBrowser
    });
    ctx.set_spell_checking_enabled(false);
    ctx
}

pub fn settings(cfg: &Config) -> Settings {
    let s = Settings::new();
    s.set_enable_javascript(cfg.javascript);
    s.set_enable_webgl(cfg.webgl);
    s.set_enable_webrtc(cfg.webrtc);
    s.set_enable_media_stream(cfg.webrtc);
    // Always, even with GPU compositing off: then the drawing is done on the
    // CPU instead (main.rs). WebKit's "Never" (2.54) aborts the page's process
    // on YouTube a minute into a video.
    s.set_hardware_acceleration_policy(HardwareAccelerationPolicy::Always);
    // Back/forward page cache keeps whole pages alive in RAM.
    s.set_enable_page_cache(!cfg.low_memory_cache);
    s.set_enable_html5_database(cfg.site_databases);
    s.set_javascript_can_open_windows_automatically(false);
    s.set_javascript_can_access_clipboard(false);
    s.set_media_playback_requires_user_gesture(true);
    s.set_enable_developer_extras(true);
    if let Some(ua) = &cfg.user_agent {
        s.set_user_agent(Some(ua));
    }
    s
}

/// Split a URL into (scheme, host) without pulling in a URL crate.
pub fn scheme_and_host(uri: &str) -> Option<(&str, &str)> {
    let (scheme, rest) = uri.split_once("://")?;
    Some((scheme, split_host_port(authority(rest)).0))
}

/// The part between `://` and the path, without any `user:password@`.
fn authority(rest: &str) -> &str {
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    authority.rsplit('@').next().unwrap_or(authority)
}

/// "host:port" or "[v6]:port" into the host (no brackets) and the port, if
/// one is given.
fn split_host_port(host_port: &str) -> (&str, Option<&str>) {
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
fn is_local_host(host: &str) -> bool {
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

/// The start of a rule's pattern: http://, with or without `user@`.
const HTTP: &str = "^http://([^/@]*@)?";

/// The hosts of `is_local_host`, as patterns for WebKit's content filter
/// (which has no "or"). Addresses are matched as WebKit writes them: four
/// numbers, or lower-case IPv6 in brackets.
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

/// A content filter that turns http:// into https:// for pages and frames
/// inside the web process, before anything is sent. The upgrade done when a
/// load starts (`rewrite_url`) comes too late for that: by then the request
/// may be on its way. `allowed` are the `http_site`s let through as they are.
pub fn https_filter(allowed: &[String]) -> String {
    let mut rules = vec![serde_json::json!({
        "trigger": { "url-filter": "^http://", "resource-type": ["document"] },
        "action": { "type": "make-https" },
    })];
    let mut skip = |pattern: String| {
        rules.push(serde_json::json!({
            "trigger": { "url-filter": pattern },
            "action": { "type": "ignore-previous-rules" },
        }));
    };
    for host in LOCAL_HOST_PATTERNS {
        skip(format!("{HTTP}{host}[:/]"));
    }
    for site in allowed {
        let Some((host, port)) = site.rsplit_once(':') else { continue };
        let host = filter_escape(host);
        skip(if port == "80" { format!("{HTTP}{host}(:80)?/") } else { format!("{HTTP}{host}:{port}/") });
    }
    serde_json::Value::Array(rules).to_string()
}

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
    TRACKING_PARAMS.contains(&name) || TRACKING_PREFIXES.iter().any(|p| name.starts_with(p))
}

fn strip_params(uri: &str) -> Option<String> {
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
        // Everything after the first rule is an exception to it.
        assert!(rules[1..].iter().all(|r| r["action"]["type"] == "ignore-previous-rules"));
        let patterns: Vec<&str> = rules[1..].iter().map(|r| r["trigger"]["url-filter"].as_str().unwrap()).collect();
        assert_eq!(patterns.len(), LOCAL_HOST_PATTERNS.len() + 3);
        assert!(patterns.contains(&r"^http://([^/@]*@)?([^/:@]*\.)?localhost[:/]"));
        assert!(patterns.contains(&r"^http://([^/@]*@)?192\.168\.[0-9.]+[:/]"));
        assert!(patterns.contains(&r"^http://([^/@]*@)?a\.com(:80)?/"));
        assert!(patterns.contains(&r"^http://([^/@]*@)?b-c\.org:8080/"));
        assert!(patterns.contains(&r"^http://([^/@]*@)?\[2001:db8::1\](:80)?/"));
    }

    #[test]
    fn parses_hosts() {
        assert_eq!(scheme_and_host("https://u:p@ex.com:81/a"), Some(("https", "ex.com")));
        assert_eq!(scheme_and_host("http://[::1]:80/"), Some(("http", "::1")));
        assert_eq!(scheme_and_host("http://[::1]/"), Some(("http", "::1")));
        assert_eq!(scheme_and_host("http://ex.com"), Some(("http", "ex.com")));
    }
}
