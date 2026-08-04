//! Husklet — a workspace manager, terminal, and host-surface composition root.
//! New-Workspace configuration window, and a per-workspace Terminal window you launch from the manager.
//!
//! * Native macOS title bars (real traffic lights); content — including the full-width tab strip — sits
//!   directly below, so nothing needs a traffic-light gap.
//! * DAEMON-FREE launch: each terminal tab runs `hl workspace launch <name>`, entering the image
//!   through the container domain. GPU-rendered through GTK4's GSK renderer; VTE is the grid.
//! * No onboarding, no popups — the app opens straight onto workspaces.
//!
//! Build + run on macOS: `nix develop . -c cargo run -p husklet --features gui --bin husklet`.

use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use vte4::prelude::*;
use vte4::TerminalExtManual;

use hl::config::{CudaDevice, TerminalPreferences, VpnConfig, WorkspaceConfig, WorkspaceStore};
use hl_ws::{Arch, Mount};
use hl_ws_term::config::{CursorShape, TermConfig};
use hl_ws_term::session::{self, Pane, PaneNode, Session, SessionTab, SplitDir};

mod components;
mod gtk_adapter;
mod host;
pub mod screens;

use components::dialog::RemoveWorkspace;
use components::layout::Field;
use components::theme::{css, ACCENT};
use components::workspace::{build_form, Form};
use gtk_adapter::{ColorPicker, FontPicker};
use host::process::ProcessGroup;
use host::pty::PtyProcess;
use host::{
    command::{application_path, Hl},
    home::Home,
};
use screens::workspace::overview::Overview;
use screens::workspace::terminal::{Terminal, TerminalWindow};

const APP_ID: &str = "com.husklet.app";

// One committed near-black palette (matches the design mockup).
// Per-workspace terminal resolution.
// Per-workspace terminal resolution.
// =================================================================================================

// PCRE2 flags for VTE's regex engine (search + URL match). VTE requires UTF + MULTILINE.
const PCRE2_CASELESS: u32 = 0x0000_0008;
const PCRE2_MULTILINE: u32 = 0x0000_0400;
const PCRE2_UCP: u32 = 0x0002_0000;
const PCRE2_UTF: u32 = 0x0008_0000;
const PCRE2_NO_UTF_CHECK: u32 = 0x4000_0000;

struct ToggleValue;

impl ToggleValue {
    fn cursor(button: &gtk::ToggleButton, value: Rc<Cell<CursorShape>>, selected: CursorShape) {
        button.connect_toggled(move |button| {
            if button.is_active() {
                value.set(selected);
            }
        });
    }
}

#[derive(Clone)]
struct Application(gtk::Application);

#[derive(Default)]
struct AppConfig {
    view: Option<String>,
    workspace: Option<String>,
    new_workspace_pane: Option<String>,
    open_color_picker: bool,
    tabs: Option<usize>,
    split: Option<String>,
    overview: bool,
    command: Option<String>,
    debug_log: Option<String>,
    typed_text: Option<String>,
    overview_pane: Option<String>,
    screenshot: Option<String>,
    screenshot_ms: u64,
    environment: host::environment::Environment,
    home: std::path::PathBuf,
}

impl AppConfig {
    fn parse() -> Self {
        Self {
            view: std::env::var("HL_TERM_VIEW").ok(),
            workspace: std::env::var("HL_TERM_WS").ok(),
            new_workspace_pane: std::env::var("HL_TERM_NEWWS_PANE").ok(),
            open_color_picker: std::env::var("HL_TERM_OPEN_COLOR").is_ok(),
            tabs: std::env::var("HL_TERM_TABS").ok().and_then(|value| value.parse().ok()),
            split: std::env::var("HL_TERM_SPLIT").ok(),
            overview: std::env::var("HL_TERM_OVERVIEW").is_ok(),
            command: std::env::var("HL_TERM_CMD").ok(),
            debug_log: std::env::var("HL_TERM_DEBUG_LOG").ok(),
            typed_text: std::env::var("HL_TERM_TYPE").ok(),
            overview_pane: std::env::var("HL_TERM_OVERVIEW_PAGE").ok(),
            screenshot: std::env::var("HL_TERM_SHOT").ok(),
            screenshot_ms: std::env::var("HL_TERM_SHOT_MS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(2500),
            home: hl::paths::home(),
            environment: host::environment::Environment::capture(),
        }
    }

    fn get() -> &'static Self {
        APP_CONFIG.get().expect("application config initialized in main")
    }
}

static APP_CONFIG: std::sync::OnceLock<AppConfig> = std::sync::OnceLock::new();

fn main() -> glib::ExitCode {
    hl::logging::configure();
    if let Some(code) = host::worker::Worker::run() {
        return glib::ExitCode::from(code);
    }
    host::runtime::Runtime::configure();
    APP_CONFIG
        .set(AppConfig::parse())
        .ok()
        .expect("application config initialized once");
    let app = gtk::Application::builder().application_id(APP_ID).build();
    app.connect_startup(|application| {
        if let Some(s) = gtk::Settings::default() {
            s.set_gtk_application_prefer_dark_theme(true);
        }
        host::appearance::Appearance::apply();
        let p = gtk::CssProvider::new();
        p.load_from_data(&css());
        if let Some(d) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(&d, &p, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        }
        let quit = gio::SimpleAction::new("quit", None);
        let app = application.clone();
        quit.connect_activate(move |_, _| app.quit());
        application.add_action(&quit);
        application.set_accels_for_action("app.quit", &["<Primary>q"]);

        let menu = gio::Menu::new();
        menu.append(Some("Quit Husklet"), Some("app.quit"));
        application.set_menubar(Some(&menu));
    });
    app.connect_activate(|application| Application(application.clone()).open_manager());
    app.run()
}

// =================================================================================================
// Window 1 — Workspace Manager
// =================================================================================================

impl Application {
    fn open_manager(&self) {
        let window = gtk::ApplicationWindow::builder()
            .application(&self.0)
            .title("Husklet")
            .default_width(480)
            .default_height(560)
            .build();

        let home = screens::home::View::new();

        let refresh: Rc<dyn Fn()> = {
            let application = self.clone();
            let list = home.workspaces.clone();
            Rc::new(move || application.refresh_workspace_list(&list))
        };
        {
            let app = self.0.clone();
            let refresh = refresh.clone();
            home.create.connect_clicked(move |_| Form::open(&app, refresh.clone()));
        }
        refresh();

        window.set_child(Some(&home.widget));
        window.present();
        host::appearance::Appearance::apply();
        Screenshot::schedule(&window, "manager");

        // Debug: jump straight to a surface for headless screenshotting.
        match AppConfig::get().view.as_deref() {
            Some("terminal") => {
                if let Ok(store) = WorkspaceStore::load(Home::current().workspaces_config()) {
                    let want = AppConfig::get().workspace.as_deref();
                    let ws = want.and_then(|n| store.get(n)).or_else(|| store.all().first());
                    if let Some(ws) = ws {
                        self.open_terminal(ws);
                    }
                }
            }
            Some("newws") => {
                let noop: Rc<dyn Fn()> = Rc::new(|| {});
                Form::open(&self.0, noop);
            }
            _ => {}
        }
    }

    fn refresh_workspace_list(&self, list: &gtk::ListBox) {
        while let Some(c) = list.first_child() {
            list.remove(&c);
        }
        let store = match WorkspaceStore::load(Home::current().workspaces_config()) {
            Ok(store) => store,
            Err(error) => {
                let row = gtk::ListBoxRow::new();
                row.set_selectable(false);
                let message = gtk::Label::new(Some(&format!(
                    "Could not load workspaces: {error}\nThe configuration was left unchanged."
                )));
                message.add_css_class("error");
                message.set_wrap(true);
                message.set_xalign(0.0);
                row.set_child(Some(&message));
                list.append(&row);
                return;
            }
        };
        if store.all().is_empty() {
            let row = gtk::ListBoxRow::new();
            row.set_selectable(false);
            let e = gtk::Label::new(Some("No workspaces yet — click + New to create one."));
            e.add_css_class("empty");
            row.set_child(Some(&e));
            list.append(&row);
            return;
        }
        for ws in store.all() {
            list.append(&self.workspace_row(ws, list));
        }
    }

    fn open_terminal(&self, ws: &WorkspaceConfig) {
        TerminalWindow::open(&self.0, ws);
    }
}

/// A prominent, color-coded os/arch badge (used beside a overview title). Shows the full
/// `os/arch` label (e.g. `linux/aarch64`) rather than a terse `arm`/`amd`.
struct ArchitectureView(Arch);

impl ArchitectureView {
    fn chip(&self) -> gtk::Label {
        let l = gtk::Label::new(Some(self.0.os_arch_label()));
        l.add_css_class("chip");
        l.add_css_class(match self.0 {
            Arch::Arm64 => "arm",
            Arch::Amd64 => "amd",
        });
        l.set_valign(gtk::Align::Center);
        l
    }
}

impl Application {
    fn workspace_row(&self, ws: &WorkspaceConfig, list: &gtk::ListBox) -> gtk::ListBoxRow {
        let row = gtk::ListBoxRow::new();
        let bx = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        bx.add_css_class("wsrow");

        let info = gtk::Box::new(gtk::Orientation::Vertical, 3);
        info.set_hexpand(true);
        let nm = gtk::Label::new(Some(&ws.name));
        nm.set_xalign(0.0);
        nm.add_css_class("nm");
        // os/arch is folded into the subtitle line as plain dim text (no chip/pill): the name on top,
        // then one clean meta line "linux/aarch64 · image · ~/path" beneath it.
        let home = Home::current();
        let dir = ws.storage_dir(&home.root());
        let meta = gtk::Label::new(Some(&format!(
            "{} · {} · {}",
            ws.arch.os_arch_label(),
            ws.image,
            home.display(&dir)
        )));
        meta.set_xalign(0.0);
        meta.add_css_class("mt");
        meta.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        info.append(&nm);
        info.append(&meta);
        bx.append(&info);

        // ▶ Play — launch the workspace.
        let play = gtk::Button::from_icon_name("media-playback-start-symbolic");
        play.add_css_class("rowbtn");
        play.set_valign(gtk::Align::Center);
        play.set_tooltip_text(Some("Launch workspace"));
        {
            let app2 = self.0.clone();
            let ws2 = ws.clone();
            play.connect_clicked(move |_| Application(app2.clone()).open_terminal(&ws2));
        }
        bx.append(&play);

        // ⋯ three-dots menu → a popover with per-workspace actions (Remove for now).
        let menu = gtk::Button::new();
        menu.add_css_class("rowbtn");
        menu.set_valign(gtk::Align::Center);
        menu.set_tooltip_text(Some("More"));
        let dots = gtk::Label::new(Some("\u{22ef}")); // ⋯ (reliable glyph vs a maybe-missing symbolic icon)
        dots.add_css_class("dots");
        menu.set_child(Some(&dots));
        let pop = gtk::Popover::new();
        pop.add_css_class("rowmenu");
        let pbox = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let settings = gtk::Button::with_label("Settings…");
        settings.add_css_class("menuitem");
        {
            let app = self.0.clone();
            let workspace = ws.clone();
            let popover = pop.clone();
            settings.connect_clicked(move |_| {
                hl_log::hl_info!(hl_log::tag::UI, "workspace settings action selected");
                popover.popdown();
                TerminalWindow::settings(&app, &workspace);
            });
        }
        pbox.append(&settings);
        let remove = gtk::Button::with_label("Remove workspace");
        remove.add_css_class("menuitem");
        {
            let name = ws.name.clone();
            let app2 = self.0.clone();
            let list2 = list.clone();
            let pop2 = pop.clone();
            remove.connect_clicked(move |_| {
                hl_log::hl_info!(hl_log::tag::UI, "workspace removal action selected");
                pop2.popdown();
                let parent = app2.active_window();
                let app = app2.clone();
                let list = list2.clone();
                let workspace_name = name.clone();
                RemoveWorkspace::new(name.clone()).present(parent.as_ref(), move || {
                    let mut store = WorkspaceStore::load(Home::current().workspaces_config())?;
                    if !store.remove(&workspace_name)? {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::NotFound,
                            format!("Workspace {workspace_name:?} no longer exists."),
                        ));
                    }
                    Application(app.clone()).refresh_workspace_list(&list);
                    Ok(())
                });
            });
        }
        pbox.append(&remove);
        pop.set_child(Some(&pbox));
        pop.set_autohide(true);
        pop.set_position(gtk::PositionType::Bottom);
        pop.set_parent(&menu);
        {
            let popover = pop.clone();
            let first_action = settings.clone();
            menu.connect_clicked(move |_| {
                if popover.is_visible() {
                    hl_log::hl_info!(hl_log::tag::UI, "workspace menu closing from anchor");
                    popover.popdown();
                } else {
                    hl_log::hl_info!(hl_log::tag::UI, "workspace menu opening from anchor");
                    popover.popup();
                    let first_action = first_action.clone();
                    glib::idle_add_local_once(move || {
                        first_action.grab_focus();
                    });
                }
            });
        }
        bx.append(&menu);

        row.set_child(Some(&bx));
        row
    }
}

impl Terminal<'_> {
    fn style(&self, cfg: &TermConfig) {
        let term = self.0;
        let mut font = gtk::pango::FontDescription::from_string(&cfg.font_string());
        if font.family().is_none() {
            font = gtk::pango::FontDescription::from_string(&format!("monospace {}", cfg.font_size as i64));
        }
        term.set_font(Some(&font));
        term.set_cell_height_scale(1.0);
        term.set_scrollback_lines(cfg.scrollback_lines()); // unlimited by default; make_terminal applies any per-ws cap
        term.set_audible_bell(false);
        term.set_hexpand(true);
        term.set_vexpand(true);
        term.set_cursor_blink_mode(if cfg.cursor_blink {
            vte4::CursorBlinkMode::On
        } else {
            vte4::CursorBlinkMode::Off
        });
        term.set_cursor_shape(match cfg.cursor_shape {
            CursorShape::Block => vte4::CursorShape::Block,
            CursorShape::Beam => vte4::CursorShape::Ibeam,
            CursorShape::Underline => vte4::CursorShape::Underline,
        });
        // OSC 8 hyperlinks: let VTE parse explicit links (URL auto-matching is added per-terminal).
        term.set_allow_hyperlink(true);
        let hex = |s: &str| gdk::RGBA::parse(s).unwrap_or_else(|_| gdk::RGBA::parse("#ffffff").unwrap());
        let palette: Vec<gdk::RGBA> = cfg.palette.iter().map(|s| hex(s)).collect();
        let refs: Vec<&gdk::RGBA> = palette.iter().collect();
        term.set_colors(Some(&hex(&cfg.foreground)), Some(&hex(&cfg.background)), &refs);
        // Search-match highlight: a visible accent block on the all-black theme (VTE highlights the current
        // match with these colors).
        term.set_color_highlight(Some(&hex(ACCENT)));
        term.set_color_highlight_foreground(Some(&hex("#ffffff")));
    }
}

/// Debug self-capture: with `HL_TERM_SHOT=<png>` (and `HL_TERM_VIEW=manager|terminal|newws` to pick the
/// surface), render this window to a PNG via GTK's own snapshot pipeline and exit — no OS screen-capture
/// permission needed. Used to verify the UI headlessly.
struct Screenshot;

impl Screenshot {
    fn schedule(window: &gtk::ApplicationWindow, tag: &str) {
        let Some(path) = AppConfig::get().screenshot.clone() else {
            return;
        };
        if AppConfig::get().view.as_deref().unwrap_or("manager") != tag {
            return;
        }
        let ms = AppConfig::get().screenshot_ms;
        let win = window.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(ms), move || {
            let w = win.width().max(400);
            let h = win.height().max(300);
            let paintable = gtk::WidgetPaintable::new(Some(win.upcast_ref::<gtk::Widget>()));
            let snapshot = gtk::Snapshot::new();
            paintable.snapshot(snapshot.upcast_ref::<gdk::Snapshot>(), w as f64, h as f64);
            match (snapshot.to_node(), win.renderer()) {
                (Some(node), Some(renderer)) => {
                    let tex = renderer.render_texture(&node, None);
                    match tex.save_to_png(&path) {
                        Ok(()) => eprintln!("[husklet] wrote screenshot {path} ({w}x{h})"),
                        Err(error) => {
                            eprintln!("[husklet] screenshot write failed for {path}: {error}")
                        }
                    }
                }
                _ => eprintln!("[husklet] screenshot failed: no render node/renderer"),
            }
            let application = win.application();
            win.close();
            if let Some(application) = application {
                application.quit();
            }
        });
    }
}

#[cfg(test)]
mod tests;
