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
    s.set_conservative_threshold(0.6);
    s.set_strict_threshold(0.85);
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
    s.set_enable_developer_extras(true);
    if let Some(ua) = &cfg.user_agent {
        s.set_user_agent(Some(ua));
    }
    s
}
