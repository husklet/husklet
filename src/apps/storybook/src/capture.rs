//! Self-capture through GTK's own snapshot pipeline, so the catalogue can be
//! reviewed from a PNG with no screen-capture permission and no display server.

use gtk::prelude::*;

/// Writes the window to a PNG and quits, when asked to by the environment.
pub(crate) struct Shot;

impl Shot {
    const PATH: &'static str = "STORYBOOK_SHOT";
    const DELAY: &'static str = "STORYBOOK_SHOT_MS";
    const DEFAULT_DELAY: u64 = 700;

    pub(crate) fn schedule(window: &gtk::ApplicationWindow) {
        let Ok(path) = std::env::var(Self::PATH) else {
            return;
        };
        let delay = std::env::var(Self::DELAY)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(Self::DEFAULT_DELAY);
        let target = window.clone();
        gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(delay), move || {
            Self::capture(&target, &path);
            if let Some(application) = target.application() {
                application.quit();
            }
            target.close();
        });
    }

    /// Renders the full content height, not just the visible viewport, so a
    /// long catalogue is captured in one image.
    fn capture(window: &gtk::ApplicationWindow, path: &str) {
        let width = window.width().max(400);
        let height = Self::content_height(window).max(window.height()).max(300);
        let paintable = gtk::WidgetPaintable::new(Some(window.upcast_ref::<gtk::Widget>()));
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot(
            snapshot.upcast_ref::<gtk::gdk::Snapshot>(),
            f64::from(width),
            f64::from(height),
        );
        let (Some(node), Some(renderer)) = (snapshot.to_node(), window.renderer()) else {
            eprintln!("[storybook] capture failed: no render node");
            return;
        };
        match renderer.render_texture(&node, None).save_to_png(path) {
            Ok(()) => eprintln!("[storybook] wrote {path} ({width}x{height})"),
            Err(error) => eprintln!("[storybook] capture write failed: {error}"),
        }
    }

    fn content_height(window: &gtk::ApplicationWindow) -> i32 {
        window
            .child()
            .and_then(|child| child.downcast::<gtk::ScrolledWindow>().ok())
            .and_then(|scroll| scroll.child())
            .map_or(0, |content| content.measure(gtk::Orientation::Vertical, -1).1)
    }
}
