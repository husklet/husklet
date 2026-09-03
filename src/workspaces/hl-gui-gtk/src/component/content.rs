//! Long-form content: source text, a running log, media and plots.

use gtk::prelude::*;
use hl_gui::{LOG_VIEW_CHARACTER_LIMIT, Tag};

use super::field;

/// Content components.
pub(crate) fn widget(tag: Tag) -> gtk::Widget {
    match tag {
        Tag::CodeView => source().upcast(),
        Tag::HexView => source().upcast(),
        Tag::MarkdownView => markdown_view().upcast(),
        Tag::JsonView => json_view().upcast(),
        Tag::LogView => log().upcast(),
        Tag::Video => gtk::Video::new().upcast(),
        Tag::Chart => chart().upcast(),
        Tag::Sparkline => sparkline().upcast(),
        Tag::FlameGraph => flame_graph().upcast(),
        Tag::MemoryMap => memory_map().upcast(),
        Tag::DisassemblyView => memory_map().upcast(),
        Tag::TimelineView => memory_map().upcast(),
        Tag::TestReportView => memory_map().upcast(),
        Tag::CoverageView => memory_map().upcast(),
        Tag::DiffViewer => diff().upcast(),
        Tag::DiffLine => diff_line().upcast(),
        Tag::StackTrace => stack_trace().upcast(),
        Tag::StackFrame => stack_frame().upcast(),
        _ => chart().upcast(),
    }
}

fn json_view() -> gtk::ScrolledWindow {
    let window = field::editor(false);
    window.set_min_content_height(180);
    window
}

const JSON_INDENT_LIMIT: usize = 32;

/// Formats JSON punctuation without parsing values or recursing. Malformed
/// input remains visible; nesting beyond the semantic depth limit is clamped.
pub(crate) fn json(widget: &gtk::Widget, source: &str) -> bool {
    let Some(view) = field::view(widget) else { return false };
    let mut output = String::with_capacity(source.len());
    let mut depth = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    for character in source.chars() {
        if quoted {
            output.push(character);
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                quoted = false;
            }
            continue;
        }
        match character {
            '"' => {
                quoted = true;
                output.push(character);
            }
            '{' | '[' => {
                output.push(character);
                depth = (depth + 1).min(JSON_INDENT_LIMIT);
                newline(&mut output, depth);
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                newline(&mut output, depth);
                output.push(character);
            }
            ',' => {
                output.push(character);
                newline(&mut output, depth);
            }
            ':' => output.push_str(": "),
            value if value.is_whitespace() => {}
            value => output.push(value),
        }
    }
    view.buffer().set_text(&output);
    true
}

fn newline(output: &mut String, depth: usize) {
    output.push('\n');
    output.extend(std::iter::repeat_n(' ', depth * 2));
}

fn stack_trace() -> gtk::Box {
    let widget = super::axis::column(2);
    widget.set_hexpand(true);
    widget
}

fn stack_frame() -> gtk::Box {
    let widget = super::axis::column(0);
    let function = super::slot::caption_label();
    function.set_selectable(true);
    function.add_css_class("monospace");
    let location = super::axis::label();
    location.set_selectable(true);
    location.add_css_class("monospace");
    location.add_css_class("dim-label");
    super::slot::field(&location);
    widget.append(&function);
    widget.append(&location);
    widget
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

fn sparkline() -> gtk::DrawingArea {
    let widget = gtk::DrawingArea::new();
    widget.set_content_width(160);
    widget.set_content_height(40);
    widget.set_hexpand(true);
    widget.set_draw_func(|area, context, width, height| trend(area, context, f64::from(width), f64::from(height)));
    widget
}

fn flame_graph() -> gtk::ScrolledWindow {
    let rows = super::axis::column(2);
    rows.set_hexpand(true);
    let window = gtk::ScrolledWindow::new();
    window.set_child(Some(&rows));
    window.set_min_content_height(160);
    window.set_hexpand(true);
    window
}

fn memory_map() -> gtk::ScrolledWindow {
    let rows = super::axis::column(2);
    rows.set_hexpand(true);
    let window = gtk::ScrolledWindow::new();
    window.set_child(Some(&rows));
    window.set_min_content_height(200);
    window.set_hexpand(true);
    window
}

/// Replaces a process map while independently enforcing the adapter ceiling.
pub(crate) fn regions(widget: &gtk::Widget, value: &str) -> bool {
    widget.set_tooltip_text(Some(value));
    let Some(window) = widget.downcast_ref::<gtk::ScrolledWindow>() else {
        return false;
    };
    let mut held = window.child();
    let rows = loop {
        let Some(child) = held else { return false };
        if let Ok(rows) = child.clone().downcast::<gtk::Box>() {
            break rows;
        }
        held = child.first_child();
    };
    while let Some(child) = rows.first_child() {
        rows.remove(&child);
    }
    for line in value.lines().take(hl_gui::MEMORY_MAP_REGION_LIMIT) {
        let columns = line.splitn(4, '\t').collect::<Vec<_>>();
        if columns.len() != 4 {
            continue;
        }
        let row = super::axis::row(8);
        for (index, text) in columns.into_iter().enumerate() {
            let label = super::axis::label();
            label.set_text(text);
            label.set_selectable(true);
            label.set_xalign(0.0);
            label.add_css_class("monospace");
            label.set_width_chars(match index {
                0 => 35,
                1 => 4,
                2 => 10,
                _ => 24,
            });
            label.set_hexpand(index == 3);
            row.append(&label);
        }
        rows.append(&row);
    }
    true
}

/// Replaces a decoded instruction listing with four selectable columns.
pub(crate) fn instructions(widget: &gtk::Widget, value: &str) -> bool {
    widget.set_tooltip_text(Some(value));
    let Some(window) = widget.downcast_ref::<gtk::ScrolledWindow>() else {
        return false;
    };
    let mut held = window.child();
    let rows = loop {
        let Some(child) = held else { return false };
        if let Ok(rows) = child.clone().downcast::<gtk::Box>() {
            break rows;
        }
        held = child.first_child();
    };
    while let Some(child) = rows.first_child() {
        rows.remove(&child);
    }
    for line in value.lines().take(hl_gui::DISASSEMBLY_INSTRUCTION_LIMIT) {
        let columns = line.splitn(4, '\t').collect::<Vec<_>>();
        if columns.len() != 4 {
            continue;
        }
        let row = super::axis::row(8);
        for (index, text) in columns.into_iter().enumerate() {
            let label = super::axis::label();
            label.set_text(text);
            label.set_selectable(true);
            label.set_xalign(0.0);
            label.add_css_class("monospace");
            label.set_width_chars(match index {
                0 => 16,
                1 => 47,
                2 => 10,
                _ => 24,
            });
            label.set_hexpand(index == 3);
            row.append(&label);
        }
        rows.append(&row);
    }
    true
}

/// Replaces a chronology with four selectable native columns.
pub(crate) fn timeline(widget: &gtk::Widget, value: &str) -> bool {
    widget.set_tooltip_text(Some(value));
    let Some(window) = widget.downcast_ref::<gtk::ScrolledWindow>() else {
        return false;
    };
    let mut held = window.child();
    let rows = loop {
        let Some(child) = held else { return false };
        if let Ok(rows) = child.clone().downcast::<gtk::Box>() {
            break rows;
        }
        held = child.first_child();
    };
    while let Some(child) = rows.first_child() {
        rows.remove(&child);
    }
    for line in value.lines().take(hl_gui::TIMELINE_EVENT_LIMIT) {
        let columns = line.splitn(4, '\t').collect::<Vec<_>>();
        if columns.len() != 4 {
            continue;
        }
        let row = super::axis::row(8);
        for (index, text) in columns.into_iter().enumerate() {
            let label = super::axis::label();
            label.set_text(text);
            label.set_selectable(true);
            label.set_xalign(0.0);
            if index == 0 {
                label.add_css_class("monospace");
            }
            label.set_width_chars(match index {
                0 => 14,
                1 => 12,
                2 => 24,
                _ => 32,
            });
            label.set_hexpand(index == 3);
            row.append(&label);
        }
        rows.append(&row);
    }
    true
}

pub(crate) fn test_report(widget: &gtk::Widget, value: &str) -> bool {
    widget.set_tooltip_text(Some(value));
    let Some(window) = widget.downcast_ref::<gtk::ScrolledWindow>() else {
        return false;
    };
    let mut held = window.child();
    let rows = loop {
        let Some(child) = held else { return false };
        if let Ok(rows) = child.clone().downcast::<gtk::Box>() {
            break rows;
        }
        held = child.first_child();
    };
    while let Some(child) = rows.first_child() {
        rows.remove(&child);
    }
    for line in value.lines().take(hl_gui::TEST_REPORT_CASE_LIMIT) {
        let columns = line.splitn(5, '\t').collect::<Vec<_>>();
        if columns.len() != 5 {
            continue;
        }
        let row = super::axis::row(8);
        for (index, text) in columns.into_iter().enumerate() {
            let label = super::axis::label();
            label.set_text(text);
            label.set_selectable(true);
            label.set_xalign(0.0);
            if index == 3 || index == 4 {
                label.add_css_class("monospace");
            }
            label.set_width_chars(match index {
                0 => 16,
                1 => 28,
                2 => 8,
                3 => 10,
                _ => 36,
            });
            label.set_hexpand(index == 4);
            row.append(&label);
        }
        rows.append(&row);
    }
    true
}

pub(crate) fn coverage(widget: &gtk::Widget, value: &str) -> bool {
    widget.set_tooltip_text(Some(value));
    let Some(window) = widget.downcast_ref::<gtk::ScrolledWindow>() else {
        return false;
    };
    let mut held = window.child();
    let rows = loop {
        let Some(child) = held else { return false };
        if let Ok(rows) = child.clone().downcast::<gtk::Box>() {
            break rows;
        }
        held = child.first_child();
    };
    while let Some(child) = rows.first_child() {
        rows.remove(&child);
    }
    for (index, line) in value.lines().take(hl_gui::COVERAGE_VIEW_LINE_LIMIT + 1).enumerate() {
        if index == hl_gui::COVERAGE_VIEW_LINE_LIMIT && !line.starts_with("…\t\t… showing ") { break; }
        let columns = line.splitn(3, '\t').collect::<Vec<_>>();
        if columns.len() != 3 {
            continue;
        }
        let row = super::axis::row(8);
        if columns[1] == "0" {
            row.add_css_class("coverage-missed");
        }
        for (index, text) in columns.into_iter().enumerate() {
            let label = super::axis::label();
            label.set_text(if index == 1 && text == "0" { "—" } else { text });
            label.set_selectable(true);
            label.set_xalign(0.0);
            label.add_css_class("monospace");
            label.set_width_chars(match index {
                0 => 8,
                1 => 8,
                _ => 72,
            });
            label.set_hexpand(index == 2);
            row.append(&label);
        }
        rows.append(&row);
    }
    true
}

/// Replaces the profile projection, bounded independently of its producer.
pub(crate) fn flames(widget: &gtk::Widget, value: &str) -> bool {
    widget.set_tooltip_text(Some(value));
    let Some(window) = widget.downcast_ref::<gtk::ScrolledWindow>() else {
        return false;
    };
    let mut held = window.child();
    let rows = loop {
        let Some(child) = held else { return false };
        if let Ok(rows) = child.clone().downcast::<gtk::Box>() {
            break rows;
        }
        held = child.first_child();
    };
    while let Some(child) = rows.first_child() {
        rows.remove(&child);
    }
    let frames = value
        .lines()
        .filter_map(|line| {
            let (samples, label) = line.split_once('\t')?;
            let samples = samples.parse::<u64>().ok()?;
            (samples > 0 && !label.trim().is_empty()).then_some((samples, label))
        })
        .take(hl_gui::FLAME_GRAPH_FRAME_LIMIT)
        .collect::<Vec<_>>();
    let maximum = frames.iter().map(|(samples, _)| *samples).max().unwrap_or(1) as f64;
    for (samples, text) in frames {
        let row = super::axis::row(8);
        let label = super::axis::label();
        label.set_text(text);
        label.set_selectable(true);
        label.add_css_class("monospace");
        label.set_width_chars(24);
        label.set_xalign(0.0);
        let bar = gtk::ProgressBar::new();
        bar.set_fraction(samples as f64 / maximum);
        bar.set_text(Some(&samples.to_string()));
        bar.set_show_text(true);
        bar.set_hexpand(true);
        row.append(&label);
        row.append(&bar);
        rows.append(&row);
    }
    true
}

/// Stores the bounded textual samples for drawing; retained semantics continue
/// to own the same Value independently of this toolkit projection.
pub(crate) fn samples(widget: &gtk::Widget, value: &str) -> bool {
    if !super::belongs(widget, Tag::Sparkline) {
        return false;
    }
    widget.set_tooltip_text(Some(value));
    widget.queue_draw();
    true
}

fn trend(area: &gtk::DrawingArea, context: &gtk::cairo::Context, width: f64, height: f64) {
    let samples = area
        .tooltip_text()
        .unwrap_or_default()
        .split(',')
        .filter_map(|sample| sample.parse::<f64>().ok())
        .filter(|sample| sample.is_finite())
        .take(hl_gui::SPARKLINE_SAMPLE_LIMIT)
        .collect::<Vec<_>>();
    if samples.len() < 2 {
        return;
    }
    let minimum = samples.iter().copied().fold(f64::INFINITY, f64::min);
    let maximum = samples.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let span = (maximum - minimum).max(f64::EPSILON);
    let ink = area.color();
    context.set_source_rgba(
        ink.red().into(),
        ink.green().into(),
        ink.blue().into(),
        ink.alpha().into(),
    );
    context.set_line_width(2.0);
    for (index, sample) in samples.iter().enumerate() {
        let x = index as f64 * width / (samples.len() - 1) as f64;
        let y = height - ((*sample - minimum) / span * height);
        if index == 0 {
            context.move_to(x, y);
        } else {
            context.line_to(x, y);
        }
    }
    let _ = context.stroke();
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
