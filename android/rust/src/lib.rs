//! wisp-core for Wisp on Android.
//!
//! The core is single-threaded, as on the desktop (where it runs on GTK's
//! thread): here it gets a thread of its own, and every call from Kotlin is
//! handed to that thread and waited for. Its timers run there too; its HTTP
//! goes back to the app (`Host::http`), which sends it through GeckoView's
//! network stack, so it takes the same route as the pages (Orbot included).

use futures_channel::{mpsc, oneshot};
use futures_executor::{LocalPool, LocalSpawner};
use futures_util::StreamExt;
use futures_util::task::LocalSpawnExt;
use serde_json::Value as Json;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use wisp_core::config::{self, Config};
use wisp_core::plugins::{self, Api, PageScope, Plugin};
use wisp_core::rt::{self, LocalFuture, Response, Runtime};
use wisp_core::state::Store;
use wisp_core::sync::Sync;
use wisp_core::{address, privacy, vault};

uniffi::setup_scaffolding!();

#[derive(Debug, uniffi::Error)]
#[uniffi(flat_error)]
pub enum WispError {
    Failed(String),
}

impl std::fmt::Display for WispError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WispError::Failed(m) => f.write_str(m),
        }
    }
}

impl From<String> for WispError {
    fn from(m: String) -> Self {
        WispError::Failed(m)
    }
}

/// What the core asks of the app.
#[uniffi::export(with_foreign)]
pub trait Host: Send + std::marker::Sync {
    /// Make an HTTP request (body as JSON) and answer with `Core::http_done`
    /// and the same `id`. Must not wait for the answer itself.
    fn http(&self, id: u64, method: String, url: String, body: Option<String>);
}

/// `url` without tracking parameters, if it had any. A plain function, not a
/// call to the core's thread: the UI thread asks it for every page loaded,
/// and must never wait for the core (which may be busy syncing).
#[uniffi::export]
pub fn strip_tracking(url: String) -> Option<String> {
    privacy::strip_params(&url)
}

/// What the user typed, as an address to load (`search_url` from the settings).
#[uniffi::export]
pub fn resolve_address(text: String, search_url: String) -> String {
    address::resolve_input(&text, &search_url)
}

/// The sync service Wisp uses unless told otherwise.
#[uniffi::export]
pub fn default_sync_service() -> String {
    wisp_core::sync::DEFAULT_SERVICE.to_string()
}

#[derive(uniffi::Record)]
pub struct SyncAccount {
    pub service: String,
    pub id: String,
    pub password: String,
}

#[derive(uniffi::Record)]
pub struct VaultStatus {
    /// Set up (with a password or without).
    pub exists: bool,
    /// Locked with a password.
    pub has_password: bool,
    pub unlocked: bool,
    /// The file lost its password without Wisp removing it.
    pub downgraded: bool,
}

#[derive(uniffi::Record)]
pub struct Visit {
    pub url: String,
    pub title: String,
    pub visits: u32,
    pub last: i64,
}

#[derive(uniffi::Record)]
pub struct SavedTab {
    pub url: String,
    pub title: String,
}

#[derive(uniffi::Record)]
pub struct PluginAction {
    pub id: String,
    pub label: String,
    /// A page of the plugin's to open (`page` or `popup` in plugin.json).
    pub page: Option<String>,
}

#[derive(uniffi::Record)]
pub struct PluginMenuItem {
    pub id: String,
    pub label: String,
    pub contexts: Vec<String>,
}

#[derive(uniffi::Record)]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub enabled: bool,
    pub actions: Vec<PluginAction>,
    pub context_menu: Vec<PluginMenuItem>,
    pub permissions: Vec<String>,
    /// "dark" for a plugin that asks pages for their dark theme.
    pub color_scheme: Option<String>,
    /// Its actions sit in desktop Wisp's toolbar (else only in its ☰ menu).
    pub toolbar: bool,
}

/// What lives on the core's thread.
struct State {
    config: RefCell<Config>,
    store: Rc<Store>,
    api: Api,
    plugins: Vec<Rc<Plugin>>,
}

type Job = Box<dyn FnOnce() + Send>;

thread_local! {
    static STATE: RefCell<Option<Rc<State>>> = const { RefCell::new(None) };
    static SPAWNER: RefCell<Option<LocalSpawner>> = const { RefCell::new(None) };
    static PENDING: RefCell<HashMap<u64, oneshot::Sender<Result<Response, String>>>> = RefCell::new(HashMap::new());
    static NEXT_ID: Cell<u64> = const { Cell::new(1) };
}

fn state() -> Rc<State> {
    STATE.with(|s| s.borrow().clone()).expect("wisp core not started")
}

struct AndroidRuntime {
    host: Arc<dyn Host>,
}

impl Runtime for AndroidRuntime {
    fn spawn_local(&self, task: LocalFuture<()>) {
        SPAWNER.with(|s| {
            if let Some(s) = s.borrow().as_ref() {
                let _ = s.spawn_local(task);
            }
        });
    }

    fn sleep(&self, delay: Duration) -> LocalFuture<()> {
        let (tx, rx) = oneshot::channel::<()>();
        std::thread::spawn(move || {
            std::thread::sleep(delay);
            let _ = tx.send(());
        });
        Box::pin(async move {
            let _ = rx.await;
        })
    }

    fn http(&self, method: &str, url: &str, body: Option<String>) -> LocalFuture<Result<Response, String>> {
        let id = NEXT_ID.with(|n| n.replace(n.get() + 1));
        let (tx, rx) = oneshot::channel();
        PENDING.with(|p| p.borrow_mut().insert(id, tx));
        self.host.http(id, method.to_string(), url.to_string(), body);
        Box::pin(async move { rx.await.unwrap_or_else(|_| Err("the request was dropped".into())) })
    }
}

#[derive(uniffi::Object)]
pub struct Core {
    jobs: mpsc::UnboundedSender<Job>,
}

impl Core {
    /// Run `f` on the core's thread and wait for what it returns.
    fn run<T: Send + 'static>(&self, f: impl FnOnce(Rc<State>) -> T + Send + 'static) -> Result<T, WispError> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.jobs
            .unbounded_send(Box::new(move || {
                let _ = tx.send(f(state()));
            }))
            .map_err(|_| WispError::Failed("wisp core stopped".into()))?;
        rx.recv().map_err(|_| WispError::Failed("wisp core stopped".into()))
    }

    /// Run the future `f` makes on the core's thread and wait for its result.
    fn run_async<T, F>(&self, f: impl FnOnce(Rc<State>) -> F + Send + 'static) -> Result<T, WispError>
    where
        T: Send + 'static,
        F: Future<Output = T> + 'static,
    {
        let (tx, rx) = std::sync::mpsc::channel();
        self.jobs
            .unbounded_send(Box::new(move || {
                let fut = f(state());
                rt::spawn_local(async move {
                    let _ = tx.send(fut.await);
                });
            }))
            .map_err(|_| WispError::Failed("wisp core stopped".into()))?;
        rx.recv().map_err(|_| WispError::Failed("wisp core stopped".into()))
    }
}

#[uniffi::export]
impl Core {
    /// Start the core: its folders (the app's own), the folder holding the
    /// built-in plugins' manifests, and the app.
    #[uniffi::constructor]
    pub fn new(config_dir: String, data_dir: String, cache_dir: String, plugin_dir: String, host: Arc<dyn Host>) -> Arc<Self> {
        let (jobs, mut rx) = mpsc::unbounded::<Job>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("wisp-core".into())
            .spawn(move || {
                let mut pool = LocalPool::new();
                SPAWNER.with(|s| *s.borrow_mut() = Some(pool.spawner()));
                config::set_dirs(config::Dirs { config: config_dir.into(), data: data_dir.into(), cache: cache_dir.into() });
                plugins::set_built_in_dir(PathBuf::from(plugin_dir));
                rt::install(Box::new(AndroidRuntime { host }));
                let config = Config::load();
                let (plugins, _) = plugins::discover(&config);
                let sync = Sync::new();
                STATE.with(|s| {
                    *s.borrow_mut() =
                        Some(Rc::new(State { config: RefCell::new(config), store: Store::new(), api: Api::new(sync), plugins }))
                });
                let _ = ready_tx.send(());
                let spawner = pool.spawner();
                let _ = spawner.spawn_local(async move {
                    while let Some(job) = rx.next().await {
                        job();
                    }
                });
                pool.run();
            })
            .expect("cannot start the wisp core thread");
        let _ = ready_rx.recv();
        Arc::new(Core { jobs })
    }

    /// The answer to `Host::http` request `id`: the status and body, or what
    /// went wrong before there was one.
    pub fn http_done(&self, id: u64, status: u16, body: Vec<u8>, error: Option<String>) {
        let _ = self.jobs.unbounded_send(Box::new(move || {
            if let Some(tx) = PENDING.with(|p| p.borrow_mut().remove(&id)) {
                let _ = tx.send(match error {
                    Some(e) => Err(e),
                    None => Ok(Response { status, body }),
                });
            }
        }));
    }

    // ---- settings ----

    /// The settings, as config.json has them.
    pub fn config_json(&self) -> Result<String, WispError> {
        self.run(|s| serde_json::to_string(&*s.config.borrow()).unwrap_or_default())
    }

    /// Change the settings named in `changes` (a JSON object) and save them.
    pub fn set_config(&self, changes: String) -> Result<(), WispError> {
        self.run(move |s| {
            let changes: Json = serde_json::from_str(&changes).map_err(|e| e.to_string())?;
            let Json::Object(changes) = changes else { return Err("settings must be a JSON object".to_string()) };
            let mut current = serde_json::to_value(&*s.config.borrow()).map_err(|e| e.to_string())?;
            for (k, v) in changes {
                if current.get(&k).is_none() {
                    return Err(format!("unknown setting \"{k}\""));
                }
                current[k] = v;
            }
            let cfg: Config = serde_json::from_value(current).map_err(|e| e.to_string())?;
            cfg.save();
            *s.config.borrow_mut() = cfg;
            Ok(())
        })?
        .map_err(WispError::from)
    }

    // ---- addresses ----

    /// What the user typed, as an address to load.
    pub fn resolve_input(&self, text: String) -> Result<String, WispError> {
        self.run(move |s| address::resolve_input(&text, &s.config.borrow().search_url))
    }

    /// `url` without tracking parameters, if it had any and that's on.
    /// (HTTPS-only is GeckoView's own on Android, with the same exceptions.)
    pub fn clean_url(&self, url: String) -> Result<Option<String>, WispError> {
        self.run(move |s| if s.config.borrow().strip_tracking_params { privacy::strip_params(&url) } else { None })
    }

    pub fn is_app_link(&self, url: String) -> bool {
        address::is_app_link(&url)
    }

    pub fn site_of(&self, url: String) -> String {
        address::site_of(&url)
    }

    // ---- the vault ----

    pub fn vault_status(&self) -> Result<VaultStatus, WispError> {
        self.run(|_| {
            let v = vault::get();
            VaultStatus { exists: v.exists(), has_password: v.protected(), unlocked: v.is_open(), downgraded: v.downgraded() }
        })
    }

    /// Set the vault up, with a password or without. Takes a moment
    /// (Argon2id): call it off the UI thread.
    pub fn vault_create(&self, password: Option<String>) -> Result<(), WispError> {
        self.run_async(move |_| async move { vault::get().create(password).await })?.map_err(WispError::from)
    }

    pub fn vault_unlock(&self, password: String) -> Result<(), WispError> {
        self.run_async(move |_| async move { vault::get().unlock(&password).await })?.map_err(WispError::from)
    }

    pub fn vault_set_password(&self, password: Option<String>) -> Result<(), WispError> {
        self.run_async(move |_| async move { vault::get().set_password(password).await })?.map_err(WispError::from)
    }

    /// Lock a password-protected vault again (Wisp was in the background
    /// a while): history, plugin data and the synced bookmarks and logins
    /// leave memory with its key, until it's unlocked. True if it locked
    /// (false without a password: there's nothing to unlock with).
    pub fn vault_lock(&self) -> Result<bool, WispError> {
        self.run(|s| {
            let v = vault::get();
            if !v.protected() || !v.is_open() {
                return false;
            }
            s.store.forget();
            s.api.forget();
            v.lock();
            true
        })
    }

    pub fn vault_accept_downgrade(&self) -> Result<(), WispError> {
        self.run(|_| vault::get().accept_downgrade())
    }

    // ---- history and tabs ----

    pub fn record_visit(&self, url: String, title: String) -> Result<(), WispError> {
        self.run(move |s| {
            if s.config.borrow().save_history {
                s.store.record(&url, &title);
            }
        })
    }

    pub fn set_title(&self, url: String, title: String) -> Result<(), WispError> {
        self.run(move |s| {
            if s.config.borrow().save_history {
                s.store.set_title(&url, &title);
            }
        })
    }

    /// Newest first.
    pub fn history(&self) -> Result<Vec<Visit>, WispError> {
        self.run(|s| {
            s.store.history().into_iter().map(|v| Visit { url: v.url, title: v.title, visits: v.visits, last: v.last }).collect()
        })
    }

    pub fn clear_history(&self) -> Result<(), WispError> {
        self.run(|s| s.store.clear_history())
    }

    /// Write what's waiting to be saved (the app is going to the background).
    pub fn save_now(&self) -> Result<(), WispError> {
        self.run(|s| s.store.save_now())
    }

    /// The open tabs, kept (encrypted) if `restore_tabs` is on.
    pub fn save_tabs(&self, tabs: Vec<SavedTab>) -> Result<(), WispError> {
        self.run(move |s| {
            if s.config.borrow().restore_tabs {
                s.store.save_session(&[tabs.into_iter().map(|t| (t.url, t.title)).collect()]);
            } else {
                s.store.clear_session();
            }
        })
    }

    pub fn load_tabs(&self) -> Result<Vec<SavedTab>, WispError> {
        self.run(|s| {
            if !s.config.borrow().restore_tabs {
                return Vec::new();
            }
            s.store.load_session().into_iter().flatten().map(|(url, title)| SavedTab { url, title }).collect()
        })
    }

    // ---- plugins ----

    pub fn plugins(&self) -> Result<Vec<PluginInfo>, WispError> {
        self.run(|s| {
            s.plugins
                .iter()
                .map(|p| {
                    let m = &p.manifest;
                    PluginInfo {
                        id: m.id.clone(),
                        name: m.name.clone(),
                        description: m.description.clone(),
                        enabled: p.enabled.get(),
                        actions: m
                            .actions
                            .iter()
                            .map(|a| PluginAction { id: a.id.clone(), label: a.label.clone(), page: a.popup.clone().or(a.page.clone()) })
                            .collect(),
                        context_menu: m
                            .context_menu
                            .iter()
                            .map(|c| PluginMenuItem { id: c.id.clone(), label: c.label.clone(), contexts: c.contexts.clone() })
                            .collect(),
                        permissions: m.permissions.clone(),
                        color_scheme: m.color_scheme.clone(),
                        toolbar: m.toolbar,
                    }
                })
                .collect()
        })
    }

    pub fn set_plugin_enabled(&self, id: String, enabled: bool) -> Result<(), WispError> {
        self.run(move |s| {
            if let Some(p) = s.plugins.iter().find(|p| p.manifest.id == id) {
                p.enabled.set(enabled);
                let mut cfg = s.config.borrow_mut();
                cfg.plugins.insert(id, enabled);
                cfg.save();
            }
        })
    }

    pub fn plugin_has_permission(&self, id: String, permission: String) -> Result<bool, WispError> {
        self.run(move |s| s.plugins.iter().any(|p| p.manifest.id == id && p.enabled.get() && p.has_permission(&permission)))
    }

    /// A `wisp` API call from plugin `id`. `page_url` is the address of the
    /// page that asked, as GeckoView reports it (never what the page says);
    /// `own_page` is true for the plugin's own pages. Returns the result as
    /// JSON, or None for calls the app answers itself (tabs, clipboard, ...).
    pub fn plugin_call(&self, id: String, op: String, args: String, page_url: Option<String>, own_page: bool) -> Result<Option<String>, WispError> {
        self.run_async(move |s| async move {
            let Some(p) = s.plugins.iter().find(|p| p.manifest.id == id && p.enabled.get()).cloned() else {
                return Err(format!("no plugin '{id}' is enabled"));
            };
            let args: Json = serde_json::from_str(&args).unwrap_or(Json::Null);
            let scope = if own_page {
                Some(PageScope::All)
            } else {
                page_url.as_deref().and_then(plugins::entry_host).map(|host| PageScope::Site { site: plugins::site_of(&host), host })
            };
            match s.api.dispatch(&p, &op, &args, scope).await {
                Some(Ok(v)) => Ok(Some(v.to_string())),
                Some(Err(e)) => Err(e),
                None => Ok(None),
            }
        })?
        .map_err(WispError::from)
    }

    /// The sync login (kept in the vault); None while locked or not set up.
    pub fn sync_account(&self) -> Result<Option<SyncAccount>, WispError> {
        self.run(|s| s.api.sync.account().map(|a| SyncAccount { service: a.service, id: a.id, password: a.password }))
    }

    /// Use an existing sync ID and password, after checking the password
    /// opens what's stored there. Waits for the network: call it off the UI thread.
    pub fn sync_use_account(&self, service: String, id: String, password: String) -> Result<(), WispError> {
        self.run_async(move |s| async move {
            let account = wisp_core::sync::Account { service: service.trim().to_string(), id: id.trim().to_string(), password };
            s.api.sync.use_account(account).await
        })?
        .map_err(WispError::from)
    }

    /// A new sync ID with a random password on `service`.
    pub fn sync_create_account(&self, service: String) -> Result<SyncAccount, WispError> {
        self.run_async(move |s| async move { s.api.sync.create_account(service.trim()).await })?
            .map(|a| SyncAccount { service: a.service, id: a.id, password: a.password })
            .map_err(WispError::from)
    }

    /// Writes not yet uploaded to the sync service.
    pub fn sync_pending(&self) -> Result<u32, WispError> {
        self.run(|s| s.api.sync.pending() as u32)
    }

    /// Start loading bookmarks and logins from the sync service, so the
    /// first page that needs one doesn't wait.
    pub fn sync_warm_up(&self) -> Result<(), WispError> {
        self.run(|s| s.api.sync.warm_up())
    }
}
