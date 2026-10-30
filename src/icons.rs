//! Icons drawn from SVG path data in the widget's text color, so they show up
//! whatever icon theme is installed and without an SVG image loader.

use gtk::prelude::*;
use gtk::{cairo, gsk};

pub const NEW_TAB: &str = include_str!("icons/wisp-tab-new-symbolic.svg");

/// A 16 px icon made of the `<path>` elements of a single-color SVG.
/// Returns None when the SVG has no usable paths.
pub fn from_svg(svg: &str) -> Option<gtk::Widget> {
    let view = attr(svg, "viewBox")
        .and_then(|v| v.split_whitespace().nth(2)?.parse::<f64>().ok())
        .unwrap_or(16.0);
    let paths: Vec<(gsk::Path, cairo::FillRule)> = svg
        .split("<path")
        .skip(1)
        .filter_map(|el| {
            let el = &el[..el.find('>')?];
            let path = gsk::Path::parse(attr(el, "d")?).ok()?;
            let rule = if attr(el, "fill-rule") == Some("evenodd") { cairo::FillRule::EvenOdd } else { cairo::FillRule::Winding };
            Some((path, rule))
        })
        .collect();
    if paths.is_empty() {
        return None;
    }
    // Fixed 16x16 and centered, so a wide button can't stretch it.
    let area = gtk::DrawingArea::builder()
        .content_width(16)
        .content_height(16)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();
    area.set_draw_func(move |area, cr, width, height| {
        let c = area.color();
        let scale = width.min(height) as f64 / view;
        cr.translate((width as f64 - view * scale) / 2.0, (height as f64 - view * scale) / 2.0);
        cr.scale(scale, scale);
        cr.set_source_rgba(c.red().into(), c.green().into(), c.blue().into(), c.alpha().into());
        for (path, rule) in &paths {
            cr.new_path();
            path.to_cairo(cr);
            cr.set_fill_rule(*rule);
            let _ = cr.fill();
        }
    });
    Some(area.upcast())
}

/// The value of `name="..."` inside an element's text.
fn attr<'a>(element: &'a str, name: &str) -> Option<&'a str> {
    let start = element.find(&format!(" {name}=\""))? + name.len() + 3;
    Some(&element[start..start + element[start..].find('"')?])
}
