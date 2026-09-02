//! Long-form content: source text, a running log, media and plots.

use gtk::prelude::*;
use hl_gui::{Tag, LOG_VIEW_CHARACTER_LIMIT};

use super::field;

/// Content components.
pub(crate) fn widget(tag: Tag) -> gtk::Widget {
    match tag {
        Tag::CodeView => source().upcast(),
        Tag::MarkdownView => markdown_view().upcast(),
        Tag::LogView => log().upcast(),
        Tag::Video => gtk::Video::new().upcast(),
        Tag::Chart => chart().upcast(),
        Tag::DiffViewer => diff().upcast(),
        Tag::DiffLine => diff_line().upcast(),
        _ => chart().upcast(),
    }
}

fn diff() -> gtk::Box {
    let widget = super::axis::column(2);
    widget.add_css_class("view");
    widget.set_hexpand(true);
    widget
}

fn diff_line() -> gtk::Box {
    let widget = super::axis::row(8);
    let status = super::slot::caption_label();
    status.add_css_class("dim-label");
    status.set_width_chars(3);
    let content = super::axis::label();
    content.set_selectable(true);
    content.set_wrap(false);
    content.set_xalign(0.0);
    content.set_hexpand(true);
    content.add_css_class("monospace");
    super::slot::field(&content);
    widget.append(&status);
    widget.append(&content);
    widget
}

fn markdown_view() -> gtk::ScrolledWindow {
    let label = super::axis::label();
    label.set_selectable(true);
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.set_xalign(0.0);
    label.set_yalign(0.0);
    let window = gtk::ScrolledWindow::new();
    window.set_child(Some(&label));
    window.set_min_content_height(120);
    window.set_hexpand(true);
    window.set_vexpand(true);
    window
}

/// Renders a deliberately small, safe Markdown subset. Text is escaped before
/// Pango sees it, so authored HTML and markup remain inert and visible.
pub(crate) fn markdown(widget: &gtk::Widget, source: &str) -> bool {
    let Some(window) = widget.downcast_ref::<gtk::ScrolledWindow>() else {
        return false;
    };
    let mut held = window.child();
    let label = loop {
        let Some(widget) = held else { return false };
        if let Ok(label) = widget.clone().downcast::<gtk::Label>() {
            break label;
        }
        held = widget.first_child();
    };
    let mut fenced = false;
    let mut rendered = Vec::new();
    for line in source.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        let (text, open, close) = if fenced {
            (line, "<tt>", "</tt>")
        } else if let Some(text) = line.strip_prefix("### ") {
            (text, "<span size=\"large\" weight=\"bold\">", "</span>")
        } else if let Some(text) = line.strip_prefix("## ") {
            (text, "<span size=\"x-large\" weight=\"bold\">", "</span>")
        } else if let Some(text) = line.strip_prefix("# ") {
            (text, "<span size=\"xx-large\" weight=\"bold\">", "</span>")
        } else if let Some(text) = line.strip_prefix("> ") {
            (text, "<i>│ ", "</i>")
        } else if let Some(text) = line.strip_prefix("- ") {
            (text, "• ", "")
        } else {
            (line, "", "")
        };
        rendered.push(format!("{open}{}{close}", gtk::glib::markup_escape_text(text)));
    }
    label.set_markup(&rendered.join("\n"));
    true
}

/// A read-only monospaced view of source text.
///
/// Without line numbers: numbering needs a gutter widget that re-measures on
/// every edit, which is what `GtkSourceView` exists for, and that library is
/// not a dependency here. A wrong or drifting gutter would be worse than none.
fn source() -> gtk::ScrolledWindow {
    let window = field::editor(false);
    window.set_min_content_height(240);
    if let Some(view) = field::view(&window.clone().upcast()) {
        view.set_wrap_mode(gtk::WrapMode::None);
    }
    window
}

/// An append-only view that follows its tail.
fn log() -> gtk::ScrolledWindow {
    let window = field::editor(false);
    window.set_vexpand(true);
    window
}

/// Appends a line to a log and follows it.
///
/// Appending rather than replacing is the component's contract: a producer
/// streams what happened since the last frame instead of resending the whole
/// log, which is what keeps a long-running log affordable on the wire.
pub(crate) fn append(widget: &gtk::Widget, content: &str) -> bool {
    let Some(view) = field::view(widget) else {
        return false;
    };
    let buffer = view.buffer();
    buffer.insert(&mut buffer.end_iter(), content);
    let excess = buffer.char_count().saturating_sub(LOG_VIEW_CHARACTER_LIMIT);
    if excess > 0 {
        let mut start = buffer.start_iter();
        let mut retained = buffer.iter_at_offset(excess);
        buffer.delete(&mut start, &mut retained);
    }
    follow(widget, &view);
    true
}

/// Scrolls to the end, so the newest line is the one on screen.
fn follow(widget: &gtk::Widget, view: &gtk::TextView) {
    let mark = view.buffer().create_mark(None, &view.buffer().end_iter(), false);
    view.scroll_mark_onscreen(&mark);
    let Some(window) = widget.downcast_ref::<gtk::ScrolledWindow>() else {
        return;
    };
    let adjustment = window.vadjustment();
    adjustment.set_value(adjustment.upper());
}

/// Charts have no toolkit equivalent, so the adapter paints them itself.
///
/// What it paints is a framed, labelled plot area and nothing more. A series
/// is a list of numbers, and the property vocabulary has no such value:
/// `PropValue` carries one number, one length, or a list of label/value
/// *strings*, and the windowed row protocol reaches a `gtk::ColumnView`, not a
/// drawing area. Reading a series out of `Prop::Choices` would widen the
/// meaning of a wire type without widening the type, which is the same change
/// with worse documentation — so the frame is drawn honestly and the data
/// awaits a series value on the wire.
fn chart() -> gtk::DrawingArea {
    let widget = gtk::DrawingArea::new();
    widget.set_content_height(120);
    widget.set_hexpand(true);
    widget.set_draw_func(|area, context, width, height| plot(area, context, f64::from(width), f64::from(height)));
    widget
}

/// Paints the plot frame in the widget's own inherited colour, so the sheet
/// still owns the palette and no per-widget provider is attached.
fn plot(area: &gtk::DrawingArea, context: &gtk::cairo::Context, width: f64, height: f64) {
    let ink = area.color();
    let inset = 8.0_f64;
    context.set_line_width(1.0);
    context.set_source_rgba(ink.red().into(), ink.green().into(), ink.blue().into(), 0.35);
    context.rectangle(
        inset,
        inset,
        (width - inset * 2.0).max(0.0),
        (height - inset * 2.0).max(0.0),
    );
    let _ = context.stroke();
    caption(area, context, width, height);
}

/// The chart's label, centred in the plot area. `Prop::Label` reaches the
/// drawing area as its tooltip — a drawing area holds no text of its own — so
/// that is where the caption is read from.
fn caption(area: &gtk::DrawingArea, context: &gtk::cairo::Context, width: f64, height: f64) {
    let ink = area.color();
    let text = area.tooltip_text().unwrap_or_else(|| "Chart".into());
    context.set_source_rgba(ink.red().into(), ink.green().into(), ink.blue().into(), 0.7);
    context.set_font_size(12.0);
    let Ok(extents) = context.text_extents(&text) else {
        return;
    };
    context.move_to((width - extents.width()) / 2.0, f64::midpoint(height, extents.height()));
    let _ = context.show_text(&text);
}
