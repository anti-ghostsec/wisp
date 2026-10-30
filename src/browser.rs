//! Browser windows and tabs.

use crate::config::Config;
use crate::downloads::{self, Downloads};
use crate::icons;
use crate::plugins::{self, PluginHost, PluginUi};
use crate::privacy;
use crate::state::{SavedTab, Store};
use crate::sync::{self, Account, Sync};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};
use webkit6::prelude::*;
use webkit6::{
    FindOptions, LoadEvent, NavigationPolicyDecision, NavigationType, NetworkSession, PolicyDecisionType, Settings,
    URISchemeRequest, UserContentManager, WebContext, WebView, WebViewSessionState,
    WebsiteDataTypes,
};

/// State shared by every window: one network session, one web context and
/// one content manager keep memory use flat no matter how many tabs are open.
pub struct Shared {
    pub app: gtk::Application,
    pub config: RefCell<Config>,
    settings: Settings,
    session: NetworkSession,
    context: WebContext,
    ucm: UserContentManager,
    pub plugins: Rc<PluginHost>,
    sync: Rc<Sync>,
    /// Sites the user allowed to open cross-site pop-ups this session.
    popup_allowed: RefCell<HashSet<String>>,
    /// Hosts the user explicitly allowed to load over plain HTTP this session.
    http_allowed: RefCell<HashSet<String>>,
    views: RefCell<Vec<glib::WeakRef<WebView>>>,
    downloads: Rc<Downloads>,
    /// Optional history and saved tabs.
    pub state: Rc<Store>,
    session_save_queued: Cell<bool>,
    /// Owns every open window; widget callbacks only hold weak references.
    windows: RefCell<Vec<Rc<BrowserWindow>>>,
}

struct UiBridge(Weak<Shared>);

impl PluginUi for UiBridge {
    fn open_tab(&self, url: &str) {
        if let Some(win) = self.0.upgrade().and_then(|s| s.last_window()) {
            win.new_tab(Some(url), None, false);
        }
    }

    fn notify(&self, title: &str, body: &str) {
        if let Some(shared) = self.0.upgrade() {
            let n = gio::Notification::new(title);
            n.set_body(Some(body));
            shared.app.send_notification(None, &n);
        }
    }

    fn plugins_changed(&self) {
        if let Some(shared) = self.0.upgrade() {
            shared.install_plugin_accels();
            shared.apply_color_scheme();
            for win in shared.windows.borrow().iter() {
                win.rebuild_plugin_bar();
            }
        }
    }
}

impl Shared {
    pub fn new(app: &gtk::Application, cfg: Config) -> Rc<Shared> {
        let session = privacy::network_session(&cfg);
        let context = privacy::web_context(&cfg);
        let settings = privacy::settings(&cfg);
        let ucm = UserContentManager::new();
        let sync = Sync::new();
        sync.warm_up();
        let plugins = PluginHost::new(&cfg, &ucm, sync.clone());
        let shared = Rc::new(Shared {
            app: app.clone(),
            config: RefCell::new(cfg),
            settings,
            session,
            context,
            ucm,
            plugins,
            sync,
            popup_allowed: RefCell::new(HashSet::new()),
            http_allowed: RefCell::new(HashSet::new()),
            views: RefCell::new(Vec::new()),
            downloads: Downloads::new(),
            state: Store::new(),
            session_save_queued: Cell::new(false),
            windows: RefCell::new(Vec::new()),
        });
        shared.watch_downloads();
        let weak = Rc::downgrade(&shared);
        shared.sync.connect_changed(move || {
            if let Some(shared) = weak.upgrade() {
                for win in shared.windows.borrow().iter() {
                    win.rebuild_bookmarks_bar();
                }
            }
        });
        shared.plugins.set_ui(Rc::new(UiBridge(Rc::downgrade(&shared))));
        shared.register_scheme();
        shared.install_accels();
        install_css();
        shared.install_plugin_accels();
        shared.apply_color_scheme();
        shared
    }

    /// Save the open tabs shortly (when restoring tabs is on).
    fn save_session_soon(self: &Rc<Self>) {
        if !self.config.borrow().restore_tabs || self.session_save_queued.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_millis(1500), move || {
            if let Some(shared) = weak.upgrade() {
                shared.save_session_now();
            }
        });
    }

    pub fn save_session_now(&self) {
        self.session_save_queued.set(false);
        if !self.config.borrow().restore_tabs {
            return;
        }
        let windows: Vec<Vec<SavedTab>> = self.windows.borrow().iter().map(|w| w.saved_tabs()).filter(|t| !t.is_empty()).collect();
        // Closing the last window keeps the tabs it had, to reopen next time.
        if !windows.is_empty() {
            self.state.save_session(&windows);
        }
    }

    /// Reopen the windows and tabs of the last session; returns the last
    /// window opened, or None when there's nothing to restore.
    pub fn restore_session(self: &Rc<Self>) -> Option<Rc<BrowserWindow>> {
        if !self.config.borrow().restore_tabs {
            return None;
        }
        let mut last = None;
        for tabs in self.state.load_session().iter().filter(|t| !t.is_empty()) {
            let win = BrowserWindow::new(self, &[tabs[0].0.clone()]);
            // The others load when you switch to them.
            for (uri, title) in &tabs[1..] {
                win.add_lazy_tab(uri, title);
            }
            last = Some(win);
        }
        last
    }

    /// Pages see `prefers-color-scheme: dark` (and the UI goes dark) while
    /// an enabled plugin asks for it, e.g. Dark Mode.
    fn apply_color_scheme(&self) {
        let dark = self.plugins.enabled().any(|p| p.manifest.color_scheme.as_deref() == Some("dark"));
        if let Some(settings) = gtk::Settings::default() {
            settings.set_gtk_application_prefer_dark_theme(dark);
        }
    }

    fn watch_downloads(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.session.connect_download_started(move |_, download| {
            let Some(shared) = weak.upgrade() else { return };
            // The window of the tab that started it, else the newest one.
            let view = download.web_view();
            let windows = shared.windows.borrow().clone();
            let win = windows
                .iter()
                .find(|w| view.as_ref().is_some_and(|v| w.tabs.borrow().iter().any(|t| t.view.borrow().as_ref() == Some(v))))
                .or(windows.last())
                .cloned();
            let (ask, dir) = {
                let cfg = shared.config.borrow();
                let dir = cfg.download_dir.as_ref().map(std::path::PathBuf::from).unwrap_or_else(downloads::default_dir);
                (cfg.ask_download_location, dir)
            };
            let parent = win.as_ref().map(|w| w.window.clone().upcast::<gtk::Window>());
            shared.downloads.add(download, parent, ask, dir);
            if let Some(win) = win {
                // Shown once the file has a name, so the save dialog comes first.
                let w = Rc::downgrade(&win);
                download.connect_created_destination(move |_, _| {
                    if let Some(win) = w.upgrade() {
                        win.downloads_button.popup();
                    }
                });
            }
        });
        let weak = Rc::downgrade(self);
        self.downloads.connect_changed(move || {
            if let Some(shared) = weak.upgrade() {
                for win in shared.windows.borrow().iter() {
                    win.refresh_downloads();
                }
            }
        });
    }

    fn register_scheme(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.context.register_uri_scheme("wisp", move |req: &URISchemeRequest| {
            let Some(shared) = weak.upgrade() else { return };
            let html = match req.path().as_deref() {
                Some("start") | Some("") | None => start_page(&shared.config.borrow()),
                Some("history") => {
                    // Only the history page itself may clear history.
                    let from_history = req.web_view().and_then(|v| v.uri()).is_some_and(|u| u.starts_with("wisp:history"));
                    if from_history && req.uri().is_some_and(|u| u.ends_with("?clear")) {
                        shared.state.clear_history();
                    }
                    history_page(&shared)
                }
                Some(other) => format!("<h1>Unknown page: wisp:{}</h1>", html_escape(other)),
            };
            let bytes = glib::Bytes::from_owned(html.into_bytes());
            let len = bytes.len() as i64;
            let stream = gio::MemoryInputStream::from_bytes(&bytes);
            req.finish(&stream, len, Some("text/html"));
        });
        let weak = Rc::downgrade(self);
        self.context.register_uri_scheme(plugins::PAGE_SCHEME, move |req: &URISchemeRequest| {
            let Some(shared) = weak.upgrade() else { return };
            // wisp-plugin://<plugin id>/<file>
            let uri = req.uri().map(|u| u.to_string()).unwrap_or_default();
            let rest = uri.split_once("://").map_or("", |(_, r)| r);
            let rest = rest.split(['?', '#']).next().unwrap_or("");
            let (id, path) = rest.split_once('/').unwrap_or((rest, ""));
            let path = if path.is_empty() { "index.html" } else { path };
            let file = shared.plugins.page_file(id, path);
            match file.as_ref().and_then(|f| std::fs::read(f).ok()) {
                Some(data) => {
                    let bytes = glib::Bytes::from_owned(data);
                    let stream = gio::MemoryInputStream::from_bytes(&bytes);
                    let mime = plugins::mime_for(file.as_deref().unwrap());
                    req.finish(&stream, bytes.len() as i64, Some(mime));
                }
                None => req.finish_error(&mut glib::Error::new(
                    gio::IOErrorEnum::NotFound,
                    "no such plugin page (is the plugin enabled?)",
                )),
            }
        });
        if let Some(sec) = self.context.security_manager() {
            sec.register_uri_scheme_as_secure("wisp");
            sec.register_uri_scheme_as_no_access("wisp");
            sec.register_uri_scheme_as_secure(plugins::PAGE_SCHEME);
            sec.register_uri_scheme_as_no_access(plugins::PAGE_SCHEME);
        }
    }

    fn install_accels(&self) {
        let accels: &[(&str, &[&str])] = &[
            ("win.new-tab", &["<Ctrl>t"]),
            ("win.new-window", &["<Ctrl>n"]),
            ("win.close-tab", &["<Ctrl>w"]),
            ("win.focus-url", &["<Ctrl>l", "F6", "<Alt>d"]),
            ("win.reload", &["<Ctrl>r", "F5"]),
            ("win.back", &["<Alt>Left"]),
            ("win.forward", &["<Alt>Right"]),
            ("win.find", &["<Ctrl>f"]),
            ("win.next-tab", &["<Ctrl>Tab", "<Ctrl>Page_Down"]),
            ("win.prev-tab", &["<Ctrl><Shift>Tab", "<Ctrl>Page_Up"]),
            ("win.zoom-in", &["<Ctrl>plus", "<Ctrl>equal"]),
            ("win.zoom-out", &["<Ctrl>minus"]),
            ("win.zoom-reset", &["<Ctrl>0"]),
            ("win.inspector", &["<Ctrl><Shift>i", "F12"]),
            ("win.quit", &["<Ctrl>q"]),
            ("win.fullscreen", &["F11"]),
            ("win.toggle-bookmarks-bar", &["<Ctrl><Shift>b"]),
            ("win.history", &["<Ctrl>h"]),
        ];
        for (action, keys) in accels {
            self.app.set_accels_for_action(action, keys);
        }
    }

    /// Keyboard shortcuts declared by plugin actions; cleared for disabled plugins.
    fn install_plugin_accels(&self) {
        for p in &self.plugins.plugins {
            for a in &p.manifest.actions {
                let Some(shortcut) = &a.shortcut else { continue };
                let name = plugin_action_name(&p.manifest.id, &a.id);
                let keys: &[&str] = if p.enabled.get() { &[shortcut.as_str()] } else { &[] };
                self.app.set_accels_for_action(&name, keys);
            }
        }
    }

    fn last_window(&self) -> Option<Rc<BrowserWindow>> {
        self.windows.borrow().last().cloned()
    }

    /// Any live view, used as `related-view` so new tabs share its web process.
    fn anchor_view(&self) -> Option<WebView> {
        let mut views = self.views.borrow_mut();
        views.retain(|w| w.upgrade().is_some());
        views.first().and_then(|w| w.upgrade())
    }

    fn rewrite(&self, uri: &str) -> Option<String> {
        let allowed = self.http_allowed.borrow();
        privacy::rewrite_url(uri, &self.config.borrow(), |h| allowed.contains(h))
    }

    /// Turn what the user typed into a URI.
    fn resolve_input(&self, text: &str) -> String {
        let t = text.trim();
        let has_scheme = t.contains("://")
            || ["about:", "wisp:", "data:", "file:", "view-source:"].iter().any(|s| t.starts_with(s));
        if has_scheme {
            return t.to_string();
        }
        if !t.contains(char::is_whitespace) {
            let host = t.split(['/', ':', '?', '#']).next().unwrap_or("");
            if host == "localhost" || host.parse::<std::net::Ipv4Addr>().is_ok() {
                return format!("http://{t}");
            }
            if host.contains('.') && !host.starts_with('.') && !host.ends_with('.') {
                return format!("https://{t}");
            }
        }
        let q = glib::Uri::escape_string(t, None, false);
        self.config.borrow().search_url.replace("%s", &q)
    }
}

pub struct Tab {
    page: gtk::Box,
    /// The tab's button in the tab strip.
    header: gtk::Box,
    label: gtk::Label,
    view: RefCell<Option<WebView>>,
    uri: RefCell<String>,
    title: RefCell<String>,
    state: RefCell<Option<WebViewSessionState>>,
    last_active: Cell<Instant>,
    /// Original http:// URL when we upgraded the current load to https://.
    upgraded_from: RefCell<Option<String>>,
}

pub struct BrowserWindow {
    shared: Rc<Shared>,
    window: gtk::ApplicationWindow,
    notebook: gtk::Notebook,
    /// Our own tab strip (the notebook's is hidden), so "+" can sit right
    /// after the last tab.
    strip: gtk::Box,
    strip_scroll: gtk::ScrolledWindow,
    /// Tab being dragged to a new position.
    dragging: RefCell<Option<Weak<Tab>>>,
    entry: gtk::Entry,
    back: gtk::Button,
    forward: gtk::Button,
    find_bar: gtk::SearchBar,
    find_entry: gtk::SearchEntry,
    /// Buttons for actions of enabled plugins.
    plugin_bar: gtk::Box,
    downloads_button: gtk::MenuButton,
    downloads_icon: gtk::DrawingArea,
    downloads_popover: gtk::Popover,
    /// Toolbar and tab strip, hidden in fullscreen.
    chrome: Vec<gtk::Widget>,
    bookmarks_bar: gtk::Box,
    /// The bar plus its » menu; what gets shown and hidden.
    bookmarks_row: gtk::Box,
    bookmarks_scroll: gtk::ScrolledWindow,
    /// (button, name, url) of each bookmark on the bar, for the » menu.
    bookmark_items: RefCell<Vec<(gtk::Button, String, String)>>,
    popup_bar: gtk::Revealer,
    popup_label: gtk::Label,
    /// (url, opener site) of the last blocked pop-up.
    blocked_popup: RefCell<Option<(String, String)>>,
    tabs: RefCell<Vec<Rc<Tab>>>,
}

impl BrowserWindow {
    pub fn new(shared: &Rc<Shared>, uris: &[String]) -> Rc<BrowserWindow> {
        let window = gtk::ApplicationWindow::builder()
            .application(&shared.app)
            .default_width(1100)
            .default_height(750)
            .title("Wisp")
            .build();

        // A plain toolbar instead of a GtkHeaderBar: the window manager keeps
        // its own title bar, so there is only one set of window controls.
        let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        toolbar.set_margin_top(4);
        toolbar.set_margin_bottom(4);
        toolbar.set_margin_start(4);
        toolbar.set_margin_end(4);
        let back = icon_button("go-previous-symbolic", "Back (Alt+Left)", "win.back");
        let forward = icon_button("go-next-symbolic", "Forward (Alt+Right)", "win.forward");
        let reload = icon_button("view-refresh-symbolic", "Reload (Ctrl+R)", "win.reload");
        toolbar.append(&back);
        toolbar.append(&forward);
        toolbar.append(&reload);

        let entry = gtk::Entry::builder()
            .hexpand(true)
            .placeholder_text("Search or enter address")
            .input_purpose(gtk::InputPurpose::Url)
            .build();
        toolbar.append(&entry);
        // Right of the address bar, as in Firefox; shown once something has
        // been downloaded.
        let downloads_popover = gtk::Popover::new();
        let downloads_icon = shared.downloads.icon();
        let downloads_button = gtk::MenuButton::builder()
            .child(&downloads_icon)
            .tooltip_text("Downloads")
            .popover(&downloads_popover)
            .visible(!shared.downloads.is_empty())
            .build();
        toolbar.append(&downloads_button);
        let plugin_bar = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        toolbar.append(&plugin_bar);

        let menu = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .tooltip_text("Plugins and settings")
            .build();
        toolbar.append(&menu);

        let find_entry = gtk::SearchEntry::builder().placeholder_text("Find in page").build();
        let find_bar = gtk::SearchBar::builder().child(&find_entry).show_close_button(true).build();
        find_bar.connect_entry(&find_entry);

        let notebook = gtk::Notebook::builder()
            .show_border(false)
            .show_tabs(false)
            .vexpand(true)
            .build();
        // The tab strip is always visible so it never appears out of nowhere.
        let strip = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let strip_scroll = gtk::ScrolledWindow::builder()
            .child(&strip)
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_width(true)
            .build();
        // The mouse wheel scrolls through tabs that don't fit.
        let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        let adj = strip_scroll.hadjustment();
        wheel.connect_scroll(move |_, dx, dy| {
            adj.set_value(adj.value() + (dx + dy) * 40.0);
            glib::Propagation::Stop
        });
        strip_scroll.add_controller(wheel);
        let new_tab = gtk::Button::builder()
            .child(&icons::from_svg(icons::NEW_TAB).unwrap())
            .tooltip_text("New tab (Ctrl+T)")
            .action_name("win.new-tab")
            .css_classes(["flat", "wisp-new-tab"])
            .valign(gtk::Align::Center)
            .build();
        let tab_bar = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        tab_bar.add_css_class("wisp-tabs");
        tab_bar.append(&strip_scroll);
        tab_bar.append(&new_tab);
        // Double-click on the empty part of the bar opens a tab, as in most browsers.
        let blank = gtk::Box::builder().hexpand(true).build();
        let double = gtk::GestureClick::builder().button(1).build();
        double.connect_pressed(|g, n, _, _| {
            if n == 2 {
                let _ = g.widget().and_then(|w| w.activate_action("win.new-tab", None).ok());
            }
        });
        blank.add_controller(double);
        tab_bar.append(&blank);

        let popup_label = gtk::Label::builder().xalign(0.0).hexpand(true).ellipsize(gtk::pango::EllipsizeMode::End).build();
        let popup_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        popup_box.set_margin_start(8);
        popup_box.set_margin_end(4);
        popup_box.set_margin_top(2);
        popup_box.set_margin_bottom(2);
        popup_box.append(&gtk::Image::from_icon_name("dialog-warning-symbolic"));
        popup_box.append(&popup_label);
        for (label, action) in [("Open", "win.popup-open"), ("Always allow on this site", "win.popup-allow")] {
            popup_box.append(&gtk::Button::builder().label(label).action_name(action).build());
        }
        let dismiss = icon_button("window-close-symbolic", "Dismiss", "win.popup-dismiss");
        dismiss.add_css_class("flat");
        popup_box.append(&dismiss);
        let popup_bar = gtk::Revealer::builder().child(&popup_box).reveal_child(false).build();

        let bookmarks_bar = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        bookmarks_bar.add_css_class("wisp-bookmarks");
        let bookmarks_scroll = gtk::ScrolledWindow::builder()
            .child(&bookmarks_bar)
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .hexpand(true)
            .build();
        // Bookmarks keep their natural width; what doesn't fit scrolls (mouse
        // wheel) and is listed under », as in Firefox.
        if let Some(viewport) = bookmarks_scroll.child().and_downcast::<gtk::Viewport>() {
            viewport.set_hscroll_policy(gtk::ScrollablePolicy::Natural);
        }
        let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        let adj = bookmarks_scroll.hadjustment();
        wheel.connect_scroll(move |_, dx, dy| {
            adj.set_value(adj.value() + (dx + dy) * 40.0);
            glib::Propagation::Stop
        });
        bookmarks_scroll.add_controller(wheel);
        let overflow_popover = gtk::Popover::new();
        let overflow = gtk::MenuButton::builder()
            .child(&gtk::Label::new(Some("»")))
            .tooltip_text("More bookmarks")
            .has_frame(false)
            .popover(&overflow_popover)
            .visible(false)
            .build();
        let adj = bookmarks_scroll.hadjustment();
        let o = overflow.clone();
        adj.connect_changed(move |a| o.set_visible(a.upper() > a.page_size() + 1.0));
        let bookmarks_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .visible(shared.config.borrow().show_bookmarks_bar)
            .build();
        bookmarks_row.append(&bookmarks_scroll);
        bookmarks_row.append(&overflow);

        let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let separator = gtk::Separator::new(gtk::Orientation::Horizontal);
        let chrome = vec![toolbar.clone().upcast(), separator.clone().upcast(), tab_bar.clone().upcast()];
        vbox.append(&toolbar);
        vbox.append(&bookmarks_row);
        vbox.append(&separator);
        vbox.append(&popup_bar);
        vbox.append(&find_bar);
        vbox.append(&tab_bar);
        vbox.append(&notebook);
        window.set_child(Some(&vbox));

        let win = Rc::new(BrowserWindow {
            shared: shared.clone(),
            window,
            notebook,
            strip,
            strip_scroll,
            dragging: RefCell::new(None),
            entry,
            back,
            forward,
            find_bar,
            find_entry,
            plugin_bar,
            downloads_button,
            downloads_icon,
            downloads_popover,
            chrome,
            bookmarks_bar,
            bookmarks_row,
            bookmarks_scroll,
            bookmark_items: RefCell::new(Vec::new()),
            popup_bar,
            popup_label,
            blocked_popup: RefCell::new(None),
            tabs: RefCell::new(Vec::new()),
        });
        win.rebuild_plugin_bar();
        win.refresh_downloads();
        win.rebuild_bookmarks_bar();
        let w = Rc::downgrade(&win);
        overflow_popover.connect_show(move |p| {
            if let Some(win) = w.upgrade() {
                p.set_child(Some(&win.hidden_bookmarks_list(p)));
            }
        });
        shared.windows.borrow_mut().push(win.clone());
        let w = Rc::downgrade(&win);
        win.window.connect_close_request(move |_| {
            if let Some(win) = w.upgrade() {
                win.shared.windows.borrow_mut().retain(|x| !Rc::ptr_eq(x, &win));
                win.shared.save_session_soon();
            }
            glib::Propagation::Proceed
        });
        menu.set_popover(Some(&win.build_menu()));
        win.connect_signals();
        win.install_actions();
        win.start_suspend_timer();

        if uris.is_empty() {
            win.new_tab(None, None, true);
        } else {
            for u in uris {
                win.new_tab(Some(&shared.resolve_input(u)), None, true);
            }
            win.notebook.set_current_page(Some(0));
        }
        win.window.present();
        win
    }

    fn connect_signals(self: &Rc<Self>) {
        // Fullscreen (F11, the window button, or a video) shows only the page.
        let w = Rc::downgrade(self);
        self.window.connect_fullscreened_notify(move |window| {
            if let Some(win) = w.upgrade() {
                for widget in &win.chrome {
                    widget.set_visible(!window.is_fullscreen());
                }
                let bar = win.shared.config.borrow().show_bookmarks_bar;
                win.bookmarks_row.set_visible(bar && !window.is_fullscreen());
            }
        });
        let w = Rc::downgrade(self);
        self.downloads_popover.connect_show(move |_| {
            if let Some(win) = w.upgrade() {
                win.shared.downloads.seen();
                win.refresh_downloads();
            }
        });
        let w = Rc::downgrade(self);
        self.entry.connect_activate(move |e| {
            let Some(win) = w.upgrade() else { return };
            let uri = win.shared.resolve_input(&e.text());
            if let Some(tab) = win.current_tab() {
                win.navigate(&tab, &uri);
                win.view_of(&tab).grab_focus();
            }
        });

        let w = Rc::downgrade(self);
        self.notebook.connect_switch_page(move |_, page, _| {
            let Some(win) = w.upgrade() else { return };
            if let Some(tab) = win.tab_for_page(page) {
                win.highlight_tab(&tab);
                tab.last_active.set(Instant::now());
                // Restores the tab if it was suspended.
                win.view_of(&tab);
                win.sync_chrome(&tab);
            }
        });

        let w = Rc::downgrade(self);
        self.find_entry.connect_search_changed(move |e| {
            if let Some(fc) = w.upgrade().and_then(|win| win.current_view()).and_then(|v| v.find_controller()) {
                let text = e.text();
                if text.is_empty() {
                    fc.search_finish();
                } else {
                    let opts = FindOptions::CASE_INSENSITIVE | FindOptions::WRAP_AROUND;
                    fc.search(&text, opts.bits(), u32::MAX);
                }
            }
        });
        let w = Rc::downgrade(self);
        self.find_entry.connect_activate(move |_| {
            if let Some(fc) = w.upgrade().and_then(|win| win.current_view()).and_then(|v| v.find_controller()) {
                fc.search_next();
            }
        });
        let w = Rc::downgrade(self);
        self.find_bar.connect_search_mode_enabled_notify(move |bar| {
            if !bar.is_search_mode() {
                if let Some(fc) = w.upgrade().and_then(|win| win.current_view()).and_then(|v| v.find_controller()) {
                    fc.search_finish();
                }
            }
        });
    }

    fn install_actions(self: &Rc<Self>) {
        let add = |name: &str, f: Box<dyn Fn(&Rc<BrowserWindow>)>| {
            let action = gio::SimpleAction::new(name, None);
            let w = Rc::downgrade(self);
            action.connect_activate(move |_, _| {
                if let Some(win) = w.upgrade() {
                    f(&win);
                }
            });
            self.window.add_action(&action);
        };
        add("new-tab", Box::new(|w| {
            w.new_tab(None, None, true);
            w.entry.grab_focus();
        }));
        // Closes every window, including ones on other workspaces.
        add("quit", Box::new(|w| w.shared.app.quit()));
        add("new-window", Box::new(|w| {
            BrowserWindow::new(&w.shared, &[]);
        }));
        add("close-tab", Box::new(|w| {
            if let Some(tab) = w.current_tab() {
                w.close_tab(&tab);
            }
        }));
        add("focus-url", Box::new(|w| {
            w.entry.grab_focus();
            w.entry.select_region(0, -1);
        }));
        add("reload", Box::new(|w| {
            if let Some(v) = w.current_view() {
                v.reload();
            }
        }));
        add("back", Box::new(|w| {
            if let Some(v) = w.current_view() {
                v.go_back();
            }
        }));
        add("forward", Box::new(|w| {
            if let Some(v) = w.current_view() {
                v.go_forward();
            }
        }));
        add("find", Box::new(|w| {
            w.find_bar.set_search_mode(true);
            w.find_entry.grab_focus();
        }));
        add("next-tab", Box::new(|w| w.cycle_tab(1)));
        add("prev-tab", Box::new(|w| w.cycle_tab(-1)));
        add("zoom-in", Box::new(|w| w.zoom(Some(1.1))));
        add("zoom-out", Box::new(|w| w.zoom(Some(1.0 / 1.1))));
        add("zoom-reset", Box::new(|w| w.zoom(None)));
        add("inspector", Box::new(|w| {
            if let Some(i) = w.current_view().and_then(|v| v.inspector()) {
                i.show();
            }
        }));
        add("sync-account", Box::new(|w| w.show_sync_dialog()));
        add("toggle-bookmarks-bar", Box::new(|w| {
            let show = {
                let mut cfg = w.shared.config.borrow_mut();
                cfg.show_bookmarks_bar = !cfg.show_bookmarks_bar;
                cfg.save();
                cfg.show_bookmarks_bar
            };
            for win in w.shared.windows.borrow().iter() {
                win.bookmarks_row.set_visible(show && !win.window.is_fullscreen());
            }
        }));
        add("history", Box::new(|w| {
            w.new_tab(Some("wisp:history"), None, true);
        }));
        add("fullscreen", Box::new(|w| {
            if w.window.is_fullscreen() {
                w.window.unfullscreen();
            } else {
                w.window.fullscreen();
            }
        }));
        add("popup-open", Box::new(|w| {
            if let Some((url, _)) = w.blocked_popup.borrow_mut().take() {
                if !url.is_empty() {
                    w.new_tab(Some(&url), None, true);
                }
            }
            w.popup_bar.set_reveal_child(false);
        }));
        add("popup-allow", Box::new(|w| {
            if let Some((url, site)) = w.blocked_popup.borrow_mut().take() {
                w.shared.popup_allowed.borrow_mut().insert(site);
                if !url.is_empty() {
                    w.new_tab(Some(&url), None, true);
                }
            }
            w.popup_bar.set_reveal_child(false);
        }));
        add("popup-dismiss", Box::new(|w| {
            w.blocked_popup.borrow_mut().take();
            w.popup_bar.set_reveal_child(false);
        }));

        // "plugin-id/action-id"
        let action = gio::SimpleAction::new("plugin-action", Some(glib::VariantTy::STRING));
        let w = Rc::downgrade(self);
        action.connect_activate(move |_, param| {
            let (Some(win), Some(target)) = (w.upgrade(), param.and_then(|p| p.str())) else { return };
            if let Some((plugin, action)) = target.split_once('/') {
                win.run_plugin_action(plugin, action);
            }
        });
        self.window.add_action(&action);
    }

    fn run_plugin_action(self: &Rc<Self>, plugin_id: &str, action_id: &str) {
        let Some(p) = self.shared.plugins.get(plugin_id).filter(|p| p.enabled.get()) else { return };
        let Some(action) = p.manifest.actions.iter().find(|a| a.id == action_id) else { return };
        if let Some(page) = &action.page {
            self.new_tab(Some(&p.page_url(page)), None, true);
            return;
        }
        let Some(tab) = self.current_tab() else { return };
        let view = self.view_of(&tab);
        let data = serde_json::json!({
            "id": action.id,
            "url": view.uri().map(|u| u.to_string()),
            "title": view.title().map(|t| t.to_string()),
        });
        let js = format!("globalThis.wisp && wisp._emit('action', {data});");
        view.evaluate_javascript(&js, Some(&p.world()), None, None::<&gio::Cancellable>, |_| {});
    }

    /// Show buttons for the actions of every enabled plugin.
    fn rebuild_plugin_bar(&self) {
        while let Some(child) = self.plugin_bar.first_child() {
            self.plugin_bar.remove(&child);
        }
        for p in self.shared.plugins.enabled() {
            for a in &p.manifest.actions {
                let tooltip = match &a.shortcut {
                    Some(s) => format!("{} ({})", a.label, shortcut_label(s)),
                    None => a.label.clone(),
                };
                let button = gtk::Button::builder().tooltip_text(&tooltip).build();
                match &a.icon {
                    // An SVG shipped with the plugin, drawn in the theme's text color.
                    Some(icon) if icon.ends_with(".svg") => {
                        match p.file(icon).and_then(|f| std::fs::read_to_string(f).ok()).and_then(|svg| icons::from_svg(&svg)) {
                            Some(image) => button.set_child(Some(&image)),
                            None => button.set_label(&a.label),
                        }
                    }
                    Some(icon) => button.set_icon_name(icon),
                    None => button.set_label(&a.label),
                }
                button.set_action_name(Some("win.plugin-action"));
                button.set_action_target_value(Some(&format!("{}/{}", p.manifest.id, a.id).to_variant()));
                self.plugin_bar.append(&button);
            }
        }
    }

    fn show_blocked_popup(&self, url: &str, opener_site: &str) {
        let dest = privacy::scheme_and_host(url).map(|(_, h)| h.to_string()).unwrap_or_else(|| "a blank page".into());
        self.popup_label.set_text(&format!("Blocked a pop-up from {opener_site} to {dest}"));
        *self.blocked_popup.borrow_mut() = Some((url.to_string(), opener_site.to_string()));
        self.popup_bar.set_reveal_child(true);
    }

    fn show_sync_dialog(self: &Rc<Self>) {
        let dialog = gtk::Window::builder()
            .transient_for(&self.window)
            .modal(true)
            .title("Sync account")
            .default_width(480)
            .resizable(false)
            .build();
        let vbox = gtk::Box::new(gtk::Orientation::Vertical, 10);
        for m in ["top", "bottom", "start", "end"] {
            match m {
                "top" => vbox.set_margin_top(16),
                "bottom" => vbox.set_margin_bottom(16),
                "start" => vbox.set_margin_start(16),
                _ => vbox.set_margin_end(16),
            }
        }
        let intro = gtk::Label::builder()
            .label("Bookmarks and passwords are encrypted on this device and synced with xBrowserSync. \
                    Enter the same sync ID and password on another device to share them.")
            .wrap(true)
            .xalign(0.0)
            .max_width_chars(56)
            .build();
        vbox.append(&intro);

        let grid = gtk::Grid::builder().row_spacing(6).column_spacing(10).build();
        let service = gtk::Entry::builder().hexpand(true).build();
        let id = gtk::Entry::builder().hexpand(true).build();
        let password = gtk::PasswordEntry::builder().show_peek_icon(true).hexpand(true).build();
        for (row, (label, widget)) in [
            ("Service", service.upcast_ref::<gtk::Widget>()),
            ("Sync ID", id.upcast_ref()),
            ("Password", password.upcast_ref()),
        ]
        .into_iter()
        .enumerate()
        {
            grid.attach(&gtk::Label::builder().label(label).xalign(1.0).build(), 0, row as i32, 1, 1);
            grid.attach(widget, 1, row as i32, 1, 1);
        }
        vbox.append(&grid);

        let status = gtk::Label::builder().wrap(true).xalign(0.0).max_width_chars(56).build();
        status.add_css_class("dim-label");
        match self.shared.sync.account() {
            Some(a) => {
                service.set_text(&a.service);
                id.set_text(&a.id);
                password.set_text(&a.password);
                status.set_text("Connected.");
            }
            None => {
                service.set_text(sync::DEFAULT_SERVICE);
                status.set_text(
                    "Not set up yet. An account is created for you automatically the first time \
                     you save a bookmark or password, or enter your own details above.",
                );
            }
        }
        vbox.append(&status);

        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        buttons.set_halign(gtk::Align::End);
        let create = gtk::Button::with_label("Create new account");
        let use_it = gtk::Button::with_label("Use this account");
        use_it.add_css_class("suggested-action");
        let close = gtk::Button::with_label("Close");
        buttons.append(&create);
        buttons.append(&use_it);
        buttons.append(&close);
        vbox.append(&buttons);
        dialog.set_child(Some(&vbox));

        let d = dialog.clone();
        close.connect_clicked(move |_| d.close());

        let busy = {
            let (create, use_it) = (create.clone(), use_it.clone());
            move |on: bool| {
                create.set_sensitive(!on);
                use_it.set_sensitive(!on);
            }
        };
        let (sync_, st, svc, idf, pw, b) =
            (self.shared.sync.clone(), status.clone(), service.clone(), id.clone(), password.clone(), busy.clone());
        create.connect_clicked(move |_| {
            let (sync_, st, svc, idf, pw, b) = (sync_.clone(), st.clone(), svc.clone(), idf.clone(), pw.clone(), b.clone());
            b(true);
            st.set_text("Creating account…");
            glib::spawn_future_local(async move {
                match sync_.create_account(&svc.text()).await {
                    Ok(a) => {
                        idf.set_text(&a.id);
                        pw.set_text(&a.password);
                        st.set_text("New account created. Keep the ID and password to sync other devices.");
                    }
                    Err(e) => st.set_text(&format!("Could not create account: {e}")),
                }
                b(false);
            });
        });
        let (sync_, st, b) = (self.shared.sync.clone(), status.clone(), busy);
        use_it.connect_clicked(move |_| {
            let account = Account {
                service: service.text().trim().to_string(),
                id: id.text().trim().to_string(),
                password: password.text().to_string(),
            };
            if account.id.is_empty() || account.password.is_empty() {
                st.set_text("Enter a sync ID and password.");
                return;
            }
            let (sync_, st, b) = (sync_.clone(), st.clone(), b.clone());
            b(true);
            st.set_text("Checking…");
            glib::spawn_future_local(async move {
                match sync_.use_account(account).await {
                    Ok(()) => st.set_text("Connected. Bookmarks and passwords now sync with this account."),
                    Err(e) => st.set_text(&format!("Could not use this account: {e}")),
                }
                b(false);
            });
        });
        dialog.present();
    }

    fn build_menu(self: &Rc<Self>) -> gtk::Popover {
        let shared = &self.shared;
        let vbox = gtk::Box::new(gtk::Orientation::Vertical, 6);
        vbox.set_margin_top(6);
        vbox.set_margin_bottom(6);
        vbox.set_margin_start(6);
        vbox.set_margin_end(6);

        let mode_label = gtk::Label::builder().wrap(true).max_width_chars(40).css_classes(["dim-label"]).build();
        let update_mode = {
            let (label, shared) = (mode_label.clone(), shared.clone());
            move || {
                let cfg = shared.config.borrow();
                let kept: Vec<&str> = [(cfg.save_history, "history"), (cfg.restore_tabs, "tabs")]
                    .into_iter()
                    .filter_map(|(on, what)| on.then_some(what))
                    .collect();
                label.set_text(&match (cfg.persistent_profile, kept.is_empty()) {
                    (true, _) => "Persistent profile: cookies and site data are kept".to_string(),
                    (false, true) => "Private session: nothing from browsing is written to disk".to_string(),
                    (false, false) => format!("Private session; {} kept, encrypted", kept.join(" and ")),
                });
            }
        };
        update_mode();
        vbox.append(&mode_label);

        // Each toggle is saved at once; turning history or tabs off deletes what was kept.
        let toggle = |label: &str, active: bool, set: fn(&Rc<Shared>, bool)| {
            let check = gtk::CheckButton::builder().label(label).active(active).build();
            let (shared, update_mode) = (shared.clone(), update_mode.clone());
            check.connect_toggled(move |c| {
                set(&shared, c.is_active());
                shared.config.borrow().save();
                update_mode();
            });
            check
        };
        let cfg = shared.config.borrow().clone();
        vbox.append(&toggle("Show bookmarks bar", cfg.show_bookmarks_bar, |shared, on| {
            shared.config.borrow_mut().show_bookmarks_bar = on;
            for win in shared.windows.borrow().iter() {
                win.bookmarks_row.set_visible(on && !win.window.is_fullscreen());
            }
        }));
        vbox.append(&toggle("Remember history", cfg.save_history, |shared, on| {
            shared.config.borrow_mut().save_history = on;
            if !on {
                shared.state.clear_history();
            }
        }));
        vbox.append(&toggle("Reopen tabs on start", cfg.restore_tabs, |shared, on| {
            shared.config.borrow_mut().restore_tabs = on;
            if on {
                shared.save_session_soon();
            } else {
                shared.state.clear_session();
            }
        }));
        vbox.append(&gtk::Button::builder().label("History (Ctrl+H)").action_name("win.history").build());
        vbox.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let heading = gtk::Label::builder().label("<b>Plugins</b>").use_markup(true).xalign(0.0).build();
        vbox.append(&heading);
        if shared.plugins.plugins.is_empty() {
            vbox.append(&gtk::Label::new(Some("No plugins installed")));
        }
        for p in &shared.plugins.plugins {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            let text = format!("{} {}", p.manifest.name, p.manifest.version);
            let name = gtk::Label::builder().label(text.trim()).xalign(0.0).hexpand(true).build();
            name.set_tooltip_text(Some(&p.manifest.description));
            let switch = gtk::Switch::builder().active(p.enabled.get()).valign(gtk::Align::Center).build();
            let w = Rc::downgrade(self);
            let id = p.manifest.id.clone();
            switch.connect_state_set(move |_, on| {
                if let Some(win) = w.upgrade() {
                    let mut cfg = win.shared.config.borrow_mut();
                    win.shared.plugins.set_enabled(&id, on, &mut cfg);
                    drop(cfg);
                    if let Some(v) = win.current_view() {
                        v.reload();
                    }
                }
                glib::Propagation::Proceed
            });
            row.append(&name);
            row.append(&switch);
            vbox.append(&row);
        }

        let install = gtk::Button::with_label("Install plugin…");
        let w = Rc::downgrade(self);
        install.connect_clicked(move |_| {
            if let Some(win) = w.upgrade() {
                win.install_plugin();
            }
        });
        vbox.append(&install);
        let open_plugins = gtk::Button::with_label("Open plugin folder");
        let w = Rc::downgrade(self);
        open_plugins.connect_clicked(move |_| {
            let dir = plugins::plugin_dirs().remove(0);
            let _ = std::fs::create_dir_all(&dir);
            if let Some(win) = w.upgrade() {
                win.open_path(&dir);
            }
        });
        vbox.append(&open_plugins);
        vbox.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let sync_button = gtk::Button::builder().label("Sync account…").action_name("win.sync-account").build();
        vbox.append(&sync_button);

        let edit = gtk::Button::with_label("Settings…");
        let w = Rc::downgrade(self);
        edit.connect_clicked(move |_| {
            if let Some(win) = w.upgrade() {
                win.show_settings_editor();
            }
        });
        vbox.append(&edit);

        let clear = gtk::Button::with_label("Clear cookies, site data and history");
        let session = shared.session.clone();
        let state = shared.state.clone();
        clear.connect_clicked(move |b| {
            state.clear_history();
            state.clear_session();
            if let Some(mgr) = session.website_data_manager() {
                b.set_sensitive(false);
                let b = glib::SendWeakRef::from(b.downgrade());
                mgr.clear(WebsiteDataTypes::ALL, glib::TimeSpan::from_seconds(0), None::<&gio::Cancellable>, move |_| {
                    if let Some(b) = b.upgrade() {
                        b.set_sensitive(true);
                    }
                });
            }
        });
        vbox.append(&clear);
        vbox.append(&gtk::Button::builder().label("Quit Wisp (Ctrl+Q)").action_name("win.quit").build());

        let popover = gtk::Popover::new();
        popover.set_child(Some(&vbox));
        popover
    }

    // ---- tabs ----

    pub fn new_tab(self: &Rc<Self>, uri: Option<&str>, related: Option<&WebView>, focus: bool) -> WebView {
        self.open_tab(uri, related, focus, None).expect("a tab that isn't lazy has a view")
    }

    /// A restored tab at the end of the strip that loads when first shown.
    pub fn add_lazy_tab(self: &Rc<Self>, uri: &str, title: &str) {
        self.open_tab(Some(uri), None, false, Some(title));
    }

    fn open_tab(self: &Rc<Self>, uri: Option<&str>, related: Option<&WebView>, focus: bool, lazy: Option<&str>) -> Option<WebView> {
        let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let label = gtk::Label::builder()
            .label("New Tab")
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .width_chars(12)
            .max_width_chars(22)
            .build();
        let tab_header = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        tab_header.add_css_class("wisp-tab");
        let tab = Rc::new(Tab {
            page: page.clone(),
            header: tab_header.clone(),
            label: label.clone(),
            view: RefCell::new(None),
            uri: RefCell::new(String::new()),
            title: RefCell::new(String::new()),
            state: RefCell::new(None),
            last_active: Cell::new(Instant::now()),
            upgraded_from: RefCell::new(None),
        });

        tab_header.append(&label);
        let close = gtk::Button::from_icon_name("window-close-symbolic");
        close.add_css_class("flat");
        let w = Rc::downgrade(self);
        let t = Rc::downgrade(&tab);
        close.connect_clicked(move |_| {
            if let (Some(win), Some(tab)) = (w.upgrade(), t.upgrade()) {
                win.close_tab(&tab);
            }
        });
        tab_header.append(&close);
        // Middle-click on a tab closes it.
        let middle = gtk::GestureClick::builder().button(2).build();
        let w = Rc::downgrade(self);
        let t = Rc::downgrade(&tab);
        middle.connect_released(move |_, _, _, _| {
            if let (Some(win), Some(tab)) = (w.upgrade(), t.upgrade()) {
                win.close_tab(&tab);
            }
        });
        tab_header.add_controller(middle);
        let select = gtk::GestureClick::builder().button(1).build();
        let w = Rc::downgrade(self);
        let t = Rc::downgrade(&tab);
        select.connect_pressed(move |_, _, _, _| {
            if let (Some(win), Some(tab)) = (w.upgrade(), t.upgrade()) {
                if let Some(n) = win.notebook.page_num(&tab.page) {
                    win.notebook.set_current_page(Some(n));
                }
            }
        });
        tab_header.add_controller(select);
        self.make_draggable(&tab);

        let view = match lazy {
            None => Some(self.create_view(&tab, related)),
            Some(title) => {
                *tab.uri.borrow_mut() = uri.unwrap_or_default().to_string();
                let shown = if title.is_empty() { uri.unwrap_or_default() } else { title };
                *tab.title.borrow_mut() = shown.to_string();
                tab.label.set_text(shown);
                tab.label.set_tooltip_text(Some(shown));
                None
            }
        };
        self.tabs.borrow_mut().push(tab.clone());
        // Pop-ups and background tabs go next to the current tab; new and
        // restored tabs at the end.
        let next_to_current = lazy.is_none() && (related.is_some() || !focus);
        let idx = if next_to_current { self.current_index().map(|i| i + 1) } else { None };
        let n = self.notebook.insert_page(&page, None::<&gtk::Widget>, idx);
        self.strip.append(&tab.header);
        self.order_strip();

        if related.is_none() && lazy.is_none() {
            let home = self.shared.config.borrow().homepage.clone();
            self.navigate(&tab, uri.unwrap_or(&home));
        }
        if focus {
            self.notebook.set_current_page(Some(n));
        }
        self.shared.save_session_soon();
        view
    }

    /// This window's tabs worth reopening, in order.
    fn saved_tabs(&self) -> Vec<SavedTab> {
        (0..self.notebook.n_pages())
            .filter_map(|n| self.notebook.nth_page(Some(n)).and_then(|p| self.tab_for_page(&p)))
            .map(|t| (t.uri.borrow().clone(), t.title.borrow().clone()))
            .filter(|(uri, _)| !uri.is_empty() && uri != "wisp:start" && !uri.starts_with("wisp-plugin:"))
            .collect()
    }

    fn create_view(self: &Rc<Self>, tab: &Rc<Tab>, related: Option<&WebView>) -> WebView {
        let shared = &self.shared;
        let anchor = related.cloned().or_else(|| {
            if shared.config.borrow().shared_web_process { shared.anchor_view() } else { None }
        });
        let view = match &anchor {
            // The builder does not inherit the content manager from the related
            // view (only webkit_web_view_new_with_related_view does), so pass it
            // explicitly or plugins silently miss every tab but the first.
            Some(r) => WebView::builder()
                .related_view(r)
                .user_content_manager(&shared.ucm)
                .settings(&shared.settings)
                .build(),
            None => WebView::builder()
                .network_session(&shared.session)
                .web_context(&shared.context)
                .user_content_manager(&shared.ucm)
                .settings(&shared.settings)
                .build(),
        };
        view.set_vexpand(true);
        shared.views.borrow_mut().push(view.downgrade());
        self.connect_view(tab, &view);
        tab.page.append(&view);
        *tab.view.borrow_mut() = Some(view.clone());
        view
    }

    fn connect_view(self: &Rc<Self>, tab: &Rc<Tab>, view: &WebView) {
        let w = Rc::downgrade(self);
        view.connect_context_menu(move |view, menu, hit| {
            if let Some(win) = w.upgrade() {
                win.add_plugin_menu_items(view, menu, hit);
            }
            false
        });
        let w = Rc::downgrade(self);
        let t = Rc::downgrade(tab);
        view.connect_title_notify(move |v| {
            let (Some(win), Some(tab)) = (w.upgrade(), t.upgrade()) else { return };
            let title = v.title().map(|s| s.to_string()).filter(|s| !s.is_empty());
            let title = title.unwrap_or_else(|| tab.uri.borrow().clone());
            tab.label.set_text(&title);
            tab.label.set_tooltip_text(Some(&title));
            if win.shared.config.borrow().save_history {
                if let Some(uri) = v.uri().filter(|u| u.starts_with("https://") || u.starts_with("http://")) {
                    win.shared.state.set_title(&uri, &title);
                }
            }
            *tab.title.borrow_mut() = title;
            win.shared.save_session_soon();
            if win.is_current(&tab) {
                win.sync_chrome(&tab);
            }
        });

        let w = Rc::downgrade(self);
        let t = Rc::downgrade(tab);
        view.connect_uri_notify(move |v| {
            let (Some(win), Some(tab)) = (w.upgrade(), t.upgrade()) else { return };
            *tab.uri.borrow_mut() = v.uri().map(|s| s.to_string()).unwrap_or_default();
            if win.is_current(&tab) {
                win.sync_chrome(&tab);
            }
        });

        let w = Rc::downgrade(self);
        let t = Rc::downgrade(tab);
        view.connect_estimated_load_progress_notify(move |v| {
            let (Some(win), Some(tab)) = (w.upgrade(), t.upgrade()) else { return };
            if win.is_current(&tab) {
                let p = v.estimated_load_progress();
                win.entry.set_progress_fraction(if p >= 1.0 { 0.0 } else { p });
            }
        });

        let w = Rc::downgrade(self);
        let t = Rc::downgrade(tab);
        view.connect_load_changed(move |v, event| {
            let (Some(win), Some(tab)) = (w.upgrade(), t.upgrade()) else { return };
            if std::env::var_os("WISP_DEBUG").is_some() {
                eprintln!("wisp: load {event:?} {}", v.uri().unwrap_or_default());
            }
            match event {
                // load-changed fires for the main frame only, which is exactly
                // where HTTPS upgrades and tracking-param stripping belong.
                LoadEvent::Started | LoadEvent::Redirected => {
                    let Some(uri) = v.uri() else { return };
                    if let Some(clean) = win.shared.rewrite(&uri) {
                        if uri.starts_with("http://") && clean.starts_with("https://") {
                            *tab.upgraded_from.borrow_mut() = Some(uri.to_string());
                        }
                        v.stop_loading();
                        v.load_uri(&clean);
                    }
                }
                LoadEvent::Committed => {
                    if v.uri().is_some_and(|u| u.starts_with("https://")) {
                        *tab.upgraded_from.borrow_mut() = None;
                    }
                    win.shared.save_session_soon();
                }
                LoadEvent::Finished => {
                    if win.shared.config.borrow().save_history {
                        if let Some(uri) = v.uri().filter(|u| u.starts_with("https://") || u.starts_with("http://")) {
                            let title = v.title().map(|t| t.to_string()).unwrap_or_default();
                            win.shared.state.record(&uri, &title);
                        }
                    }
                }
                _ => {}
            }
            if win.is_current(&tab) {
                win.sync_chrome(&tab);
            }
        });

        let t = Rc::downgrade(tab);
        view.connect_load_failed(move |v, _, failing_uri, error| {
            let Some(tab) = t.upgrade() else { return false };
            let Some(original) = tab.upgraded_from.borrow().clone() else { return false };
            let same_host = privacy::scheme_and_host(failing_uri).map(|(_, h)| h)
                == privacy::scheme_and_host(&original).map(|(_, h)| h);
            if !failing_uri.starts_with("https://") || !same_host || is_cancellation(error) {
                return false;
            }
            v.load_alternate_html(&https_failed_page(&original, error.message()), failing_uri, None);
            true
        });

        let w = Rc::downgrade(self);
        let t = Rc::downgrade(tab);
        view.connect_decide_policy(move |v, decision, kind| {
            let (Some(win), Some(tab)) = (w.upgrade(), t.upgrade()) else { return false };
            if kind != PolicyDecisionType::NavigationAction {
                return false;
            }
            let Some(nav) = decision.downcast_ref::<NavigationPolicyDecision>() else { return false };
            let Some(action) = nav.navigation_action() else { return false };
            let Some(uri) = action.request().and_then(|r| r.uri()) else { return false };

            // "Continue over HTTP" link on the HTTPS-failure page.
            if let Some(target) = uri.strip_prefix("wisp:allow-http?") {
                decision.ignore();
                let target = glib::Uri::unescape_string(target, None::<&str>).map(|s| s.to_string());
                let expected = tab.upgraded_from.borrow().clone();
                if let (Some(target), Some(expected)) = (target, expected) {
                    if target == expected {
                        if let Some((_, host)) = privacy::scheme_and_host(&target) {
                            win.shared.http_allowed.borrow_mut().insert(host.to_string());
                        }
                        v.load_uri(&target);
                    }
                }
                return true;
            }

            // Middle-click or Ctrl+click opens links in a background tab.
            let ctrl = gdk::ModifierType::CONTROL_MASK.bits();
            if action.is_user_gesture() && (action.mouse_button() == 2 || action.modifiers() & ctrl != 0) {
                decision.ignore();
                win.new_tab(Some(&uri), None, false);
                return true;
            }
            false
        });

        // target=_blank links open as tabs sharing the opener's process.
        // Script pop-ups (window.open) to another site are the usual
        // click-hijack/popunder trick, so they're held back behind a bar.
        let w = Rc::downgrade(self);
        view.connect_create(move |v, action| {
            let win = w.upgrade()?;
            let target = action.request().and_then(|r| r.uri()).map(|u| u.to_string()).unwrap_or_default();
            let opener = v.uri().map(|u| u.to_string()).unwrap_or_default();
            let opener_site = site_of(&opener);
            let is_link = action.navigation_type() == NavigationType::LinkClicked;
            let same_site = !target.is_empty() && site_of(&target) == opener_site;
            let allowed = win.shared.popup_allowed.borrow().contains(&opener_site);
            if is_link || same_site || allowed {
                let new_view = win.new_tab(None, Some(v), true);
                return Some(new_view.upcast());
            }
            win.show_blocked_popup(&target, &opener_site);
            None
        });

        let w = Rc::downgrade(self);
        let t = Rc::downgrade(tab);
        view.connect_close(move |_| {
            if let (Some(win), Some(tab)) = (w.upgrade(), t.upgrade()) {
                win.close_tab(&tab);
            }
        });

        // Deny geolocation, camera, microphone, notifications, ... by default.
        view.connect_permission_request(|_, req| {
            req.deny();
            true
        });

        view.connect_web_process_terminated(|v, reason| {
            let uri = v.uri().map(|s| s.to_string()).unwrap_or_default();
            eprintln!("wisp: web process for {uri} terminated: {reason:?}");
            let html = format!(
                "<title>Tab crashed</title><body style='font-family:sans-serif;padding:3em'>\
                 <h2>This page stopped ({reason:?})</h2><p>Press Ctrl+R to reload.</p>"
            );
            v.load_alternate_html(&html, &uri, None);
        });
    }

    fn navigate(self: &Rc<Self>, tab: &Rc<Tab>, uri: &str) {
        let view = self.view_of(tab);
        let uri = self.shared.rewrite(uri).unwrap_or_else(|| uri.to_string());
        *tab.uri.borrow_mut() = uri.clone();
        *tab.state.borrow_mut() = None;
        view.load_uri(&uri);
    }

    /// The tab's view, re-creating it if the tab was suspended.
    fn view_of(self: &Rc<Self>, tab: &Rc<Tab>) -> WebView {
        if let Some(v) = tab.view.borrow().clone() {
            return v;
        }
        while let Some(child) = tab.page.first_child() {
            tab.page.remove(&child);
        }
        let view = self.create_view(tab, None);
        let state = tab.state.borrow_mut().take();
        match state {
            Some(state) => {
                view.restore_session_state(&state);
                match view.back_forward_list().and_then(|l| l.current_item()) {
                    Some(item) => view.go_to_back_forward_list_item(&item),
                    None => view.load_uri(&tab.uri.borrow()),
                }
            }
            None => {
                let uri = tab.uri.borrow().clone();
                if !uri.is_empty() {
                    view.load_uri(&uri);
                }
            }
        }
        view
    }

    /// Free a background tab's web content, keeping its URL and history.
    fn suspend(&self, tab: &Tab) {
        let Some(view) = tab.view.borrow_mut().take() else { return };
        *tab.state.borrow_mut() = view.session_state();
        tab.page.remove(&view);
        let note = gtk::Label::new(Some("Tab suspended to save memory"));
        note.add_css_class("dim-label");
        note.set_vexpand(true);
        tab.page.append(&note);
    }

    fn start_suspend_timer(self: &Rc<Self>) {
        let w = Rc::downgrade(self);
        glib::timeout_add_seconds_local(60, move || {
            let Some(win) = w.upgrade() else { return glib::ControlFlow::Break };
            let minutes = win.shared.config.borrow().suspend_tabs_after_minutes;
            if minutes == 0 {
                return glib::ControlFlow::Continue;
            }
            let limit = Duration::from_secs(u64::from(minutes) * 60);
            let current = win.current_tab();
            for tab in win.tabs.borrow().iter() {
                let is_current = current.as_ref().is_some_and(|c| Rc::ptr_eq(c, tab));
                let busy = tab.view.borrow().as_ref().is_some_and(|v| v.is_playing_audio() || v.is_loading());
                if !is_current && !busy && tab.last_active.get().elapsed() > limit {
                    win.suspend(tab);
                }
            }
            glib::ControlFlow::Continue
        });
    }

    fn close_tab(self: &Rc<Self>, tab: &Rc<Tab>) {
        if let Some(n) = self.notebook.page_num(&tab.page) {
            self.notebook.remove_page(Some(n));
        }
        self.strip.remove(&tab.header);
        self.shared.save_session_soon();
        self.tabs.borrow_mut().retain(|t| !Rc::ptr_eq(t, tab));
        tab.view.borrow_mut().take();
        if self.tabs.borrow().is_empty() {
            self.window.close();
            return;
        }
    }

    /// Right-click menu entries of enabled plugins.
    fn add_plugin_menu_items(&self, view: &WebView, menu: &webkit6::ContextMenu, hit: &webkit6::HitTestResult) {
        let mut separated = false;
        for p in self.shared.plugins.enabled() {
            for item in &p.manifest.context_menu {
                let fits = item.contexts.iter().any(|c| match c.as_str() {
                    "editable" => hit.context_is_editable(),
                    "link" => hit.context_is_link(),
                    "selection" => hit.context_is_selection(),
                    "page" => true,
                    _ => false,
                });
                if !fits {
                    continue;
                }
                if !separated {
                    menu.append(&webkit6::ContextMenuItem::new_separator());
                    separated = true;
                }
                let action = gio::SimpleAction::new(&format!("{}-{}", p.manifest.id, item.id), None);
                let (view, world) = (view.downgrade(), p.world());
                let data = serde_json::json!({ "id": item.id });
                action.connect_activate(move |_, _| {
                    let Some(view) = view.upgrade() else { return };
                    let js = format!("globalThis.wisp && wisp._emit('context-menu', {data});");
                    view.evaluate_javascript(&js, Some(&world), None, None::<&gio::Cancellable>, |_| {});
                });
                menu.append(&webkit6::ContextMenuItem::from_gaction(&action, &item.label, None));
            }
        }
    }

    /// Tabs can be dragged to a new position in the strip.
    fn make_draggable(self: &Rc<Self>, tab: &Rc<Tab>) {
        let source = gtk::DragSource::builder().actions(gdk::DragAction::MOVE).build();
        let w = Rc::downgrade(self);
        let t = Rc::downgrade(tab);
        source.connect_prepare(move |source, _, _| {
            let (win, tab) = (w.upgrade()?, t.upgrade()?);
            source.set_icon(Some(&gtk::WidgetPaintable::new(Some(&tab.header))), 0, 0);
            *win.dragging.borrow_mut() = Some(Rc::downgrade(&tab));
            Some(gdk::ContentProvider::for_value(&"wisp-tab".to_value()))
        });
        tab.header.add_controller(source);

        let target = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::MOVE);
        let w = Rc::downgrade(self);
        let t = Rc::downgrade(tab);
        target.connect_drop(move |_, _, x, _| {
            let (Some(win), Some(onto)) = (w.upgrade(), t.upgrade()) else { return false };
            let Some(moved) = win.dragging.borrow_mut().take().and_then(|m| m.upgrade()) else { return false };
            let Some(mut to) = win.notebook.page_num(&onto.page) else { return false };
            // Dropped on the right half: place after that tab.
            if x > onto.header.width() as f64 / 2.0 {
                to += 1;
            }
            let from = win.notebook.page_num(&moved.page).unwrap_or(to);
            if from < to {
                to -= 1;
            }
            win.notebook.reorder_child(&moved.page, Some(to));
            win.order_strip();
            win.shared.save_session_soon();
            true
        });
        tab.header.add_controller(target);
    }

    /// Put the strip's tab buttons in the notebook's page order.
    fn order_strip(&self) {
        let mut prev: Option<gtk::Widget> = None;
        for n in 0..self.notebook.n_pages() {
            let Some(tab) = self.notebook.nth_page(Some(n)).and_then(|p| self.tab_for_page(&p)) else { continue };
            self.strip.reorder_child_after(&tab.header, prev.as_ref());
            prev = Some(tab.header.clone().upcast());
        }
    }

    fn highlight_tab(&self, tab: &Rc<Tab>) {
        for t in self.tabs.borrow().iter() {
            t.header.remove_css_class("active");
        }
        tab.header.add_css_class("active");
        // Scroll it into view once it has a position.
        let (header, strip, scroll) = (tab.header.clone(), self.strip.clone(), self.strip_scroll.clone());
        glib::idle_add_local_once(move || {
            let Some(b) = header.compute_bounds(&strip) else { return };
            let adj = scroll.hadjustment();
            let (x, w) = (b.x() as f64, b.width() as f64);
            if x < adj.value() {
                adj.set_value(x);
            } else if x + w > adj.value() + adj.page_size() {
                adj.set_value(x + w - adj.page_size());
            }
        });
    }

    fn cycle_tab(&self, delta: i32) {
        let n = self.notebook.n_pages() as i32;
        if n == 0 {
            return;
        }
        let cur = self.notebook.current_page().unwrap_or(0) as i32;
        self.notebook.set_current_page(Some(((cur + delta).rem_euclid(n)) as u32));
    }

    fn zoom(&self, factor: Option<f64>) {
        if let Some(v) = self.current_view() {
            v.set_zoom_level(factor.map_or(1.0, |f| (v.zoom_level() * f).clamp(0.3, 5.0)));
        }
    }

    /// For testing: save the window as a PNG after `delay`, then quit.
    /// Bookmarks as buttons under the address bar, in their saved order: click
    /// opens in this tab, middle-click in a new one, drag to reorder,
    /// right-click for more.
    fn rebuild_bookmarks_bar(self: &Rc<Self>) {
        while let Some(child) = self.bookmarks_bar.first_child() {
            self.bookmarks_bar.remove(&child);
        }
        self.bookmark_items.borrow_mut().clear();
        let marks = self.shared.sync.cached("b_");
        if marks.is_empty() {
            let hint = gtk::Label::builder().label("Bookmarks you add (Ctrl+D) show up here").css_classes(["dim-label"]).margin_start(8).build();
            self.bookmarks_bar.append(&hint);
        }
        for b in marks {
            let url = b["url"].as_str().unwrap_or("").to_string();
            let name = b["name"].as_str().filter(|n| !n.trim().is_empty()).unwrap_or(&url).trim().to_string();
            // Never narrower than the name (up to 22 characters), so names
            // can't be squeezed down to "…".
            let label = gtk::Label::builder()
                .label(name.as_str())
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .width_chars(name.chars().count().min(22) as i32)
                .max_width_chars(22)
                .build();
            let button = gtk::Button::builder().child(&label).tooltip_text(format!("{name}\n{url}")).css_classes(["flat"]).build();
            let (w, u) = (Rc::downgrade(self), url.clone());
            button.connect_clicked(move |_| {
                let Some(win) = w.upgrade() else { return };
                if let Some(tab) = win.current_tab() {
                    win.navigate(&tab, &u);
                }
            });
            let middle = gtk::GestureClick::builder().button(2).build();
            let (w, u) = (Rc::downgrade(self), url);
            middle.connect_released(move |_, _, _, _| {
                if let Some(win) = w.upgrade() {
                    win.new_tab(Some(&u), None, false);
                }
            });
            button.add_controller(middle);
            if let Some(id) = b["id"].as_i64() {
                self.make_bookmark_movable(&button, id, &b["url"]);
            }
            self.bookmarks_bar.append(&button);
            let url = b["url"].as_str().unwrap_or("").to_string();
            self.bookmark_items.borrow_mut().push((button, name, url));
        }
    }

    /// The » menu: bookmarks that are scrolled out of view on the bar.
    fn hidden_bookmarks_list(self: &Rc<Self>, popover: &gtk::Popover) -> gtk::Widget {
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let adj = self.bookmarks_scroll.hadjustment();
        let (left, right) = (adj.value(), adj.value() + adj.page_size());
        for (button, name, url) in self.bookmark_items.borrow().iter() {
            let Some(b) = button.compute_bounds(&self.bookmarks_bar) else { continue };
            if (b.x() as f64) >= left - 1.0 && (b.x() + b.width()) as f64 <= right + 1.0 {
                continue;
            }
            let label = gtk::Label::builder().label(name.as_str()).xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).max_width_chars(40).build();
            let item = gtk::Button::builder().child(&label).tooltip_text(url.as_str()).css_classes(["flat"]).build();
            let (w, u, p) = (Rc::downgrade(self), url.clone(), popover.clone());
            item.connect_clicked(move |_| {
                p.popdown();
                if let Some(win) = w.upgrade() {
                    if let Some(tab) = win.current_tab() {
                        win.navigate(&tab, &u);
                    }
                }
            });
            list.append(&item);
        }
        let scroll = gtk::ScrolledWindow::builder()
            .child(&list)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(500)
            .build();
        scroll.upcast()
    }

    fn make_bookmark_movable(self: &Rc<Self>, button: &gtk::Button, id: i64, url: &serde_json::Value) {
        let source = gtk::DragSource::builder().actions(gdk::DragAction::MOVE).build();
        let b = button.clone();
        source.connect_prepare(move |source, _, _| {
            source.set_icon(Some(&gtk::WidgetPaintable::new(Some(&b))), 0, 0);
            Some(gdk::ContentProvider::for_value(&format!("wisp-bookmark:{id}").to_value()))
        });
        button.add_controller(source);

        let target = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::MOVE);
        let (sync, b) = (self.shared.sync.clone(), button.clone());
        target.connect_drop(move |_, value, x, _| {
            let Some(moved) = value.get::<String>().ok().and_then(|v| v.strip_prefix("wisp-bookmark:")?.parse::<i64>().ok()) else {
                return false;
            };
            // Dropped on the right half: place after this one.
            let after = x > b.width() as f64 / 2.0;
            let sync = sync.clone();
            glib::spawn_future_local(async move {
                if let Err(e) = sync.move_entry(moved, id, after, &["b_".to_string()]).await {
                    eprintln!("wisp: moving bookmark: {e}");
                }
            });
            true
        });
        button.add_controller(target);

        let menu = gtk::GestureClick::builder().button(3).build();
        let (w, b, url) = (Rc::downgrade(self), button.clone(), url.as_str().unwrap_or("").to_string());
        menu.connect_pressed(move |_, _, _, _| {
            let Some(win) = w.upgrade() else { return };
            let popover = gtk::Popover::new();
            popover.set_parent(&b);
            let items = gtk::Box::new(gtk::Orientation::Vertical, 2);
            let open = gtk::Button::builder().label("Open in new tab").css_classes(["flat"]).build();
            let manage = gtk::Button::builder().label("Manage bookmarks…").css_classes(["flat"]).build();
            let delete = gtk::Button::builder().label("Delete").css_classes(["flat"]).build();
            for item in [&open, &manage, &delete] {
                item.child().and_then(|c| c.downcast::<gtk::Label>().ok()).inspect(|l| l.set_xalign(0.0));
                items.append(item);
            }
            popover.set_child(Some(&items));
            let (w2, u, p) = (Rc::downgrade(&win), url.clone(), popover.clone());
            open.connect_clicked(move |_| {
                p.popdown();
                if let Some(win) = w2.upgrade() {
                    win.new_tab(Some(&u), None, false);
                }
            });
            let (w2, p) = (Rc::downgrade(&win), popover.clone());
            manage.connect_clicked(move |_| {
                p.popdown();
                if let Some(win) = w2.upgrade() {
                    win.run_plugin_action("bookmarks", "list");
                }
            });
            let (sync, p) = (win.shared.sync.clone(), popover.clone());
            delete.connect_clicked(move |_| {
                p.popdown();
                let sync = sync.clone();
                glib::spawn_future_local(async move {
                    if let Err(e) = sync.remove(id, &["b_".to_string()]).await {
                        eprintln!("wisp: deleting bookmark: {e}");
                    }
                });
            });
            // Unparent once closed, so rebuilding the bar can't leak it.
            popover.connect_closed(|p| {
                let p = p.clone();
                glib::idle_add_local_once(move || p.unparent());
            });
            popover.popup();
        });
        button.add_controller(menu);
    }

    fn refresh_downloads(self: &Rc<Self>) {
        let list = &self.shared.downloads;
        self.downloads_button.set_visible(!list.is_empty());
        self.downloads_icon.queue_draw();
        let active = list.active();
        self.downloads_button.set_tooltip_text(Some(&match active {
            0 => "Downloads".to_string(),
            n => format!("Downloads ({n} in progress)"),
        }));
        if self.downloads_popover.is_visible() || self.downloads_popover.child().is_none() {
            let w = Rc::downgrade(self);
            let open: Rc<dyn Fn(&std::path::Path)> = Rc::new(move |p| {
                if let Some(win) = w.upgrade() {
                    win.downloads_popover.popdown();
                    win.open_path(p);
                }
            });
            self.downloads_popover.set_child(Some(&list.build_list(open)));
        }
    }

    /// Open a file or folder outside Wisp (see launch.rs). Failing that,
    /// files WebKit can show open in a tab; anything else shows its path.
    fn open_path(self: &Rc<Self>, path: &std::path::Path) {
        if crate::launch::open(path) {
            return;
        }
        let file = gio::File::for_path(path);
        let (mime, _) = gio::content_type_guess(Some(path), None);
        let viewable = path.is_file()
            && (crate::launch::is_text(&mime) || mime.starts_with("image/") || mime.starts_with("video/") || mime.starts_with("audio/") || mime == "application/pdf");
        if viewable {
            self.new_tab(Some(&file.uri()), None, true);
            return;
        }
        let (message, detail) = if path.is_dir() {
            (
                "No file manager found".to_string(),
                format!("{}\n\nWisp looked for a folder handler, the FileManager1 service and common file managers.", path.display()),
            )
        } else {
            ("No app is set up to open this file".to_string(), path.display().to_string())
        };
        let dialog = gtk::AlertDialog::builder()
            .message(message)
            .detail(detail)
            .buttons(["Copy path", "Close"])
            .default_button(0)
            .cancel_button(1)
            .build();
        let (shown, window) = (path.to_string_lossy().into_owned(), self.window.clone());
        dialog.choose(Some(&self.window), None::<&gio::Cancellable>, move |res| {
            if res == Ok(0) {
                window.clipboard().set_text(&shown);
            }
        });
    }

    /// Edit config.json in a window. The text is checked before saving, so a
    /// typo can't break the settings file.
    fn show_settings_editor(self: &Rc<Self>) {
        let text = serde_json::to_string_pretty(&*self.shared.config.borrow()).unwrap_or_default();
        let buffer = gtk::TextBuffer::new(None);
        buffer.set_text(&text);
        let view = gtk::TextView::builder()
            .buffer(&buffer)
            .monospace(true)
            .top_margin(8)
            .bottom_margin(8)
            .left_margin(10)
            .right_margin(10)
            .build();
        let scroll = gtk::ScrolledWindow::builder().child(&view).vexpand(true).build();
        let hint = gtk::Label::builder()
            .label("Changes apply after restarting Wisp (Ctrl+Q). The README describes each setting.")
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label"])
            .build();
        let error = gtk::Label::builder().xalign(0.0).wrap(true).css_classes(["error"]).visible(false).build();
        let cancel = gtk::Button::with_label("Cancel");
        let save = gtk::Button::builder().label("Save").css_classes(["suggested-action"]).build();
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        buttons.set_halign(gtk::Align::End);
        buttons.append(&cancel);
        buttons.append(&save);
        let vbox = gtk::Box::new(gtk::Orientation::Vertical, 8);
        vbox.set_margin_top(8);
        vbox.set_margin_bottom(8);
        vbox.set_margin_start(10);
        vbox.set_margin_end(10);
        vbox.append(&hint);
        vbox.append(&scroll);
        vbox.append(&error);
        vbox.append(&buttons);
        let dialog = gtk::Window::builder()
            .title("Settings")
            .transient_for(&self.window)
            .modal(true)
            .default_width(620)
            .default_height(640)
            .child(&vbox)
            .build();

        let d = dialog.clone();
        cancel.connect_clicked(move |_| d.close());
        let (d, shared) = (dialog.clone(), self.shared.clone());
        save.connect_clicked(move |_| {
            let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
            match parse_settings(&text) {
                Ok(cfg) => {
                    cfg.save();
                    *shared.config.borrow_mut() = cfg;
                    d.close();
                }
                Err(e) => {
                    error.set_label(&e);
                    error.set_visible(true);
                }
            }
        });
        dialog.present();
    }

    /// Copy a plugin folder (one with a plugin.json) into the plugin folder.
    fn install_plugin(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::builder().title("Choose a plugin folder").modal(true).build();
        let w = Rc::downgrade(self);
        dialog.select_folder(Some(&self.window), None::<&gio::Cancellable>, move |res| {
            let (Some(win), Some(src)) = (w.upgrade(), res.ok().and_then(|f| f.path())) else { return };
            let result = (|| -> Result<String, String> {
                let manifest = std::fs::read_to_string(src.join("plugin.json"))
                    .map_err(|_| "That folder has no plugin.json, so it isn't a Wisp plugin.".to_string())?;
                let manifest: serde_json::Value =
                    serde_json::from_str(&manifest).map_err(|e| format!("Its plugin.json is not valid: {e}"))?;
                let id = manifest["id"].as_str().filter(|id| !id.is_empty() && !id.contains(['/', '\\', '.']));
                let id = id.ok_or("Its plugin.json has no usable \"id\".")?;
                let dest = plugins::plugin_dirs().remove(0).join(id);
                if dest.exists() {
                    std::fs::remove_dir_all(&dest).map_err(|e| e.to_string())?;
                }
                copy_dir(&src, &dest).map_err(|e| e.to_string())?;
                Ok(manifest["name"].as_str().unwrap_or(id).to_string())
            })();
            let (message, detail) = match result {
                Ok(name) => (format!("Installed {name}"), "Restart Wisp (Ctrl+Q, then start it again) to load it.".to_string()),
                Err(e) => ("Couldn't install the plugin".to_string(), e),
            };
            gtk::AlertDialog::builder().message(message).detail(detail).build().show(Some(&win.window));
        });
    }

    /// Opening Wisp while it runs only adds a window to the running process,
    /// so after a rebuild the old version keeps going. Say so.
    pub fn warn_if_outdated(&self) {
        let replaced = std::env::current_exe().is_ok_and(|p| p.to_string_lossy().ends_with(" (deleted)"));
        if replaced {
            gtk::AlertDialog::builder()
                .message("A newer Wisp is installed")
                .detail("This window belongs to the Wisp that was already running, which is the older version. Close every Wisp window, then start Wisp again to use the new one.")
                .build()
                .show(Some(&self.window));
        }
    }

    pub fn screenshot_and_quit(self: &Rc<Self>, path: String, delay: Duration) {
        let win = self.clone();
        glib::timeout_add_local_once(delay, move || {
            if let Some(title) = win.current_view().and_then(|v| v.title()) {
                println!("wisp: screenshot of page \"{title}\"");
            }
            let paintable = gtk::WidgetPaintable::new(Some(&win.window));
            let snapshot = gtk::Snapshot::new();
            paintable.snapshot(&snapshot, win.window.width() as f64, win.window.height() as f64);
            match (snapshot.to_node(), win.window.renderer()) {
                (Some(node), Some(renderer)) => {
                    if let Err(e) = renderer.render_texture(&node, None).save_to_png(&path) {
                        eprintln!("wisp: screenshot: {e}");
                    }
                }
                _ => eprintln!("wisp: screenshot: nothing to render"),
            }
            win.shared.app.quit();
        });
    }

    // ---- helpers ----

    fn current_index(&self) -> Option<u32> {
        self.notebook.current_page()
    }

    fn tab_for_page(&self, page: &gtk::Widget) -> Option<Rc<Tab>> {
        self.tabs.borrow().iter().find(|t| t.page.upcast_ref::<gtk::Widget>() == page).cloned()
    }

    fn current_tab(&self) -> Option<Rc<Tab>> {
        let page = self.notebook.nth_page(self.current_index())?;
        self.tab_for_page(&page)
    }

    fn current_view(&self) -> Option<WebView> {
        self.current_tab().and_then(|t| t.view.borrow().clone())
    }

    fn is_current(&self, tab: &Rc<Tab>) -> bool {
        self.current_tab().is_some_and(|c| Rc::ptr_eq(&c, tab))
    }

    /// Update toolbar, URL bar and window title for the current tab.
    fn sync_chrome(&self, tab: &Rc<Tab>) {
        let uri = tab.uri.borrow().clone();
        let shown = if uri == "wisp:start" { String::new() } else { uri };
        if !self.entry.has_focus() || self.entry.text().is_empty() {
            self.entry.set_text(&shown);
        }
        let title = tab.title.borrow().clone();
        self.window.set_title(Some(if title.is_empty() { "Wisp" } else { &title }));
        let view = tab.view.borrow().clone();
        self.back.set_sensitive(view.as_ref().is_some_and(|v| v.can_go_back()));
        self.forward.set_sensitive(view.as_ref().is_some_and(|v| v.can_go_forward()));
        let progress = view.as_ref().map_or(0.0, |v| v.estimated_load_progress());
        self.entry.set_progress_fraction(if progress >= 1.0 { 0.0 } else { progress });
    }
}

fn install_css() {
    let Some(display) = gdk::Display::default() else { return };
    let css = gtk::CssProvider::new();
    css.load_from_string(
        ".wisp-tabs { padding: 3px 4px 0 4px; }
         .wisp-tab { padding: 1px 2px 1px 10px; margin: 0 1px; border-radius: 7px 7px 0 0; }
         .wisp-tab:hover { background: alpha(currentColor, 0.06); }
         .wisp-tab.active { background: alpha(currentColor, 0.13); }
         .wisp-tab button { min-height: 22px; min-width: 22px; padding: 0; }
         .wisp-new-tab { min-height: 26px; min-width: 26px; padding: 0; }
         .wisp-downloads { padding: 4px; }
         .wisp-bookmarks { padding: 0 4px 3px 4px; }
         .wisp-bookmarks button { padding: 2px 8px; min-height: 24px; font-size: 0.95em; }
         .wisp-dl-row > button { padding: 6px 8px; border-radius: 6px; }
         .wisp-dl-name { font-weight: 500; }
         .wisp-dl-status { font-size: 0.85em; }
         .wisp-downloads progressbar trough, .wisp-downloads progressbar progress { min-height: 3px; }",
    );
    gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
}

fn icon_button(icon: &str, tooltip: &str, action: &str) -> gtk::Button {
    gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(tooltip)
        .action_name(action)
        .build()
}

fn plugin_action_name(plugin: &str, action: &str) -> String {
    format!("win.plugin-action('{plugin}/{action}')")
}

/// "<Ctrl><Shift>o" -> "Ctrl+Shift+O"
fn shortcut_label(accel: &str) -> String {
    match gtk::accelerator_parse(accel) {
        Some((key, mods)) => gtk::accelerator_get_label(key, mods).to_string(),
        None => accel.to_string(),
    }
}

/// Approximate registrable domain ("news.bbc.co.uk" -> "bbc.co.uk").
fn site_of(uri: &str) -> String {
    let host = privacy::scheme_and_host(uri).map(|(_, h)| h).unwrap_or("").to_ascii_lowercase();
    if host.parse::<std::net::IpAddr>().is_ok() {
        return host;
    }
    let labels: Vec<&str> = host.split('.').filter(|l| !l.is_empty()).collect();
    let n = labels.len();
    if n <= 2 {
        return labels.join(".");
    }
    // Second-level public suffixes like co.uk, com.au, org.br.
    let take = if labels[n - 1].len() == 2 && ["co", "com", "org", "net", "gov", "ac", "edu"].contains(&labels[n - 2]) {
        3
    } else {
        2
    };
    labels[n - take..].join(".")
}

/// Settings text to a Config, refusing invalid JSON and unknown names (typos
/// would otherwise be silently ignored).
fn parse_settings(text: &str) -> Result<Config, String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("Line {}, column {}: {e}", e.line(), e.column()))?;
    let known = serde_json::to_value(Config::default()).unwrap_or_default();
    if let (Some(given), Some(known)) = (value.as_object(), known.as_object()) {
        let unknown: Vec<&str> = given.keys().filter(|k| !known.contains_key(*k)).map(String::as_str).collect();
        if !unknown.is_empty() {
            return Err(format!("Unknown setting: {}", unknown.join(", ")));
        }
    }
    serde_json::from_value(value).map_err(|e| e.to_string())
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn is_cancellation(error: &glib::Error) -> bool {
    error.matches(webkit6::NetworkError::Cancelled)
        || error.matches(webkit6::PolicyError::FrameLoadInterruptedByPolicyChange)
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn https_failed_page(http_uri: &str, reason: &str) -> String {
    let allow = format!("wisp:allow-http?{}", glib::Uri::escape_string(http_uri, None, false));
    format!(
        r#"<!doctype html><title>Secure connection failed</title>
<style>body{{font-family:sans-serif;max-width:40em;margin:4em auto;padding:0 1em;line-height:1.5}}
a.btn{{display:inline-block;margin-top:1em;padding:.5em 1em;border:1px solid #c33;border-radius:6px;color:#c33;text-decoration:none}}</style>
<h2>This site doesn't support a secure connection</h2>
<p>Wisp tried <b>https://</b> for <code>{site}</code> but it failed: {reason}</p>
<p>Loading it over plain HTTP lets anyone on your network see and change the page.</p>
<a class="btn" href="{allow}">Continue over HTTP (insecure)</a>"#,
        site = html_escape(http_uri),
        reason = html_escape(reason),
        allow = html_escape(&allow),
    )
}

/// wisp:history: newest first, grouped by day, filtered as you type.
fn history_page(shared: &Shared) -> String {
    let enabled = shared.config.borrow().save_history;
    let visits = shared.state.history();
    let mut rows = String::new();
    let mut last_day = String::new();
    for v in visits.iter().take(5000) {
        let when = glib::DateTime::from_unix_local(v.last).ok();
        let day = when.as_ref().and_then(|d| d.format("%A, %e %B %Y").ok()).map(|s| s.to_string()).unwrap_or_default();
        let time = when.as_ref().and_then(|d| d.format("%H:%M").ok()).map(|s| s.to_string()).unwrap_or_default();
        if day != last_day {
            rows.push_str(&format!("<h2>{}</h2>", html_escape(&day)));
            last_day = day;
        }
        let title = if v.title.is_empty() { &v.url } else { &v.title };
        rows.push_str(&format!(
            "<a class=v href=\"{url}\"><span class=t>{time}</span><span class=n>{title}</span><span class=u>{url}</span></a>",
            url = html_escape(&v.url),
            title = html_escape(title),
        ));
    }
    let note = match (enabled, visits.is_empty()) {
        (false, true) => "History is off. Turn on <b>Remember history</b> in the ☰ menu to keep it (encrypted on disk).",
        (false, false) => "History is off; these are from before.",
        (true, true) => "Nothing yet. Pages you visit will show up here.",
        (true, false) => "",
    };
    format!(
        r#"<!doctype html><meta charset=utf-8><title>History</title>
<style>:root{{color-scheme:light dark}} body{{font:14px system-ui,sans-serif;max-width:60em;margin:2em auto;padding:0 1em}}
header{{display:flex;gap:1em;align-items:center}} h1{{font-weight:400;margin:0;flex:1}}
input{{font:inherit;padding:.45em .7em;border-radius:6px;border:1px solid #8889;width:18em}}
h2{{font-size:.95em;font-weight:600;margin:1.6em 0 .4em;color:GrayText}}
.v{{display:flex;gap:1em;padding:.35em .4em;border-radius:5px;text-decoration:none;color:inherit;white-space:nowrap}}
.v:hover{{background:#8882}} .t{{color:GrayText;width:3.2em;flex:none}} .n{{overflow:hidden;text-overflow:ellipsis;max-width:50%}}
.u{{color:GrayText;overflow:hidden;text-overflow:ellipsis}} a.clear{{color:GrayText}} p{{color:GrayText}}</style>
<header><h1>History</h1><input id=q type=search placeholder="Search history" autofocus><a class=clear href="wisp:history?clear" onclick="return confirm('Delete all history?')">Clear history</a></header>
<p>{note}</p><div id=list>{rows}</div>
<script>q.oninput=()=>{{const s=q.value.toLowerCase();for(const a of document.querySelectorAll('.v'))a.hidden=s&&!a.textContent.toLowerCase().includes(s);for(const h of document.querySelectorAll('h2')){{let n=h.nextElementSibling,any=false;while(n&&n.tagName!=='H2'){{if(!n.hidden)any=true;n=n.nextElementSibling}}h.hidden=!any}}}}</script>"#
    )
}

fn start_page(cfg: &Config) -> String {
    let search = serde_json::to_string(&cfg.search_url).unwrap_or_default();
    let mode = if cfg.persistent_profile { "Persistent profile" } else { "Private session &middot; nothing is saved" };
    format!(
        r#"<!doctype html><meta charset="utf-8"><title>New Tab</title>
<style>
:root{{color-scheme:light dark}}
body{{font-family:system-ui,sans-serif;display:grid;place-items:center;height:90vh;margin:0}}
main{{text-align:center;width:min(36em,90vw)}}
h1{{font-weight:300;font-size:3em;margin:0 0 .4em;letter-spacing:.1em}}
input{{width:100%;box-sizing:border-box;font-size:1.1em;padding:.7em 1em;border-radius:999px;border:1px solid #8888}}
p{{opacity:.6;font-size:.9em}}
</style>
<main><h1>wisp</h1>
<form id="f"><input id="q" autofocus placeholder="Search privately" autocomplete="off"></form>
<p>{mode}</p></main>
<script>
document.getElementById('f').onsubmit = e => {{
  e.preventDefault();
  const q = document.getElementById('q').value.trim();
  if (q) location.href = {search}.replace('%s', encodeURIComponent(q));
}};
</script>"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_are_checked() {
        assert!(parse_settings(r#"{ "https_only": false }"#).is_ok_and(|c| !c.https_only));
        assert!(parse_settings(r#"{ "https_onyl": false }"#).unwrap_err().contains("https_onyl"));
        assert!(parse_settings(r#"{ "https_only": false, }"#).unwrap_err().starts_with("Line 1"));
    }
}
