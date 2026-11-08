//! Big copies, without GTK.
//!
//! On X11 GTK 4 hands a copy over in pieces (INCR) once it's bigger than
//! 256 KB, and that hangs: it takes the request and never sends a byte, so
//! Wisp freezes, and so does every program that pastes from it. Ctrl+A,
//! Ctrl+C on most web pages is that big (the formatted copy is several
//! times the text). So a big copy (page_fixes.js decides) comes here as
//! plain text instead, and a thread with its own connection to the display
//! owns the clipboard and hands it over, in pieces where needed, until
//! something else is copied.

use gtk::prelude::*;
use std::thread;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ChangeWindowAttributesAux, ConnectionExt as _, CreateWindowAux, EventMask, PropMode, Property,
    SelectionNotifyEvent, SelectionRequestEvent, Window, WindowClass, SELECTION_NOTIFY_EVENT,
};
use x11rb::rust_connection::RustConnection;
use x11rb::NONE;

/// Pieces of this size: well under any display's limit for one request.
const PIECE: usize = 64 * 1024;

/// Whether Wisp runs on X11 (only there is this needed, or possible).
pub fn available() -> bool {
    gtk::gdk::Display::default().is_some_and(|d| d.type_().name() == "GdkX11Display")
}

/// Put `text` on the clipboard, replacing what Wisp copied before.
pub fn copy(text: String) {
    thread::Builder::new()
        .name("wisp-bigcopy".into())
        .spawn(move || {
            if let Err(e) = serve(text.into_bytes()) {
                eprintln!("wisp: copying: {e}");
            }
        })
        .ok();
}

struct Atoms {
    clipboard: Atom,
    targets: Atom,
    timestamp: Atom,
    incr: Atom,
    utf8: Atom,
    text: Atom,
    plain_utf8: Atom,
    plain: Atom,
}

impl Atoms {
    fn new(c: &RustConnection) -> Result<Self, Box<dyn std::error::Error>> {
        let a = |n: &[u8]| -> Result<Atom, Box<dyn std::error::Error>> { Ok(c.intern_atom(false, n)?.reply()?.atom) };
        Ok(Atoms {
            clipboard: a(b"CLIPBOARD")?,
            targets: a(b"TARGETS")?,
            timestamp: a(b"TIMESTAMP")?,
            incr: a(b"INCR")?,
            utf8: a(b"UTF8_STRING")?,
            text: a(b"TEXT")?,
            plain_utf8: a(b"text/plain;charset=utf-8")?,
            plain: a(b"text/plain")?,
        })
    }
}

/// A transfer in pieces: to whom, into which property, as what, how far.
struct Sending {
    requestor: Window,
    property: Atom,
    kind: Atom,
    at: usize,
}

fn serve(data: Vec<u8>) -> Result<(), Box<dyn std::error::Error>> {
    let (c, screen) = x11rb::connect(None)?;
    let root = c.setup().roots[screen].root;
    let atoms = Atoms::new(&c)?;
    let win = c.generate_id()?;
    c.create_window(
        0,
        win,
        root,
        -10,
        -10,
        1,
        1,
        0,
        WindowClass::INPUT_ONLY,
        0,
        &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    )?;
    // A real timestamp to own it with: a property change on our window gives one.
    c.change_property8(PropMode::REPLACE, win, AtomEnum::WM_NAME, AtomEnum::STRING, b"wisp copy")?;
    c.flush()?;
    let time = loop {
        if let Event::PropertyNotify(e) = c.wait_for_event()?
            && e.window == win
        {
            break e.time;
        }
    };
    c.set_selection_owner(win, atoms.clipboard, time)?;
    if c.get_selection_owner(atoms.clipboard)?.reply()?.owner != win {
        return Err("the clipboard couldn't be taken".into());
    }
    c.flush()?;

    let mut sending: Vec<Sending> = Vec::new();
    let mut owned = true;
    while owned || !sending.is_empty() {
        match c.wait_for_event()? {
            Event::SelectionClear(e) if e.selection == atoms.clipboard => owned = false,
            Event::SelectionRequest(e) if owned => answer(&c, &atoms, &data, time, &e, &mut sending)?,
            // The requestor took a piece (deleted the property): the next one,
            // and after the last, an empty one.
            Event::PropertyNotify(e) if e.state == Property::DELETE => {
                if let Some(i) = sending.iter().position(|s| s.requestor == e.window && s.property == e.atom) {
                    let s = &mut sending[i];
                    let end = (s.at + PIECE).min(data.len());
                    c.change_property8(PropMode::REPLACE, s.requestor, s.property, s.kind, &data[s.at..end])?;
                    if s.at == end {
                        sending.remove(i);
                    } else {
                        s.at = end;
                    }
                }
            }
            _ => {}
        }
        c.flush()?;
    }
    Ok(())
}

fn answer(
    c: &RustConnection,
    atoms: &Atoms,
    data: &[u8],
    time: u32,
    e: &SelectionRequestEvent,
    sending: &mut Vec<Sending>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Old programs leave the property out: the target is used instead.
    let property = if e.property == NONE { e.target } else { e.property };
    let text_kinds = [atoms.utf8, atoms.plain_utf8, atoms.text, atoms.plain, AtomEnum::STRING.into()];
    let given = if e.target == atoms.targets {
        let list = [atoms.targets, atoms.timestamp, atoms.utf8, atoms.plain_utf8, atoms.text, atoms.plain, AtomEnum::STRING.into()];
        c.change_property32(PropMode::REPLACE, e.requestor, property, AtomEnum::ATOM, &list)?;
        property
    } else if e.target == atoms.timestamp {
        c.change_property32(PropMode::REPLACE, e.requestor, property, AtomEnum::INTEGER, &[time])?;
        property
    } else if text_kinds.contains(&e.target) {
        // TEXT is answered as UTF-8; the others as what they asked for.
        let kind = if e.target == atoms.text { atoms.utf8 } else { e.target };
        if data.len() <= PIECE {
            c.change_property8(PropMode::REPLACE, e.requestor, property, kind, data)?;
        } else {
            // In pieces: told the size first; each deletion asks for the next.
            c.change_window_attributes(e.requestor, &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE))?;
            let size = u32::try_from(data.len()).unwrap_or(u32::MAX);
            c.change_property32(PropMode::REPLACE, e.requestor, property, atoms.incr, &[size])?;
            sending.retain(|s| !(s.requestor == e.requestor && s.property == property));
            sending.push(Sending { requestor: e.requestor, property, kind, at: 0 });
        }
        property
    } else {
        NONE
    };
    let notify = SelectionNotifyEvent {
        response_type: SELECTION_NOTIFY_EVENT,
        sequence: 0,
        time: e.time,
        requestor: e.requestor,
        selection: e.selection,
        target: e.target,
        property: given,
    };
    c.send_event(false, e.requestor, EventMask::NO_EVENT, notify)?;
    Ok(())
}
