//! User configuration, stored as JSON in `~/.config/wisp/config.json`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;

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
    /// Run pages' JavaScript without the JIT compiler (interpreter only).
    /// Most browser exploits go through the JIT, so this closes that door,
    /// at the price of slower scripts on heavy sites. Takes effect when
    /// Wisp next starts.
    pub no_jit: bool,
    /// Custom user agent; null keeps WebKit's generic default.
    pub user_agent: Option<String>,

    // --- Memory ---
    /// Composite pages on the GPU: smooth scrolling and video, ~100 MB more
    /// per web process. Page content is still drawn by the CPU.
    pub gpu_compositing: bool,
    /// Also draw page content and the browser UI on the GPU.
    pub hardware_acceleration: bool,
    /// Hand video frames to the page on the GPU (as textures). Off: as
    /// plain pictures, a little more work for the CPU. Some GPU drivers
    /// (seen with Intel's iris) show some sites' videos black that way,
    /// though the frames are fine. Takes effect when Wisp next starts.
    pub gpu_video: bool,
    /// Tabs share one web process where WebKit allows it.
    pub shared_web_process: bool,
    /// Unload background tabs after this many idle minutes (0 = never).
    pub suspend_tabs_after_minutes: u32,
    /// Soft memory limit for each web process in MB (0 = WebKit default).
    /// Above it WebKit drops caches and runs GC aggressively.
    pub memory_limit_mb: u32,
    /// Don't keep whole pages in memory for Back and Forward.
    pub low_memory_cache: bool,
    /// Ask where to save each download. Otherwise files go to `download_dir`.
    pub ask_download_location: bool,
    /// Folder for downloads; null means your Downloads folder.
    pub download_dir: Option<String>,
    /// Show bookmarks under the address bar (Ctrl+Shift+B toggles).
    pub show_bookmarks_bar: bool,
    /// Wisp's own window dark (toolbar, tabs, menus). Web pages are left as
    /// their sites made them: they aren't told, and nothing is inverted.
    pub dark_ui: bool,
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
    /// Plugins of your own used instead of the built-in plugin with the same id.
    pub plugin_overrides: Vec<String>,
    /// Plugin helper programs you allowed to run, as "plugin-id: command".
    pub allowed_native_helpers: Vec<String>,
    /// Plugins of your own you allowed, by id: what each declared then
    /// (`Manifest::declaration_key`). One that declares something else
    /// stays off until you allow it again.
    pub approved_plugins: BTreeMap<String, String>,
    /// A minisign public key (the line after the comment in a .pub file).
    /// When set, a plugin of your own runs only if its folder is signed
    /// with it (`plugin.minisig`, see PLUGINS.md).
    pub plugin_signing_key: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            homepage: "wisp:start".into(),
            search_url: SEARCH_URL.into(),
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
            no_jit: false,
            user_agent: None,
            gpu_compositing: true,
            hardware_acceleration: false,
            gpu_video: true,
            shared_web_process: true,
            suspend_tabs_after_minutes: 15,
            memory_limit_mb: 768,
            low_memory_cache: true,
            ask_download_location: true,
            download_dir: None,
            show_bookmarks_bar: true,
            dark_ui: false,
            save_history: false,
            restore_tabs: false,
            plugins: BTreeMap::new(),
            plugin_order: Vec::new(),
            toolbar_hidden: Vec::new(),
            toolbar_shown: Vec::new(),
            plugin_overrides: Vec::new(),
            allowed_native_helpers: Vec::new(),
            approved_plugins: BTreeMap::new(),
            plugin_signing_key: None,
        }
    }
}

/// Where Wisp keeps its settings, its data and its cache.
#[derive(Debug, Clone)]
pub struct Dirs {
    pub config: PathBuf,
    pub data: PathBuf,
    pub cache: PathBuf,
}

static DIRS: OnceLock<Dirs> = OnceLock::new();

/// Set once by the host before anything is read: the desktop browser's
/// XDG folders (each with `wisp` added), Android's app folders.
pub fn set_dirs(dirs: Dirs) {
    let _ = DIRS.set(dirs);
}

/// The XDG folders, for a host that didn't set any.
fn xdg_dirs() -> Dirs {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let xdg = |var: &str, fallback: &str| {
        std::env::var_os(var).map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| home.join(fallback)).join(APP_NAME)
    };
    Dirs { config: xdg("XDG_CONFIG_HOME", ".config"), data: xdg("XDG_DATA_HOME", ".local/share"), cache: xdg("XDG_CACHE_HOME", ".cache") }
}

fn dirs() -> &'static Dirs {
    DIRS.get_or_init(xdg_dirs)
}

pub fn config_dir() -> PathBuf {
    dirs().config.clone()
}

pub fn data_dir() -> PathBuf {
    dirs().data.clone()
}

pub fn cache_dir() -> PathBuf {
    dirs().cache.clone()
}

/// DuckDuckGo with its ads turned off (k1=-1).
const SEARCH_URL: &str = "https://duckduckgo.com/?q=%s&k1=-1";
/// What Wisp searched with before: the no-script page, ads and all.
const OLD_SEARCH_URL: &str = "https://html.duckduckgo.com/html/?q=%s";

impl Config {
    fn path() -> PathBuf {
        config_dir().join("config.json")
    }

    pub fn load() -> Config {
        let path = Self::path();
        match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).map(Config::upgrade).unwrap_or_else(|e| {
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

    /// Settings still at an old default move to the new one.
    fn upgrade(mut self) -> Config {
        if self.search_url == OLD_SEARCH_URL {
            self.search_url = SEARCH_URL.into();
            self.save();
        }
        self
    }

    pub fn save(&self) {
        let path = Self::path();
        let _ = fs::create_dir_all(config_dir());
        match serde_json::to_string_pretty(self) {
            Ok(text) => {
                // Readable by you only, and replaced in one step: a crash
                // half-way never leaves half a file.
                if let Err(e) = crate::vault::write_private(&path, &text) {
                    eprintln!("wisp: cannot write {}: {e}", path.display());
                }
            }
            Err(e) => eprintln!("wisp: cannot serialize config: {e}"),
        }
    }
}
