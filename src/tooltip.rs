//! The small label a page shows when the pointer rests on something with a
//! `title` (a video's name on YouTube), drawn by Wisp inside its own window.
//!
//! GTK would put it in a little window of its own. On X11 without a
//! compositor that window is on screen before anything is drawn in it, and
//! at times before it's where it belongs: a black box flashes, now and then
//! beside the pointer, and then the label appears under it. A label that is
//! part of Wisp's window is drawn with the page in one go, so it's simply
//! there, with the round corners and see-through black GTK meant it to have.

use gtk::glib;
use gtk::glib::translate::{IntoGlib, ToGlibPtr};
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};
use webkit6::prelude::*;
use webkit6::WebView;

/// How long the pointer rests before the label shows, as in GTK.
const REST: Duration = Duration::from_millis(500);
/// And when another label was showing a moment ago.
const REST_BROWSING: Duration = Duration::from_millis(60);
/// How far under the pointer's tip the label goes, to clear the arrow.
const UNDER_POINTER: i32 = 20;

pub struct PageTip {
    /// The window's page area; the label lies over it.
    stage: gtk::Overlay,
    bubble: gtk::Box,
    label: gtk::Label,
    /// Put into GTK's own tooltip while a page is asked for its text, to
    /// find the label GTK keeps that text in (see `page_text`).
    probe: gtk::Box,
    /// The text waiting for the pointer to rest, and where the pointer is.
    wanted: RefCell<Option<(String, i32, i32)>>,
    timer: RefCell<Option<glib::SourceId>>,
    hidden_at: Cell<Option<Instant>>,
    /// The view the label belongs to.
    owner: glib::WeakRef<WebView>,
    /// Where the pointer was at a click or a key: no label there again
    /// until it moves, whatever the page goes on to say.
    dismissed: Cell<Option<(i32, i32)>>,
    pointer: Cell<(i32, i32)>,
}

impl PageTip {
    pub fn new(window: &gtk::ApplicationWindow, stage: &gtk::Overlay) -> Rc<PageTip> {
        // Named and classed like GTK's tooltip, so the theme styles it the same.
        let bubble = gtk::Box::builder()
            .css_name("tooltip")
            .css_classes(["background"])
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Start)
            .can_target(false)
            .can_focus(false)
            .visible(false)
            .build();
        let label = gtk::Label::builder()
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .max_width_chars(50)
            .build();
        bubble.append(&label);
        stage.add_overlay(&bubble);
        let tip = Rc::new(PageTip {
            stage: stage.clone(),
            bubble,
            label,
            probe: gtk::Box::builder().visible(false).build(),
            wanted: RefCell::new(None),
            timer: RefCell::new(None),
            hidden_at: Cell::new(None),
            owner: glib::WeakRef::new(),
            dismissed: Cell::new(None),
            pointer: Cell::new((0, 0)),
        });
        // Gone on a click, a key or the wheel, wherever in the window, as
        // GTK's is.
        let events = gtk::EventControllerLegacy::builder().propagation_phase(gtk::PropagationPhase::Capture).build();
        let weak = Rc::downgrade(&tip);
        events.connect_event(move |_, event| {
            use gtk::gdk::EventType as E;
            let kind = event.event_type();
            if matches!(kind, E::ButtonPress | E::KeyPress | E::Scroll | E::TouchBegin)
                && let Some(tip) = weak.upgrade()
            {
                // After the wheel the pointer is over something else,
                // which may have a label of its own.
                let here = tip.bubble.is_visible() || tip.wanted.borrow().is_some();
                if here && kind != E::Scroll {
                    tip.dismissed.set(Some(tip.pointer.get()));
                }
                tip.hide();
            }
            glib::Propagation::Proceed
        });
        window.add_controller(events);
        tip
    }

    /// Show this view's tooltips here instead of in GTK's tooltip window.
    pub fn watch(self: &Rc<Self>, view: &WebView) {
        let tip = Rc::downgrade(self);
        view.connect_query_tooltip(move |view, x, y, keyboard, tooltip| {
            let Some(tip) = tip.upgrade() else { return false };
            // No pointer to put it under, or GTK isn't built the way this
            // expects: GTK shows it, as it always did.
            let Some(text) = (if keyboard { None } else { tip.page_text(view, x, y, tooltip) }) else { return false };
            // WebKit's own answer, which follows this one, would have GTK
            // show its window.
            view.stop_signal_emission_by_name("query-tooltip");
            match text {
                Some(text) => tip.want(view, text, x, y),
                None => tip.hide_for(view),
            }
            false
        });
        // WebKit says there is nothing to show any more by switching the
        // view's tooltip off, and then nothing asks.
        let tip = Rc::downgrade(self);
        view.connect_has_tooltip_notify(move |view| {
            if let (Some(tip), false) = (tip.upgrade(), view.has_tooltip()) {
                tip.hide_for(view);
            }
        });
        // Gone when the pointer leaves, and with the page itself.
        let motion = gtk::EventControllerMotion::new();
        let (tip, v) = (Rc::downgrade(self), view.downgrade());
        motion.connect_leave(move |_| {
            if let (Some(tip), Some(view)) = (tip.upgrade(), v.upgrade()) {
                tip.hide_for(&view);
            }
        });
        view.add_controller(motion);
        let tip = Rc::downgrade(self);
        view.connect_unmap(move |view| {
            if let Some(tip) = tip.upgrade() {
                tip.hide_for(view);
            }
        });
        let tip = Rc::downgrade(self);
        view.connect_load_changed(move |view, event| {
            if let (Some(tip), webkit6::LoadEvent::Committed) = (tip.upgrade(), event) {
                tip.hide_for(view);
            }
        });
    }

    /// What the page wants shown at this point: `Some(None)` for nothing.
    /// `None` when it can't be found out.
    ///
    /// WebKit keeps the text to itself and only ever hands it to GTK's
    /// tooltip, which has no way to read it back. But a tooltip takes a
    /// widget of ours next to its label, and from there the label is in
    /// reach: WebKit is asked the way GTK would ask, and the label read.
    fn page_text(&self, view: &WebView, x: i32, y: i32, tooltip: &gtk::Tooltip) -> Option<Option<String>> {
        tooltip.set_custom(Some(&self.probe));
        let mut label = None;
        let mut next = self.probe.parent().and_then(|p| p.first_child());
        while let Some(widget) = next {
            next = widget.next_sibling();
            if let Ok(found) = widget.downcast::<gtk::Label>() {
                label = Some(found);
                break;
            }
        }
        let widget: &gtk::Widget = view.upcast_ref();
        let ptr: *mut gtk::ffi::GtkWidget = widget.to_glib_none().0;
        // SAFETY: `ptr` is a live GtkWidget, so its class is a GtkWidgetClass
        // (WebKit's), and its `query_tooltip` is called with the arguments
        // GTK itself gave this signal's handler.
        let shown = label.as_ref().and_then(|_| unsafe {
            let class = (*(ptr as *mut glib::gobject_ffi::GTypeInstance)).g_class as *const gtk::ffi::GtkWidgetClass;
            (*class).query_tooltip.map(|ask| ask(ptr, x, y, false.into_glib(), tooltip.to_glib_none().0) != glib::ffi::GFALSE)
        });
        tooltip.set_custom(None::<&gtk::Widget>);
        let text = label?.text();
        Some(if shown? && !text.is_empty() { Some(text.into()) } else { None })
    }

    fn want(self: &Rc<Self>, view: &WebView, text: String, x: i32, y: i32) {
        let Some(at) = view.compute_point(&self.stage, &gtk::graphene::Point::new(x as f32, y as f32)) else { return };
        let (x, y) = (at.x().round() as i32, at.y().round() as i32);
        self.pointer.set((x, y));
        if self.dismissed.get() == Some((x, y)) {
            return;
        }
        self.dismissed.set(None);
        self.owner.set(Some(view));
        if self.bubble.is_visible() {
            // It stays where it is while the pointer moves over the same
            // thing, and follows to the next thing with a title.
            if self.label.text() != text {
                self.show(&text, x, y);
            }
            return;
        }
        *self.wanted.borrow_mut() = Some((text, x, y));
        if let Some(timer) = self.timer.take() {
            timer.remove();
        }
        let browsing = self.hidden_at.get().is_some_and(|t| t.elapsed() < REST);
        let tip = Rc::downgrade(self);
        let timer = glib::timeout_add_local_once(if browsing { REST_BROWSING } else { REST }, move || {
            let Some(tip) = tip.upgrade() else { return };
            tip.timer.take();
            if let Some((text, x, y)) = tip.wanted.take() {
                tip.show(&text, x, y);
            }
        });
        *self.timer.borrow_mut() = Some(timer);
    }

    /// Centered under the pointer, or above it at the bottom of the page,
    /// and always whole inside the page area.
    fn show(&self, text: &str, x: i32, y: i32) {
        self.label.set_text(text);
        let (stage_w, stage_h) = (self.stage.width(), self.stage.height());
        // Shown first: a hidden widget has no size to measure. Nothing is
        // drawn until this returns.
        self.bubble.set_margin_start(0);
        self.bubble.set_margin_top(0);
        self.bubble.set_visible(true);
        let w = self.bubble.measure(gtk::Orientation::Horizontal, -1).1.min(stage_w);
        let h = self.bubble.measure(gtk::Orientation::Vertical, w).1;
        let (left, top) = place(x, y, w, h, stage_w, stage_h);
        self.bubble.set_margin_start(left);
        self.bubble.set_margin_top(top);
    }

    /// Hide it if it is this view's (not for a tab in the background).
    fn hide_for(&self, view: &WebView) {
        if self.owner.upgrade().as_ref() == Some(view) {
            self.hide();
        }
    }

    fn hide(&self) {
        self.wanted.take();
        if let Some(timer) = self.timer.take() {
            timer.remove();
        }
        if self.bubble.is_visible() {
            self.bubble.set_visible(false);
            self.hidden_at.set(Some(Instant::now()));
        }
    }
}

/// The top left corner of a `w` by `h` label for a pointer at (`x`, `y`),
/// in an area `area_w` by `area_h`.
fn place(x: i32, y: i32, w: i32, h: i32, area_w: i32, area_h: i32) -> (i32, i32) {
    let left = (x - w / 2).min(area_w - w).max(0);
    let below = y + UNDER_POINTER;
    let top = if below + h > area_h { y - h - UNDER_POINTER / 2 } else { below };
    (left, top.max(0))
}

#[cfg(test)]
mod tests {
    use super::place;

    #[test]
    fn label_stays_inside_the_page() {
        // Under the pointer, centered.
        assert_eq!(place(400, 100, 200, 30, 1000, 600), (300, 120));
        // Not past the left or the right edge.
        assert_eq!(place(20, 100, 200, 30, 1000, 600), (0, 120));
        assert_eq!(place(990, 100, 200, 30, 1000, 600), (800, 120));
        // Above the pointer when there's no room under it.
        assert_eq!(place(400, 590, 200, 30, 1000, 600), (300, 550));
        // Wider than the page: from its left edge, not off to the left.
        assert_eq!(place(400, 100, 1200, 30, 1000, 600), (0, 120));
        assert_eq!(place(400, 10, 200, 700, 1000, 600), (300, 0));
    }
}
