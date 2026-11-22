//! Asking for the Wisp password: setting the vault up and unlocking it.
//! The vault itself is in wisp-core (`wisp_core::vault`).

pub use wisp_core::vault::*;
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use webkit6::glib;

// ---- asking for the password ----

/// What `ask` calls when it's done.
type Done = Box<dyn FnOnce(bool)>;

thread_local! {
    /// The last password window was closed with its X rather than answered.
    static CLOSED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether the last password window was closed with its X (at start: the
/// person wants Wisp gone, not opened locked), rather than answered.
pub fn closed() -> bool {
    CLOSED.get()
}

#[derive(Clone, Copy, PartialEq)]
pub enum Ask {
    /// First start: choose a password (or none).
    Setup,
    /// Enter it.
    Unlock,
    /// Change it, or remove it.
    Change,
}

/// On the browser window `parent` (as a modal dialog), which closing takes
/// it along, as if put off.
fn attach(window: &gtk::Window, parent: Option<&gtk::Window>) {
    let Some(p) = parent else { return };
    window.set_transient_for(Some(p));
    window.set_modal(true);
    let w = window.downgrade();
    p.connect_close_request(move |_| {
        if let Some(w) = w.upgrade() {
            w.close();
        }
        glib::Propagation::Proceed
    });
}

/// The vault's window. `done(true)` once the vault is open (set up, unlocked
/// or changed), `done(false)` if put off; called once.
pub fn ask(app: &gtk::Application, parent: Option<&gtk::Window>, kind: Ask, done: impl FnOnce(bool) + 'static) {
    CLOSED.set(false);
    let vault = get();
    if vault.downgraded() {
        return warn_downgraded(app, parent, done);
    }
    let window = gtk::Window::builder().application(app).title("Wisp password").default_width(440).resizable(false).build();
    attach(&window, parent);
    let done: Rc<RefCell<Option<Done>>> = Rc::new(RefCell::new(Some(Box::new(done))));
    let finish = {
        let (done, window) = (done.clone(), window.clone());
        move |ok: bool| {
            // Taken first: closing the window comes back here.
            let f = done.borrow_mut().take();
            if let Some(f) = f {
                window.close();
                f(ok);
            }
        }
    };
    {
        let (finish, done) = (finish.clone(), done.clone());
        window.connect_close_request(move |_| {
            // Still waiting for an answer: closed with its X, not a button.
            if done.borrow().is_some() {
                CLOSED.set(true);
            }
            finish(false);
            glib::Propagation::Proceed
        });
    }

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 10);
    vbox.set_margin_top(18);
    vbox.set_margin_bottom(18);
    vbox.set_margin_start(18);
    vbox.set_margin_end(18);
    let (title, text) = match kind {
        Ask::Setup => (
            "Protect what Wisp keeps",
            "Saved passwords, bookmarks, history and open tabs are kept encrypted. Choose a password \
             to lock them: Wisp asks for it each time it starts, and without it they can't be read, \
             even by someone with your files. It can't be recovered if you forget it.",
        ),
        Ask::Unlock => ("Wisp is locked", "Enter your Wisp password to use your saved passwords, bookmarks, history and tabs."),
        Ask::Change if vault.protected() => {
            ("Wisp password", "Choose a new password, or remove it (then what Wisp keeps is only encrypted with a key stored beside it).")
        }
        Ask::Change => (
            "Wisp password",
            "What Wisp keeps isn't locked with a password: anyone who can read your files can read it. Choose one to lock it.",
        ),
    };
    vbox.append(&gtk::Label::builder().label(format!("<b>{title}</b>")).use_markup(true).xalign(0.0).build());
    vbox.append(&gtk::Label::builder().label(text).wrap(true).xalign(0.0).max_width_chars(52).build());

    let password = gtk::PasswordEntry::builder().show_peek_icon(true).hexpand(true).activates_default(true).build();
    let confirm = gtk::PasswordEntry::builder().show_peek_icon(true).hexpand(true).activates_default(true).build();
    let grid = gtk::Grid::builder().row_spacing(6).column_spacing(10).build();
    let label = if kind == Ask::Unlock { "Password" } else { "New password" };
    grid.attach(&gtk::Label::builder().label(label).xalign(1.0).build(), 0, 0, 1, 1);
    grid.attach(&password, 1, 0, 1, 1);
    if kind != Ask::Unlock {
        grid.attach(&gtk::Label::builder().label("Again").xalign(1.0).build(), 0, 1, 1, 1);
        grid.attach(&confirm, 1, 1, 1, 1);
    }
    vbox.append(&grid);
    let status = gtk::Label::builder().wrap(true).xalign(0.0).max_width_chars(52).build();
    status.add_css_class("dim-label");
    vbox.append(&status);

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    buttons.set_halign(gtk::Align::End);
    let other = match kind {
        Ask::Setup => Some("Don't use a password"),
        Ask::Change if vault.protected() => Some("Remove password"),
        _ => None,
    }
    .map(gtk::Button::with_label);
    let later = gtk::Button::with_label(if kind == Ask::Change { "Close" } else { "Not now" });
    let main = gtk::Button::with_label(match kind {
        Ask::Setup => "Use this password",
        Ask::Unlock => "Unlock",
        Ask::Change if vault.protected() => "Change password",
        Ask::Change => "Set password",
    });
    main.add_css_class("suggested-action");
    if let Some(b) = &other {
        buttons.append(b);
    }
    // At first start, putting it off would just ask again next time.
    if kind != Ask::Setup {
        buttons.append(&later);
    }
    buttons.append(&main);
    vbox.append(&buttons);
    window.set_child(Some(&vbox));
    window.set_default_widget(Some(&main));

    let busy = {
        let (main, other, later) = (main.clone(), other.clone(), later.clone());
        move |on: bool| {
            main.set_sensitive(!on);
            later.set_sensitive(!on);
            if let Some(o) = &other {
                o.set_sensitive(!on);
            }
        }
    };

    {
        let finish = finish.clone();
        later.connect_clicked(move |_| finish(false));
    }
    if let Some(other) = &other {
        let (finish, status, busy) = (finish.clone(), status.clone(), busy.clone());
        other.connect_clicked(move |_| {
            let (finish, status, busy) = (finish.clone(), status.clone(), busy.clone());
            busy(true);
            glib::spawn_future_local(async move {
                let vault = get();
                let result = if kind == Ask::Setup { vault.create(None).await } else { vault.set_password(None).await };
                match result {
                    Ok(()) => finish(true),
                    Err(e) => {
                        status.set_text(&e);
                        busy(false);
                    }
                }
            });
        });
    }
    main.connect_clicked(move |_| {
        let (pw, again) = (password.text().to_string(), confirm.text().to_string());
        if pw.is_empty() {
            status.set_text("Enter a password.");
            return;
        }
        if kind != Ask::Unlock && pw != again {
            status.set_text("The two passwords aren't the same.");
            return;
        }
        let (finish, status, busy, password) = (finish.clone(), status.clone(), busy.clone(), password.clone());
        busy(true);
        status.set_text(if kind == Ask::Unlock { "Unlocking…" } else { "Locking…" });
        glib::spawn_future_local(async move {
            let vault = get();
            let result = match kind {
                Ask::Setup => vault.create(Some(pw)).await,
                Ask::Unlock => vault.unlock(&pw).await,
                Ask::Change => vault.set_password(Some(pw)).await,
            };
            match result {
                Ok(()) => finish(true),
                Err(e) => {
                    status.set_text(&e);
                    password.set_text("");
                    password.grab_focus();
                    busy(false);
                }
            }
        });
    });
    window.present();
}

/// Shown in place of the password window when the vault had a password and
/// its file now has none, though it wasn't removed in Wisp. Nothing is
/// opened unless the user says to go on without one.
fn warn_downgraded(app: &gtk::Application, parent: Option<&gtk::Window>, done: impl FnOnce(bool) + 'static) {
    let window = gtk::Window::builder().application(app).title("Wisp password").default_width(460).resizable(false).build();
    attach(&window, parent);
    let done: Rc<RefCell<Option<Done>>> = Rc::new(RefCell::new(Some(Box::new(done))));
    let finish = {
        let (done, window) = (done.clone(), window.clone());
        move |ok: bool| {
            let f = done.borrow_mut().take();
            if let Some(f) = f {
                window.close();
                f(ok);
            }
        }
    };
    {
        let (finish, done) = (finish.clone(), done.clone());
        window.connect_close_request(move |_| {
            // Still waiting for an answer: closed with its X, not a button.
            if done.borrow().is_some() {
                CLOSED.set(true);
            }
            finish(false);
            glib::Propagation::Proceed
        });
    }
    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 10);
    vbox.set_margin_top(18);
    vbox.set_margin_bottom(18);
    vbox.set_margin_start(18);
    vbox.set_margin_end(18);
    vbox.append(&gtk::Label::builder().label("<b>Wisp's password is gone</b>").use_markup(true).xalign(0.0).build());
    let text = "What Wisp keeps was locked with a password. The file that holds its key (vault.json) now says there is \
                none, and the password wasn't removed in Wisp: something else replaced that file.\n\n\
                If that wasn't you, keep it locked: anything saved from here on could be read by whoever replaced \
                the file. Quit Wisp and put vault.json back from a backup to get your saved passwords, bookmarks, \
                history and tabs back.";
    vbox.append(&gtk::Label::builder().label(text).wrap(true).xalign(0.0).max_width_chars(54).build());
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    buttons.set_halign(gtk::Align::End);
    let accept = gtk::Button::with_label("Use it without a password");
    accept.add_css_class("destructive-action");
    let keep = gtk::Button::with_label("Keep it locked");
    keep.add_css_class("suggested-action");
    buttons.append(&accept);
    buttons.append(&keep);
    vbox.append(&buttons);
    window.set_child(Some(&vbox));
    window.set_default_widget(Some(&keep));
    {
        let finish = finish.clone();
        keep.connect_clicked(move |_| finish(false));
    }
    accept.connect_clicked(move |_| {
        get().accept_downgrade();
        finish(true);
    });
    window.present();
}
