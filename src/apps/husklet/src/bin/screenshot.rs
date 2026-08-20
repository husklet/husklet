//! Debug self-capture of a Husklet window, through GTK's own snapshot pipeline.
//!
//! The application has no monitor on a build host, and screen capture needs a permission no CI
//! runner grants. Rendering the window to a texture from inside the process needs neither, so this
//! is how the UI is verified headlessly.

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;

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
            Self::capture(&win, &path);
            let application = win.application();
            win.close();
            if let Some(application) = application {
                application.quit();
            }
        });
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
