//! User configuration, stored as JSON in `~/.config/wisp/config.json`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use webkit6::glib;

pub const APP_NAME: &str = "wisp";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Page opened in new tabs. "wisp:start" is the built-in start page.
    pub homepage: String,
    /// Search URL; `%s` is replaced by the query.
    pub search_url: String,

    // --- Privacy ---
    /// false = nothing (cookies, cache, storage) ever touches the disk.
    pub persistent_profile: bool,
    /// Upgrade http:// navigations to https://.
    pub https_only: bool,
    /// Remove utm_*, fbclid, gclid, ... from URLs.
    pub strip_tracking_params: bool,
    pub block_third_party_cookies: bool,
    /// WebKit Intelligent Tracking Prevention.
    pub tracking_prevention: bool,
    pub javascript: bool,
    pub webgl: bool,
    /// Let sites use IndexedDB. In a private session it lives in memory
    /// and is gone when Wisp quits, like cookies.
    pub site_databases: bool,
    /// Kinds of app links (e.g. "claude", "mailto") opened without asking.
    pub allowed_app_links: Vec<String>,
    pub webrtc: bool,
    /// Custom user agent; null keeps WebKit's generic default.
    pub user_agent: Option<String>,

    // --- Memory ---
    /// Composite pages on the GPU: smooth scrolling and video, ~100 MB more
    /// per web process. Page content is still drawn by the CPU.
    pub gpu_compositing: bool,
    /// Also draw page content and the browser UI on the GPU.
    pub hardware_acceleration: bool,
    /// Tabs share one web process where WebKit allows it.
    pub shared_web_process: bool,
    /// Unload background tabs after this many idle minutes (0 = never).
    pub suspend_tabs_after_minutes: u32,
    /// Soft memory limit for each web process in MB (0 = WebKit default).
    /// Above it WebKit drops caches and runs GC aggressively.
    pub memory_limit_mb: u32,
    /// Keep the in-memory page/resource cache small.
    pub low_memory_cache: bool,
    /// Ask where to save each download. Otherwise files go to `download_dir`.
    pub ask_download_location: bool,
    /// Folder for downloads; null means your Downloads folder.
    pub download_dir: Option<String>,
    /// Show bookmarks under the address bar (Ctrl+Shift+B toggles).
    pub show_bookmarks_bar: bool,
    /// Remember visited pages (encrypted on disk). Off: nothing is kept.
    pub save_history: bool,
    /// Reopen your tabs when Wisp starts (encrypted on disk).
    pub restore_tabs: bool,

    /// Plugin id -> enabled. Plugins not listed use their manifest default.
    pub plugins: BTreeMap<String, bool>,
    /// Plugin ids in the order their toolbar buttons appear (others after).
    pub plugin_order: Vec<String>,
    /// Plugins whose buttons are kept out of the toolbar (still in the ☰ menu).
    pub toolbar_hidden: Vec<String>,
    /// Plugins that start out of the toolbar but were put in it.
    pub toolbar_shown: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            homepage: "wisp:start".into(),
            search_url: "https://html.duckduckgo.com/html/?q=%s".into(),
            persistent_profile: false,
            https_only: true,
            strip_tracking_params: true,
            block_third_party_cookies: true,
            tracking_prevention: true,
            javascript: true,
            webgl: false,
            site_databases: true,
            allowed_app_links: Vec::new(),
            webrtc: false,
            user_agent: None,
            gpu_compositing: true,
            hardware_acceleration: false,
            shared_web_process: true,
            suspend_tabs_after_minutes: 15,
            memory_limit_mb: 768,
            low_memory_cache: true,
            ask_download_location: true,
            download_dir: None,
            show_bookmarks_bar: true,
            save_history: false,
            restore_tabs: false,
            plugins: BTreeMap::new(),
            plugin_order: Vec::new(),
            toolbar_hidden: Vec::new(),
            toolbar_shown: Vec::new(),
        }
    }
}

pub fn config_dir() -> PathBuf {
    glib::user_config_dir().join(APP_NAME)
}

pub fn data_dir() -> PathBuf {
    glib::user_data_dir().join(APP_NAME)
}

pub fn cache_dir() -> PathBuf {
    glib::user_cache_dir().join(APP_NAME)
}

impl Config {
    fn path() -> PathBuf {
        config_dir().join("config.json")
    }

    pub fn load() -> Config {
        let path = Self::path();
        match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                eprintln!("wisp: {}: {e}; using defaults", path.display());
                // Keep the broken file around instead of silently overwriting it later.
                let _ = fs::rename(&path, path.with_extension("json.bak"));
                Config::default()
            }),
            Err(_) => {
                let cfg = Config::default();
                cfg.save();
                cfg
            }
        }
    }

    pub fn save(&self) {
        let path = Self::path();
        let _ = fs::create_dir_all(config_dir());
        match serde_json::to_string_pretty(self) {
            Ok(text) => {
                if let Err(e) = fs::write(&path, text) {
                    eprintln!("wisp: cannot write {}: {e}", path.display());
                }
            }
            Err(e) => eprintln!("wisp: cannot serialize config: {e}"),
        }
    }
}
