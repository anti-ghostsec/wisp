//! Wisp's core: everything that doesn't depend on the web engine or the
//! toolkit, shared by the desktop browser (WebKitGTK) and Wisp for Android
//! (GeckoView).
//!
//! The host (the app around it) installs a [`rt::Runtime`] at start, which
//! gives the core its timers, background tasks and HTTP, and sets the
//! folders Wisp keeps its files in ([`config::set_dirs`]). All of it runs
//! on one thread, the host's UI (or core) thread.

pub mod adblock;
pub mod config;
pub mod plugins;
pub mod privacy;
pub mod rt;
pub mod state;
pub mod sync;
pub mod vault;
