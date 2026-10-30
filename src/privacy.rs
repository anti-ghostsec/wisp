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
    ctx.set_cache_model(if cfg.low_memory_cache {
        CacheModel::DocumentViewer
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
    s.set_hardware_acceleration_policy(if cfg.gpu_compositing || cfg.hardware_acceleration {
        HardwareAccelerationPolicy::Always
    } else {
        HardwareAccelerationPolicy::Never
    });
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
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    let host = if host_port.starts_with('[') {
        host_port.split(']').next().map(|h| &h[1..]).unwrap_or(host_port)
    } else {
        host_port.split(':').next().unwrap_or(host_port)
    };
    Some((scheme, host))
}

fn is_local_host(host: &str) -> bool {
    host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".onion")
        || host.ends_with(".test")
        || host.parse::<std::net::IpAddr>().is_ok()
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
/// `http_allowed(host)` lets the user opt out of HTTPS-only per host.
pub fn rewrite_url(uri: &str, cfg: &Config, http_allowed: impl Fn(&str) -> bool) -> Option<String> {
    let mut out: Option<String> = None;
    if cfg.strip_tracking_params {
        out = strip_params(uri);
    }
    if cfg.https_only {
        let current = out.as_deref().unwrap_or(uri);
        if let Some(("http", host)) = scheme_and_host(current) {
            if !is_local_host(host) && !http_allowed(host) {
                out = Some(format!("https://{}", &current["http://".len()..]));
            }
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
        assert_eq!(rewrite_url("http://a.com/", &cfg, |h| h == "a.com"), None);
        assert_eq!(rewrite_url("https://a.com/", &cfg, no), None);
    }

    #[test]
    fn parses_hosts() {
        assert_eq!(scheme_and_host("https://u:p@ex.com:81/a"), Some(("https", "ex.com")));
        assert_eq!(scheme_and_host("http://[::1]:80/"), Some(("http", "::1")));
    }
}
