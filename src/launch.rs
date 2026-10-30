//! Opening files and folders with whatever the system has, on any Linux:
//! desktop-standard associations first, then the file-manager D-Bus service,
//! per-extension association files, any app that handles the type, and
//! finally file managers and editors found on PATH.

use gtk::{gio, glib};
use gtk::prelude::*;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// File managers tried in order when nothing is registered for folders.
const FILE_MANAGERS: &[&str] = &[
    "ferro-files", "nautilus", "dolphin", "thunar", "nemo", "caja", "pcmanfm-qt", "pcmanfm", "spacefm", "konqueror", "doublecmd",
];
/// Text editors tried when nothing is registered for text files.
const EDITORS: &[&str] = &["gnome-text-editor", "gedit", "kate", "kwrite", "mousepad", "xed", "pluma", "featherpad", "geany", "leafpad"];

/// Try to open `path` outside Wisp. Returns false when nothing could.
pub fn open(path: &Path) -> bool {
    if path.is_dir() { open_folder(path) } else { open_file(path) }
}

fn open_folder(path: &Path) -> bool {
    let file = gio::File::for_path(path);
    default_app(&file)
        || show_folder_over_dbus(&file)
        || any_app_for("inode/directory", &file)
        || FILE_MANAGERS.iter().any(|fm| spawn(&[OsStr::new(fm), path.as_os_str()]))
}

fn open_file(path: &Path) -> bool {
    let file = gio::File::for_path(path);
    if default_app(&file) || from_association_files(path) {
        return true;
    }
    let (mime, _) = gio::content_type_guess(Some(path), None);
    if any_app_for(&mime, &file) {
        return true;
    }
    is_text(&mime)
        && (any_app_for("text/plain", &file) || EDITORS.iter().any(|ed| spawn(&[OsStr::new(ed), path.as_os_str()])))
}

pub fn is_text(mime: &str) -> bool {
    mime.starts_with("text/") || ["application/json", "application/xml", "application/javascript", "application/x-shellscript"].contains(&mime)
}

fn default_app(file: &gio::File) -> bool {
    gio::AppInfo::launch_default_for_uri(&file.uri(), None::<&gio::AppLaunchContext>).is_ok()
}

/// The first installed app that declares it handles `mime`.
fn any_app_for(mime: &str, file: &gio::File) -> bool {
    gio::AppInfo::all_for_type(mime)
        .iter()
        .any(|app| app.launch(&[file.clone()], None::<&gio::AppLaunchContext>).is_ok())
}

/// org.freedesktop.FileManager1, which most file managers provide.
fn show_folder_over_dbus(file: &gio::File) -> bool {
    const NAME: &str = "org.freedesktop.FileManager1";
    let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>) else { return false };
    let call = |method: &str, args: glib::Variant| {
        bus.call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            method,
            Some(&args),
            None,
            gio::DBusCallFlags::NONE,
            500,
            None::<&gio::Cancellable>,
        )
    };
    let owned = call("NameHasOwner", (NAME,).to_variant()).ok().and_then(|v| v.get::<(bool,)>()).is_some_and(|(b,)| b);
    let activatable = || {
        call("ListActivatableNames", ().to_variant())
            .ok()
            .and_then(|v| v.get::<(Vec<String>,)>())
            .is_some_and(|(names,)| names.iter().any(|n| n == NAME))
    };
    if !owned && !activatable() {
        return false;
    }
    bus.call_sync(
        Some(NAME),
        "/org/freedesktop/FileManager1",
        NAME,
        "ShowFolders",
        Some(&(vec![file.uri().to_string()], "").to_variant()),
        None,
        gio::DBusCallFlags::NONE,
        2000,
        None::<&gio::Cancellable>,
    )
    .is_ok()
}

/// `extension = command %f` files, as FerroOS uses: the user's own first.
fn from_association_files(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(OsStr::to_str).map(str::to_ascii_lowercase) else { return false };
    let user = glib::user_config_dir();
    let candidates = [user.join("ferro/associations.conf"), PathBuf::from("/etc/ferro/associations.conf")];
    for conf in candidates {
        let Ok(text) = std::fs::read_to_string(conf) else { continue };
        if let Some(command) = association(&text, &ext) {
            if run_command(&command, path) {
                return true;
            }
        }
    }
    false
}

fn association(text: &str, ext: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .find(|(k, _)| k.trim().trim_start_matches('.').eq_ignore_ascii_case(ext))
        .map(|(_, cmd)| cmd.trim().to_string())
        .filter(|c| !c.is_empty())
}

/// Run a desktop-style command line, putting the file in for %f/%F/%u/%U
/// (or at the end when there's no placeholder).
fn run_command(command: &str, path: &Path) -> bool {
    let Ok(words) = glib::shell_parse_argv(command) else { return false };
    let uri = gio::File::for_path(path).uri().to_string();
    let mut placed = false;
    let mut argv: Vec<std::ffi::OsString> = Vec::new();
    for w in words {
        let w = w.to_string_lossy().into_owned();
        match w.as_str() {
            "%f" | "%F" => {
                argv.push(path.as_os_str().to_owned());
                placed = true;
            }
            "%u" | "%U" => {
                argv.push(uri.clone().into());
                placed = true;
            }
            _ => argv.push(w.into()),
        }
    }
    if !placed {
        argv.push(path.as_os_str().to_owned());
    }
    let argv: Vec<&OsStr> = argv.iter().map(|a| a.as_os_str()).collect();
    spawn(&argv)
}

/// Start a program found on PATH; GIO reaps it when it exits.
fn spawn(argv: &[&OsStr]) -> bool {
    let Some(program) = argv.first() else { return false };
    if glib::find_program_in_path(program).is_none() {
        return false;
    }
    gio::Subprocess::newv(argv, gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_SILENCE).is_ok()
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn association_lines() {
        let conf = "# FerroOS File Associations\npng = ferro-viewer %f\n.MP4 = mpv %f\nbad line\n";
        assert_eq!(association(conf, "png").as_deref(), Some("ferro-viewer %f"));
        assert_eq!(association(conf, "mp4").as_deref(), Some("mpv %f"));
        assert_eq!(association(conf, "txt"), None);
    }
}
