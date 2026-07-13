#![allow(unused_imports, dead_code)]
use gtk::prelude::*;
use std::ffi::OsStr;

// ---- bundle environment ----------------------------------------------------

/// When running from inside `dd.app`, point GTK at the bundled runtime data. No-op for a dev
/// build (the Resources/Frameworks dirs won't exist), and never overrides an env var already set.
pub fn setup_bundle_env() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Some(contents) = exe.parent().and_then(|p| p.parent()) else {
        return;
    };
    let res = contents.join("Resources");
    let fw = contents.join("Frameworks");
    if !res.exists() || !fw.exists() {
        return; // not a bundle — dev run
    }
    let loaders = res.join("lib/gdk-pixbuf-2.0/2.10.0/loaders");
    set_if_absent(
        "GSETTINGS_SCHEMA_DIR",
        res.join("glib-2.0/schemas").as_os_str(),
    );
    set_if_absent("GSETTINGS_BACKEND", OsStr::new("memory"));
    set_if_absent(
        "GDK_PIXBUF_MODULE_FILE",
        loaders.join("loaders.cache").as_os_str(),
    );
    set_if_absent("GDK_PIXBUF_MODULEDIR", loaders.as_os_str());
    set_if_absent("XDG_DATA_DIRS", res.as_os_str());
    set_if_absent("XDG_DATA_HOME", res.as_os_str());
    set_if_absent("GTK_PATH", fw.as_os_str());
    set_if_absent(
        "FONTCONFIG_FILE",
        res.join("fontconfig/fonts.conf").as_os_str(),
    );
    set_if_absent("GSK_RENDERER", OsStr::new("gl"));
}

pub(crate) fn set_if_absent(key: &str, val: &OsStr) {
    if std::env::var_os(key).is_none() {
        std::env::set_var(key, val);
    }
}

// ---- headless verification ------------------------------------------------------------------------
// Render the live window to a PNG offscreen so the UI can be verified without an interactive session
// (`DD_SHOT=/path/out.png dd-app` screenshots once, then quits). Uses the window's own GSK renderer
// against a WidgetPaintable — no extra window, no user input. Pair with `GSK_RENDERER=cairo` for a
// deterministic software render.
pub fn screenshot(win: &gtk::ApplicationWindow, path: &str) -> Result<(), String> {
    let w = win.width().max(1);
    let h = win.height().max(1);
    let paintable = gtk::WidgetPaintable::new(Some(win));
    let snapshot = gtk::Snapshot::new();
    PaintableExt::snapshot(&paintable, &snapshot, w as f64, h as f64);
    let node = snapshot
        .to_node()
        .ok_or("empty render tree (window not drawn yet)")?;
    let renderer = win
        .renderer()
        .ok_or("window has no GSK renderer (not realized)")?;
    let texture = renderer.render_texture(&node, None);
    texture.save_to_png(path).map_err(|e| e.to_string())
}
