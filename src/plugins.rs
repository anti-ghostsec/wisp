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

use wisp_core::adblock;
use crate::config::{self, Config};
use crate::native::NativePort;
use crate::sync::Sync;
pub use wisp_core::plugins::*;
use serde_json::{Value as Json, json};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
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

/// Wisp's own fixes for how some pages behave in WebKit (page_fixes.js).
const PAGE_FIXES: &str = include_str!("page_fixes.js");

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
    /// Load a URL in the current tab.
    fn go(&self, url: &str);
    /// Start a download (the user picks where it goes, as usual).
    fn download(&self, url: &str);
    /// Open a file or folder with the system's app for it.
    fn open_path(&self, path: &Path) -> bool;
    /// Whether the user let this native helper command run (see `native_key`).
    fn native_allowed(&self, key: &str) -> bool;
    fn allow_native(&self, key: &str);
    /// Ask the user; resolves to the button chosen, None if dismissed.
    fn ask(&self, message: String, detail: String, buttons: &[&str]) -> Pin<Box<dyn Future<Output = Option<i32>>>>;
}

pub struct PluginHost {
    pub plugins: Vec<Rc<Plugin>>,
    ucm: UserContentManager,
    filter_store: UserContentFilterStore,
    /// Where Wisp's own filter is compiled: it names the sites you let
    /// through over plain HTTP, so it's kept in the runtime folder (in
    /// memory, gone when you log out), never with the cache on disk.
    own_store: UserContentFilterStore,
    /// Compiled filters per plugin: (identifier, filter).
    filters: RefCell<HashMap<String, Vec<(String, UserContentFilter)>>>,
    /// Wisp's own filter (the HTTPS upgrade), kept through `apply`.
    own_filter: RefCell<Option<(String, UserContentFilter)>>,
    /// Counts `set_own_filter` calls, so the newest one wins.
    own_filter_turn: Cell<u64>,
    /// storage, session and sync calls (wisp-core).
    api: Api,
    http: soup::Session,
    ui: RefCell<Option<Rc<dyn PluginUi>>>,
    /// Pages are composited on the GPU (see configure_rendering).
    gpu_compositing: bool,
    network: NetworkSession,
    /// Background pages of enabled plugins, by plugin id.
    backgrounds: RefCell<HashMap<String, WebView>>,
    /// Running native helpers, by plugin id.
    natives: RefCell<HashMap<String, Rc<NativePort>>>,
    /// Plugins whose helper the user is being asked about, or turned down
    /// this run.
    native_asking: RefCell<std::collections::HashSet<String>>,
    native_denied: RefCell<std::collections::HashSet<String>>,
    /// Plugins in your own plugins folder with the id of a built-in one,
    /// left out: (id, your copy's folder).
    pub shadowed: Vec<(String, PathBuf)>,
    /// Toolbar badge text, by plugin id.
    badges: RefCell<HashMap<String, String>>,
    /// Plugins of your own that can't run, with why: `plugin_signing_key`
    /// is set and they aren't signed with it.
    refused: HashMap<String, String>,
    /// The keys given to pages, each for one plugin in one view, and what
    /// they may reach: see `page_loaded`.
    pages: RefCell<HashMap<String, PageKey>>,
    /// Content filters still being loaded or compiled, and what waits for
    /// them (the first window's pages: see `when_filters_ready`).
    filters_pending: Rc<FilterGate>,
}

/// Counts content filters on their way in; runs what waits for them once
/// there are none left.
type Waiter = Box<dyn FnOnce()>;

#[derive(Default)]
pub struct FilterGate {
    pending: Cell<usize>,
    waiting: RefCell<Vec<Waiter>>,
}

impl FilterGate {
    /// One more filter on its way: in force when the guard is dropped.
    pub fn hold(self: &Rc<Self>) -> FilterGuard {
        self.pending.set(self.pending.get() + 1);
        FilterGuard(self.clone())
    }

    fn release_all(&self) {
        let waiting = std::mem::take(&mut *self.waiting.borrow_mut());
        for f in waiting {
            f();
        }
    }
}

pub struct FilterGuard(Rc<FilterGate>);

impl Drop for FilterGuard {
    fn drop(&mut self) {
        let left = self.0.pending.get().saturating_sub(1);
        self.0.pending.set(left);
        if left == 0 {
            self.0.release_all();
        }
    }
}

struct PageKey {
    plugin: String,
    view: glib::WeakRef<WebView>,
    scope: PageScope,
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
  // The page's key, given by Wisp when the page loads (see page_loaded):
  // which page asks, as Wisp knows it.
  // A synced-entries call made before the key has come waits for it a
  // little.
  let key = globalThis.__wispPageKey || null;
  delete globalThis.__wispPageKey;
  let gotKey;
  const keyCame = new Promise((r) => {{ gotKey = r; }});
  const call = async (op, args = {{}}) => {{
    if (!handler) throw new Error("wisp API unavailable");
    if (!key && op.startsWith("sync.")) await Promise.race([keyCame, new Promise((r) => setTimeout(r, 3000))]);
    return handler.postMessage({{ op, args, key }});
  }};
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
    clipboard: Object.freeze({{ write: (text, {{ secret = false }} = {{}}) => call("clipboard.write", {{ text: String(text), secret: !!secret }}) }}),
    tabs: Object.freeze({{
      open: (url) => call("tabs.open", {{ url }}),
      current: () => call("tabs.current"),
      go: (url) => call("tabs.go", {{ url }}),
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
    _key: (k) => {{ key = k; gotKey(); }},
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
        // Older Wisps kept their own filter with the plugins'.
        for entry in fs::read_dir(&store_dir).into_iter().flatten().flatten() {
            if entry.file_name().to_string_lossy().contains("wisp-own-") {
                wisp_core::vault::wipe(&entry.path());
            }
        }
        let own_dir = glib::user_runtime_dir().join("wisp").join("own-filter");
        let _ = fs::remove_dir_all(&own_dir);
        let _ = fs::create_dir_all(&own_dir);
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(own_dir.parent().unwrap_or(&own_dir), fs::Permissions::from_mode(0o700));
        }
        let http = soup::Session::new();
        http.set_timeout(60);
        let (plugins, shadowed) = discover(cfg);
        let mut refused = HashMap::new();
        if let Some(key) = cfg.plugin_signing_key.as_deref().filter(|k| !k.trim().is_empty()) {
            for p in plugins.iter().filter(|p| !p.built_in) {
                if let Err(why) = crate::signing::check_plugin(&p.dir, key) {
                    eprintln!("wisp: plugin {} left off: {why}", p.manifest.id);
                    p.enabled.set(false);
                    p.trusted.set(false);
                    refused.insert(p.manifest.id.clone(), why);
                }
            }
        }
        let host = Rc::new(PluginHost {
            plugins,
            ucm: ucm.clone(),
            filter_store: UserContentFilterStore::new(&store_dir.to_string_lossy()),
            own_store: UserContentFilterStore::new(&own_dir.to_string_lossy()),
            filters: RefCell::new(HashMap::new()),
            own_filter: RefCell::new(None),
            own_filter_turn: Cell::new(0),
            api: Api::new(sync),
            http,
            ui: RefCell::new(None),
            // WebKit always composites now (on the CPU when GPU compositing
            // is off; privacy.rs), so pages have layers either way.
            gpu_compositing: true,
            network: session.clone(),
            backgrounds: RefCell::new(HashMap::new()),
            natives: RefCell::new(HashMap::new()),
            native_asking: RefCell::default(),
            native_denied: RefCell::default(),
            shadowed,
            badges: RefCell::new(HashMap::new()),
            refused,
            pages: RefCell::default(),
            filters_pending: Rc::default(),
        });
        for p in &host.plugins {
            host.register_handler(p);
        }
        host.apply();
        host
    }

    /// A filter is on its way in (Wisp's own, say): see `when_filters_ready`.
    pub fn hold_filters(&self) -> FilterGuard {
        self.filters_pending.hold()
    }

    /// Run `f` once the content filters (ad and tracker blocking, the
    /// HTTPS upgrade) are in force, so no page loads before them; after 10
    /// seconds at most, should one take that long (a list downloaded for
    /// the first time).
    pub fn when_filters_ready(&self, f: impl FnOnce() + 'static) {
        if self.filters_pending.pending.get() == 0 {
            return f();
        }
        // Run once: by the last filter in, or by the timeout.
        let once: Rc<Cell<Option<Waiter>>> = Rc::new(Cell::new(Some(Box::new(f))));
        let run = once.clone();
        self.filters_pending.waiting.borrow_mut().push(Box::new(move || {
            if let Some(f) = run.take() {
                f();
            }
        }));
        glib::timeout_add_local_once(Duration::from_secs(10), move || {
            if let Some(f) = once.take() {
                eprintln!("wisp: content filters still loading after 10 s; opening anyway");
                f();
            }
        });
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

    /// (Re)install Wisp's own page fixes, and the scripts, styles and filters
    /// of all enabled plugins.
    pub fn apply(self: &Rc<Self>) {
        self.ucm.remove_all_scripts();
        self.ucm.remove_all_style_sheets();
        self.ucm.remove_all_filters();
        if let Some((_, filter)) = &*self.own_filter.borrow() {
            self.ucm.add_filter(filter);
        }
        self.ucm.add_script(&UserScript::for_world(
            PAGE_FIXES,
            UserContentInjectedFrames::AllFrames,
            UserScriptInjectionTime::Start,
            "wisp-page-fixes",
            &[],
            &[],
        ));
        let enabled: Vec<_> = self.enabled().cloned().collect();
        for p in &enabled {
            self.install(p);
        }
    }

    /// Why plugin `id` can't run at all, if it can't.
    pub fn refused(&self, id: &str) -> Option<&str> {
        self.refused.get(id).map(String::as_str)
    }

    /// You allowed what plugin `id` declares (`Manifest::declaration`), as
    /// it declares it now.
    pub fn approve(&self, id: &str, cfg: &RefCell<Config>) {
        let Some(p) = self.get(id).filter(|_| !self.refused.contains_key(id)) else { return };
        p.trusted.set(true);
        let mut cfg = cfg.borrow_mut();
        cfg.approved_plugins.insert(id.to_string(), p.manifest.declaration_key());
        cfg.save();
    }

    /// Plugins of your own that would be on but wait for you to allow what
    /// they declare: new ones, and ones whose declaration changed.
    pub fn to_review(&self, cfg: &Config) -> Vec<Rc<Plugin>> {
        self.plugins
            .iter()
            .filter(|p| !p.trusted.get() && !self.refused.contains_key(&p.manifest.id))
            .filter(|p| cfg.plugins.get(&p.manifest.id).copied().unwrap_or(p.manifest.enabled_by_default))
            .cloned()
            .collect()
    }

    /// The settings are released before the UI is rebuilt, which reads them.
    /// A plugin you haven't allowed (`approve`) isn't turned on.
    pub fn set_enabled(self: &Rc<Self>, id: &str, enabled: bool, cfg: &RefCell<Config>) {
        if let Some(p) = self.get(id).filter(|p| !enabled || p.trusted.get()) {
            p.enabled.set(enabled);
            {
                let mut cfg = cfg.borrow_mut();
                cfg.plugins.insert(id.to_string(), enabled);
                cfg.save();
            }
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

    async fn native_port(self: &Rc<Self>, p: &Rc<Plugin>) -> Result<Rc<NativePort>, String> {
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
        self.ask_native(p, &argv).await?;
        // Another call may have started it while the question was up.
        if let Some(port) = self.natives.borrow().get(&p.manifest.id) {
            return Ok(port.clone());
        }
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

    /// A helper is a program running as you, outside the browser's
    /// sandbox: it starts only once you've allowed that exact command for
    /// that plugin.
    async fn ask_native(&self, p: &Plugin, argv: &[std::ffi::OsString]) -> Result<(), String> {
        let id = &p.manifest.id;
        let command = argv.iter().map(|a| a.to_string_lossy()).collect::<Vec<_>>().join(" ");
        let key = native_key(id, &command);
        let ui = self.ui.borrow().clone().ok_or("the browser isn't ready yet")?;
        if ui.native_allowed(&key) {
            return Ok(());
        }
        let refused = || format!("{} isn't allowed to start its helper program", p.manifest.name);
        if self.native_denied.borrow().contains(id) {
            return Err(refused());
        }
        if !self.native_asking.borrow_mut().insert(id.clone()) {
            return Err(format!("waiting for you to allow {}'s helper program", p.manifest.name));
        }
        let choice = ui
            .ask(
                format!("Let {} run a program?", p.manifest.name),
                format!(
                    "The {} plugin wants to start its helper:\n\n{command}\n\nIt runs as you, outside the browser's sandbox, and can do anything you can. Allow it only if you trust this plugin.",
                    p.manifest.name
                ),
                &["Don't Allow", "Allow"],
            )
            .await;
        self.native_asking.borrow_mut().remove(id);
        if choice == Some(1) {
            ui.allow_native(&key);
            Ok(())
        } else {
            self.native_denied.borrow_mut().insert(id.clone());
            Err(refused())
        }
    }

    // ---- content blocking ----

    /// Wisp's own content filter, beside the plugins': `rules` take the
    /// place of the ones before, None removes it. Done once it's in force.
    pub async fn set_own_filter(&self, rules: Option<String>) {
        let turn = self.own_filter_turn.get() + 1;
        self.own_filter_turn.set(turn);
        let new = match rules {
            Some(json) => {
                let ident = format!("wisp-own-{:016x}", hash(&json));
                let bytes = glib::Bytes::from_owned(json.into_bytes());
                match self.own_store.save_future(&ident, &bytes).await {
                    Ok(filter) => Some((ident, filter)),
                    Err(e) => {
                        eprintln!("wisp: cannot compile Wisp's own content filter: {e}");
                        return;
                    }
                }
            }
            None => None,
        };
        // A later call is on its way, or got here first.
        if self.own_filter_turn.get() != turn {
            return;
        }
        let old = self.own_filter.replace(new.clone());
        let same = old.as_ref().map(|(id, _)| id) == new.as_ref().map(|(id, _)| id);
        if let Some((id, _)) = &old {
            self.ucm.remove_filter_by_id(id);
        }
        if let Some((_, filter)) = &new {
            self.ucm.add_filter(filter);
        }
        if let Some((id, _)) = old.filter(|_| !same) {
            let _ = self.own_store.remove_future(&id).await;
        }
    }

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
            let guard = self.hold_filters();
            glib::spawn_future_local(async move {
                if let Some(host) = host.upgrade() {
                    host.load_or_compile(&plugin, &ident, || Some(src)).await;
                }
                drop(guard);
            });
        }
        for list in p.manifest.filter_lists.clone() {
            let host = Rc::downgrade(self);
            let plugin = p.clone();
            let guard = self.hold_filters();
            glib::spawn_future_local(async move {
                if let Some(host) = host.upgrade() {
                    host.load_filter_list(&plugin, &list, guard).await;
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

    /// `guard` is let go once the list is in force: from the cache, or
    /// downloaded the first time.
    async fn load_filter_list(self: &Rc<Self>, p: &Rc<Plugin>, list: &FilterList, guard: FilterGuard) {
        let mut guard = Some(guard);
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
            if self.load_or_compile(p, ident, || fs::read_to_string(&json_path).ok()).await {
                guard = None;
            }
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
        drop(guard);
    }

    /// A filter list, if it's no bigger than `MAX_LIST_BYTES`: read a piece
    /// at a time, so a server can't make Wisp hold whatever it sends.
    async fn fetch_text(&self, url: &str) -> Result<String, String> {
        let msg = soup::Message::new("GET", url).map_err(|e| e.to_string())?;
        let stream = self.http.send_future(&msg, glib::Priority::LOW).await.map_err(|e| e.to_string())?;
        let status = msg.status().into_glib();
        if status != 200 {
            return Err(format!("HTTP {status}"));
        }
        let mut text = Vec::new();
        loop {
            let piece = stream.read_bytes_future(64 * 1024, glib::Priority::LOW).await.map_err(|e| e.to_string())?;
            if piece.is_empty() {
                break;
            }
            if over_limit(text.len(), piece.len()) {
                return Err(format!("the list is larger than {} MB", MAX_LIST_BYTES / (1024 * 1024)));
            }
            text.extend_from_slice(&piece);
        }
        String::from_utf8(text).map_err(|e| e.to_string())
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
            let scope = host.scope_of(&p, msg["key"].as_str());
            match host.dispatch(&p, op, &msg["args"], scope).await {
                Ok(result) => {
                    let text = serde_json::to_string(&result).unwrap_or_else(|_| "null".into());
                    reply.return_value(&javascriptcore::Value::from_json(&ctx, &text));
                }
                Err(e) => reply.return_error_message(&e),
            }
        });
    }

    async fn dispatch(self: &Rc<Self>, p: &Rc<Plugin>, op: &str, args: &Json, scope: Option<PageScope>) -> Result<Json, String> {
        if let Some(result) = self.api.dispatch(p, op, args, scope).await {
            return result;
        }
        let need = |perm: &str| {
            if p.has_permission(perm) {
                Ok(())
            } else {
                Err(format!("plugin '{}' lacks the \"{perm}\" permission", p.manifest.id))
            }
        };
        let (area, verb) = op.split_once('.').unwrap_or((op, ""));
        match (area, verb) {
            ("clipboard", "write") => {
                need("clipboard")?;
                let text = args["text"].as_str().ok_or("missing \"text\"")?.to_string();
                let display = gtk::gdk::Display::default().ok_or("no display")?;
                if !args["secret"].as_bool().unwrap_or(false) {
                    display.clipboard().set_text(&text);
                    return Ok(Json::Null);
                }
                // A secret is marked so clipboard histories don't keep it
                // (the hint KDE's and FerroOS's honor), and cleared after 45
                // seconds if it's still there.
                let provider = gtk::gdk::ContentProvider::new_union(&[
                    gtk::gdk::ContentProvider::for_value(&text.to_value()),
                    gtk::gdk::ContentProvider::for_bytes("x-kde-passwordManagerHint", &glib::Bytes::from_static(b"secret")),
                ]);
                display.clipboard().set_content(Some(&provider)).map_err(|e| e.to_string())?;
                glib::timeout_add_seconds_local_once(45, move || {
                    let clipboard = display.clipboard();
                    glib::spawn_future_local(async move {
                        if clipboard.read_text_future().await.ok().flatten().is_some_and(|t| t == text) {
                            clipboard.set_text("");
                        }
                    });
                });
                Ok(Json::Null)
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
            ("tabs", "go") => {
                need("tabs")?;
                let url = args["url"].as_str().ok_or("missing \"url\"")?;
                if !(url.starts_with("https://") || url.starts_with("http://")) {
                    return Err("tabs.go only accepts http(s) URLs".into());
                }
                if let Some(ui) = self.ui.borrow().clone() {
                    ui.go(url);
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
                self.native_port(p).await?.post(&args["message"])?;
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
                if old.as_deref() != Some(&text)
                    && let Some(ui) = self.ui.borrow().clone() {
                        ui.plugins_changed();
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

    /// A page has loaded in `view` (its main frame committed): each plugin
    /// is given a new key for it, the view's earlier keys no longer count.
    /// The key says what the page may reach, from the address Wisp loaded,
    /// not anything the page says.
    pub fn page_loaded(&self, view: &WebView) {
        let mut pages = self.pages.borrow_mut();
        pages.retain(|_, k| k.view.upgrade().is_some_and(|v| &v != view));
        let Some(uri) = view.uri() else { return };
        for p in self.enabled() {
            let scope = if uri.starts_with(&p.page_url("")) {
                PageScope::All
            } else if let Some(host) = entry_host(&uri) {
                PageScope::Site { site: site_of(&host), host }
            } else {
                continue;
            };
            let key = page_key();
            let js = format!("globalThis.wisp ? wisp._key({0:?}) : (globalThis.__wispPageKey = {0:?});", key);
            view.evaluate_javascript(&js, Some(&p.world()), None, None::<&gio::Cancellable>, |_| {});
            pages.insert(key, PageKey { plugin: p.manifest.id.clone(), view: view.downgrade(), scope });
        }
    }

    /// What the page that sent `key` to plugin `p` may reach, if the key is
    /// one Wisp gave it and its view is still open.
    fn scope_of(&self, p: &Plugin, key: Option<&str>) -> Option<PageScope> {
        let pages = self.pages.borrow();
        let k = pages.get(key?)?;
        (k.plugin == p.manifest.id && k.view.upgrade().is_some()).then(|| k.scope.clone())
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
