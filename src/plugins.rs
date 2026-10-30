//! Wisp plugins.
//!
//! A plugin is a directory containing `plugin.json` plus the files it names.
//! It can contribute:
//!   * content scripts (JS) run in an isolated world with access to the `wisp` API,
//!     or in the page's own world (no API) when it must change page-visible objects;
//!   * user style sheets (CSS);
//!   * content-blocker rule lists: WebKit JSON files shipped with the plugin, or
//!     Adblock Plus/EasyList lists downloaded and converted (see adblock.rs);
//!   * toolbar actions (buttons + shortcuts, optionally opening a popup page)
//!     and pages served at `wisp-plugin://<id>/<file>`;
//!   * a background script that runs while the plugin is enabled, which
//!     pages message, and which can talk to a native helper program.
//!
//! See PLUGINS.md for the format and API.

use crate::adblock;
use crate::config::{self, Config};
use crate::native::NativePort;
use crate::sync::Sync;
use serde::Deserialize;
use serde_json::{Map, Value as Json, json};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime};
use webkit6::glib::translate::IntoGlib;
use webkit6::javascriptcore;
use webkit6::soup::prelude::*;
use webkit6::prelude::*;
use webkit6::{
    NetworkSession, ScriptMessageReply, WebView, UserContentFilter, UserContentFilterStore, UserContentInjectedFrames,
    UserContentManager, UserScript, UserScriptInjectionTime, UserStyleLevel, UserStyleSheet,
};
use webkit6::{gio, glib, soup};

pub const PAGE_SCHEME: &str = "wisp-plugin";
/// The (empty) page background scripts run in: wisp-plugin://<id>/_background.html
pub const BACKGROUND_PAGE: &str = "_background.html";

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "yes")]
    pub enabled_by_default: bool,
    /// API permissions: "storage", "tabs", "notifications", "sync".
    #[serde(default)]
    pub permissions: Vec<String>,
    /// Title prefixes of synced entries this plugin may see and change.
    #[serde(default)]
    pub sync_prefixes: Vec<String>,
    #[serde(default)]
    pub content_scripts: Vec<ContentScript>,
    #[serde(default)]
    pub blocklists: Vec<String>,
    #[serde(default)]
    pub filter_lists: Vec<FilterList>,
    #[serde(default)]
    pub actions: Vec<Action>,
    /// Whether the actions start out as toolbar buttons; with false they
    /// sit in the ☰ menu until the user ticks "Toolbar".
    #[serde(default = "yes")]
    pub toolbar: bool,
    #[serde(default)]
    pub context_menu: Vec<MenuItem>,
    /// Scripts run in a hidden page for as long as the plugin is enabled.
    #[serde(default)]
    pub background: Vec<String>,
    /// A helper program the background talks to (needs "native"): the
    /// command and its arguments; arguments naming files in the plugin folder
    /// become full paths.
    #[serde(default)]
    pub native: Vec<String>,
    /// "dark": while enabled, pages are told the user prefers a dark color
    /// scheme (`prefers-color-scheme: dark`), and the browser UI turns dark.
    #[serde(default)]
    pub color_scheme: Option<String>,
}

/// An entry added to the page's right-click menu. Choosing it sends a
/// `context-menu` event to the plugin's isolated script on that page.
#[derive(Debug, Clone, Deserialize)]
pub struct MenuItem {
    pub id: String,
    pub label: String,
    /// Where it shows: "editable" (text fields), "link", "media" (video and
    /// audio), "selection" or "page" (always).
    #[serde(default = "page_context")]
    pub contexts: Vec<String>,
}

fn page_context() -> Vec<String> {
    vec!["page".into()]
}

#[derive(Debug, Clone, Deserialize)]
pub struct ContentScript {
    #[serde(default = "all_urls")]
    pub matches: Vec<String>,
    #[serde(default)]
    pub exclude_matches: Vec<String>,
    #[serde(default)]
    pub js: Vec<String>,
    #[serde(default)]
    pub css: Vec<String>,
    #[serde(default)]
    pub run_at: RunAt,
    #[serde(default)]
    pub all_frames: bool,
    #[serde(default)]
    pub world: World,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RunAt {
    DocumentStart,
    #[default]
    DocumentEnd,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum World {
    #[default]
    Isolated,
    Page,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FilterList {
    pub name: String,
    pub url: String,
    #[serde(default = "four")]
    pub refresh_days: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Action {
    pub id: String,
    pub label: String,
    /// Icon name from the icon theme, e.g. "starred-symbolic".
    #[serde(default)]
    pub icon: Option<String>,
    /// GTK accelerator, e.g. "<Ctrl>d".
    #[serde(default)]
    pub shortcut: Option<String>,
    /// Open this plugin page in a new tab instead of messaging the plugin.
    #[serde(default)]
    pub page: Option<String>,
    /// Show this plugin page in a popup under the button, as browser
    /// extensions do. It gets `?tab=<id>` of the current tab.
    #[serde(default)]
    pub popup: Option<String>,
}

fn yes() -> bool {
    true
}

fn four() -> u64 {
    4
}

fn all_urls() -> Vec<String> {
    vec!["*://*/*".into()]
}

pub struct Plugin {
    pub manifest: Manifest,
    pub dir: PathBuf,
    pub enabled: Cell<bool>,
}

impl Plugin {
    /// Name usable as a JS identifier / world name.
    fn key(&self) -> String {
        sanitize(&self.manifest.id)
    }

    pub fn world(&self) -> String {
        format!("wisp-plugin-{}", self.key())
    }

    fn handler(&self) -> String {
        format!("wisp_{}", self.key())
    }

    pub fn has_background(&self) -> bool {
        !self.manifest.background.is_empty()
    }

    pub fn page_url(&self, page: &str) -> String {
        format!("{PAGE_SCHEME}://{}/{}", self.manifest.id, page.trim_start_matches('/'))
    }

    /// Path of a file inside the plugin directory, refusing anything outside it.
    pub fn file(&self, rel: &str) -> Option<PathBuf> {
        let path = self.dir.join(rel).canonicalize().ok()?;
        path.starts_with(self.dir.canonicalize().ok()?).then_some(path)
    }

    fn read(&self, rel: &str) -> Option<String> {
        match self.file(rel) {
            Some(p) => fs::read_to_string(p)
                .map_err(|e| eprintln!("wisp: plugin {}: {rel}: {e}", self.manifest.id))
                .ok(),
            None => {
                eprintln!("wisp: plugin {}: missing or invalid file {rel}", self.manifest.id);
                None
            }
        }
    }

    fn has_permission(&self, p: &str) -> bool {
        self.manifest.permissions.iter().any(|x| x == p)
    }
}

/// Things the plugin API needs from the browser UI.
pub trait PluginUi {
    /// Open a tab; `focus` switches to it.
    fn open_tab(&self, url: &str, focus: bool);
    fn notify(&self, title: &str, body: &str);
    /// Plugins were enabled/disabled (or a badge changed): refresh toolbars.
    fn plugins_changed(&self);
    /// A web view that isn't shown anywhere, for background pages.
    fn hidden_view(&self) -> WebView;
    /// Every open web view (tabs and popups).
    fn views(&self) -> Vec<WebView>;
    /// { id, url, title } of the current tab.
    fn current_tab(&self) -> Json;
    /// Start a download (the user picks where it goes, as usual).
    fn download(&self, url: &str);
    /// Open a file or folder with the system's app for it.
    fn open_path(&self, path: &Path) -> bool;
}

pub struct PluginHost {
    pub plugins: Vec<Rc<Plugin>>,
    ucm: UserContentManager,
    filter_store: UserContentFilterStore,
    /// Compiled filters per plugin: (identifier, filter).
    filters: RefCell<HashMap<String, Vec<(String, UserContentFilter)>>>,
    storage: RefCell<HashMap<String, Map<String, Json>>>,
    session: RefCell<HashMap<String, Map<String, Json>>>,
    sync: Rc<Sync>,
    http: soup::Session,
    ui: RefCell<Option<Rc<dyn PluginUi>>>,
    /// Pages are composited on the GPU (see configure_rendering).
    gpu_compositing: bool,
    network: NetworkSession,
    /// Background pages of enabled plugins, by plugin id.
    backgrounds: RefCell<HashMap<String, WebView>>,
    /// Running native helpers, by plugin id.
    natives: RefCell<HashMap<String, Rc<NativePort>>>,
    /// Toolbar badge text, by plugin id.
    badges: RefCell<HashMap<String, String>>,
}

/// Directories searched for plugins, in priority order (first id wins).
pub fn plugin_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![config::data_dir().join("plugins")];
    if let Ok(extra) = std::env::var("WISP_PLUGIN_PATH") {
        dirs.extend(std::env::split_paths(&extra));
    }
    dirs.push(PathBuf::from("/usr/share/wisp/plugins"));
    // Bundled plugins when running from the source tree.
    dirs.push(PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/plugins")));
    dirs
}

fn discover(cfg: &Config) -> Vec<Rc<Plugin>> {
    let mut found: Vec<Rc<Plugin>> = Vec::new();
    for dir in plugin_dirs() {
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        entries.sort();
        for path in entries {
            let manifest_path = path.join("plugin.json");
            let Ok(text) = fs::read_to_string(&manifest_path) else { continue };
            let manifest: Manifest = match serde_json::from_str(&text) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("wisp: {}: {e}", manifest_path.display());
                    continue;
                }
            };
            if found.iter().any(|p| p.manifest.id == manifest.id) {
                continue;
            }
            let enabled = cfg.plugins.get(&manifest.id).copied().unwrap_or(manifest.enabled_by_default);
            found.push(Rc::new(Plugin { manifest, dir: path, enabled: Cell::new(enabled) }));
        }
    }
    found
}

/// Runs first in each plugin's isolated world and defines the `wisp` API object.
fn api_prelude(p: &Plugin, gpu_compositing: bool) -> String {
    let meta = json!({
        "id": p.manifest.id,
        "name": p.manifest.name,
        "version": p.manifest.version,
    });
    format!(
        r#"(() => {{
  if (globalThis.wisp) return;
  const handler = window.webkit && window.webkit.messageHandlers[{handler:?}];
  const call = (op, args = {{}}) => handler
    ? handler.postMessage({{ op, args }})
    : Promise.reject(new Error("wisp API unavailable"));
  const listeners = {{}};
  const handlers = [];
  const store = (prefix) => Object.freeze({{
    get: (key) => call(prefix + ".get", {{ key }}),
    set: (key, value) => call(prefix + ".set", {{ key, value }}),
    remove: (key) => call(prefix + ".remove", {{ key }}),
    keys: () => call(prefix + ".keys"),
  }});
  globalThis.wisp = Object.freeze({{
    plugin: Object.freeze({meta}),
    browser: Object.freeze({{ gpuCompositing: {gpu_compositing} }}),
    storage: store("storage"),
    session: store("session"),
    sync: Object.freeze({{
      list: (prefix) => call("sync.list", {{ prefix }}),
      add: (entry) => call("sync.add", {{ entry }}),
      update: (id, fields) => call("sync.update", {{ id, fields }}),
      remove: (id) => call("sync.remove", {{ id }}),
      move: (id, target, after = false) => call("sync.move", {{ id, target, after }}),
      status: () => call("sync.status"),
    }}),
    runtime: Object.freeze({{
      // To the background script; resolves to what its handler returns.
      sendMessage: (message) => call("runtime.send", {{ message }}),
      // From the background script to the plugin's open pages ("broadcast" event).
      broadcast: (message) => call("runtime.broadcast", {{ message }}),
    }}),
    onMessage: (fn) => {{ handlers.push(fn); }},
    native: Object.freeze({{
      post: (message) => call("native.post", {{ message }}),
      close: () => call("native.close"),
    }}),
    action: Object.freeze({{ setBadge: (text) => call("action.badge", {{ text: String(text || "") }}) }}),
    downloads: Object.freeze({{ start: (url) => call("downloads.start", {{ url }}) }}),
    cookies: Object.freeze({{ netscape: (url) => call("cookies.netscape", {{ url }}) }}),
    openPath: (path) => call("open.path", {{ path }}),
    tabs: Object.freeze({{
      open: (url) => call("tabs.open", {{ url }}),
      current: () => call("tabs.current"),
    }}),
    notify: (title, body = "") => call("notify", {{ title, body }}),
    log: (...parts) => call("log", {{ message: parts.map(String).join(" ") }}),
    on: (type, fn) => {{ (listeners[type] = listeners[type] || []).push(fn); }},
    _handle: async (message) => {{
      for (const fn of handlers) {{
        const r = await fn(message);
        if (r !== undefined) return r;
      }}
      return null;
    }},
    _emit: (type, data) => {{
      for (const fn of listeners[type] || []) {{
        try {{ fn(data); }} catch (e) {{ console.error(e); }}
      }}
    }},
  }});
}})();"#,
        handler = p.handler(),
    )
}

impl PluginHost {
    pub fn new(cfg: &Config, ucm: &UserContentManager, sync: Rc<Sync>, session: &NetworkSession) -> Rc<PluginHost> {
        let store_dir = config::cache_dir().join("content-filters");
        let _ = fs::create_dir_all(&store_dir);
        let http = soup::Session::new();
        http.set_timeout(60);
        let host = Rc::new(PluginHost {
            plugins: discover(cfg),
            ucm: ucm.clone(),
            filter_store: UserContentFilterStore::new(&store_dir.to_string_lossy()),
            filters: RefCell::new(HashMap::new()),
            storage: RefCell::new(HashMap::new()),
            session: RefCell::new(HashMap::new()),
            sync,
            http,
            ui: RefCell::new(None),
            gpu_compositing: cfg.gpu_compositing || cfg.hardware_acceleration,
            network: session.clone(),
            backgrounds: RefCell::new(HashMap::new()),
            natives: RefCell::new(HashMap::new()),
            badges: RefCell::new(HashMap::new()),
        });
        for p in &host.plugins {
            host.register_handler(p);
        }
        host.apply();
        host
    }

    pub fn set_ui(&self, ui: Rc<dyn PluginUi>) {
        *self.ui.borrow_mut() = Some(ui);
    }

    pub fn get(&self, id: &str) -> Option<Rc<Plugin>> {
        self.plugins.iter().find(|p| p.manifest.id == id).cloned()
    }

    pub fn enabled(&self) -> impl Iterator<Item = &Rc<Plugin>> {
        self.plugins.iter().filter(|p| p.enabled.get())
    }

    /// A file of an enabled plugin, for the `wisp-plugin://` scheme.
    pub fn page_file(&self, id: &str, path: &str) -> Option<PathBuf> {
        self.get(id).filter(|p| p.enabled.get()).and_then(|p| p.file(path))
    }

    fn register_handler(self: &Rc<Self>, p: &Rc<Plugin>) {
        let name = p.handler();
        // Only scripts in the plugin's own world can see this handler, so the
        // sender is always that plugin and web pages cannot call the API.
        self.ucm.register_script_message_handler_with_reply(&name, Some(&p.world()));
        let weak = Rc::downgrade(self);
        let plugin = p.clone();
        self.ucm.connect_script_message_with_reply_received(Some(&name), move |_, value, reply| {
            if let Some(host) = weak.upgrade() {
                host.handle_message(plugin.clone(), value, reply);
            }
            true
        });
    }

    /// (Re)install scripts, styles and filters of all enabled plugins.
    pub fn apply(self: &Rc<Self>) {
        self.ucm.remove_all_scripts();
        self.ucm.remove_all_style_sheets();
        self.ucm.remove_all_filters();
        let enabled: Vec<_> = self.enabled().cloned().collect();
        for p in &enabled {
            self.install(p);
        }
    }

    pub fn set_enabled(self: &Rc<Self>, id: &str, enabled: bool, cfg: &mut Config) {
        if let Some(p) = self.get(id) {
            p.enabled.set(enabled);
            cfg.plugins.insert(id.to_string(), enabled);
            cfg.save();
            self.apply();
            self.sync_backgrounds();
            if let Some(ui) = self.ui.borrow().clone() {
                ui.plugins_changed();
            }
        }
    }

    fn install(self: &Rc<Self>, p: &Rc<Plugin>) {
        let world = p.world();
        let mut prelude_added = false;
        for cs in &p.manifest.content_scripts {
            let frames = if cs.all_frames {
                UserContentInjectedFrames::AllFrames
            } else {
                UserContentInjectedFrames::TopFrame
            };
            let time = match cs.run_at {
                RunAt::DocumentStart => UserScriptInjectionTime::Start,
                RunAt::DocumentEnd => UserScriptInjectionTime::End,
            };
            let allow: Vec<&str> = cs.matches.iter().map(String::as_str).collect();
            let block: Vec<&str> = cs.exclude_matches.iter().map(String::as_str).collect();

            if cs.world == World::Isolated && !cs.js.is_empty() && !prelude_added {
                self.add_prelude(p);
                prelude_added = true;
            }
            for file in &cs.js {
                let Some(src) = p.read(file) else { continue };
                let src = format!("{src}\n//# sourceURL={}", p.page_url(file));
                let script = match cs.world {
                    World::Isolated => UserScript::for_world(&src, frames, time, &world, &allow, &block),
                    World::Page => UserScript::new(&src, frames, time, &allow, &block),
                };
                self.ucm.add_script(&script);
            }
            for file in &cs.css {
                let Some(src) = p.read(file) else { continue };
                let sheet = UserStyleSheet::new(&src, frames, UserStyleLevel::User, &allow, &block);
                self.ucm.add_style_sheet(&sheet);
            }
        }
        if p.has_background() {
            if !prelude_added {
                self.add_prelude(p);
            }
            let page = p.page_url(BACKGROUND_PAGE);
            for file in &p.manifest.background {
                let Some(src) = p.read(file) else { continue };
                let src = format!("{src}\n//# sourceURL={}", p.page_url(file));
                let script = UserScript::for_world(
                    &src,
                    UserContentInjectedFrames::TopFrame,
                    UserScriptInjectionTime::End,
                    &world,
                    &[page.as_str()],
                    &[],
                );
                self.ucm.add_script(&script);
            }
        }
        self.install_filters(p);
    }

    /// The `wisp` API, injected at document start everywhere the plugin's
    /// scripts could run.
    fn add_prelude(&self, p: &Plugin) {
        let prelude = UserScript::for_world(
            &api_prelude(p, self.gpu_compositing),
            UserContentInjectedFrames::AllFrames,
            UserScriptInjectionTime::Start,
            &p.world(),
            &[],
            &[],
        );
        self.ucm.add_script(&prelude);
    }

    // ---- background pages, native helpers, events ----

    /// Start background pages of enabled plugins and stop those of disabled ones.
    pub fn sync_backgrounds(self: &Rc<Self>) {
        let Some(ui) = self.ui.borrow().clone() else { return };
        for p in &self.plugins {
            let id = &p.manifest.id;
            let want = p.enabled.get() && p.has_background();
            let running = self.backgrounds.borrow().contains_key(id);
            if want && !running {
                let view = ui.hidden_view();
                view.load_uri(&p.page_url(BACKGROUND_PAGE));
                self.backgrounds.borrow_mut().insert(id.clone(), view);
            } else if !want && running {
                self.backgrounds.borrow_mut().remove(id);
                if let Some(port) = self.natives.borrow_mut().remove(id) {
                    port.close();
                }
                self.badges.borrow_mut().remove(id);
            }
        }
    }

    pub fn badge(&self, plugin: &str) -> Option<String> {
        self.badges.borrow().get(plugin).cloned().filter(|b| !b.is_empty())
    }

    /// Run `wisp._emit(kind, data)` in the plugin's background page.
    pub fn emit_to_background(&self, plugin: &Plugin, kind: &str, data: &Json) -> bool {
        let Some(view) = self.backgrounds.borrow().get(&plugin.manifest.id).cloned() else { return false };
        emit(&view, plugin, kind, data);
        true
    }

    /// Video and audio a page loads, for plugins with the "media" permission.
    pub fn wants_media(&self) -> bool {
        self.enabled().any(|p| p.has_background() && p.has_permission("media"))
    }

    pub fn media_event(&self, data: &Json) {
        for p in self.enabled().filter(|p| p.has_permission("media")) {
            self.emit_to_background(p, "media", data);
        }
    }

    /// "tab-updated" { tabId, url } and "tab-closed" { tabId }, for
    /// background pages with the "tabs" permission.
    pub fn tab_event(&self, kind: &str, data: &Json) {
        for p in self.enabled().filter(|p| p.has_permission("tabs")) {
            self.emit_to_background(p, kind, data);
        }
    }

    fn native_port(self: &Rc<Self>, p: &Rc<Plugin>) -> Result<Rc<NativePort>, String> {
        if let Some(port) = self.natives.borrow().get(&p.manifest.id) {
            return Ok(port.clone());
        }
        if p.manifest.native.is_empty() {
            return Err(format!("plugin '{}' declares no native helper", p.manifest.id));
        }
        // Files of the plugin become full paths; anything else (python3) is
        // looked up on PATH.
        let argv: Vec<std::ffi::OsString> = p
            .manifest
            .native
            .iter()
            .map(|a| p.file(a).filter(|f| f.is_file()).map(|f| f.into_os_string()).unwrap_or_else(|| a.into()))
            .collect();
        let (weak, plugin) = (Rc::downgrade(self), p.clone());
        let on_message = move |msg: Json| {
            if let Some(host) = weak.upgrade() {
                host.emit_to_background(&plugin, "native", &msg);
            }
        };
        let (weak, plugin) = (Rc::downgrade(self), p.clone());
        let started = std::time::Instant::now();
        let on_exit = move |reason: String| {
            let Some(host) = weak.upgrade() else { return };
            // Only forget it if it's still the current one.
            let current = host.natives.borrow().get(&plugin.manifest.id).is_some_and(|port| port.started == started);
            if current {
                host.natives.borrow_mut().remove(&plugin.manifest.id);
            }
            host.emit_to_background(&plugin, "native-disconnect", &json!({ "error": reason }));
        };
        let port = NativePort::spawn(&argv, &p.dir, started, on_message, on_exit)?;
        self.natives.borrow_mut().insert(p.manifest.id.clone(), port.clone());
        Ok(port)
    }

    // ---- content blocking ----

    fn install_filters(self: &Rc<Self>, p: &Rc<Plugin>) {
        if let Some(compiled) = self.filters.borrow().get(&p.manifest.id) {
            for (_, f) in compiled {
                self.ucm.add_filter(f);
            }
            return;
        }
        self.filters.borrow_mut().insert(p.manifest.id.clone(), Vec::new());
        for file in &p.manifest.blocklists {
            let Some(src) = p.read(file) else { continue };
            let ident = format!("{}-{}-{:016x}", p.key(), sanitize(file), hash(&src));
            let host = Rc::downgrade(self);
            let plugin = p.clone();
            glib::spawn_future_local(async move {
                if let Some(host) = host.upgrade() {
                    host.load_or_compile(&plugin, &ident, || Some(src)).await;
                }
            });
        }
        for list in p.manifest.filter_lists.clone() {
            let host = Rc::downgrade(self);
            let plugin = p.clone();
            glib::spawn_future_local(async move {
                if let Some(host) = host.upgrade() {
                    host.load_filter_list(&plugin, &list).await;
                }
            });
        }
    }

    /// Load a compiled filter by identifier, compiling `source()` if it isn't cached.
    async fn load_or_compile(
        &self,
        p: &Rc<Plugin>,
        ident: &str,
        source: impl FnOnce() -> Option<String>,
    ) -> bool {
        let filter = match self.filter_store.load_future(ident).await {
            Ok(f) => f,
            Err(_) => {
                let Some(src) = source() else { return false };
                let bytes = glib::Bytes::from_owned(src.into_bytes());
                match self.filter_store.save_future(ident, &bytes).await {
                    Ok(f) => f,
                    Err(e) => {
                        eprintln!("wisp: plugin {}: cannot compile {ident}: {e}", p.manifest.id);
                        return false;
                    }
                }
            }
        };
        self.add_compiled(p, ident, filter);
        true
    }

    fn add_compiled(&self, p: &Plugin, ident: &str, filter: UserContentFilter) {
        let prefix = ident.rsplit_once('-').map_or(ident, |(a, _)| a);
        let mut filters = self.filters.borrow_mut();
        let list = filters.entry(p.manifest.id.clone()).or_default();
        // Replace an older version of the same list.
        list.retain(|(id, _)| {
            let same = id.rsplit_once('-').map_or(id.as_str(), |(a, _)| a) == prefix;
            if same {
                self.ucm.remove_filter_by_id(id);
            }
            !same
        });
        list.push((ident.to_string(), filter.clone()));
        if p.enabled.get() {
            self.ucm.add_filter(&filter);
        }
    }

    async fn load_filter_list(self: &Rc<Self>, p: &Rc<Plugin>, list: &FilterList) {
        let dir = config::cache_dir().join("filter-lists");
        let base = format!("{}-{}", p.key(), sanitize(&list.name));
        let meta_path = dir.join(format!("{base}.meta"));
        // meta = "<identifier>\n" ; its mtime is when the list was fetched.
        let cached = fs::read_to_string(&meta_path).ok().map(|s| s.trim().to_string());
        let age = fs::metadata(&meta_path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok());

        if let Some(ident) = &cached {
            let json_path = dir.join(format!("{base}.json"));
            self.load_or_compile(p, ident, || fs::read_to_string(&json_path).ok()).await;
        }
        let stale = age.is_none_or(|a| a > Duration::from_secs(list.refresh_days * 86_400));
        if !stale {
            return;
        }
        if cached.is_some() {
            // Don't compete with page loads right after startup.
            glib::timeout_future_seconds(30).await;
        }
        match self.fetch_text(&list.url).await {
            Ok(text) => {
                let converted = adblock::convert(&text);
                let ident = format!("{base}-{:016x}", hash(&converted.json));
                let _ = fs::create_dir_all(&dir);
                let _ = fs::write(dir.join(format!("{base}.json")), &converted.json);
                let json = converted.json;
                if self.load_or_compile(p, &ident, move || Some(json)).await {
                    let _ = fs::write(&meta_path, &ident);
                    if let Some(old) = cached.filter(|o| *o != ident) {
                        let _ = self.filter_store.remove_future(&old).await;
                    }
                    println!(
                        "wisp: {}: {} rules active ({} unsupported skipped)",
                        list.name, converted.rules, converted.skipped
                    );
                }
            }
            Err(e) => eprintln!("wisp: cannot download {}: {e}", list.name),
        }
    }

    async fn fetch_text(&self, url: &str) -> Result<String, String> {
        let msg = soup::Message::new("GET", url).map_err(|e| e.to_string())?;
        let bytes = self.http.send_and_read_future(&msg, glib::Priority::LOW).await.map_err(|e| e.to_string())?;
        let status = msg.status().into_glib();
        if status != 200 {
            return Err(format!("HTTP {status}"));
        }
        String::from_utf8(bytes.to_vec()).map_err(|e| e.to_string())
    }

    // ---- the wisp API ----

    fn handle_message(self: &Rc<Self>, p: Rc<Plugin>, value: &javascriptcore::Value, reply: &ScriptMessageReply) {
        let Some(ctx) = value.context() else { return };
        let msg: Json = value
            .to_json(0)
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(Json::Null);
        let host = self.clone();
        let reply = reply.clone();
        glib::spawn_future_local(async move {
            let op = msg["op"].as_str().unwrap_or("");
            match host.dispatch(&p, op, &msg["args"]).await {
                Ok(result) => {
                    let text = serde_json::to_string(&result).unwrap_or_else(|_| "null".into());
                    reply.return_value(&javascriptcore::Value::from_json(&ctx, &text));
                }
                Err(e) => reply.return_error_message(&e),
            }
        });
    }

    async fn dispatch(self: &Rc<Self>, p: &Rc<Plugin>, op: &str, args: &Json) -> Result<Json, String> {
        let need = |perm: &str| {
            if p.has_permission(perm) {
                Ok(())
            } else {
                Err(format!("plugin '{}' lacks the \"{perm}\" permission", p.manifest.id))
            }
        };
        let key = || args["key"].as_str().map(str::to_owned).ok_or("missing \"key\"".to_string());
        let (area, verb) = op.split_once('.').unwrap_or((op, ""));
        match (area, verb) {
            ("log", _) => {
                println!("[plugin {}] {}", p.manifest.id, args["message"].as_str().unwrap_or(""));
                Ok(Json::Null)
            }
            ("storage" | "session", _) => {
                if area == "storage" {
                    need("storage")?;
                }
                let map = if area == "storage" { &self.storage } else { &self.session };
                let persist = area == "storage";
                match verb {
                    "get" => {
                        let k = key()?;
                        Ok(self.with_map(map, p, persist, |m| m.get(&k).cloned()).unwrap_or(Json::Null))
                    }
                    "keys" => Ok(self.with_map(map, p, persist, |m| Json::from(m.keys().cloned().collect::<Vec<_>>()))),
                    "set" | "remove" => {
                        let k = key()?;
                        let v = args["value"].clone();
                        self.update_map(map, p, persist, |m| {
                            if verb == "set" {
                                m.insert(k, v);
                            } else {
                                m.remove(&k);
                            }
                        })?;
                        Ok(Json::Null)
                    }
                    _ => Err(format!("unknown wisp API call \"{op}\"")),
                }
            }
            ("sync", _) => {
                need("sync")?;
                self.dispatch_sync(p, verb, args).await
            }
            ("tabs", "open") => {
                need("tabs")?;
                let url = args["url"].as_str().ok_or("missing \"url\"")?;
                let own_page = url.starts_with(&p.page_url(""));
                if !(url.starts_with("https://") || url.starts_with("http://") || own_page) {
                    return Err("tabs.open only accepts http(s) URLs and the plugin's own pages".into());
                }
                // The plugin's own pages (settings, lists) are shown right away.
                if let Some(ui) = self.ui.borrow().clone() {
                    ui.open_tab(url, own_page);
                }
                Ok(Json::Null)
            }
            ("tabs", "current") => {
                need("tabs")?;
                Ok(self.ui.borrow().as_ref().map(|ui| ui.current_tab()).unwrap_or(Json::Null))
            }
            ("runtime", "send") => {
                let view = self.backgrounds.borrow().get(&p.manifest.id).cloned();
                let view = view.ok_or_else(|| format!("plugin '{}' has no background script running", p.manifest.id))?;
                let dict = glib::VariantDict::new(None);
                dict.insert("m", args["message"].to_string());
                let value = view
                    .call_async_javascript_function_future(
                        "return await globalThis.wisp._handle(JSON.parse(m));",
                        Some(&dict.end()),
                        Some(&p.world()),
                        None,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(value.to_json(0).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Json::Null))
            }
            ("runtime", "broadcast") => {
                let prefix = p.page_url("");
                if let Some(ui) = self.ui.borrow().clone() {
                    for view in ui.views().iter().filter(|v| v.uri().is_some_and(|u| u.starts_with(&prefix))) {
                        emit(view, p, "broadcast", &args["message"]);
                    }
                }
                Ok(Json::Null)
            }
            ("native", "post") => {
                need("native")?;
                self.native_port(p)?.post(&args["message"])?;
                Ok(Json::Null)
            }
            ("native", "close") => {
                if let Some(port) = self.natives.borrow_mut().remove(&p.manifest.id) {
                    port.close();
                }
                Ok(Json::Null)
            }
            ("action", "badge") => {
                let text: String = args["text"].as_str().unwrap_or("").chars().take(4).collect();
                let old = self.badges.borrow_mut().insert(p.manifest.id.clone(), text.clone());
                if old.as_deref() != Some(&text) {
                    if let Some(ui) = self.ui.borrow().clone() {
                        ui.plugins_changed();
                    }
                }
                Ok(Json::Null)
            }
            ("downloads", "start") => {
                need("downloads")?;
                let url = args["url"].as_str().filter(|u| u.starts_with("https://") || u.starts_with("http://"));
                let url = url.ok_or("downloads.start only accepts http(s) URLs")?;
                if let Some(ui) = self.ui.borrow().clone() {
                    ui.download(url);
                }
                Ok(Json::Null)
            }
            ("cookies", "netscape") => {
                need("cookies")?;
                let url = args["url"].as_str().filter(|u| u.starts_with("https://") || u.starts_with("http://"));
                let url = url.ok_or("cookies.netscape only accepts http(s) URLs")?;
                let manager = self.network.cookie_manager().ok_or("no cookie store")?;
                let mut cookies = manager.cookies_future(url).await.map_err(|e| e.to_string())?;
                Ok(Json::String(netscape_cookies(&mut cookies)))
            }
            ("open", "path") => {
                need("native")?;
                let path = args["path"].as_str().ok_or("missing \"path\"")?;
                let path = if path.is_empty() { crate::downloads::default_dir() } else { PathBuf::from(path) };
                Ok(json!(self.ui.borrow().as_ref().is_some_and(|ui| ui.open_path(&path))))
            }
            ("notify", _) => {
                need("notifications")?;
                let title = args["title"].as_str().unwrap_or(&p.manifest.name);
                if let Some(ui) = self.ui.borrow().clone() {
                    ui.notify(title, args["body"].as_str().unwrap_or(""));
                }
                Ok(Json::Null)
            }
            _ => Err(format!("unknown wisp API call \"{op}\"")),
        }
    }

    async fn dispatch_sync(&self, p: &Plugin, verb: &str, args: &Json) -> Result<Json, String> {
        let prefixes = &p.manifest.sync_prefixes;
        if prefixes.is_empty() {
            return Err(format!("plugin '{}' declares no sync_prefixes", p.manifest.id));
        }
        match verb {
            "list" => {
                let wanted: Vec<&String> = match args["prefix"].as_str() {
                    Some(px) => prefixes.iter().filter(|x| x.as_str() == px).collect(),
                    None => prefixes.iter().collect(),
                };
                if wanted.is_empty() {
                    return Err(format!("prefix not in this plugin's sync_prefixes {prefixes:?}"));
                }
                let mut all = Vec::new();
                for px in wanted {
                    all.extend(self.sync.list(px).await?);
                }
                Ok(Json::Array(all))
            }
            "add" => {
                let entry = &args["entry"];
                let title = entry["title"].as_str().ok_or("entry needs a \"title\"")?;
                if !prefixes.iter().any(|px| title.starts_with(px.as_str())) {
                    return Err(format!("entry title must start with one of {prefixes:?}"));
                }
                let url = entry["url"].as_str().ok_or("entry needs a \"url\"")?;
                let mut item = json!({ "title": title, "url": url });
                if let Some(d) = entry.get("description").filter(|d| d.is_string()) {
                    item["description"] = d.clone();
                }
                if let Some(t) = entry.get("tags").filter(|t| t.is_array()) {
                    item["tags"] = t.clone();
                }
                Ok(json!(self.sync.add(item).await?))
            }
            "update" => {
                let id = args["id"].as_i64().ok_or("missing \"id\"")?;
                self.sync.update(id, args["fields"].clone(), prefixes).await?;
                Ok(Json::Null)
            }
            "remove" => {
                let id = args["id"].as_i64().ok_or("missing \"id\"")?;
                self.sync.remove(id, prefixes).await?;
                Ok(Json::Null)
            }
            "move" => {
                let id = args["id"].as_i64().ok_or("missing \"id\"")?;
                let target = args["target"].as_i64().ok_or("missing \"target\"")?;
                self.sync.move_entry(id, target, args["after"].as_bool().unwrap_or(false), prefixes).await?;
                Ok(Json::Null)
            }
            "status" => Ok(match self.sync.account() {
                Some(a) => json!({ "configured": true, "service": a.service, "id": a.id, "pending": self.sync.pending() }),
                None => json!({ "configured": false }),
            }),
            _ => Err(format!("unknown wisp API call \"sync.{verb}\"")),
        }
    }

    fn storage_path(p: &Plugin) -> PathBuf {
        config::data_dir().join("plugin-data").join(format!("{}.json", p.key()))
    }

    fn with_map<R>(
        &self,
        map: &RefCell<HashMap<String, Map<String, Json>>>,
        p: &Plugin,
        persist: bool,
        f: impl FnOnce(&Map<String, Json>) -> R,
    ) -> R {
        let mut all = map.borrow_mut();
        let m = all.entry(p.manifest.id.clone()).or_insert_with(|| {
            if persist { load_map(&Self::storage_path(p)) } else { Map::new() }
        });
        f(m)
    }

    fn update_map(
        &self,
        map: &RefCell<HashMap<String, Map<String, Json>>>,
        p: &Plugin,
        persist: bool,
        f: impl FnOnce(&mut Map<String, Json>),
    ) -> Result<(), String> {
        let mut all = map.borrow_mut();
        let m = all.entry(p.manifest.id.clone()).or_insert_with(|| {
            if persist { load_map(&Self::storage_path(p)) } else { Map::new() }
        });
        let mut updated = m.clone();
        f(&mut updated);
        let text = serde_json::to_string(&updated).map_err(|e| e.to_string())?;
        // 1 MB per plugin keeps a misbehaving plugin from filling disk or RAM.
        if text.len() > 1 << 20 {
            return Err("plugin storage quota (1 MB) exceeded".into());
        }
        if persist {
            let path = Self::storage_path(p);
            let _ = fs::create_dir_all(path.parent().unwrap());
            fs::write(&path, text).map_err(|e| e.to_string())?;
        }
        *m = updated;
        Ok(())
    }
}

/// `wisp._emit(kind, data)` in a view, in the plugin's world.
fn emit(view: &WebView, p: &Plugin, kind: &str, data: &Json) {
    let js = format!("globalThis.wisp && wisp._emit({}, {data});", Json::from(kind));
    view.evaluate_javascript(&js, Some(&p.world()), None, None::<&gio::Cancellable>, |_| {});
}

/// Cookies in the cookies.txt format curl and yt-dlp read.
fn netscape_cookies(cookies: &mut [soup::Cookie]) -> String {
    let mut out = String::from("# Netscape HTTP Cookie File\n");
    for c in cookies {
        let domain = c.domain().map(|d| d.to_string()).unwrap_or_default();
        let expires = c.expires().map(|d| d.to_unix()).unwrap_or(0);
        out.push_str(&format!(
            "{domain}\t{}\t{}\t{}\t{expires}\t{}\t{}\n",
            if domain.starts_with('.') { "TRUE" } else { "FALSE" },
            c.path().map(|p| p.to_string()).unwrap_or_else(|| "/".into()),
            if c.is_secure() { "TRUE" } else { "FALSE" },
            c.name().map(|n| n.to_string()).unwrap_or_default(),
            c.value().map(|v| v.to_string()).unwrap_or_default(),
        ));
    }
    out
}

fn load_map(path: &Path) -> Map<String, Json> {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect()
}

fn hash(s: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// MIME type for plugin page files.
pub fn mime_for(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" => "text/javascript",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        _ => "application/octet-stream",
    }
}
