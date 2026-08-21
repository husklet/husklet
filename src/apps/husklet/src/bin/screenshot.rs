//! Debug hooks that drive and capture a Husklet window on a host with no monitor.
//!
//! The application has no monitor on a build host, and screen capture needs a permission no CI
//! runner grants. Rendering the window to a texture from inside the process needs neither, so this
//! is how the UI is verified headlessly.

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use vte4::prelude::*;

use crate::AppConfig;

/// Debug self-capture: with `HL_TERM_SHOT=<png>` (and `HL_TERM_VIEW=manager|terminal|newws` to pick the
/// surface), render this window to a PNG via GTK's own snapshot pipeline and exit — no OS screen-capture
/// permission needed. Used to verify the UI headlessly.
pub(crate) struct Screenshot;

impl Screenshot {
    pub(crate) fn schedule(window: &gtk::ApplicationWindow, tag: &str) {
        let Some(path) = AppConfig::get().screenshot.clone() else {
            return;
        };
        if AppConfig::get().view.as_deref().unwrap_or("manager") != tag {
            return;
        }
        let ms = AppConfig::get().screenshot_ms;
        let win = window.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(ms), move || {
            Self::write_pane_text(&win);
            Self::capture(&win, &path);
            let application = win.application();
            win.close();
            if let Some(application) = application {
                application.quit();
            }
        });
    }

    /// Debug: `HL_TERM_RESIZE=<ms>:<width>x<height>` resizes this window, in pixels, at
    /// that offset from opening.
    ///
    /// This is the resize a developer performs by dragging the window edge, on a host
    /// with no window manager to drag it with. It deliberately resizes the *window*
    /// rather than calling `set_size` on the panes: a pane is `hexpand`/`vexpand` inside
    /// its layout, so a grid set directly is overwritten by the next allocation and the
    /// tty never learns anything -- measured, the guest still reported the old geometry
    /// while the hook reported success.
    pub(crate) fn schedule_resize(window: &gtk::ApplicationWindow) {
        let Some(request) = AppConfig::get().resize.clone() else {
            return;
        };
        let Some((offset, geometry)) = request.split_once(':') else {
            return;
        };
        let Some((width, height)) = geometry.split_once('x') else {
            return;
        };
        let (Ok(offset), Ok(width), Ok(height)) = (offset.parse::<u64>(), width.parse::<i32>(), height.parse::<i32>())
        else {
            return;
        };
        let window = window.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(offset), move || {
            window.set_default_size(width, height);
            let mut panes = Vec::new();
            crate::screens::workspace::terminal::PaneView::all(window.upcast_ref::<gtk::Widget>(), &mut panes);
            eprintln!(
                "[husklet] resized the window to {width}x{height} px over {} pane(s)",
                panes.len()
            );
        });
    }

    /// Debug: with `HL_TERM_TEXT=<path>`, write what every pane in this window is
    /// showing, as text, beside the PNG.
    ///
    /// A screenshot proves pixels were produced; it cannot say which glyphs they
    /// are without a human or an OCR pass. This reads the same grid VTE drew from,
    /// so a headless run can assert on the output of a command it typed.
    fn write_pane_text(window: &gtk::ApplicationWindow) {
        let Some(path) = AppConfig::get().pane_text.clone() else {
            return;
        };
        let mut panes = Vec::new();
        crate::screens::workspace::terminal::PaneView::all(window.upcast_ref::<gtk::Widget>(), &mut panes);
        let mut text = String::new();
        for (index, pane) in panes.iter().enumerate() {
            let (rows, _older) = crate::screens::workspace::terminal::Terminal::new(pane).tail(400);
            text.push_str(&format!(
                "--- pane {index} grid {}x{} ---\n",
                pane.column_count(),
                pane.row_count()
            ));
            for row in rows {
                text.push_str(&row);
                text.push('\n');
            }
        }
        match std::fs::write(&path, text) {
            Ok(()) => eprintln!("[husklet] wrote pane text {path} ({} panes)", panes.len()),
            Err(error) => eprintln!("[husklet] pane text write failed for {path}: {error}"),
        }
    }

    fn capture(window: &gtk::ApplicationWindow, path: &str) {
        let width = window.width().max(400);
        let height = window.height().max(300);
        let paintable = gtk::WidgetPaintable::new(Some(window.upcast_ref::<gtk::Widget>()));
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot(snapshot.upcast_ref::<gdk::Snapshot>(), width as f64, height as f64);
        let (Some(node), Some(renderer)) = (snapshot.to_node(), window.renderer()) else {
            eprintln!("[husklet] screenshot failed: no render node/renderer");
            return;
        };
        let texture = renderer.render_texture(&node, None);
        match texture.save_to_png(path) {
            Ok(()) => eprintln!("[husklet] wrote screenshot {path} ({width}x{height})"),
            Err(error) => eprintln!("[husklet] screenshot write failed for {path}: {error}"),
        }
    }
}
