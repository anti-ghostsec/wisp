//! The engine-free half of Wisp's plugins: the manifest, finding plugins,
//! and the parts of the `wisp` API that keep data (`storage`, `session`,
//! `sync`), with the rule that a web page reaches only its own site's
//! synced entries (the Passwords plugin's logins). The host browser runs
//! the scripts, pages and blocklists with its engine.

use crate::config::{self, Config};
use crate::sync::Sync;
use crate::vault;
use serde::Deserialize;
use serde_json::{Map, Value as Json, json};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::rc::Rc;

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
    /// A web page's scripts see only the synced entries of its own site (by
    /// each entry's URL), add or change only those of the site, move only
    /// those (to the front), and remove none unless `site_may_remove`: the
    /// plugin's own pages see them all. Wisp decides
    /// which page asks, so a web page whose renderer has been taken over
    /// still gets nothing of other sites (the Passwords plugin's logins).
    #[serde(default)]
    pub sync_by_site: bool,
    /// With `sync_by_site`: a web page may also remove its own site's
    /// entries (the Bookmarks star taking a bookmark away).
    #[serde(default)]
    pub site_may_remove: bool,
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
    pub fn key(&self) -> String {
        sanitize(&self.manifest.id)
    }

    pub fn world(&self) -> String {
        format!("wisp-plugin-{}", self.key())
    }

    pub fn handler(&self) -> String {
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

    pub fn read(&self, rel: &str) -> Option<String> {
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

    pub fn has_permission(&self, p: &str) -> bool {
        self.manifest.permissions.iter().any(|x| x == p)
    }
}

/// What a page given a key may reach of its plugin's synced entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageScope {
    /// The plugin's own pages and background page: everything.
    All,
    /// A web page: its host (as the Passwords plugin keys logins) and the
    /// rough site it belongs to.
    Site { host: String, site: String },
}

/// The data side of the `wisp` API, for every plugin.
pub struct Api {
    storage: RefCell<HashMap<String, Map<String, Json>>>,
    session: RefCell<HashMap<String, Map<String, Json>>>,
    pub sync: Rc<Sync>,
}

impl Api {
    pub fn new(sync: Rc<Sync>) -> Api {
        Api { storage: RefCell::new(HashMap::new()), session: RefCell::new(HashMap::new()), sync }
    }

    /// The calls of the `wisp` API that don't depend on the browser around
    /// it: `storage.*`, `session.*`, `sync.*` and `log`. None for the others,
    /// which the host answers. `scope` is what the asking page may reach
    /// (from the address the host loaded, never from what the page says).
    pub async fn dispatch(&self, p: &Plugin, op: &str, args: &Json, scope: Option<PageScope>) -> Option<Result<Json, String>> {
        let (area, _) = op.split_once('.').unwrap_or((op, ""));
        match area {
            "storage" | "session" | "sync" | "log" => Some(self.dispatch_here(p, op, args, scope).await),
            _ => None,
        }
    }

    async fn dispatch_here(&self, p: &Plugin, op: &str, args: &Json, scope: Option<PageScope>) -> Result<Json, String> {
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
                // Session values a web page keeps are its site's alone (the
                // login Passwords is about to offer to save).
                let key = || if persist { key() } else { session_key(scope.as_ref(), &key()?) };
                match verb {
                    "get" => {
                        let k = key()?;
                        Ok(self.with_map(map, p, persist, |m| m.get(&k).cloned()).unwrap_or(Json::Null))
                    }
                    "keys" if !persist => {
                        let mine = session_key(scope.as_ref(), "")?;
                        Ok(self.with_map(map, p, persist, |m| {
                            Json::from(m.keys().filter_map(|k| k.strip_prefix(mine.as_str())).map(str::to_owned).collect::<Vec<_>>())
                        }))
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
                if !p.manifest.sync_by_site {
                    return self.dispatch_sync(p, verb, args).await;
                }
                match scope {
                    Some(PageScope::All) => self.dispatch_sync(p, verb, args).await,
                    Some(PageScope::Site { host, site }) => self.dispatch_sync_for_site(p, verb, args, &host, &site).await,
                    None => Err("this page can't use the plugin's synced entries".into()),
                }
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

    /// `sync.*` for a web page of a `sync_by_site` plugin: only its host's
    /// entries are listed, and only its site's added to or changed.
    async fn dispatch_sync_for_site(&self, p: &Plugin, verb: &str, args: &Json, host: &str, site: &str) -> Result<Json, String> {
        let of_site = |url: &str| entry_host(url).is_some_and(|h| site_of(&h) == site);
        match verb {
            "list" => {
                let all = self.dispatch_sync(p, verb, args).await?;
                let mine = all.as_array().into_iter().flatten().filter(|e| e["url"].as_str().and_then(entry_host).as_deref() == Some(host));
                Ok(Json::Array(mine.cloned().collect()))
            }
            "add" => {
                if !args["entry"]["url"].as_str().is_some_and(of_site) {
                    return Err("a page may only save entries for its own site".into());
                }
                self.dispatch_sync(p, verb, args).await
            }
            "update" => {
                let id = args["id"].as_i64().ok_or("missing \"id\"")?;
                let all = self.dispatch_sync(p, "list", &Json::Null).await?;
                let entry = all.as_array().into_iter().flatten().find(|e| e["id"].as_i64() == Some(id));
                let url_ok = |u: Option<&str>| u.is_some_and(of_site);
                if !entry.is_some_and(|e| url_ok(e["url"].as_str())) || args["fields"].get("url").is_some_and(|u| !url_ok(u.as_str())) {
                    return Err("a page may only change entries of its own site".into());
                }
                self.dispatch_sync(p, verb, args).await
            }
            "remove" | "move" => {
                let id = args["id"].as_i64().ok_or("missing \"id\"")?;
                if verb == "remove" && !p.manifest.site_may_remove {
                    return Err("a web page can't use \"sync.remove\"".into());
                }
                let all = self.dispatch_sync(p, "list", &Json::Null).await?;
                let all = all.as_array().map(Vec::as_slice).unwrap_or_default();
                let Some(entry) = all.iter().find(|e| e["id"].as_i64() == Some(id) && e["url"].as_str().is_some_and(of_site)) else {
                    return Err("a page may only change entries of its own site".into());
                };
                if verb == "remove" {
                    return self.dispatch_sync(p, verb, args).await;
                }
                // To the front of its kind (the bookmarks bar), past entries
                // the page never sees.
                let title = entry["title"].as_str().unwrap_or("");
                let prefix = p.manifest.sync_prefixes.iter().find(|px| title.starts_with(px.as_str()));
                let first = all.iter().find(|e| prefix.is_some_and(|px| e["title"].as_str().is_some_and(|t| t.starts_with(px.as_str()))));
                match first.and_then(|e| e["id"].as_i64()) {
                    Some(target) if target != id => self.dispatch_sync(p, "move", &json!({ "id": id, "target": target, "after": false })).await,
                    _ => Ok(Json::Null),
                }
            }
            "status" => self.dispatch_sync(p, verb, args).await,
            _ => Err(format!("a web page can't use \"sync.{verb}\"")),
        }
    }

    fn storage_path(p: &Plugin) -> PathBuf {
        vault::plugin_data_dir().join(format!("{}.json", p.key()))
    }

    fn with_map<R>(
        &self,
        map: &RefCell<HashMap<String, Map<String, Json>>>,
        p: &Plugin,
        persist: bool,
        f: impl FnOnce(&Map<String, Json>) -> R,
    ) -> R {
        // Locked: kept data can't be read; nothing is cached, so it's read
        // once the vault is open.
        if persist && vault::get().data_key().is_none() {
            return f(&Map::new());
        }
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
        let key = vault::get().data_key();
        if persist && key.is_none() {
            return Err(vault::LOCKED.into());
        }
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
        if let (true, Some(key)) = (persist, key) {
            // Encrypted with the vault's key: which sites you never save
            // passwords for, which login you used last, ...
            let blob = crate::sync::encrypt(&key, &text)?;
            vault::write_private(&Self::storage_path(p), &blob).map_err(|e| e.to_string())?;
        }
        *m = updated;
        Ok(())
    }
}

/// Your own plugins folder.
pub fn user_plugin_dir() -> PathBuf {
    config::data_dir().join("plugins")
}

/// Directories searched for plugins, in priority order (first id wins).
/// Built-in plugins come first: a plugin of your own with the same id (one
/// called "passwords", say) doesn't replace one silently, only when you've
/// chosen to use it instead (`plugin_overrides`).
pub fn plugin_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = BUILT_IN.get() {
        dirs.push(dir.clone());
    }
    if let Ok(extra) = std::env::var("WISP_PLUGIN_PATH") {
        dirs.extend(std::env::split_paths(&extra));
    }
    #[cfg(not(target_os = "android"))]
    {
        dirs.push(PathBuf::from("/usr/share/wisp/plugins"));
        // Bundled plugins when running from the source tree.
        dirs.push(PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../plugins")));
    }
    dirs.push(user_plugin_dir());
    dirs
}

static BUILT_IN: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Where the host keeps the plugins it comes with (Android unpacks their
/// manifests from the app), searched first.
pub fn set_built_in_dir(dir: PathBuf) {
    let _ = BUILT_IN.set(dir);
}

/// The plugins found, and your own ones left out for having a built-in
/// plugin's id.
pub fn discover(cfg: &Config) -> (Vec<Rc<Plugin>>, Vec<(String, PathBuf)>) {
    let mut found: Vec<Rc<Plugin>> = Vec::new();
    let mut shadowed = Vec::new();
    let user_dir = user_plugin_dir();
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
            let enabled = cfg.plugins.get(&manifest.id).copied().unwrap_or(manifest.enabled_by_default);
            if let Some(i) = found.iter().position(|p| p.manifest.id == manifest.id) {
                if dir == user_dir && found[i].dir != path {
                    if cfg.plugin_overrides.contains(&manifest.id) {
                        found[i] = Rc::new(Plugin { manifest, dir: path, enabled: Cell::new(enabled) });
                    } else {
                        eprintln!("wisp: {} has the id of a built-in plugin; using the built-in one", path.display());
                        shadowed.push((manifest.id, path));
                    }
                }
                continue;
            }
            found.push(Rc::new(Plugin { manifest, dir: path, enabled: Cell::new(enabled) }));
        }
    }
    (found, shadowed)
}

/// How an allowed helper is remembered in `allowed_native_helpers`: the
/// plugin and the exact command, so a changed command asks again.
pub fn native_key(id: &str, command: &str) -> String {
    format!("{id}: {command}")
}

/// 128 random bits as hex: a key no page can guess.
pub fn page_key() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("no OS randomness");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// An address on this machine or the local network (a router, a NAS box):
/// such a site is told apart by its port too.
fn is_local(host: &str) -> bool {
    host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".test")
        || host.starts_with('[')
        || (host.split('.').count() == 4 && host.split('.').all(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())))
}

/// The host an http(s) address belongs to, as the Passwords plugin keys
/// logins (its `hostOf`): without "www.", or with its port for a local
/// address. None for other addresses.
/// Where a session value is kept: a web page's under its site (so another
/// site's page can't read it), the plugin's own pages' as named. A page Wisp
/// gave no key keeps nothing.
pub fn session_key(scope: Option<&PageScope>, key: &str) -> Result<String, String> {
    match scope {
        Some(PageScope::All) => Ok(key.to_owned()),
        Some(PageScope::Site { site, .. }) => Ok(format!("{site}\u{1}{key}")),
        None => Err("this page can't keep session values".into()),
    }
}

pub fn entry_host(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "https" && scheme != "http" {
        return None;
    }
    let (host, port) = crate::privacy::split_host_port(crate::privacy::authority(rest));
    let host = host.to_ascii_lowercase();
    if host.is_empty() || host.contains(char::is_whitespace) {
        return None;
    }
    let host = if host.contains(':') { format!("[{host}]") } else { host };
    if is_local(&host) {
        let default = if scheme == "https" { 443 } else { 80 };
        let port: Option<u32> = port.and_then(|p| p.parse().ok());
        return Some(match port {
            Some(port) if port != default => format!("{host}:{port}"),
            _ => host,
        });
    }
    Some(host.strip_prefix("www.").unwrap_or(&host).to_string())
}

/// The rough site a host is part of, as the Passwords plugin ties a
/// login's steps together (its `siteOf`): the last two labels, three for
/// names like example.co.uk; a local address is its own.
pub fn site_of(host: &str) -> String {
    let bare = host.rsplit_once(':').filter(|(_, port)| port.chars().all(|c| c.is_ascii_digit())).map_or(host, |(h, _)| h);
    if is_local(bare) {
        return host.to_string();
    }
    let labels: Vec<&str> = host.split('.').collect();
    let n = labels.len();
    let second = ["co", "com", "org", "net", "gov", "ac", "edu", "ne", "or"];
    let keep = if n >= 3 && second.contains(&labels[n - 2]) && labels[n - 1].len() == 2 && labels[n - 1].chars().all(|c| c.is_ascii_lowercase()) {
        3
    } else {
        2
    };
    labels[n.saturating_sub(keep)..].join(".")
}

/// A plugin's kept data, decrypted (or as it is, from before it was
/// encrypted; the next write encrypts it).
fn load_map(path: &Path) -> Map<String, Json> {
    let Some(text) = fs::read_to_string(path).ok() else { return Map::new() };
    let text = if vault::is_plain_json(&text) {
        text
    } else {
        match vault::get().data_key().and_then(|k| crate::sync::decrypt(&k, &text).ok()) {
            Some(t) => t,
            None => return Map::new(),
        }
    };
    serde_json::from_str(&text).unwrap_or_default()
}

pub fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect()
}

/// The most a downloaded filter list may weigh. The big ones (EasyList,
/// EasyPrivacy) are a few megabytes.
pub const MAX_LIST_BYTES: usize = 32 * 1024 * 1024;

/// Whether `more` bytes on top of `have` go past `MAX_LIST_BYTES`.
pub fn over_limit(have: usize, more: usize) -> bool {
    have.saturating_add(more) > MAX_LIST_BYTES
}

pub fn hash(s: &str) -> u64 {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_keeps_session_values_for_its_site_alone() {
        let a = PageScope::Site { host: "login.example.com".into(), site: "example.com".into() };
        let b = PageScope::Site { host: "evil.test".into(), site: "evil.test".into() };
        let ka = session_key(Some(&a), "pending:example.com").unwrap();
        assert_ne!(ka, session_key(Some(&b), "pending:example.com").unwrap());
        assert!(ka.starts_with(&session_key(Some(&a), "").unwrap()));
        assert_eq!(session_key(Some(&PageScope::All), "x").unwrap(), "x");
        assert!(session_key(None, "x").is_err());
    }

    #[test]
    fn a_login_s_host_is_the_passwords_plugin_s() {
        assert_eq!(entry_host("https://www.example.com/login").as_deref(), Some("example.com"));
        assert_eq!(entry_host("https://Accounts.Example.COM/").as_deref(), Some("accounts.example.com"));
        assert_eq!(entry_host("https://example.com:8443/").as_deref(), Some("example.com"), "ports only count locally");
        assert_eq!(entry_host("http://192.168.1.1:8080/admin").as_deref(), Some("192.168.1.1:8080"));
        assert_eq!(entry_host("http://192.168.1.1/").as_deref(), Some("192.168.1.1"));
        assert_eq!(entry_host("https://localhost:443/").as_deref(), Some("localhost"), "the usual port isn't written");
        assert_eq!(entry_host("http://[::1]:3000/").as_deref(), Some("[::1]:3000"));
        assert_eq!(entry_host("wisp-plugin://passwords/list.html"), None);
        assert_eq!(entry_host("about:blank"), None);
        assert_eq!(entry_host("not a url"), None);
    }

    #[test]
    fn a_site_is_the_plugin_s_rough_one() {
        assert_eq!(site_of("accounts.example.com"), "example.com");
        assert_eq!(site_of("example.com"), "example.com");
        assert_eq!(site_of("login.bbc.co.uk"), "bbc.co.uk");
        assert_eq!(site_of("co.uk"), "co.uk");
        assert_eq!(site_of("192.168.1.1:8080"), "192.168.1.1:8080");
        assert_eq!(site_of("app.localhost:3000"), "app.localhost:3000");
        assert_ne!(site_of("evil.com"), site_of("example.com"));
    }

    #[test]
    fn filter_lists_have_a_size_limit() {
        assert!(!over_limit(0, 64 * 1024));
        assert!(!over_limit(MAX_LIST_BYTES - 1, 1));
        assert!(over_limit(MAX_LIST_BYTES, 1));
        assert!(over_limit(usize::MAX, 1));
    }
}
