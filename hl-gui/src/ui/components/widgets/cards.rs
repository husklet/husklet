#![allow(unused_imports, dead_code)]
use gtk::prelude::*;

/// A card of key/value rows (selectable monospace values) for the Settings page.
pub(crate) fn setting_card(rows: &[(&str, &str)]) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
    card.add_css_class("dd-step-card");
    for (k, v) in rows {
        let key = gtk::Label::new(Some(k));
        key.set_xalign(0.0);
        key.set_width_request(72);
        key.add_css_class("dim-label");
        key.add_css_class("caption");
        let val = gtk::Label::new(Some(v));
        val.set_xalign(0.0);
        val.set_hexpand(true);
        val.set_selectable(true);
        val.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        val.add_css_class("dd-mono");
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        row.append(&key);
        row.append(&val);
        card.append(&row);
    }
    card
}

// ---- sparkline stat card --------------------------------------------------------------------------

/// A dashboard card: a big current value, a caption, and a sparkline of its recent history.
pub(crate) fn sparkline_card(
    title: &str,
    value: &str,
    series: Vec<f64>,
    accent: bool,
) -> gtk::Widget {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 6);
    card.add_css_class("dd-stat-card");
    card.set_hexpand(true);

    let val = gtk::Label::new(Some(value));
    val.set_xalign(0.0);
    val.add_css_class("dd-stat-value");
    if accent {
        val.add_css_class("accent");
    }
    let name = gtk::Label::new(Some(&title.to_uppercase()));
    name.set_xalign(0.0);
    name.add_css_class("dd-stat-name");
    card.append(&val);
    card.append(&name);

    let area = gtk::DrawingArea::new();
    area.set_content_height(38);
    area.set_hexpand(true);
    area.set_margin_top(4);
    area.set_draw_func(move |_, cr, w, h| draw_sparkline(cr, w, h, &series, accent));
    card.append(&area);
    card.upcast()
}

fn draw_sparkline(cr: &gtk::cairo::Context, w: i32, h: i32, data: &[f64], accent: bool) {
    if data.len() < 2 {
        return;
    }
    let (w, h) = (w as f64, h as f64);
    let (mut lo, mut hi) = (f64::MAX, f64::MIN);
    for &v in data {
        lo = lo.min(v);
        hi = hi.max(v);
    }
    if (hi - lo).abs() < 1e-9 {
        hi = lo + 1.0;
    }
    let n = data.len();
    let x = |i: usize| (i as f64) / ((n - 1) as f64) * w;
    let y = |v: f64| h - 3.0 - ((v - lo) / (hi - lo)) * (h - 6.0);
    let (r, g, b) = if accent {
        (0.17, 0.79, 0.35)
    } else {
        (0.04, 0.52, 1.0)
    };

    // soft area fill under the curve
    cr.move_to(0.0, h);
    for (i, &v) in data.iter().enumerate() {
        cr.line_to(x(i), y(v));
    }
    cr.line_to(w, h);
    cr.close_path();
    cr.set_source_rgba(r, g, b, 0.10);
    let _ = cr.fill();

    // the line itself
    for (i, &v) in data.iter().enumerate() {
        if i == 0 {
            cr.move_to(x(i), y(v));
        } else {
            cr.line_to(x(i), y(v));
        }
    }
    cr.set_source_rgba(r, g, b, 0.9);
    cr.set_line_width(1.6);
    let _ = cr.stroke();
}

/// A compact key/value detail block (denser than stacked sections). Empty values show an em-dash.
pub(crate) fn two_col(rows: &[(&str, String)]) -> gtk::Widget {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
    for (k, v) in rows {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        row.set_margin_top(3);
        row.set_margin_bottom(3);
        let key = gtk::Label::new(Some(k));
        key.set_xalign(0.0);
        key.add_css_class("dd-kv-key");
        let val = gtk::Label::new(Some(if v.is_empty() { "—" } else { v.as_str() }));
        val.set_xalign(0.0);
        val.set_hexpand(true);
        val.set_wrap(true);
        // Only real values are selectable — an empty "—" shouldn't show a text cursor / be clickable.
        val.set_selectable(!v.is_empty());
        val.add_css_class("dd-kv-val");
        row.append(&key);
        row.append(&val);
        card.append(&row);
    }
    card.upcast()
}
