//! Privacy and memory hardening for the WebKit objects shared by every tab.
//! The engine-free rules (HTTPS-only, tracking parameters) are in wisp-core.

pub use wisp_core::privacy::*;

use crate::config::{self, Config};
use webkit6::{
    CacheModel, CookieAcceptPolicy, HardwareAccelerationPolicy, MemoryPressureSettings,
    NetworkSession, Settings, WebContext,
};

fn memory_settings(cfg: &Config) -> Option<MemoryPressureSettings> {
    if cfg.memory_limit_mb == 0 {
        return None;
    }
    let mut s = MemoryPressureSettings::new();
    s.set_memory_limit(cfg.memory_limit_mb);
    // Trimming starts at 60% of the limit, not WebKit's 33%: with every tab
    // in one process, a page of big pictures got there at once, and the
    // decoded pictures were dropped and decoded again while scrolling
    // (jumpy, and slow to show). Never kill a process just for being big.
    // Strict first: WebKit refuses a conservative threshold that isn't
    // below the strict one already set (its default is 0.5).
    s.set_strict_threshold(0.85);
    s.set_conservative_threshold(0.6);
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
    // WebBrowser's cache holds the most decoded pictures, so scrolling
    // back doesn't fetch and decode them again (the smaller models made
    // big pictures pop in late, and YouTube's thumbnails flash). It's in
    // memory only: the session keeps nothing on disk unless the profile
    // is persistent. low_memory_cache now only turns off the back/forward
    // cache of whole pages (below).
    ctx.set_cache_model(CacheModel::WebBrowser);
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
    // The inspector (and "Inspect Element") only once it's asked for with
    // Ctrl+Shift+I or F12, which turns it on for the rest of the run.
    s.set_enable_developer_extras(false);
    // WebKit's fixes for particular sites, some of which tell those sites
    // a different browser: off, so every site sees the same Wisp.
    s.set_enable_site_specific_quirks(false);
    // Nothing looked up or connected to before it's used: names in links,
    // <link rel=prefetch> and preconnect hints, speculation rules.
    #[allow(deprecated)]
    s.set_enable_dns_prefetching(false);
    if let Some(features) = Settings::all_features() {
        for i in 0..features.length() {
            let Some(f) = features.get(i) else { continue };
            let id = f.identifier().map(|g| g.to_string()).unwrap_or_default();
            if ["DNSPrefetchingEnabled", "LinkPrefetchEnabled", "LinkPreconnectEarlyHintsEnabled", "SpeculationRulesPrefetchEnabled"]
                .contains(&id.as_str())
            {
                s.set_feature_enabled(&f, false);
            }
        }
    }
    // Web Audio stays on: games, calls and some players need it.
    if let Some(ua) = &cfg.user_agent {
        s.set_user_agent(Some(ua));
    }
    s
}
