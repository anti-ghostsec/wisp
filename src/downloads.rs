//! Downloads: where they go, and the list shown from the toolbar.
//!
//! The list lives in memory only, like the rest of the browsing session.

use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};
use webkit6::Download;

#[derive(Clone, Copy, PartialEq)]
pub enum State {
    Active,
    Done,
    Failed,
    Cancelled,
}

pub struct Item {
    download: Download,
    name: RefCell<String>,
    path: RefCell<Option<PathBuf>>,
    state: Cell<State>,
}

pub struct Downloads {
    items: RefCell<Vec<Rc<Item>>>,
    listeners: RefCell<Vec<Box<dyn Fn()>>>,
    last_progress: Cell<Option<Instant>>,
    /// A download finished since the list was last opened: the button turns blue.
    attention: Cell<bool>,
}

/// The user's Downloads folder (created if missing), else their home.
pub fn default_dir() -> PathBuf {
    let dir = glib::user_special_dir(glib::UserDirectory::Downloads).unwrap_or_else(|| glib::home_dir().join("Downloads"));
    if std::fs::create_dir_all(&dir).is_ok() { dir } else { glib::home_dir() }
}

/// `dir/name`, or `dir/name (2).ext` and so on if that's taken.
fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s, format!(".{e}")),
        _ => (name, String::new()),
    };
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .unwrap()
}

/// A file name that can't escape the folder or hide itself.
fn safe_name(suggested: &str) -> String {
    let name: String = suggested
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    let name = name.trim().trim_start_matches('.').to_string();
    if name.is_empty() { "download".into() } else { name }
}

fn size(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 30 => format!("{:.1} GB", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{} KB", b >> 10),
        b => format!("{b} bytes"),
    }
}

impl Downloads {
    pub fn new() -> Rc<Downloads> {
        Rc::new(Downloads {
            items: RefCell::new(Vec::new()),
            listeners: RefCell::new(Vec::new()),
            last_progress: Cell::new(None),
            attention: Cell::new(false),
        })
    }

    pub fn connect_changed(&self, f: impl Fn() + 'static) {
        self.listeners.borrow_mut().push(Box::new(f));
    }

    fn changed(&self) {
        for f in self.listeners.borrow().iter() {
            f();
        }
    }

    pub fn is_empty(&self) -> bool {
        self.items.borrow().is_empty()
    }

    /// Combined progress of running downloads, or None when none are running.
    pub fn progress(&self) -> Option<f64> {
        let items = self.items.borrow();
        let active: Vec<_> = items.iter().filter(|i| i.state.get() == State::Active).collect();
        if active.is_empty() {
            return None;
        }
        Some(active.iter().map(|i| i.download.estimated_progress()).sum::<f64>() / active.len() as f64)
    }

    pub fn seen(&self) {
        self.attention.set(false);
    }

    /// The toolbar icon: an arrow over a tray, Firefox style. While files
    /// download the tray becomes a progress bar; a finished download turns
    /// the arrow blue until the list is opened.
    pub fn icon(self: &Rc<Self>) -> gtk::DrawingArea {
        let area = gtk::DrawingArea::builder()
            .content_width(16)
            .content_height(16)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
        let me = Rc::downgrade(self);
        area.set_draw_func(move |area, cr, width, height| {
            let Some(me) = me.upgrade() else { return };
            let fg = area.color();
            let (fr, fg_, fb, fa) = (fg.red() as f64, fg.green() as f64, fg.blue() as f64, fg.alpha() as f64);
            const BLUE: (f64, f64, f64) = (0.21, 0.52, 0.89);
            cr.translate((width as f64 - 16.0) / 2.0, (height as f64 - 16.0) / 2.0);
            cr.set_line_width(1.6);
            cr.set_line_cap(gtk::cairo::LineCap::Round);
            cr.set_line_join(gtk::cairo::LineJoin::Round);
            let progress = me.progress();
            if me.attention.get() {
                cr.set_source_rgb(BLUE.0, BLUE.1, BLUE.2);
            } else {
                cr.set_source_rgba(fr, fg_, fb, fa);
            }
            // The arrow sits higher while a progress bar is shown.
            let lift = if progress.is_some() { 2.0 } else { 0.0 };
            cr.move_to(8.0, 1.5 - lift / 2.0);
            cr.line_to(8.0, 10.5 - lift);
            cr.move_to(4.0, 6.5 - lift);
            cr.line_to(8.0, 10.5 - lift);
            cr.line_to(12.0, 6.5 - lift);
            let _ = cr.stroke();
            match progress {
                Some(p) => {
                    cr.set_line_width(2.4);
                    cr.set_source_rgba(fr, fg_, fb, fa * 0.25);
                    cr.move_to(2.5, 13.8);
                    cr.line_to(13.5, 13.8);
                    let _ = cr.stroke();
                    cr.set_source_rgb(BLUE.0, BLUE.1, BLUE.2);
                    cr.move_to(2.5, 13.8);
                    cr.line_to(2.5 + 11.0 * p.clamp(0.02, 1.0), 13.8);
                    let _ = cr.stroke();
                }
                None => {
                    cr.move_to(2.5, 14.0);
                    cr.line_to(13.5, 14.0);
                    let _ = cr.stroke();
                }
            }
        });
        area
    }

    pub fn active(&self) -> usize {
        self.items.borrow().iter().filter(|i| i.state.get() == State::Active).count()
    }

    /// Track a new download. With `ask`, a save dialog picks the file;
    /// otherwise it goes straight into `dir`.
    pub fn add(self: &Rc<Self>, download: &Download, parent: Option<gtk::Window>, ask: bool, dir: PathBuf) {
        let item = Rc::new(Item {
            download: download.clone(),
            name: RefCell::new("Starting…".into()),
            path: RefCell::new(None),
            state: Cell::new(State::Active),
        });
        self.items.borrow_mut().insert(0, item.clone());

        let it = item.clone();
        download.connect_decide_destination(move |d, suggested| {
            let name = safe_name(suggested);
            *it.name.borrow_mut() = name.clone();
            if !ask {
                d.set_destination(&unique_path(&dir, &name).to_string_lossy());
                return true;
            }
            let dialog = gtk::FileDialog::builder()
                .title("Save file")
                .initial_name(name.as_str())
                .initial_folder(&gio::File::for_path(&dir))
                .modal(true)
                .build();
            let d = d.clone();
            // The dialog asks before replacing an existing file.
            dialog.save(parent.as_ref(), None::<&gio::Cancellable>, move |res| match res.ok().and_then(|f| f.path()) {
                Some(path) => {
                    d.set_allow_overwrite(true);
                    d.set_destination(&path.to_string_lossy());
                }
                None => d.cancel(),
            });
            true
        });
        let (it, me) = (item.clone(), Rc::downgrade(self));
        download.connect_created_destination(move |_, dest| {
            let path = PathBuf::from(dest);
            if let Some(n) = path.file_name() {
                *it.name.borrow_mut() = n.to_string_lossy().into_owned();
            }
            *it.path.borrow_mut() = Some(path);
            if let Some(me) = me.upgrade() {
                me.changed();
            }
        });
        let me = Rc::downgrade(self);
        download.connect_received_data(move |_, _| {
            // Progress redraws at most four times a second.
            let Some(me) = me.upgrade() else { return };
            if me.last_progress.get().is_none_or(|t| t.elapsed() > Duration::from_millis(250)) {
                me.last_progress.set(Some(Instant::now()));
                me.changed();
            }
        });
        let (it, me) = (item.clone(), Rc::downgrade(self));
        download.connect_failed(move |_, err| {
            let cancelled = err.matches(webkit6::DownloadError::CancelledByUser);
            it.state.set(if cancelled { State::Cancelled } else { State::Failed });
            if !cancelled {
                eprintln!("wisp: download failed: {err}");
            }
            if let Some(me) = me.upgrade() {
                me.changed();
            }
        });
        let (it, me) = (item, Rc::downgrade(self));
        // Also emitted after "failed".
        download.connect_finished(move |_| {
            if let Some(me) = me.upgrade() {
                if it.state.get() == State::Active {
                    it.state.set(State::Done);
                    me.attention.set(true);
                }
                me.changed();
            }
        });
        self.changed();
    }

    /// The popover content, newest first, laid out like Firefox's: click a
    /// finished download to open it, the folder button shows where it is.
    pub fn build_list(self: &Rc<Self>, open: Rc<dyn Fn(&Path)>) -> gtk::Widget {
        let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        outer.set_width_request(360);
        outer.add_css_class("wisp-downloads");
        let items = self.items.borrow().clone();
        if items.is_empty() {
            outer.append(&gtk::Label::builder().label("No downloads for this session").css_classes(["dim-label"]).margin_top(12).margin_bottom(12).build());
        }
        for item in items {
            let d = &item.download;
            let state = item.state.get();
            let got = d.received_data_length();
            let total = d.response().map(|r| r.content_length()).unwrap_or(0);
            let status = match state {
                State::Active => {
                    let speed = got as f64 / d.elapsed_time().max(0.5);
                    let amount = if total > 0 { format!("{} of {}", size(got), size(total)) } else { size(got) };
                    format!("{amount} — {}/s", size(speed as u64))
                }
                State::Done => format!("Completed — {}", size(got)),
                State::Failed => "Failed".into(),
                State::Cancelled => "Canceled".into(),
            };

            let content = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            content.append(&file_glyph(state));
            let text = gtk::Box::new(gtk::Orientation::Vertical, 3);
            text.set_hexpand(true);
            text.set_valign(gtk::Align::Center);
            let name = item.name.borrow().clone();
            text.append(&gtk::Label::builder().label(name.as_str()).xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::Middle).css_classes(["wisp-dl-name"]).build());
            if state == State::Active {
                let bar = gtk::ProgressBar::new();
                bar.set_fraction(d.estimated_progress());
                text.append(&bar);
            }
            text.append(&gtk::Label::builder().label(status.as_str()).xalign(0.0).css_classes(["dim-label", "wisp-dl-status"]).build());
            content.append(&text);

            let row = gtk::Box::new(gtk::Orientation::Horizontal, 2);
            row.add_css_class("wisp-dl-row");
            let path = item.path.borrow().clone();
            let main = gtk::Button::builder().child(&content).hexpand(true).css_classes(["flat"]).build();
            match (&path, state) {
                (Some(p), State::Done) => {
                    let (open, p) = (open.clone(), p.clone());
                    main.set_tooltip_text(Some("Open file"));
                    main.connect_clicked(move |_| open(&p));
                }
                _ => {}
            }
            row.append(&main);
            let side = match (state, path) {
                (State::Active, _) => {
                    let b = side_button("Cancel", ICON_CANCEL);
                    let d = d.clone();
                    b.connect_clicked(move |_| d.cancel());
                    Some(b)
                }
                (State::Done, Some(p)) => p.parent().map(Path::to_path_buf).map(|dir| {
                    let b = side_button("Show in folder", ICON_FOLDER);
                    let open = open.clone();
                    b.connect_clicked(move |_| open(&dir));
                    b
                }),
                _ => None,
            };
            if let Some(b) = side {
                row.append(&b);
            }
            outer.append(&row);
        }
        if self.items.borrow().iter().any(|i| i.state.get() != State::Active) {
            outer.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            let clear = gtk::Button::builder().label("Clear finished downloads").css_classes(["flat"]).build();
            let me = Rc::downgrade(self);
            clear.connect_clicked(move |_| {
                if let Some(me) = me.upgrade() {
                    me.items.borrow_mut().retain(|i| i.state.get() == State::Active);
                    me.changed();
                }
            });
            outer.append(&clear);
        }
        outer.upcast()
    }
}

const ICON_FOLDER: &str = r#"<svg viewBox="0 0 16 16"><path fill-rule="evenodd" d="M1.5 3A1.5 1.5 0 0 1 3 1.5h3.4l1.6 2h5A1.5 1.5 0 0 1 14.5 5v7.5A1.5 1.5 0 0 1 13 14H3a1.5 1.5 0 0 1-1.5-1.5zM3 3v9.5h10V5H7.3L5.7 3z"/></svg>"#;
const ICON_CANCEL: &str = r#"<svg viewBox="0 0 16 16"><path d="M3.8 2.7 8 6.9l4.2-4.2 1.1 1.1L9.1 8l4.2 4.2-1.1 1.1L8 9.1l-4.2 4.2-1.1-1.1L6.9 8 2.7 3.8z"/></svg>"#;
const ICON_FILE: &str = r#"<svg viewBox="0 0 16 16"><path fill-rule="evenodd" d="M3 1h6.5L13 4.5V15H3zm1.4 1.4v11.2h7.2V5.3H8.6V2.4z"/></svg>"#;

fn side_button(tooltip: &str, svg: &str) -> gtk::Button {
    gtk::Button::builder()
        .child(&crate::icons::from_svg(svg).unwrap())
        .tooltip_text(tooltip)
        .valign(gtk::Align::Center)
        .css_classes(["flat", "circular"])
        .build()
}

/// A page glyph, dimmed for failed and canceled downloads.
fn file_glyph(state: State) -> gtk::Widget {
    let glyph = crate::icons::from_svg(ICON_FILE).unwrap();
    glyph.set_size_request(24, 24);
    glyph.set_valign(gtk::Align::Center);
    if matches!(state, State::Failed | State::Cancelled) {
        glyph.set_opacity(0.4);
    }
    glyph
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_stay_in_the_folder() {
        assert_eq!(safe_name("../../.bashrc"), "bashrc");
        assert_eq!(safe_name("a\\b\\photo.png"), "photo.png");
        assert_eq!(safe_name("  "), "download");
    }

    #[test]
    fn unique_names() {
        let dir = std::env::temp_dir().join(format!("wisp-dl-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("cat.png"), b"").unwrap();
        assert_eq!(unique_path(&dir, "cat.png"), dir.join("cat (2).png"));
        assert_eq!(unique_path(&dir, "new.png"), dir.join("new.png"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
