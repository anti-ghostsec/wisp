//! Wisp: a small, privacy-focused WebKitGTK browser.

mod browser;
mod config;
mod downloads;
mod icons;
mod launch;
mod plugins;
mod privacy;
mod state;
mod sync;
mod adblock;

use browser::{BrowserWindow, Shared};
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::rc::Rc;

const APP_ID: &str = "io.github.wisp.Browser";

const HELP: &str = "Usage: wisp [URL...]

Options:
  --version     Show version
  --help        Show this help

Settings: ~/.config/wisp/config.json
Plugins:  ~/.local/share/wisp/plugins/<id>/plugin.json
";

fn main() -> glib::ExitCode {
    // Handled here so the output goes to this terminal, not a running instance.
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.iter().any(|a| a == "--version") {
        println!("wisp {}", env!("CARGO_PKG_VERSION"));
        return glib::ExitCode::SUCCESS;
    }
    if argv.iter().any(|a| a == "--help" || a == "-h") {
        print!("{HELP}");
        return glib::ExitCode::SUCCESS;
    }

    let cfg = config::Config::load();
    configure_rendering(&cfg);

    // One process serves every window: launching wisp again opens a window
    // in the running instance instead of starting a second browser.
    let app = gtk::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();

    let cfg = RefCell::new(Some(cfg));
    let shared: Rc<RefCell<Option<Rc<Shared>>>> = Rc::new(RefCell::new(None));
    let on_exit = shared.clone();
    app.connect_command_line(move |app, cmd| {
        let args: Vec<String> = cmd
            .arguments()
            .iter()
            .skip(1)
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let uris: Vec<String> = args
            .iter()
            .filter(|a| !a.starts_with("--"))
            .map(|a| {
                // Local files given as paths become file:// URIs.
                let file = cmd.create_file_for_arg(a);
                if !a.contains(':') && file.query_exists(None::<&gio::Cancellable>) {
                    file.uri().to_string()
                } else {
                    a.clone()
                }
            })
            .collect();
        let starting = shared.borrow().is_none();
        let shared = shared.borrow_mut().get_or_insert_with(|| Shared::new(app, cfg.take().unwrap_or_default())).clone();
        // The first window of a run brings back the last session's tabs, if asked to.
        let restored = if starting && uris.is_empty() { shared.restore_session() } else { None };
        let window = restored.unwrap_or_else(|| BrowserWindow::new(&shared, &uris));
        window.warn_if_outdated();
        if let Ok(path) = std::env::var("WISP_SCREENSHOT") {
            let secs = std::env::var("WISP_SCREENSHOT_DELAY").ok().and_then(|s| s.parse().ok()).unwrap_or(8);
            window.screenshot_and_quit(path, std::time::Duration::from_secs(secs));
        }
        glib::ExitCode::SUCCESS
    });
    // Write history that hasn't been saved yet.
    app.connect_shutdown(move |_| {
        if let Some(shared) = on_exit.borrow().as_ref() {
            shared.state.save_now();
        }
    });
    app.run()
}

/// Pick the renderer through environment variables, before GTK or WebKit read
/// them. Variables the user set themselves always win.
fn configure_rendering(cfg: &config::Config) {
    let mut vars: Vec<(&str, String)> = Vec::new();
    if !cfg.hardware_acceleration {
        // Page content drawn by the CPU and a cairo-drawn UI keep text crisp,
        // and keep ~100 MB of GPU driver out of the UI process.
        vars.push(("GSK_RENDERER", "cairo".into()));
        vars.push(("WEBKIT_SKIA_ENABLE_CPU_RENDERING", "1".into()));
    }
    if cfg.gpu_compositing || cfg.hardware_acceleration {
        // Handing frames to the UI as DMA-BUFs hangs the UI thread on some
        // drivers; shared memory works everywhere.
        vars.push(("WEBKIT_DMABUF_RENDERER_FORCE_SHM", "1".into()));
        // WebKit otherwise takes the first render node, which on hybrid
        // systems can be an idle GPU that isn't driving the display.
        if let Some(node) = display_render_node() {
            vars.push(("WEBKIT_WEB_RENDER_DEVICE_FILE", node));
        }
    } else {
        vars.push(("WEBKIT_DISABLE_DMABUF_RENDERER", "1".into()));
    }
    for (key, value) in vars {
        if std::env::var_os(key).is_none() {
            // SAFETY: no other threads exist yet.
            unsafe { std::env::set_var(key, value) };
        }
    }
}

/// The render node (/dev/dri/renderD*) of the GPU with a connected display.
fn display_render_node() -> Option<String> {
    let drm = std::path::Path::new("/sys/class/drm");
    for entry in std::fs::read_dir(drm).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // Connectors look like "card1-HDMI-A-2".
        let Some((card, _)) = name.split_once('-') else { continue };
        let connected = std::fs::read_to_string(entry.path().join("status")).is_ok_and(|s| s.trim() == "connected");
        if !connected || !card.starts_with("card") {
            continue;
        }
        let nodes = std::fs::read_dir(drm.join(card).join("device/drm")).ok()?;
        for node in nodes.flatten() {
            let node = node.file_name().to_string_lossy().into_owned();
            if node.starts_with("renderD") {
                return Some(format!("/dev/dri/{node}"));
            }
        }
    }
    None
}
