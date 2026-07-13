//! `dd` — one black, iTerm2-style product: a Workspace Manager window that opens first, a rich
//! New-Workspace configuration window, and a per-workspace Terminal window you launch from the manager.
//!
//! * Native macOS title bars (real traffic lights); content — including the full-width tab strip — sits
//!   directly below, so nothing needs a traffic-light gap.
//! * DAEMON-FREE launch: each terminal tab runs `ddcli workspace launch <name>`, entering the image
//!   in-process via dd-jit. GPU-rendered through GTK4's GSK renderer; VTE is the grid.
//! * No onboarding, no popups — the app opens straight onto workspaces.
//!
//! Build + run on macOS: `nix develop ./nix -c cargo run -p hl-gui --bin dd-term`.

use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::SystemTime;
use vte4::prelude::*;
use vte4::TerminalExtManual;

use hl_term::config::{CursorShape, TermConfig};
use hl_term::session::{self, Pane, PaneNode, Session, SessionTab, SplitDir};
use hl_term::workspace::{Arch, CudaDevice, Mount, VpnConfig, Workspace, WorkspaceStore};

const APP_ID: &str = "com.dd.term";

// One committed near-black palette (matches the design mockup).
const BG0: &str = "#0d0e11"; // window ground
const BG1: &str = "#15171c"; // strips / sidebars
const BG2: &str = "#1a1d23"; // cards / terminal
const BG3: &str = "#232732"; // hover / raised
const LINE: &str = "#2b2f39";
const LINE_S: &str = "#20232b";
const TXT: &str = "#e7e9ee";
const DIM: &str = "#878e9c";
const FAINT: &str = "#565c69";
const ACCENT: &str = "#2f80ff";

fn css() -> String {
    format!(
        "
window {{ background-color:{BG0}; color:{TXT}; }}
* {{ outline:none; }}
label {{ color:{TXT}; }}

/* ---- generic slim controls ---- */
.strip {{ background-color:{BG1}; box-shadow: inset 0 -1px 0 0 {LINE_S}; min-height:38px; padding:0 10px 0 14px; }}
.h {{ font-size:14px; font-weight:700; letter-spacing:-.01em; }}
/* Unified button — used by New, Launch, Cancel, Create, Browse. */
.tbtn, .btn {{ font-size:12.5px; font-weight:600; color:{TXT}; background-color:{BG2}; border:1px solid {LINE}; border-radius:7px; padding:5px 13px; min-height:0; box-shadow:none; }}
.tbtn:hover, .btn:hover {{ background-color:{BG3}; border-color:{DIM}; }}
.tbtn .plus {{ color:{ACCENT}; font-weight:700; }}

/* ---- manager list ---- */
list.wslist {{ background:transparent; padding:6px 8px; }}
list.wslist > row {{ background:transparent; border-radius:9px; margin:2px 4px; padding:0; }}
list.wslist > row:hover {{ background-color:{BG1}; }}
list.wslist > row:selected {{ background-color:{BG1}; }}
.wsrow {{ padding:9px 11px; }}
.wsrow .nm {{ font-size:13.5px; font-weight:600; letter-spacing:-.01em; }}
.wsrow .mt {{ font-size:11.5px; color:{DIM}; font-family:'SF Mono',ui-monospace,monospace; }}
.chip {{ font-family:'SF Mono',ui-monospace,monospace; font-size:10.5px; font-weight:600; padding:2px 6px; border-radius:5px; }}
.chip.arm {{ color:#2dd4bf; background:rgba(45,212,191,.15); }}
.chip.amd {{ color:#a78bfa; background:rgba(167,139,250,.16); }}
.chip.dar {{ color:#f0a35e; background:rgba(240,163,94,.15); }}
.go {{ font-size:12px; color:{ACCENT}; font-weight:600; }}
.empty {{ color:{DIM}; font-size:13px; padding:26px; }}
/* per-row action affordances: ▶ play + ⋯ menu — frameless (no button box), color-only hover */
.rowbtn, .rowbtn > button {{ min-height:0; min-width:0; padding:2px 7px; background:none; border:none; box-shadow:none; outline:none; color:{DIM}; }}
.rowbtn:hover, .rowbtn:hover > button, .rowbtn > button:hover {{ color:{TXT}; background:none; }}
.rowbtn > button:checked, .rowbtn > button:active {{ background:none; box-shadow:none; }}
.rowbtn image {{ -gtk-icon-size:15px; }}
.rowbtn .dots {{ font-size:18px; font-weight:700; margin-top:-8px; letter-spacing:1px; }}
.rowmenu contents {{ background-color:{BG2}; border:1px solid {LINE}; border-radius:9px; padding:5px; }}
.menuitem {{ background:transparent; border:none; box-shadow:none; padding:7px 14px; border-radius:6px; color:{TXT}; font-size:12.5px; }}
.menuitem:hover {{ background-color:{BG3}; }}

/* ---- new-workspace sheet ---- */
.nav {{ background-color:{BG1}; box-shadow: inset -1px 0 0 0 {LINE_S}; padding:10px 8px; min-width:150px; }}
.navi {{ padding:7px 10px; border-radius:7px; color:{DIM}; font-weight:500; font-size:12.5px; }}
.navi:hover {{ background-color:{BG3}; color:{TXT}; }}
.navi.on {{ background-color:{BG3}; color:{TXT}; }}
.pane {{ padding:18px 20px; }}
.ptitle {{ font-size:13px; font-weight:650; }}
.flabel {{ font-size:11px; color:{DIM}; font-weight:600; }}
.fhint {{ font-size:11px; color:{FAINT}; }}
entry {{ background-color:{BG2}; color:{TXT}; border:1px solid {LINE}; border-radius:7px; padding:6px 9px; min-height:0; caret-color:{ACCENT}; }}
entry:focus {{ border-color:{ACCENT}; }}
entry.mono {{ font-family:'SF Mono',ui-monospace,monospace; font-size:12.5px; }}
spinbutton {{ background-color:{BG2}; border:1px solid {LINE}; border-radius:7px; color:{TXT}; min-height:0; }}
spinbutton entry {{ border:none; background:transparent; }}
.seg {{ background-color:{BG2}; border:1px solid {LINE}; border-radius:7px; padding:2px; }}
.seg button {{ font-family:'SF Mono',ui-monospace,monospace; font-size:11.5px; color:{DIM}; background:transparent; border:none; border-radius:5px; padding:4px 12px; min-height:0; box-shadow:none; }}
.seg button:checked {{ background-color:{ACCENT}; color:#fff; font-weight:600; }}
.xbtn {{ color:{DIM}; background:transparent; border:1px solid transparent; border-radius:7px; min-height:0; min-width:32px; padding:5px; }}
.xbtn:hover {{ color:#ff6b6b; background-color:rgba(255,90,90,.12); }}
.xbtn image {{ -gtk-icon-size:15px; }}
/* macOS-like slim toggle */
.dockrow switch {{ min-width:38px; min-height:21px; border-radius:11px; }}
.dockrow switch > slider {{ min-width:17px; min-height:17px; margin:1px; border-radius:50%; }}
/* required-field error state */
entry.err {{ border-color:#ff6b6b; box-shadow:0 0 0 2px rgba(255,90,90,.22); }}
.addrow {{ font-size:11.5px; color:{ACCENT}; font-weight:600; background:transparent; border:none; box-shadow:none; padding:2px 0; min-height:0; }}
.footer {{ background-color:{BG1}; box-shadow: inset 0 1px 0 0 {LINE_S}; padding:10px 14px; }}
.btn.primary {{ background-color:{ACCENT}; border-color:{ACCENT}; color:#fff; }}
.btn.primary:hover {{ background-color:#3a9bff; }}
.dockrow {{ background-color:{BG2}; border:1px solid {LINE}; border-radius:8px; padding:10px 12px; }}
.dockrow .tt {{ font-size:12.5px; font-weight:600; }}
.dockrow .td {{ font-size:11px; color:{DIM}; }}
/* image-selection window */
.imghead {{ padding:16px 18px 8px 18px; }}
.imglist {{ background:transparent; padding:4px 10px; }}
.imglist > row {{ border-radius:8px; margin:2px 0; }}
.imglist > row:hover {{ background-color:{BG3}; }}
.imgrow {{ padding:9px 12px; }}
.imgname {{ font-size:13px; font-weight:600; color:{TXT}; }}
.imgref {{ font-size:11px; color:{DIM}; font-family:'SF Mono',ui-monospace,monospace; }}

/* ---- terminal window ---- */
.tabbar {{ background-color:{BG1}; box-shadow: inset 0 -1px 0 0 {LINE_S}; min-height:34px; }}
.tab {{ background-color:{BG1}; color:{DIM}; box-shadow: inset -1px 0 0 0 {LINE_S}; padding:0 10px; }}
.tab:hover {{ background-color:{BG3}; color:{TXT}; }}
.tab.on {{ background-color:{BG2}; color:{TXT}; box-shadow: inset -1px 0 0 0 {LINE_S}, inset 0 -2px 0 0 {ACCENT}; }}
.tab label {{ font-size:12px; font-weight:500; }}
.tab .di {{ color:{ACCENT}; }}
button.tabx {{ min-height:16px; min-width:16px; padding:0; margin-left:6px; background:transparent; border:none; box-shadow:none; opacity:0; color:{DIM}; }}
.tab:hover button.tabx, .tab.on button.tabx {{ opacity:.6; }}
button.tabx:hover {{ opacity:1; background-color:rgba(255,255,255,.14); border-radius:4px; }}
.newtab {{ min-width:30px; padding:0; color:{DIM}; background:transparent; border:none; box-shadow: inset 1px 0 0 0 {LINE_S}; border-radius:0; }}
.newtab label {{ font-size:14px; font-weight:400; }}
.newtab:hover {{ background-color:{BG3}; color:{TXT}; }}
stack.pages {{ background-color:{BG2}; }}

/* ---- dashboard ---- */
.dside {{ background-color:{BG1}; padding:9px 8px; min-width:130px; }}
.dsi {{ padding:7px 10px; border-radius:7px; color:{DIM}; font-weight:500; font-size:12.5px; }}
.dsi:hover {{ background-color:{BG3}; color:{TXT}; }}
.dsi.on {{ background-color:{BG3}; color:{TXT}; }}
.dbadge {{ font-family:'SF Mono',ui-monospace,monospace; font-size:10px; color:{FAINT}; }}
.dmain {{ padding:16px 18px; }}
.dashtitle {{ font-size:16px; font-weight:700; letter-spacing:-.01em; }}
.kvk {{ font-size:11.5px; color:{DIM}; font-weight:600; }}
.kvv {{ font-size:12.5px; font-family:'SF Mono',ui-monospace,monospace; color:{TXT}; }}
/* Every table row (Processes / Containers / Images / …) is the SAME height (min-height + 0 vertical
   padding), so all the dashboard tables line up uniformly regardless of whether a row has buttons. */
.trow {{ padding:0 8px; min-height:32px; box-shadow: inset 0 -1px 0 0 {LINE_S}; }}
.trow.thead {{ min-height:26px; box-shadow: inset 0 -1px 0 0 {LINE}; }}
.tcell {{ font-family:'SF Mono',ui-monospace,monospace; font-size:11.5px; color:{TXT}; }}
.trow.thead .tcell {{ color:{FAINT}; font-size:10.5px; font-weight:600; letter-spacing:.04em; }}
/* Compact, flat signal buttons that fit inside a row's height (so a Processes row is no taller). */
.sigbtn {{ color:{DIM}; background:transparent; border:1px solid transparent; border-radius:6px; min-height:0; min-width:26px; padding:3px; margin:0; }}
.sigbtn image {{ -gtk-icon-size:15px; }}
.sigbtn:hover {{ color:#ff6b6b; background-color:rgba(255,90,90,.12); }}
.dhead {{ font-size:11px; font-weight:650; color:{DIM}; letter-spacing:.05em; }}
.dhint {{ color:{DIM}; font-size:13px; }}
.mono {{ font-family:'SF Mono',ui-monospace,monospace; }}
/* split handle — ONE consistent 2px line, SAME color (#3a4150), in BOTH the dashboard sidebar split and
   the terminal splits. (The `.dside` no longer draws its own edge line, so there is no double-thickness.)
   Hover is only a SUBTLE lighter grey — never the bright accent, which read as a big blue bar. */
paned > separator {{ background-color:#3a4150; min-width:2px; min-height:2px; padding:0; margin:0; -gtk-icon-source:none; }}
paned > separator:hover {{ background-color:#4a5262; }}
/* terminal: a little inset so the leftmost column is selectable + not flush to the edge */
vte-terminal, terminal {{ padding:3px 6px 3px 8px; }}
/* copy/scroll-mode: a subtle accent frame so the mode is visible */
vte-terminal.copymode, terminal.copymode {{ box-shadow: inset 0 0 0 1px {ACCENT}; }}

/* ---- search bar (Cmd+F) — slim, black, floats top-right over the terminal ---- */
.searchbar {{ background-color:{BG1}; border:1px solid {LINE}; border-top:none; border-radius:0 0 9px 9px; padding:6px 8px; margin:0 10px 0 0; box-shadow:0 4px 14px rgba(0,0,0,.4); }}
.searchfield {{ background-color:{BG2}; color:{TXT}; border:1px solid {LINE}; border-radius:6px; padding:4px 8px; min-height:0; font-size:12.5px; caret-color:{ACCENT}; }}
.searchfield:focus {{ border-color:{ACCENT}; }}
.searchinfo {{ font-size:11px; color:{FAINT}; min-width:56px; }}
.searchinfo.nomatch {{ color:#ff6b6b; }}
"
    )
}

// =================================================================================================
// User config (~/.dd/term.conf) — loaded at startup, applied to every VTE terminal, live-reloaded.
// =================================================================================================

// PCRE2 flags for VTE's regex engine (search + URL match). VTE requires UTF + MULTILINE.
const PCRE2_CASELESS: u32 = 0x0000_0008;
const PCRE2_MULTILINE: u32 = 0x0000_0400;
const PCRE2_UCP: u32 = 0x0002_0000;
const PCRE2_UTF: u32 = 0x0008_0000;
const PCRE2_NO_UTF_CHECK: u32 = 0x4000_0000;

thread_local! {
    static CONFIG: RefCell<TermConfig> = RefCell::new(TermConfig::default());
    static CFG_MTIME: Cell<Option<SystemTime>> = const { Cell::new(None) };
    // Weak refs to every live terminal, so a config live-reload can re-style them all.
    static TERMS: RefCell<Vec<glib::WeakRef<vte4::Terminal>>> = const { RefCell::new(Vec::new()) };
}

fn config_file() -> std::path::PathBuf {
    dd_root().join("term.conf")
}

fn current_config() -> TermConfig {
    CONFIG.with(|c| c.borrow().clone())
}

/// Load the config at startup, writing a commented sample on first run so users have something to edit.
fn load_config_initial() {
    let path = config_file();
    if !path.exists() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&path, TermConfig::sample());
    }
    let cfg = TermConfig::load(&path);
    CONFIG.with(|c| *c.borrow_mut() = cfg);
    CFG_MTIME.with(|m| m.set(std::fs::metadata(&path).and_then(|md| md.modified()).ok()));
}

/// Poll the config file's mtime once a second; on change, re-parse and re-apply to all open terminals.
fn install_config_watcher() {
    glib::timeout_add_local(std::time::Duration::from_secs(1), || {
        let path = config_file();
        let now = std::fs::metadata(&path).and_then(|md| md.modified()).ok();
        let changed = CFG_MTIME.with(|m| {
            if m.get() != now {
                m.set(now);
                true
            } else {
                false
            }
        });
        if changed {
            let cfg = TermConfig::load(&path);
            CONFIG.with(|c| *c.borrow_mut() = cfg);
            apply_config_to_all();
        }
        glib::ControlFlow::Continue
    });
}

/// Re-style every live terminal with the current config (prunes dead weak refs).
fn apply_config_to_all() {
    let cfg = current_config();
    TERMS.with(|t| {
        let mut v = t.borrow_mut();
        v.retain(|w| {
            if let Some(term) = w.upgrade() {
                style_terminal(&term, &cfg);
                // Preserve a per-workspace scrollback cap already applied in make_terminal by not
                // forcing it here unless the config explicitly sets one.
                if cfg.scrollback.is_some() {
                    term.set_scrollback_lines(cfg.scrollback_lines());
                }
                true
            } else {
                false
            }
        });
    });
}

fn register_terminal(term: &vte4::Terminal) {
    TERMS.with(|t| t.borrow_mut().push(term.downgrade()));
}

fn main() -> glib::ExitCode {
    let app = gtk::Application::builder().application_id(APP_ID).build();
    app.connect_startup(|_| {
        if let Some(s) = gtk::Settings::default() {
            s.set_gtk_application_prefer_dark_theme(true);
        }
        macshim::force_dark(); // dark native title bars (all-black, no white bar)
        let p = gtk::CssProvider::new();
        p.load_from_data(&css());
        if let Some(d) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(&d, &p, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        }
        load_config_initial();
        install_config_watcher();
    });
    app.connect_activate(open_manager);
    app.run()
}

// =================================================================================================
// Window 1 — Workspace Manager
// =================================================================================================

fn open_manager(app: &gtk::Application) {
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("dd")
        .default_width(480)
        .default_height(560)
        .build();

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);

    // Slim top strip: "Workspaces" + a small "+ New" (no fat GTK header).
    let strip = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    strip.add_css_class("strip");
    let h = gtk::Label::new(Some("Workspaces"));
    h.add_css_class("h");
    h.set_xalign(0.0);
    h.set_hexpand(true);
    strip.append(&h);
    let gear = gtk::Button::from_icon_name("emblem-system-symbolic");
    gear.add_css_class("btn");
    gear.set_valign(gtk::Align::Center);
    gear.set_tooltip_text(Some("Settings"));
    strip.append(&gear);
    let newb = gtk::Button::with_label("+ New");
    newb.add_css_class("btn");
    newb.add_css_class("primary");
    newb.set_valign(gtk::Align::Center); // don't stretch to the full strip height
    strip.append(&newb);
    root.append(&strip);
    {
        let app = app.clone();
        gear.connect_clicked(move |_| open_settings_window(&app));
    }

    let list = gtk::ListBox::new();
    list.add_css_class("wslist");
    list.set_selection_mode(gtk::SelectionMode::None);
    let scroller = gtk::ScrolledWindow::builder().vexpand(true).child(&list).build();
    scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    root.append(&scroller);

    let refresh: Rc<dyn Fn()> = {
        let app = app.clone();
        let list = list.clone();
        Rc::new(move || refresh_workspace_list(&app, &list))
    };
    {
        let app = app.clone();
        let refresh = refresh.clone();
        newb.connect_clicked(move |_| open_new_workspace(&app, refresh.clone()));
    }
    refresh();

    window.set_child(Some(&root));
    window.present();
    macshim::force_dark();
    maybe_shot(&window, "manager");

    // Debug: jump straight to a surface for headless screenshotting.
    match std::env::var("DD_TERM_VIEW").as_deref() {
        Ok("terminal") => {
            let store = WorkspaceStore::load(workspaces_conf());
            let want = std::env::var("DD_TERM_WS").ok();
            let ws = want
                .as_deref()
                .and_then(|n| store.get(n))
                .or_else(|| store.all().first());
            if let Some(ws) = ws {
                open_terminal_window(app, ws);
            }
        }
        Ok("newws") => {
            let noop: Rc<dyn Fn()> = Rc::new(|| {});
            open_new_workspace(app, noop);
        }
        Ok("settings") => open_settings_window(app),
        _ => {}
    }
}

fn refresh_workspace_list(app: &gtk::Application, list: &gtk::ListBox) {
    while let Some(c) = list.first_child() {
        list.remove(&c);
    }
    let store = WorkspaceStore::load(workspaces_conf());
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
        list.append(&workspace_row(app, ws, list));
    }
}

/// A prominent, color-coded os/arch badge (used beside a dashboard title). Shows the full
/// `os/arch` label (e.g. `linux/aarch64`) rather than a terse `arm`/`amd`.
fn arch_chip(arch: Arch) -> gtk::Label {
    let l = gtk::Label::new(Some(arch.os_arch_label()));
    l.add_css_class("chip");
    l.add_css_class(match arch {
        Arch::Arm64 => "arm",
        Arch::Amd64 => "amd",
        Arch::DarwinArm64 => "dar",
    });
    l.set_valign(gtk::Align::Center);
    l
}

fn workspace_row(app: &gtk::Application, ws: &Workspace, list: &gtk::ListBox) -> gtk::ListBoxRow {
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
    let dir = ws.storage_dir(&dd_root());
    let meta = gtk::Label::new(Some(&format!(
        "{} · {} · {}",
        ws.arch.os_arch_label(),
        ws.image,
        tilde(&dir)
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
        let app2 = app.clone();
        let ws2 = ws.clone();
        play.connect_clicked(move |_| open_terminal_window(&app2, &ws2));
    }
    bx.append(&play);

    // ⋯ three-dots menu → a popover with per-workspace actions (Remove for now).
    let menu = gtk::MenuButton::new();
    menu.add_css_class("rowbtn");
    menu.set_valign(gtk::Align::Center);
    menu.set_always_show_arrow(false); // just the ⋯, no dropdown arrow
    menu.set_tooltip_text(Some("More"));
    let dots = gtk::Label::new(Some("\u{22ef}")); // ⋯ (reliable glyph vs a maybe-missing symbolic icon)
    dots.add_css_class("dots");
    menu.set_child(Some(&dots));
    let pop = gtk::Popover::new();
    pop.add_css_class("rowmenu");
    let pbox = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let remove = gtk::Button::with_label("Remove workspace");
    remove.add_css_class("menuitem");
    {
        let name = ws.name.clone();
        let app2 = app.clone();
        let list2 = list.clone();
        let pop2 = pop.clone();
        remove.connect_clicked(move |_| {
            pop2.popdown();
            if confirm_dialog(&format!("Remove workspace {name}?  Its files on disk are kept — only the workspace entry is removed.")) {
                let mut store = WorkspaceStore::load(workspaces_conf());
                let _ = store.remove(&name);
                refresh_workspace_list(&app2, &list2);
            }
        });
    }
    pbox.append(&remove);
    pop.set_child(Some(&pbox));
    menu.set_popover(Some(&pop));
    bx.append(&menu);

    row.set_child(Some(&bx));
    row
}

/// A native macOS confirmation dialog (osascript) — GTK's dialogs are unreliable on the quartz backend.
/// Returns true if the user confirmed (clicked Delete).
fn confirm_dialog(message: &str) -> bool {
    let script = format!("display dialog \"{message}\" buttons {{\"Cancel\", \"Delete\"}} default button \"Cancel\" with icon caution");
    std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).contains("Delete"))
        .unwrap_or(false)
}

// =================================================================================================
// Window — Settings (global): one coherent surface over everything dd has built. A left nav drives a
// stack of sections, matching the all-black New-Workspace sheet idiom.
//
//   * Appearance         → the terminal look (`~/.dd/term.conf`): font + size, theme fg/bg, cursor,
//                          default scrollback. Persisted and LIVE-applied to every open terminal (the
//                          startup config watcher re-reads term.conf on change).
//   * Workspace defaults → the image/os-arch/storage/docker a NEW workspace starts from.
//   * Device (CUDA)      → the default simulated-CUDA device for new workspaces (name / cc / VRAM).
//   * Network (VPN)      → the default per-workspace VPN/proxy egress for new workspaces.
//   * Rendering (GPU)    → whether new workspaces enable accelerated GUI rendering (`--gui`).
//
// The "defaults" sections write `~/.dd/term-defaults.conf` (a tiny key=value file this binary owns);
// `apply_ws_defaults` pre-fills the New-Workspace form from it. Nothing here invents backend features —
// each control maps to an existing `TermConfig` or `Workspace` field.
// =================================================================================================

/// The values a freshly-created workspace starts from (edited in Settings → Workspace defaults/Device/
/// Network/Rendering, persisted to `~/.dd/term-defaults.conf`). Every field mirrors a `Workspace` field.
struct WsDefaults {
    image: String,
    arch: Arch,
    storage: String,
    docker_sock: bool,
    gui: bool,
    /// Raw scrollback text: "" / "unlimited" = unlimited, else a line count.
    scrollback: String,
    /// Raw VPN spec: "" = direct egress.
    vpn: String,
    cuda_on: bool,
    cuda: CudaDevice,
}

impl Default for WsDefaults {
    fn default() -> Self {
        WsDefaults {
            image: "ubuntu:24.04".to_string(),
            arch: Arch::Arm64,
            storage: String::new(),
            docker_sock: true,
            gui: false,
            scrollback: "unlimited".to_string(),
            vpn: String::new(),
            cuda_on: false,
            cuda: CudaDevice::default_device(),
        }
    }
}

impl WsDefaults {
    fn path() -> std::path::PathBuf {
        dd_root().join("term-defaults.conf")
    }

    fn load() -> WsDefaults {
        let mut d = WsDefaults::default();
        let Ok(text) = std::fs::read_to_string(WsDefaults::path()) else { return d };
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim());
            match k {
                "image" if !v.is_empty() => d.image = v.to_string(),
                "arch" => {
                    if let Some(a) = Arch::parse(v) {
                        d.arch = a;
                    }
                }
                "storage" => d.storage = v.to_string(),
                "docker_sock" => d.docker_sock = matches!(v, "true" | "1" | "yes" | "on"),
                "gui" => d.gui = matches!(v, "true" | "1" | "yes" | "on"),
                "scrollback" => d.scrollback = v.to_string(),
                "vpn" => d.vpn = v.to_string(),
                "cuda" => d.cuda_on = matches!(v, "true" | "1" | "yes" | "on"),
                "cuda_device" if !v.is_empty() => {
                    if let Some(c) = CudaDevice::parse(v) {
                        d.cuda = c;
                    }
                }
                _ => {}
            }
        }
        d
    }

    fn save(&self) -> std::io::Result<()> {
        let path = WsDefaults::path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut s = String::from("# dd — defaults for newly-created workspaces (edited in Settings)\n\n");
        s.push_str(&format!("image = {}\n", self.image));
        s.push_str(&format!("arch = {}\n", self.arch.as_str()));
        s.push_str(&format!("storage = {}\n", self.storage));
        s.push_str(&format!("docker_sock = {}\n", self.docker_sock));
        s.push_str(&format!("gui = {}\n", self.gui));
        s.push_str(&format!("scrollback = {}\n", self.scrollback));
        s.push_str(&format!("vpn = {}\n", self.vpn));
        s.push_str(&format!("cuda = {}\n", self.cuda_on));
        s.push_str(&format!("cuda_device = {}\n", self.cuda.to_spec()));
        std::fs::write(&path, s)
    }
}

/// Pre-fill the New-Workspace form from the saved defaults. Called after the panes are built (they set
/// their own baseline first). Only used on the create path — the dashboard Settings pane fills from the
/// actual workspace instead.
fn apply_ws_defaults(form: &Rc<Form>, d: &WsDefaults) {
    form.image.set_text(&d.image);
    form.os_linux.set(d.arch != Arch::DarwinArm64);
    form.cpu_amd.set(d.arch == Arch::Amd64);
    form.storage.set_text(&d.storage);
    form.docker.set_active(d.docker_sock);
    form.gui.set_active(d.gui);
    form.scrollback.set_text(&d.scrollback);
    form.vpn.set_text(&d.vpn);
    form.cuda_on.set_active(d.cuda_on);
    form.cuda_name.set_text(&d.cuda.name);
    form.cuda_cc.set_text(&d.cuda.compute_capability);
    form.cuda_vram.set_text(&d.cuda.vram_mb.to_string());
}

/// Serialize a [`TermConfig`] back to `~/.dd/term.conf` (a commented, human-readable rewrite). The
/// startup config watcher notices the file change within a second and re-applies it to every open
/// terminal; we also update the in-memory config + re-style immediately so the change is instant.
fn save_term_config(cfg: &TermConfig) -> std::io::Result<()> {
    let path = config_file();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut s = String::from("# dd terminal config — ~/.dd/term.conf\n# edit here or via Settings; open terminals live-reload.\n\n");
    s.push_str(&format!("font_family = {}\n", cfg.font_family));
    s.push_str(&format!("font_size = {}\n", if cfg.font_size.fract().abs() < f64::EPSILON { format!("{}", cfg.font_size as i64) } else { format!("{}", cfg.font_size) }));
    s.push_str("# scrollback: a number of lines, or `unlimited`\n");
    s.push_str(&format!("scrollback = {}\n", cfg.scrollback.map(|n| n.to_string()).unwrap_or_else(|| "unlimited".to_string())));
    s.push_str(&format!("cursor_shape = {}   # block | beam | underline\n", cfg.cursor_shape.as_str()));
    s.push_str(&format!("cursor_blink = {}\n\n", cfg.cursor_blink));
    s.push_str("# colors (#rrggbb)\n");
    s.push_str(&format!("foreground = {}\n", cfg.foreground));
    s.push_str(&format!("background = {}\n", cfg.background));
    for (i, c) in cfg.palette.iter().enumerate() {
        s.push_str(&format!("color{i} = {c}\n"));
    }
    std::fs::write(&path, s)?;
    // Apply immediately (don't wait up to 1s for the watcher) and keep the watcher from re-applying it.
    CONFIG.with(|c| *c.borrow_mut() = cfg.clone());
    CFG_MTIME.with(|m| m.set(std::fs::metadata(&path).and_then(|md| md.modified()).ok()));
    apply_config_to_all();
    Ok(())
}

/// Handles to every Settings control, so Save can gather all sections at once.
struct SettingsUi {
    // Appearance (term.conf).
    font: gtk::Entry,
    size: gtk::SpinButton,
    fg: gtk::Entry,
    bg: gtk::Entry,
    cursor: Rc<Cell<CursorShape>>,
    blink: gtk::Switch,
    scrollback: gtk::Entry,
    // Workspace defaults.
    d_image: gtk::Entry,
    d_arch: Rc<Cell<Arch>>,
    d_storage: gtk::Entry,
    d_docker: gtk::Switch,
    d_gui: gtk::Switch,
    d_vpn: gtk::Entry,
    d_cuda_on: gtk::Switch,
    d_cuda_name: gtk::Entry,
    d_cuda_cc: gtk::Entry,
    d_cuda_vram: gtk::Entry,
}

fn open_settings_window(app: &gtk::Application) {
    let cfg = current_config();
    let defs = WsDefaults::load();

    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Settings")
        .default_width(640)
        .default_height(500)
        .modal(false)
        .build();

    let ui = Rc::new(SettingsUi {
        font: entry("Menlo", true),
        size: gtk::SpinButton::with_range(6.0, 48.0, 1.0),
        fg: entry("#e7e9ee", true),
        bg: entry("#1a1d23", true),
        cursor: Rc::new(Cell::new(cfg.cursor_shape)),
        blink: gtk::Switch::new(),
        scrollback: entry("unlimited", false),
        d_image: entry("ubuntu:24.04", true),
        d_arch: Rc::new(Cell::new(defs.arch)),
        d_storage: entry("", true),
        d_docker: gtk::Switch::new(),
        d_gui: gtk::Switch::new(),
        d_vpn: entry("socks5:127.30.0.1:1080  (blank = direct)", true),
        d_cuda_on: gtk::Switch::new(),
        d_cuda_name: entry("dd Metal (CUDA-sim) Device", true),
        d_cuda_cc: entry("8.6", true),
        d_cuda_vram: entry("4096", true),
    });

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let split = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    split.set_vexpand(true);

    let nav = gtk::Box::new(gtk::Orientation::Vertical, 2);
    nav.add_css_class("nav");
    let pages = gtk::Stack::new();
    pages.set_hexpand(true);
    pages.set_transition_type(gtk::StackTransitionType::None);

    pages.add_named(&settings_appearance(&ui), Some("Appearance"));
    pages.add_named(&settings_defaults(&ui), Some("Workspace defaults"));
    pages.add_named(&settings_device(&ui), Some("Device (CUDA)"));
    pages.add_named(&settings_network(&ui), Some("Network (VPN)"));
    pages.add_named(&settings_rendering(&ui), Some("Rendering (GPU)"));

    // Apply the current values AFTER the panes are built (each pane sets its own baseline first).
    ui.font.set_text(&cfg.font_family);
    ui.size.set_value(cfg.font_size);
    ui.fg.set_text(&cfg.foreground);
    ui.bg.set_text(&cfg.background);
    ui.blink.set_active(cfg.cursor_blink);
    ui.scrollback.set_text(&cfg.scrollback.map(|n| n.to_string()).unwrap_or_else(|| "unlimited".to_string()));
    ui.d_image.set_text(&defs.image);
    ui.d_storage.set_text(&defs.storage);
    ui.d_docker.set_active(defs.docker_sock);
    ui.d_gui.set_active(defs.gui);
    ui.d_vpn.set_text(&defs.vpn);
    ui.d_cuda_on.set_active(defs.cuda_on);
    ui.d_cuda_name.set_text(&defs.cuda.name);
    ui.d_cuda_cc.set_text(&defs.cuda.compute_capability);
    ui.d_cuda_vram.set_text(&defs.cuda.vram_mb.to_string());

    let items = ["Appearance", "Workspace defaults", "Device (CUDA)", "Network (VPN)", "Rendering (GPU)"];
    let nav_labels: Rc<RefCell<Vec<gtk::Label>>> = Rc::new(RefCell::new(Vec::new()));
    for (i, name) in items.iter().enumerate() {
        let l = gtk::Label::new(Some(name));
        l.add_css_class("navi");
        l.set_xalign(0.0);
        if i == 0 {
            l.add_css_class("on");
        }
        let click = gtk::GestureClick::new();
        let pages2 = pages.clone();
        let name2 = name.to_string();
        let labels2 = nav_labels.clone();
        click.connect_released(move |_, _, _, _| {
            pages2.set_visible_child_name(&name2);
            for lb in labels2.borrow().iter() {
                if lb.text() == name2.as_str() {
                    lb.add_css_class("on");
                } else {
                    lb.remove_css_class("on");
                }
            }
        });
        l.add_controller(click);
        nav.append(&l);
        nav_labels.borrow_mut().push(l);
    }

    split.append(&nav);
    split.append(&pages);
    root.append(&split);

    let footer = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    footer.add_css_class("footer");
    let status = gtk::Label::new(None);
    status.add_css_class("fhint");
    status.set_xalign(0.0);
    status.set_hexpand(true);
    footer.append(&status);
    let close = gtk::Button::with_label("Close");
    close.add_css_class("btn");
    let save = gtk::Button::with_label("Save");
    save.add_css_class("btn");
    save.add_css_class("primary");
    footer.append(&close);
    footer.append(&save);
    root.append(&footer);

    {
        let w = window.clone();
        close.connect_clicked(move |_| w.close());
    }
    {
        let ui = ui.clone();
        let status = status.clone();
        save.connect_clicked(move |_| match save_settings(&ui) {
            Ok(()) => {
                status.remove_css_class("err");
                status.set_text("Saved — appearance is live now; defaults apply to new workspaces.");
            }
            Err(e) => {
                status.add_css_class("err");
                status.set_text(&format!("Could not save: {e}"));
            }
        });
    }

    // Debug: DD_TERM_SETTINGS_PANE selects a section for headless screenshotting.
    if let Ok(p) = std::env::var("DD_TERM_SETTINGS_PANE") {
        pages.set_visible_child_name(&p);
        for l in nav_labels.borrow().iter() {
            if l.text() == p {
                l.add_css_class("on");
            } else {
                l.remove_css_class("on");
            }
        }
    }

    window.set_child(Some(&root));
    window.present();
    macshim::force_dark();
    maybe_shot(&window, "settings");
}

/// Gather every Settings control into a [`TermConfig`] + [`WsDefaults`] and persist both.
fn save_settings(ui: &Rc<SettingsUi>) -> std::io::Result<()> {
    // ---- Appearance → term.conf ----
    let mut cfg = current_config();
    let fam = ui.font.text().trim().to_string();
    if !fam.is_empty() {
        cfg.font_family = fam;
    }
    let sz = ui.size.value();
    if sz > 0.0 {
        cfg.font_size = sz;
    }
    let fg = ui.fg.text().trim().to_string();
    if is_hex6(&fg) {
        cfg.foreground = fg;
    }
    let bg = ui.bg.text().trim().to_string();
    if is_hex6(&bg) {
        cfg.background = bg;
    }
    cfg.cursor_shape = ui.cursor.get();
    cfg.cursor_blink = ui.blink.is_active();
    cfg.scrollback = match ui.scrollback.text().trim().to_ascii_lowercase().as_str() {
        "" | "0" | "unlimited" | "infinite" | "inf" => None,
        other => other.parse::<u64>().ok().filter(|n| *n > 0),
    };
    save_term_config(&cfg)?;

    // ---- Workspace defaults / Device / Network / Rendering → term-defaults.conf ----
    let mut cuda = CudaDevice::default_device();
    let n = ui.d_cuda_name.text().trim().to_string();
    if !n.is_empty() {
        cuda.name = n;
    }
    let cc = ui.d_cuda_cc.text().trim().to_string();
    if !cc.is_empty() {
        cuda.compute_capability = cc;
    }
    if let Ok(mb) = ui.d_cuda_vram.text().trim().parse::<u32>() {
        cuda.vram_mb = mb.max(1);
    }
    let image = ui.d_image.text().trim().to_string();
    let defs = WsDefaults {
        image: if image.is_empty() { "ubuntu:24.04".to_string() } else { image },
        arch: ui.d_arch.get(),
        storage: ui.d_storage.text().trim().to_string(),
        docker_sock: ui.d_docker.is_active(),
        gui: ui.d_gui.is_active(),
        scrollback: ui.scrollback.text().trim().to_string(),
        vpn: ui.d_vpn.text().trim().to_string(),
        cuda_on: ui.d_cuda_on.is_active(),
        cuda,
    };
    defs.save()
}

fn is_hex6(v: &str) -> bool {
    let body = v.strip_prefix('#').unwrap_or(v);
    (body.len() == 6 || body.len() == 3) && body.chars().all(|c| c.is_ascii_hexdigit())
}

fn settings_appearance(ui: &Rc<SettingsUi>) -> gtk::Box {
    let p = pane("Appearance");
    let intro = gtk::Label::new(Some(
        "dd's terminal look — the all-black aesthetic. Changes save to ~/.dd/term.conf and apply live \
         to every open terminal.",
    ));
    intro.add_css_class("fhint");
    intro.set_xalign(0.0);
    intro.set_wrap(true);
    intro.set_max_width_chars(52);
    p.append(&intro);

    ui.font.set_hexpand(true);
    p.append(&field("TERMINAL FONT", &ui.font, Some("Any installed monospace family, e.g. Menlo, SF Mono, JetBrains Mono.")));
    ui.size.set_halign(gtk::Align::Start);
    p.append(&spin_field("FONT SIZE (pt)", &ui.size));

    // Theme colors (the all-black ground + text).
    ui.bg.set_hexpand(true);
    p.append(&field("BACKGROUND (#rrggbb)", &ui.bg, Some("Terminal background. dd ships near-black #1a1d23.")));
    ui.fg.set_hexpand(true);
    p.append(&field("FOREGROUND (#rrggbb)", &ui.fg, Some("Default text color.")));

    // Cursor shape segmented control → the shared cursor Cell.
    let seg = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    seg.add_css_class("seg");
    let block = gtk::ToggleButton::with_label("block");
    let beam = gtk::ToggleButton::with_label("beam");
    let under = gtk::ToggleButton::with_label("underline");
    beam.set_group(Some(&block));
    under.set_group(Some(&block));
    match ui.cursor.get() {
        CursorShape::Block => block.set_active(true),
        CursorShape::Beam => beam.set_active(true),
        CursorShape::Underline => under.set_active(true),
    }
    for (btn, shape) in [
        (&block, CursorShape::Block),
        (&beam, CursorShape::Beam),
        (&under, CursorShape::Underline),
    ] {
        let cell = ui.cursor.clone();
        btn.connect_toggled(move |t| {
            if t.is_active() {
                cell.set(shape);
            }
        });
        seg.append(btn);
    }
    p.append(&labeled("CURSOR SHAPE", &seg));
    p.append(&switch_row("Cursor blink", "Blink the text cursor.", &ui.blink));

    ui.scrollback.set_max_width_chars(14);
    ui.scrollback.set_halign(gtk::Align::Start);
    p.append(&field(
        "DEFAULT SCROLLBACK (blank = unlimited)",
        &ui.scrollback,
        Some("Lines of history each shell keeps unless a workspace overrides it; blank/0 = unlimited."),
    ));

    // Tab behavior is fixed but worth documenting so Settings is a complete picture.
    let tabs = gtk::Label::new(Some(
        "Tabs: \u{2318}T new tab · \u{2318}W close · \u{2318}D split (⇧ = vertical). Tabs are equal-width \
         and fill the strip.",
    ));
    tabs.add_css_class("fhint");
    tabs.set_xalign(0.0);
    tabs.set_wrap(true);
    tabs.set_max_width_chars(52);
    tabs.set_margin_top(4);
    p.append(&tabs);
    p
}

fn settings_defaults(ui: &Rc<SettingsUi>) -> gtk::Box {
    let p = pane("Workspace defaults");
    let intro = gtk::Label::new(Some("What a new workspace starts from. Pre-fills the New-Workspace form; you can still change each field per workspace."));
    intro.add_css_class("fhint");
    intro.set_xalign(0.0);
    intro.set_wrap(true);
    intro.set_max_width_chars(52);
    p.append(&intro);

    ui.d_image.set_hexpand(true);
    p.append(&field("DEFAULT IMAGE", &ui.d_image, Some("Docker image reference a new workspace defaults to.")));

    // Default OS/arch segmented control → the shared arch Cell.
    let seg = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    seg.add_css_class("seg");
    let arm = gtk::ToggleButton::with_label("linux/aarch64");
    let amd = gtk::ToggleButton::with_label("linux/x86_64");
    let dar = gtk::ToggleButton::with_label("darwin/aarch64");
    amd.set_group(Some(&arm));
    dar.set_group(Some(&arm));
    match ui.d_arch.get() {
        Arch::Arm64 => arm.set_active(true),
        Arch::Amd64 => amd.set_active(true),
        Arch::DarwinArm64 => dar.set_active(true),
    }
    for (btn, a) in [(&arm, Arch::Arm64), (&amd, Arch::Amd64), (&dar, Arch::DarwinArm64)] {
        let cell = ui.d_arch.clone();
        btn.connect_toggled(move |t| {
            if t.is_active() {
                cell.set(a);
            }
        });
        seg.append(btn);
    }
    p.append(&labeled("DEFAULT OS / ARCH", &seg));

    // Storage location + native folder picker.
    let srow = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let sl = gtk::Label::new(Some("DEFAULT STORAGE LOCATION"));
    sl.add_css_class("flabel");
    sl.set_xalign(0.0);
    srow.append(&sl);
    let sbox = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    ui.d_storage.set_hexpand(true);
    let browse = gtk::Button::with_label("Browse…");
    browse.add_css_class("btn");
    sbox.append(&ui.d_storage);
    sbox.append(&browse);
    srow.append(&sbox);
    let sh = gtk::Label::new(Some("Blank = ~/.dd/workspaces/<name>. A folder here becomes the parent for new workspaces you can override."));
    sh.add_css_class("fhint");
    sh.set_xalign(0.0);
    sh.set_wrap(true);
    sh.set_max_width_chars(52);
    srow.append(&sh);
    {
        let entry = ui.d_storage.clone();
        browse.connect_clicked(move |_| {
            if let Some(path) = pick_folder_dialog() {
                entry.set_text(&path);
            }
        });
    }
    p.append(&srow);

    p.append(&switch_row(
        "Mount docker socket by default",
        "Sets DOCKER_HOST inside new workspaces so the docker CLI works.",
        &ui.d_docker,
    ));
    p
}

fn settings_device(ui: &Rc<SettingsUi>) -> gtk::Box {
    let p = pane("Device (CUDA)");
    p.append(&switch_row(
        "Simulated CUDA device by default",
        "New Linux workspaces present an NVIDIA-looking CUDA device (nvidia-smi, torch.cuda.is_available()); \
         GPU work is forwarded to the host Apple Metal GPU.",
        &ui.d_cuda_on,
    ));
    ui.d_cuda_name.set_hexpand(true);
    p.append(&field("DEVICE NAME", &ui.d_cuda_name, Some("Name reported by nvidia-smi / cudaGetDeviceProperties.")));
    ui.d_cuda_cc.set_hexpand(true);
    p.append(&field("COMPUTE CAPABILITY", &ui.d_cuda_cc, Some("Reported CUDA compute capability, e.g. 8.6 (Ampere-class).")));
    ui.d_cuda_vram.set_hexpand(true);
    p.append(&field("VRAM (MB)", &ui.d_cuda_vram, Some("Reported device memory in MB (carved from unified memory).")));
    let hint = gtk::Label::new(Some("See docs/ideas/CUDA_ON_METAL.md. Full framework acceleration (PyTorch/TF) is a work in progress."));
    hint.add_css_class("fhint");
    hint.set_xalign(0.0);
    hint.set_wrap(true);
    hint.set_max_width_chars(52);
    p.append(&hint);
    p
}

fn settings_network(ui: &Rc<SettingsUi>) -> gtk::Box {
    let p = pane("Network (VPN)");
    ui.d_vpn.set_hexpand(true);
    p.append(&field(
        "DEFAULT VPN / PROXY EGRESS (blank = direct)",
        &ui.d_vpn,
        Some("Route new workspaces' outbound traffic through a VPN/proxy. A SOCKS5 host:port (e.g. \
              127.30.0.1:1080) or a <kind>:<endpoint> spec (socks5:/http:/wireguard:/openvpn:). Blank = direct."),
    ));
    let hint = gtk::Label::new(Some(
        "SOCKS5 endpoints wire directly into the engine's egress redirect; tunnel kinds (WireGuard/OpenVPN) \
         are saved but need the userspace-tunnel helper. See docs/VPN.md.",
    ));
    hint.add_css_class("fhint");
    hint.set_xalign(0.0);
    hint.set_wrap(true);
    hint.set_max_width_chars(52);
    p.append(&hint);
    p
}

fn settings_rendering(ui: &Rc<SettingsUi>) -> gtk::Box {
    let p = pane("Rendering (GPU)");
    p.append(&switch_row(
        "Accelerated GUI rendering (--gui) by default",
        "New workspaces bind-mount the host dd-display socket + set WAYLAND_DISPLAY so a Linux GUI app \
         renders on the Mac (GPU-accelerated). Off = headless (terminal only).",
        &ui.d_gui,
    ));
    let hint = gtk::Label::new(Some("Only affects workspaces that launch a graphical app. See docs/ideas/RENDERING_PLAN.md."));
    hint.add_css_class("fhint");
    hint.set_xalign(0.0);
    hint.set_wrap(true);
    hint.set_max_width_chars(52);
    p.append(&hint);
    p
}

// =================================================================================================
// Window 2 — New Workspace (settings sheet)
// =================================================================================================

/// Handles to every field, so Create can gather the full config.
struct Form {
    name: gtk::Entry,
    image: gtk::Entry,
    shell: gtk::Entry,
    storage: gtk::Entry,
    os_linux: Rc<Cell<bool>>, // true = Linux, false = macOS
    cpu_amd: Rc<Cell<bool>>,  // true = x86-64, false = arm64

    cpus: gtk::SpinButton,
    mem: gtk::SpinButton,
    scrollback: gtk::Entry,
    docker: gtk::Switch,
    /// Accelerated GUI rendering (`--gui`): bind-mount the host `dd-display` socket so a Linux GUI app
    /// in the workspace renders on the Mac. Maps to [`Workspace::gui`].
    gui: gtk::Switch,
    vpn: gtk::Entry,
    // Device tab: a simulated CUDA device backed by the host Metal GPU (see docs/ideas/CUDA_ON_METAL.md).
    cuda_on: gtk::Switch,
    cuda_name: gtk::Entry,
    cuda_cc: gtk::Entry,
    cuda_vram: gtk::Entry,
    env_box: gtk::Box,
    env_rows: RefCell<Vec<(gtk::Entry, gtk::Entry)>>,
    mount_box: gtk::Box,
    mount_rows: RefCell<Vec<(gtk::Entry, gtk::Entry, gtk::CheckButton)>>,
}

fn open_new_workspace(app: &gtk::Application, on_created: Rc<dyn Fn()>) {
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("New workspace")
        .default_width(620)
        .default_height(430)
        .modal(false)
        .build();

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let split = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    split.set_vexpand(true);

    // Left mini-nav drives a stack of panes.
    let nav = gtk::Box::new(gtk::Orientation::Vertical, 2);
    nav.add_css_class("nav");
    let pages = gtk::Stack::new();
    pages.set_hexpand(true);
    pages.set_transition_type(gtk::StackTransitionType::None);

    let form = Rc::new(build_form());
    pages.add_named(&pane_general(&form), Some("General"));
    pages.add_named(&pane_resources(&form), Some("Resources"));
    pages.add_named(&pane_env(&form), Some("Environment"));
    pages.add_named(&pane_mounts(&form), Some("Mounts"));
    pages.add_named(&pane_docker(&form), Some("Docker"));
    pages.add_named(&pane_network(&form), Some("Network"));
    pages.add_named(&pane_device(&form), Some("Device"));
    pages.add_named(&pane_rendering(&form), Some("Rendering"));

    // Pre-fill the form from the saved new-workspace defaults (Settings → Workspace defaults). Must run
    // AFTER the panes are built, since each pane builder sets its own baseline first.
    apply_ws_defaults(&form, &WsDefaults::load());

    let items = ["General", "Resources", "Environment", "Mounts", "Docker", "Network", "Device", "Rendering"];
    let nav_labels: Rc<RefCell<Vec<gtk::Label>>> = Rc::new(RefCell::new(Vec::new()));
    for (i, name) in items.iter().enumerate() {
        let l = gtk::Label::new(Some(name));
        l.add_css_class("navi");
        l.set_xalign(0.0);
        if i == 0 {
            l.add_css_class("on");
        }
        let click = gtk::GestureClick::new();
        let pages2 = pages.clone();
        let name2 = name.to_string();
        let labels2 = nav_labels.clone();
        click.connect_released(move |_, _, _, _| {
            pages2.set_visible_child_name(&name2);
            for lb in labels2.borrow().iter() {
                if lb.text() == name2.as_str() {
                    lb.add_css_class("on");
                } else {
                    lb.remove_css_class("on");
                }
            }
        });
        l.add_controller(click);
        nav.append(&l);
        nav_labels.borrow_mut().push(l);
    }

    split.append(&nav);
    split.append(&pages);
    root.append(&split);

    // Footer: Cancel / Create.
    let footer = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    footer.add_css_class("footer");
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    footer.append(&spacer);
    let cancel = gtk::Button::with_label("Cancel");
    cancel.add_css_class("btn");
    let create = gtk::Button::with_label("Create workspace");
    create.add_css_class("btn");
    create.add_css_class("primary");
    footer.append(&cancel);
    footer.append(&create);
    root.append(&footer);

    {
        let w = window.clone();
        cancel.connect_clicked(move |_| w.close());
    }
    {
        let form = form.clone();
        let w = window.clone();
        let on_created = on_created.clone();
        let pages = pages.clone();
        create.connect_clicked(move |_| {
            // Validate: name + image are required. Mark empties red and jump to General.
            let name_ok = !form.name.text().trim().is_empty();
            let img_ok = !form.image.text().trim().is_empty();
            form.name.remove_css_class("err");
            form.image.remove_css_class("err");
            if !name_ok || !img_ok {
                if !name_ok {
                    form.name.add_css_class("err");
                }
                if !img_ok {
                    form.image.add_css_class("err");
                }
                pages.set_visible_child_name("General");
                if !name_ok {
                    form.name.grab_focus();
                } else {
                    form.image.grab_focus();
                }
                return;
            }
            if save_workspace(&form) {
                on_created();
                w.close();
            }
        });
    }

    // Debug: DD_TERM_NEWWS_PANE selects a config pane for screenshotting.
    if let Ok(p) = std::env::var("DD_TERM_NEWWS_PANE") {
        pages.set_visible_child_name(&p);
        for l in nav_labels.borrow().iter() {
            if l.text() == p {
                l.add_css_class("on");
            } else {
                l.remove_css_class("on");
            }
        }
    }

    window.set_child(Some(&root));
    window.present();
    macshim::force_dark();
    maybe_shot(&window, "newws");
}

fn build_form() -> Form {
    Form {
        name: entry("name", false),
        image: entry("ubuntu:24.04", true),
        shell: entry("/bin/bash -l", true),
        storage: entry("", true),
        os_linux: Rc::new(Cell::new(true)),
        cpu_amd: Rc::new(Cell::new(false)),
        cpus: gtk::SpinButton::with_range(0.0, 64.0, 1.0),
        mem: gtk::SpinButton::with_range(0.0, 65536.0, 256.0),
        scrollback: entry("unlimited", false),
        docker: gtk::Switch::new(),
        gui: gtk::Switch::new(),
        vpn: entry("socks5:127.30.0.1:1080  (blank = direct)", true),
        cuda_on: gtk::Switch::new(),
        cuda_name: entry("dd Metal (CUDA-sim) Device", true),
        cuda_cc: entry("8.6", true),
        cuda_vram: entry("4096", true),
        env_box: gtk::Box::new(gtk::Orientation::Vertical, 6),
        env_rows: RefCell::new(Vec::new()),
        mount_box: gtk::Box::new(gtk::Orientation::Vertical, 6),
        mount_rows: RefCell::new(Vec::new()),
    }
}

fn pane_general(form: &Rc<Form>) -> gtk::Box {
    let p = pane("General");
    p.append(&field("NAME", &form.name, Some("A friendly name for this workspace.")));

    // Architecture segmented control (arm64 / x86-64) — built first so the OS control can toggle it.
    let a_seg = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    a_seg.add_css_class("seg");
    let arm = gtk::ToggleButton::with_label("arm64");
    let amd = gtk::ToggleButton::with_label("x86-64");
    amd.set_group(Some(&arm));
    arm.set_active(true);
    {
        let c = form.cpu_amd.clone();
        arm.connect_toggled(move |t| {
            if t.is_active() {
                c.set(false);
            }
        });
    }
    {
        let c = form.cpu_amd.clone();
        amd.connect_toggled(move |t| {
            if t.is_active() {
                c.set(true);
            }
        });
    }
    a_seg.append(&arm);
    a_seg.append(&amd);

    // OS segmented control (Linux / macOS). macOS supports arm64 only → disable x86-64 there.
    let os_seg = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    os_seg.add_css_class("seg");
    let linux = gtk::ToggleButton::with_label("Linux");
    let macos = gtk::ToggleButton::with_label("macOS");
    macos.set_group(Some(&linux));
    linux.set_active(true);
    {
        let c = form.os_linux.clone();
        let amd = amd.clone();
        let img = form.image.clone();
        linux.connect_toggled(move |t| {
            if t.is_active() {
                c.set(true);
                amd.set_sensitive(true);
                img.set_text(""); // Linux images differ from macOS — re-pick for the new OS
            }
        });
    }
    {
        let c = form.os_linux.clone();
        let arm = arm.clone();
        let amd = amd.clone();
        let img = form.image.clone();
        macos.connect_toggled(move |t| {
            if t.is_active() {
                c.set(false);
                arm.set_active(true); // force arm64 (there is no macOS x86-64)
                amd.set_sensitive(false); // no darwin/x86-64
                img.set_text(""); // clear the Linux image; pick a ddmac template instead
            }
        });
    }
    os_seg.append(&linux);
    os_seg.append(&macos);
    p.append(&labeled("OPERATING SYSTEM", &os_seg));
    p.append(&labeled("ARCHITECTURE", &a_seg));

    // IMAGE comes AFTER OS + ARCH: pick the arch first, then choose from images built for it. The
    // "Choose…" picker reads the current os/arch selection, so it only offers matching templates.
    let irow = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let il = gtk::Label::new(Some("IMAGE"));
    il.add_css_class("flabel");
    il.set_xalign(0.0);
    irow.append(&il);
    let ibox = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    form.image.set_hexpand(true);
    let choose = gtk::Button::with_label("Choose…");
    choose.add_css_class("btn");
    ibox.append(&form.image);
    ibox.append(&choose);
    irow.append(&ibox);
    let ih = gtk::Label::new(Some("Pick a template for the selected architecture, or type any Docker image reference."));
    ih.add_css_class("fhint");
    ih.set_xalign(0.0);
    irow.append(&ih);
    {
        let form2 = form.clone();
        choose.connect_clicked(move |b| {
            if let Some(win) = b.root().and_downcast::<gtk::Window>() {
                open_image_picker(&win, &form2);
            }
        });
    }
    p.append(&irow);

    p.append(&field("DEFAULT SHELL", &form.shell, None));

    // Storage location: an entry + a Browse… folder picker.
    let srow = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let sl = gtk::Label::new(Some("STORAGE LOCATION"));
    sl.add_css_class("flabel");
    sl.set_xalign(0.0);
    srow.append(&sl);
    let sbox = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    form.storage.set_hexpand(true);
    let browse = gtk::Button::with_label("Browse…");
    browse.add_css_class("btn");
    sbox.append(&form.storage);
    sbox.append(&browse);
    srow.append(&sbox);
    let sh = gtk::Label::new(Some("Holds this workspace's docker images, volumes + state. Blank = ~/.dd/workspaces/<name>."));
    sh.add_css_class("fhint");
    sh.set_xalign(0.0);
    srow.append(&sh);
    {
        // GTK4's macOS FileDialog backend crashes the app, so use the native osascript folder chooser.
        let entry = form.storage.clone();
        browse.connect_clicked(move |_| {
            if let Some(path) = pick_folder_dialog() {
                entry.set_text(&path);
            }
        });
    }
    p.append(&srow);
    p
}

/// Native macOS folder picker via `osascript` (GTK4's FileDialog backend crashes on macOS). Returns the
/// chosen POSIX path, or None if cancelled. Blocks until the user picks — it's a modal chooser.
fn pick_folder_dialog() -> Option<String> {
    let out = std::process::Command::new("osascript")
        .arg("-e")
        .arg("POSIX path of (choose folder with prompt \"Choose a folder for this workspace\")")
        .output()
        .ok()?;
    if !out.status.success() {
        return None; // cancelled
    }
    let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if p.is_empty() { None } else { Some(p) }
}

/// Curated image templates for an arch — the "predefined images" the picker offers. `(display, ref, desc)`.
fn curated_images(arch: &str) -> Vec<(&'static str, &'static str, &'static str)> {
    match arch {
        "darwin-arm64" => vec![
            ("ddmac slim", "huttarichard/ddmac:base", "Lean macOS base — a minimal Darwin userland."),
            ("ddmac full", "huttarichard/ddmac:dev", "Full macOS dev image — toolchains + common tools."),
        ],
        // Linux arm64 / amd64 share the same catalog.
        _ => vec![
            ("Ubuntu 24.04 LTS", "ubuntu:24.04", "Latest Ubuntu LTS — the default dev base."),
            ("Ubuntu 22.04 LTS", "ubuntu:22.04", "Previous Ubuntu LTS."),
            ("Debian 12 (Bookworm)", "debian:bookworm", "Stable Debian."),
            ("Alpine", "alpine:latest", "Tiny musl-based image."),
            ("Fedora", "fedora:latest", "Fedora — recent packages."),
            ("AlmaLinux 9", "almalinux:9", "RHEL-compatible enterprise base."),
        ],
    }
}

/// The image-selection window: a list of predefined templates for the workspace's currently-selected
/// architecture. Clicking a row fills the IMAGE field. (Custom refs can still be typed directly.)
fn open_image_picker(parent: &gtk::Window, form: &Rc<Form>) {
    let arch = if !form.os_linux.get() {
        "darwin-arm64"
    } else if form.cpu_amd.get() {
        "amd64"
    } else {
        "arm64"
    };

    let win = gtk::Window::builder()
        .title("Choose an image")
        .modal(true)
        .transient_for(parent)
        .default_width(480)
        .default_height(440)
        .build();

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("imgpick");

    let head = gtk::Box::new(gtk::Orientation::Vertical, 2);
    head.add_css_class("imghead");
    let t = gtk::Label::new(Some("Predefined images"));
    t.add_css_class("ptitle");
    t.set_xalign(0.0);
    let sub = gtk::Label::new(Some(&format!("for {arch} — or Cancel and type a custom image reference")));
    sub.add_css_class("fhint");
    sub.set_xalign(0.0);
    head.append(&t);
    head.append(&sub);
    root.append(&head);

    let list = gtk::ListBox::new();
    list.add_css_class("imglist");
    list.set_selection_mode(gtk::SelectionMode::None);
    for (name, iref, desc) in curated_images(arch) {
        let row = gtk::ListBoxRow::new();
        let bx = gtk::Box::new(gtk::Orientation::Vertical, 2);
        bx.add_css_class("imgrow");
        let n = gtk::Label::new(Some(name));
        n.add_css_class("imgname");
        n.set_xalign(0.0);
        let r = gtk::Label::new(Some(&format!("{iref}  ·  {desc}")));
        r.add_css_class("imgref");
        r.set_xalign(0.0);
        bx.append(&n);
        bx.append(&r);
        row.set_child(Some(&bx));
        let click = gtk::GestureClick::new();
        let form2 = form.clone();
        let win2 = win.clone();
        let iref_s = iref.to_string();
        click.connect_released(move |_, _, _, _| {
            form2.image.set_text(&iref_s);
            win2.close();
        });
        row.add_controller(click);
        list.append(&row);
    }
    let scroller = gtk::ScrolledWindow::builder().vexpand(true).child(&list).build();
    root.append(&scroller);

    let footer = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    footer.add_css_class("footer");
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    footer.append(&spacer);
    let cancel = gtk::Button::with_label("Cancel");
    cancel.add_css_class("btn");
    let win3 = win.clone();
    cancel.connect_clicked(move |_| win3.close());
    footer.append(&cancel);
    root.append(&footer);

    win.set_child(Some(&root));
    win.present();
    macshim::force_dark();
}

fn pane_resources(form: &Rc<Form>) -> gtk::Box {
    let p = pane("Resources");
    form.cpus.set_value(0.0);
    form.mem.set_value(0.0);
    p.append(&spin_field("CPU CORES (0 = unlimited)", &form.cpus));
    p.append(&spin_field("MEMORY MB (0 = unlimited)", &form.mem));
    let hint = gtk::Label::new(Some("Caps applied to the workspace's containers."));
    hint.add_css_class("fhint");
    hint.set_xalign(0.0);
    p.append(&hint);
    // Terminal scrollback (lines of history each shell keeps). Empty/0 = unlimited.
    form.scrollback.set_max_width_chars(12);
    form.scrollback.set_halign(gtk::Align::Start);
    p.append(&field("TERMINAL SCROLLBACK (blank = unlimited)", &form.scrollback, Some("Lines of history each shell retains; blank or 0 = unlimited.")));
    p
}

fn pane_env(form: &Rc<Form>) -> gtk::Box {
    let p = pane("Environment");
    p.append(&form.env_box);
    let add = gtk::Button::with_label("+ Add variable");
    add.add_css_class("addrow");
    add.set_halign(gtk::Align::Start);
    let form2 = form.clone();
    add.connect_clicked(move |_| add_env_row(&form2));
    p.append(&add);
    add_env_row(form); // start with one empty row
    p
}

fn pane_mounts(form: &Rc<Form>) -> gtk::Box {
    let p = pane("Mounts");
    p.append(&form.mount_box);
    let add = gtk::Button::with_label("+ Add mount");
    add.add_css_class("addrow");
    add.set_halign(gtk::Align::Start);
    let form2 = form.clone();
    add.connect_clicked(move |_| add_mount_row(&form2));
    p.append(&add);
    p
}

/// A titled toggle row (the `.dockrow` idiom: bold title + dim wrapped description on the left, a
/// macOS-like switch on the right). Reused by the Docker/Rendering panes and the Settings window.
fn switch_row(title: &str, desc: &str, sw: &gtk::Switch) -> gtk::Box {
    let rowb = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    rowb.add_css_class("dockrow");
    let tb = gtk::Box::new(gtk::Orientation::Vertical, 2);
    tb.set_hexpand(true);
    let tt = gtk::Label::new(Some(title));
    tt.add_css_class("tt");
    tt.set_xalign(0.0);
    let td = gtk::Label::new(Some(desc));
    td.add_css_class("td");
    td.set_xalign(0.0);
    td.set_wrap(true);
    td.set_max_width_chars(46);
    tb.append(&tt);
    tb.append(&td);
    rowb.append(&tb);
    sw.set_valign(gtk::Align::Center);
    rowb.append(sw);
    rowb
}

fn pane_docker(form: &Rc<Form>) -> gtk::Box {
    let p = pane("Docker");
    form.docker.set_active(true);
    p.append(&switch_row(
        "Mount docker socket",
        "Sets DOCKER_HOST inside the workspace so the docker CLI works.",
        &form.docker,
    ));
    p
}

fn pane_rendering(form: &Rc<Form>) -> gtk::Box {
    let p = pane("Rendering");
    p.append(&switch_row(
        "Accelerated GUI rendering (--gui)",
        "Bind-mount the host dd-display socket + set WAYLAND_DISPLAY so a Linux GUI app in this \
         workspace renders on the Mac (GPU-accelerated). Off = headless (terminal only).",
        &form.gui,
    ));
    let hint = gtk::Label::new(Some(
        "Only affects workspaces that launch a graphical app; a plain shell is unchanged. \
         See docs/ideas/RENDERING_PLAN.md.",
    ));
    hint.add_css_class("fhint");
    hint.set_xalign(0.0);
    hint.set_wrap(true);
    hint.set_max_width_chars(46);
    p.append(&hint);
    p
}

fn pane_network(form: &Rc<Form>) -> gtk::Box {
    let p = pane("Network");
    form.vpn.set_hexpand(true);
    p.append(&field(
        "VPN / PROXY EGRESS (blank = direct)",
        &form.vpn,
        Some("Route this workspace's outbound traffic through a VPN/proxy. Enter a SOCKS5 host:port \
              (e.g. 127.30.0.1:1080) or a <kind>:<endpoint> spec (socks5:host:port, http:host:port, \
              wireguard:/path/wg.conf). Blank = direct egress (no VPN).")));
    let hint = gtk::Label::new(Some(
        "SOCKS5 endpoints are wired directly into the engine's egress redirect; tunnel kinds \
         (WireGuard/OpenVPN) are saved but need the userspace-tunnel helper."));
    hint.add_css_class("fhint");
    hint.set_xalign(0.0);
    hint.set_wrap(true);
    hint.set_max_width_chars(46);
    p.append(&hint);
    p
}

fn pane_device(form: &Rc<Form>) -> gtk::Box {
    let p = pane("Device");

    // Enable toggle (mirrors the Docker switch row).
    let rowb = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    rowb.add_css_class("dockrow");
    let tb = gtk::Box::new(gtk::Orientation::Vertical, 2);
    tb.set_hexpand(true);
    let tt = gtk::Label::new(Some("Simulated CUDA device"));
    tt.add_css_class("tt");
    tt.set_xalign(0.0);
    let td = gtk::Label::new(Some(
        "Show this Linux workspace an NVIDIA-looking CUDA device (nvidia-smi, \
         torch.cuda.is_available()). The GPU work is forwarded to the host Apple Metal GPU.",
    ));
    td.add_css_class("td");
    td.set_xalign(0.0);
    td.set_wrap(true);
    td.set_max_width_chars(46);
    tb.append(&tt);
    tb.append(&td);
    rowb.append(&tb);
    form.cuda_on.set_valign(gtk::Align::Center);
    rowb.append(&form.cuda_on);
    p.append(&rowb);

    // Reported device identity.
    form.cuda_name.set_hexpand(true);
    p.append(&field(
        "DEVICE NAME",
        &form.cuda_name,
        Some("Name reported by nvidia-smi / cudaGetDeviceProperties."),
    ));
    form.cuda_cc.set_hexpand(true);
    p.append(&field(
        "COMPUTE CAPABILITY",
        &form.cuda_cc,
        Some("Reported CUDA compute capability, e.g. 8.6 (Ampere-class)."),
    ));
    form.cuda_vram.set_hexpand(true);
    p.append(&field(
        "VRAM (MB)",
        &form.cuda_vram,
        Some("Reported device memory in MB. On Apple Silicon this is carved from unified memory."),
    ));

    let hint = gtk::Label::new(Some(
        "Real memory ops and many custom kernels run on Metal today; full framework acceleration \
         (PyTorch/TF via cuBLAS/cuDNN) is a work in progress. See docs/ideas/CUDA_ON_METAL.md.",
    ));
    hint.add_css_class("fhint");
    hint.set_xalign(0.0);
    hint.set_wrap(true);
    hint.set_max_width_chars(46);
    p.append(&hint);
    p
}

fn add_env_row(form: &Rc<Form>) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let k = entry("KEY", true);
    let v = entry("value", true);
    k.set_hexpand(true);
    v.set_hexpand(true);
    let x = gtk::Button::from_icon_name("user-trash-symbolic");
    x.add_css_class("xbtn");
    x.set_tooltip_text(Some("Remove"));
    row.append(&k);
    row.append(&v);
    row.append(&x);
    form.env_box.append(&row);
    form.env_rows.borrow_mut().push((k.clone(), v.clone()));
    let form2 = form.clone();
    let row2 = row.clone();
    let k2 = k.clone();
    x.connect_clicked(move |_| {
        form2.env_box.remove(&row2);
        form2.env_rows.borrow_mut().retain(|(kk, _)| kk != &k2);
    });
}

fn add_mount_row(form: &Rc<Form>) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let host = entry("/host/path", true);
    let cont = entry("/container/path", true);
    host.set_hexpand(true);
    cont.set_hexpand(true);
    let ro = gtk::CheckButton::with_label("ro");
    ro.set_valign(gtk::Align::Center);
    let x = gtk::Button::from_icon_name("user-trash-symbolic");
    x.add_css_class("xbtn");
    x.set_tooltip_text(Some("Remove"));
    row.append(&host);
    row.append(&cont);
    row.append(&ro);
    row.append(&x);
    form.mount_box.append(&row);
    form.mount_rows.borrow_mut().push((host.clone(), cont.clone(), ro.clone()));
    let form2 = form.clone();
    let row2 = row.clone();
    let h2 = host.clone();
    x.connect_clicked(move |_| {
        form2.mount_box.remove(&row2);
        form2.mount_rows.borrow_mut().retain(|(hh, _, _)| hh != &h2);
    });
}

fn save_workspace(form: &Rc<Form>) -> bool {
    let name = form.name.text().trim().to_string();
    let image = form.image.text().trim().to_string();
    if name.is_empty() || image.is_empty() {
        return false;
    }
    // Map OS + Arch to the internal target. dd supports macOS on arm64 only.
    let arch = match (form.os_linux.get(), form.cpu_amd.get()) {
        (false, _) => Arch::DarwinArm64,
        (true, true) => Arch::Amd64,
        (true, false) => Arch::Arm64,
    };
    let mut ws = Workspace::new(&name, &image, arch);
    let shell = form.shell.text().trim().to_string();
    if !shell.is_empty() {
        ws.shell = Some(shell);
    }
    let storage = form.storage.text().trim().to_string();
    if !storage.is_empty() {
        ws.storage = Some(std::path::PathBuf::from(storage));
    }
    let c = form.cpus.value() as u32;
    if c > 0 {
        ws.cpus = Some(c);
    }
    let m = form.mem.value() as u32;
    if m > 0 {
        ws.memory_mb = Some(m);
    }
    // Terminal scrollback: blank / 0 / "unlimited" → None (unlimited); a positive number → cap.
    let sb = form.scrollback.text().trim().to_ascii_lowercase();
    ws.scrollback = match sb.as_str() {
        "" | "0" | "unlimited" => None,
        _ => sb.parse::<u64>().ok().filter(|n| *n > 0),
    };
    ws.docker_sock = form.docker.is_active();
    ws.gui = form.gui.is_active();
    // VPN/proxy egress: blank → direct (None); otherwise parse the spec (bare host:port defaults to SOCKS5).
    ws.vpn = VpnConfig::parse(form.vpn.text().trim());
    // Simulated CUDA device: off → None; on → build the reported device props (backed by host Metal).
    ws.cuda = if form.cuda_on.is_active() {
        let mut d = CudaDevice::default_device();
        let name = form.cuda_name.text().trim().to_string();
        if !name.is_empty() {
            d.name = name;
        }
        let cc = form.cuda_cc.text().trim().to_string();
        if !cc.is_empty() {
            d.compute_capability = cc;
        }
        if let Ok(mb) = form.cuda_vram.text().trim().parse::<u32>() {
            d.vram_mb = mb.max(1);
        }
        Some(d)
    } else {
        None
    };
    for (k, v) in form.env_rows.borrow().iter() {
        let key = k.text().trim().to_string();
        if !key.is_empty() {
            ws.env.push((key, v.text().trim().to_string()));
        }
    }
    for (h, c, ro) in form.mount_rows.borrow().iter() {
        let host = h.text().trim().to_string();
        let cont = c.text().trim().to_string();
        if !host.is_empty() && !cont.is_empty() {
            ws.mounts.push(Mount { host, container: cont, ro: ro.is_active() });
        }
    }
    let mut store = WorkspaceStore::load(workspaces_conf());
    store.upsert(ws).is_ok()
}

// ---- new-workspace widget helpers ----
fn pane(title: &str) -> gtk::Box {
    let p = gtk::Box::new(gtk::Orientation::Vertical, 14);
    p.add_css_class("pane");
    let t = gtk::Label::new(Some(title));
    t.add_css_class("ptitle");
    t.set_xalign(0.0);
    p.append(&t);
    p
}

fn entry(placeholder: &str, mono: bool) -> gtk::Entry {
    let e = gtk::Entry::new();
    e.set_placeholder_text(Some(placeholder));
    if mono {
        e.add_css_class("mono");
    }
    e
}

fn field(label: &str, e: &gtk::Entry, hint: Option<&str>) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let l = gtk::Label::new(Some(label));
    l.add_css_class("flabel");
    l.set_xalign(0.0);
    b.append(&l);
    b.append(e);
    if let Some(h) = hint {
        let hl = gtk::Label::new(Some(h));
        hl.add_css_class("fhint");
        hl.set_xalign(0.0);
        // Wrap + cap the natural width, else a long hint forces the whole window wide (GTK sizes a
        // non-wrapping label to its full single-line width).
        hl.set_wrap(true);
        hl.set_max_width_chars(46);
        b.append(&hl);
    }
    b
}

/// A labelled row wrapping an arbitrary control (used for the OS/Arch segmented controls).
fn labeled(label: &str, w: &impl IsA<gtk::Widget>) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let l = gtk::Label::new(Some(label));
    l.add_css_class("flabel");
    l.set_xalign(0.0);
    b.append(&l);
    b.append(w);
    b
}

fn spin_field(label: &str, s: &gtk::SpinButton) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let l = gtk::Label::new(Some(label));
    l.add_css_class("flabel");
    l.set_xalign(0.0);
    s.set_halign(gtk::Align::Start);
    b.append(&l);
    b.append(s);
    b
}

// =================================================================================================
// Window 3 — per-workspace Terminal window (native titlebar; full-width tabs below)
// =================================================================================================

struct TermWin {
    stack: gtk::Stack,
    tabs: gtk::Box,
    ws: Workspace,
    focused: RefCell<Option<vte4::Terminal>>,
    entries: RefCell<Vec<TabEntry>>,
    pids: RefCell<HashMap<String, Vec<Rc<Cell<i32>>>>>,
    counter: Cell<u32>,
    shell_no: Cell<u32>,
    /// Monotonic per-pane checkpoint-slot allocator. Each terminal pane (its own engine) gets a stable
    /// slot string ("0", "1", …) that survives close→reopen (persisted in the session `Pane.slot`), so
    /// the pane freezes/restores its OWN process tree independently of the others.
    slot_ctr: Cell<u32>,
    /// Registry of every live pane: its terminal (weak), its checkpoint slot, and its init pid. The
    /// window's close handler iterates this to freeze EACH pane into its own slot (a multi-tab/split
    /// window has no single coherent freeze — one slot per pane fixes that).
    panes: RefCell<Vec<(glib::WeakRef<vte4::Terminal>, String, Rc<Cell<i32>>)>>,
    /// Slim Cmd+F search bar over the focused terminal.
    search: SearchUi,
    /// Keyboard scrollback-navigation ("copy") mode is active.
    copymode: Cell<bool>,
    /// The window is closing (freezing every pane). While set, a shell dying from our own kill must NOT
    /// discard its slot — that would destroy the checkpoint we just wrote.
    closing: Cell<bool>,
}
struct TabEntry {
    name: String,
    button: gtk::Box,
}

/// The minimalist search bar: a slim black overlay with a query field + a match-state hint.
struct SearchUi {
    bar: gtk::Box,
    entry: gtk::Entry,
    info: gtk::Label,
    caseless: Cell<bool>,
}

fn open_terminal_window(app: &gtk::Application, ws: &Workspace) {
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title(&ws.name)
        .default_width(1040)
        .default_height(680)
        .build();

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);

    // Full-width tab strip: a homogeneous box so tabs are EXACTLY equal width (100/50/33/25…) and fill
    // the entire width. No `+` button — new tabs come from ⌘T — so nothing eats into the tab widths.
    let tabbar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    tabbar.add_css_class("tabbar");
    let tabs = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    tabs.set_homogeneous(true);
    tabs.set_hexpand(true);
    tabbar.append(&tabs);

    let stack = gtk::Stack::new();
    stack.add_css_class("pages");
    stack.set_vexpand(true);
    stack.set_hexpand(true);
    // Size to the visible child (a terminal), NOT the tallest child — otherwise the grid is capped.
    stack.set_vhomogeneous(false);
    stack.set_hhomogeneous(false);
    stack.set_transition_type(gtk::StackTransitionType::None);

    // The search bar floats over the terminal stack via an Overlay (top-right, hidden until Cmd+F).
    let overlay = gtk::Overlay::new();
    overlay.set_vexpand(true);
    overlay.set_hexpand(true);
    overlay.set_child(Some(&stack));
    let search = build_search_ui();
    overlay.add_overlay(&search.bar);

    root.append(&tabbar);
    root.append(&overlay);

    let tw = Rc::new(TermWin {
        stack,
        tabs,
        ws: ws.clone(),
        focused: RefCell::new(None),
        entries: RefCell::new(Vec::new()),
        pids: RefCell::new(HashMap::new()),
        counter: Cell::new(0),
        shell_no: Cell::new(0),
        slot_ctr: Cell::new(0),
        panes: RefCell::new(Vec::new()),
        search,
        copymode: Cell::new(false),
        closing: Cell::new(false),
    });
    wire_search(&tw);

    let keys = gtk::EventControllerKey::new();
    // CAPTURE phase so ⌘-shortcuts are handled by the window BEFORE the focused VTE swallows them
    // (otherwise ⌘T/⌘D just type into the terminal).
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    {
        let tw = tw.clone();
        keys.connect_key_pressed(move |_, key, _c, state| {
            // Copy/scroll mode intercepts plain (unmodified) keys for keyboard scrollback navigation.
            if tw.copymode.get() && !state.contains(gdk::ModifierType::META_MASK) {
                if copymode_key(&tw, key, state) {
                    return glib::Propagation::Stop;
                }
            }
            if !state.contains(gdk::ModifierType::META_MASK) {
                return glib::Propagation::Proceed;
            }
            let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
            match key {
                gdk::Key::t | gdk::Key::T => {
                    add_terminal_tab(&tw);
                    glib::Propagation::Stop
                }
                gdk::Key::w | gdk::Key::W => {
                    if let Some(name) = tw.stack.visible_child_name() {
                        close_page(&tw, name.as_str());
                    }
                    glib::Propagation::Stop
                }
                gdk::Key::d | gdk::Key::D => {
                    let o = if shift { gtk::Orientation::Vertical } else { gtk::Orientation::Horizontal };
                    split_focused(&tw, o);
                    glib::Propagation::Stop
                }
                // Cmd+F — toggle the search bar over the focused terminal.
                gdk::Key::f | gdk::Key::F => {
                    search_toggle(&tw);
                    glib::Propagation::Stop
                }
                // Cmd+Shift+C — enter keyboard scroll/copy mode (Esc/q exits).
                gdk::Key::c | gdk::Key::C if shift => {
                    copymode_enter(&tw);
                    glib::Propagation::Stop
                }
                gdk::Key::c | gdk::Key::C => {
                    if let Some(t) = tw.focused.borrow().as_ref() {
                        if t.has_selection() {
                            t.copy_clipboard_format(vte4::Format::Text);
                            return glib::Propagation::Stop;
                        }
                    }
                    glib::Propagation::Proceed // no selection → let ⌘C fall through
                }
                gdk::Key::v | gdk::Key::V => {
                    if let Some(t) = tw.focused.borrow().as_ref() {
                        t.paste_clipboard();
                    }
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
    }
    window.add_controller(keys);

    // On close: FREEZE every live pane into its OWN checkpoint slot (each pane is a separate engine), so
    // reopening restores ALL the shells' process trees — not just a single-shell window. A pane whose
    // freeze fails has just its own slot discarded (so it reopens fresh); the others are unaffected.
    {
        let tw = tw.clone();
        window.connect_close_request(move |_| {
            // Mark closing FIRST so the kill_pg below (→ shells die → child-watch → close_terminal_pane)
            // does not discard the very slots we're about to freeze.
            tw.closing.set(true);
            // Persist the session layout (with each pane's slot) + scrollback BEFORE tearing down, so
            // reopening restores the tabs/splits + on-screen history and re-attaches each pane to its slot.
            save_session(&tw);
            let base = dd_root();
            // Snapshot the live panes (upgradeable weak refs = still-open panes).
            let live: Vec<(String, Rc<Cell<i32>>)> = tw
                .panes
                .borrow()
                .iter()
                .filter_map(|(w, slot, pid)| w.upgrade().map(|_| (slot.clone(), pid.clone())))
                .collect();
            // Checkpoint every slot CONCURRENTLY: spawn all the per-slot `ddcli workspace checkpoint`
            // children at once, then join. Each pane is a SEPARATE engine/slot, so the freezes are
            // independent — running them sequentially made closing an N-tab window take N× a single engine
            // dump (seconds of frozen UI = the "window takes a while to close" report). MUST go through the
            // clean-env `ddcli_command` (dd-term runs under the nix devshell; a raw Command would poison
            // ddcli's loader + its forked engine and silently lose the processes). Every child is joined
            // before the kill_pg below, so all slots are fully frozen before any pane is torn down.
            let mut freezes: Vec<(String, Option<std::process::Child>)> = live
                .iter()
                .map(|(slot, _pid)| {
                    let child =
                        ddcli_command(&["workspace", "checkpoint", &tw.ws.name, "--slot", slot]).spawn().ok();
                    (slot.clone(), child)
                })
                .collect();
            for (slot, child) in &mut freezes {
                let ok = child.take().and_then(|mut c| c.wait().ok()).map(|s| s.success()).unwrap_or(false);
                if !ok {
                    eprintln!("[dd-term] freeze of {:?} slot {slot} failed — discarding that slot", tw.ws.name);
                    let d = tw.ws.checkpoint_slot_dir(&base, slot);
                    let _ = std::fs::remove_dir_all(&d);
                    // Also drop the control-channel leftovers so the discarded slot reopens truly fresh.
                    let ds = d.to_string_lossy().into_owned();
                    let _ = std::fs::remove_file(format!("{ds}.trigger"));
                    let _ = std::fs::remove_file(format!("{ds}.pid"));
                }
            }
            for (_slot, pid) in &live {
                kill_pg(pid.get());
            }
            glib::Propagation::Proceed
        });
    }

    add_dashboard_tab(&tw);
    // Restore the saved session (tabs + splits + per-pane history) if this workspace has one; else open a
    // single fresh shell. The debug hooks below still layer on top.
    let saved = Session::load(&tw.ws.storage_dir(&dd_root()));
    if saved.tabs.is_empty() {
        add_terminal_tab(&tw);
    } else {
        restore_session(&tw, &saved);
    }
    // Debug: DD_TERM_TABS=N opens N total shell tabs (to verify exact equal-width tabs).
    if let Some(n) = std::env::var("DD_TERM_TABS").ok().and_then(|s| s.parse::<usize>().ok()) {
        for _ in 1..n {
            add_terminal_tab(&tw);
        }
    }
    // Debug: DD_TERM_SPLIT=h|v splits the current shell tab (to screenshot the split separator).
    if let Ok(dir) = std::env::var("DD_TERM_SPLIT") {
        if let Some(t) = tw.stack.visible_child().and_then(|c| first_terminal_in(&c)) {
            *tw.focused.borrow_mut() = Some(t);
            let o = if dir == "v" { gtk::Orientation::Vertical } else { gtk::Orientation::Horizontal };
            split_focused(&tw, o);
        }
    }
    // Debug: DD_TERM_DASH selects the dashboard (first) tab for screenshotting.
    if std::env::var("DD_TERM_DASH").is_ok() {
        let first = tw.entries.borrow().first().map(|e| e.name.clone());
        if let Some(n) = first {
            select_page(&tw, &n);
        }
    }

    window.set_child(Some(&root));
    window.present();
    macshim::force_dark();
    maybe_shot(&window, "terminal");
}

// -------------------------------------------------------------------------------------------------
// Search bar (Cmd+F) — minimalist, highlights matches via VTE's search API.
// -------------------------------------------------------------------------------------------------

fn build_search_ui() -> SearchUi {
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    bar.add_css_class("searchbar");
    bar.set_halign(gtk::Align::End);
    bar.set_valign(gtk::Align::Start);
    bar.set_visible(false);
    let entry = gtk::Entry::new();
    entry.add_css_class("searchfield");
    entry.set_placeholder_text(Some("Find"));
    entry.set_width_chars(22);
    let info = gtk::Label::new(None);
    info.add_css_class("searchinfo");
    bar.append(&entry);
    bar.append(&info);
    SearchUi { bar, entry, info, caseless: Cell::new(true) }
}

fn wire_search(tw: &Rc<TermWin>) {
    {
        let tw = tw.clone();
        tw.search.entry.clone().connect_changed(move |_| search_update(&tw));
    }
    let kc = gtk::EventControllerKey::new();
    kc.set_propagation_phase(gtk::PropagationPhase::Capture);
    {
        let tw = tw.clone();
        kc.connect_key_pressed(move |_, key, _c, state| {
            let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
            match key {
                gdk::Key::Return | gdk::Key::KP_Enter => {
                    search_step(&tw, !shift);
                    glib::Propagation::Stop
                }
                gdk::Key::Escape => {
                    search_hide(&tw);
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
    }
    tw.search.entry.add_controller(kc);
}

fn search_toggle(tw: &Rc<TermWin>) {
    if tw.search.bar.get_visible() {
        search_hide(tw);
    } else {
        tw.search.bar.set_visible(true);
        tw.search.entry.grab_focus();
        if !tw.search.entry.text().is_empty() {
            search_update(tw);
        }
    }
}

fn search_hide(tw: &Rc<TermWin>) {
    tw.search.bar.set_visible(false);
    if let Some(t) = tw.focused.borrow().clone() {
        t.search_set_regex(None, 0);
        t.unselect_all();
        t.grab_focus();
    }
}

/// (Re)compile the query and jump to the first match. The query is tried as a regex; if it doesn't
/// compile, it's escaped and matched literally (so plain text always "just works").
fn search_update(tw: &Rc<TermWin>) {
    let Some(t) = tw.focused.borrow().clone() else { return };
    let text = tw.search.entry.text().to_string();
    if text.is_empty() {
        t.search_set_regex(None, 0);
        tw.search.info.set_text("");
        tw.search.info.remove_css_class("nomatch");
        return;
    }
    let mut flags = PCRE2_UTF | PCRE2_NO_UTF_CHECK | PCRE2_MULTILINE | PCRE2_UCP;
    if tw.search.caseless.get() {
        flags |= PCRE2_CASELESS;
    }
    let re = vte4::Regex::for_search(&text, flags).or_else(|_| {
        let escaped = glib::Regex::escape_string(text.as_str());
        vte4::Regex::for_search(escaped.as_str(), flags)
    });
    match re {
        Ok(re) => {
            t.search_set_regex(Some(&re), 0);
            t.search_set_wrap_around(true);
            let found = t.search_find_next();
            search_set_state(tw, found);
        }
        Err(_) => search_set_state(tw, false),
    }
}

fn search_step(tw: &Rc<TermWin>, forward: bool) {
    let Some(t) = tw.focused.borrow().clone() else { return };
    if tw.search.entry.text().is_empty() {
        return;
    }
    let found = if forward { t.search_find_next() } else { t.search_find_previous() };
    search_set_state(tw, found);
}

fn search_set_state(tw: &Rc<TermWin>, found: bool) {
    if found {
        tw.search.info.set_text("");
        tw.search.info.remove_css_class("nomatch");
    } else {
        tw.search.info.set_text("no match");
        tw.search.info.add_css_class("nomatch");
    }
}

// -------------------------------------------------------------------------------------------------
// Copy / scroll mode (Cmd+Shift+C) — keyboard scrollback navigation without the mouse.
//
// VTE 0.8 exposes no API to set an arbitrary text selection by cell coordinates, so a full vi visual
// selection isn't achievable without reimplementing the grid. This mode therefore focuses on what IS
// possible: keyboard scrollback navigation (j/k, Ctrl-d/u, g/G), `/` to hand off to search, and
// select-all + yank. Esc/q exits.
// -------------------------------------------------------------------------------------------------

fn copymode_enter(tw: &Rc<TermWin>) {
    tw.copymode.set(true);
    if let Some(t) = tw.focused.borrow().clone() {
        t.add_css_class("copymode");
    }
}

fn copymode_exit(tw: &Rc<TermWin>) {
    tw.copymode.set(false);
    if let Some(t) = tw.focused.borrow().clone() {
        t.remove_css_class("copymode");
    }
}

/// Handle a plain (unmodified/Ctrl) key while in copy mode. Returns true if the key was consumed
/// (copy mode swallows all keys so nothing leaks into the shell).
fn copymode_key(tw: &Rc<TermWin>, key: gdk::Key, state: gdk::ModifierType) -> bool {
    let Some(t) = tw.focused.borrow().clone() else {
        copymode_exit(tw);
        return true;
    };
    let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
    let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
    let scroll = |lines: f64| {
        if let Some(adj) = t.vadjustment() {
            let max = (adj.upper() - adj.page_size()).max(adj.lower());
            adj.set_value((adj.value() + lines).clamp(adj.lower(), max));
        }
    };
    let page = t.vadjustment().map(|a| a.page_size()).unwrap_or(20.0);
    match key {
        gdk::Key::Escape | gdk::Key::q | gdk::Key::Q => copymode_exit(tw),
        gdk::Key::j | gdk::Key::Down => scroll(1.0),
        gdk::Key::k | gdk::Key::Up => scroll(-1.0),
        gdk::Key::d | gdk::Key::D if ctrl => scroll(page / 2.0),
        gdk::Key::u | gdk::Key::U if ctrl => scroll(-page / 2.0),
        gdk::Key::Page_Down | gdk::Key::space => scroll(page),
        gdk::Key::Page_Up => scroll(-page),
        gdk::Key::g if !shift => {
            if let Some(adj) = t.vadjustment() {
                adj.set_value(adj.lower());
            }
        }
        gdk::Key::g | gdk::Key::G if shift => {
            if let Some(adj) = t.vadjustment() {
                adj.set_value((adj.upper() - adj.page_size()).max(adj.lower()));
            }
        }
        gdk::Key::slash => {
            copymode_exit(tw);
            search_toggle(tw);
        }
        gdk::Key::a | gdk::Key::A | gdk::Key::v | gdk::Key::V => t.select_all(),
        gdk::Key::y | gdk::Key::Y | gdk::Key::Return | gdk::Key::KP_Enter => {
            if t.has_selection() {
                t.copy_clipboard_format(vte4::Format::Text);
            }
            copymode_exit(tw);
        }
        _ => {}
    }
    true
}

// -------------------------------------------------------------------------------------------------
// Session / multiplexer — persist the tab+split layout + per-pane history; restore on reopen.
// -------------------------------------------------------------------------------------------------

/// Snapshot the window's tabs (skipping the dashboard) + each pane's scrollback into a [`Session`] and
/// write it (layout + history files) under the workspace storage dir.
fn save_session(tw: &Rc<TermWin>) {
    let storage = tw.ws.storage_dir(&dd_root());
    // Fresh history files each save (avoid stale accumulation).
    let dir = Session::dir(&storage);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);

    let mut hist_idx = 0usize;
    let mut tabs = Vec::new();
    // entries[0] is the non-closable dashboard; shells are the rest.
    let entries: Vec<(String, String)> = {
        let es = tw.entries.borrow();
        es.iter().skip(1).map(|e| (e.name.clone(), tab_title(&e.button))).collect()
    };
    for (page_name, title) in entries {
        let Some(child) = tw.stack.child_by_name(&page_name) else { continue };
        if let Some(root) = snapshot_node(tw, &child, &storage, &mut hist_idx) {
            tabs.push(SessionTab { title, root });
        }
    }
    let session = Session { tabs };
    if session.tabs.is_empty() {
        Session::clear(&storage);
    } else {
        let _ = session.save(&storage);
    }
}

/// Read a tab button's title label text (for restoring the tab title).
fn tab_title(button: &gtk::Box) -> String {
    // button = [inner Box [ (icon?) label ], (x button)]; find the first Label in the tree.
    fn find_label(w: &gtk::Widget) -> Option<String> {
        if let Some(l) = w.downcast_ref::<gtk::Label>() {
            let t = l.text().to_string();
            if !t.is_empty() {
                return Some(t);
            }
        }
        let mut c = w.first_child();
        while let Some(ch) = c {
            if let Some(s) = find_label(&ch) {
                return Some(s);
            }
            c = ch.next_sibling();
        }
        None
    }
    find_label(button.upcast_ref()).unwrap_or_else(|| "shell".to_string())
}

/// Walk a page's widget subtree into a [`PaneNode`], dumping each terminal's history to a file and
/// recording each pane's checkpoint slot (so the pane re-attaches to its frozen tree on reopen).
fn snapshot_node(tw: &Rc<TermWin>, w: &gtk::Widget, storage: &std::path::Path, hist_idx: &mut usize) -> Option<PaneNode> {
    if let Some(t) = w.downcast_ref::<vte4::Terminal>() {
        let cwd = t.current_directory_uri().and_then(|u| session::cwd_from_uri(&u));
        let text = dump_terminal_history(t);
        let history_file = if text.trim().is_empty() {
            None
        } else {
            let file = format!("hist-{}.txt", *hist_idx);
            *hist_idx += 1;
            if std::fs::write(Session::history_path(storage, &file), &text).is_ok() {
                Some(file)
            } else {
                None
            }
        };
        let slot = slot_of_terminal(tw, t);
        return Some(PaneNode::Leaf(Pane { cwd, history_file, slot }));
    }
    if let Some(paned) = w.downcast_ref::<gtk::Paned>() {
        let dir = if paned.orientation() == gtk::Orientation::Horizontal {
            SplitDir::Horizontal
        } else {
            SplitDir::Vertical
        };
        let dim = if dir == SplitDir::Horizontal { paned.width() } else { paned.height() };
        let ratio = if dim > 1 { paned.position() as f64 / dim as f64 } else { 0.5 };
        let a = paned.start_child().and_then(|c| snapshot_node(tw, &c, storage, hist_idx));
        let b = paned.end_child().and_then(|c| snapshot_node(tw, &c, storage, hist_idx));
        return match (a, b) {
            (Some(a), Some(b)) => Some(PaneNode::Split { dir, ratio, a: Box::new(a), b: Box::new(b) }),
            (Some(n), None) | (None, Some(n)) => Some(n),
            (None, None) => None,
        };
    }
    // A container (the paneroot Box) — descend into its first meaningful child.
    let mut c = w.first_child();
    while let Some(ch) = c {
        if let Some(n) = snapshot_node(tw, &ch, storage, hist_idx) {
            return Some(n);
        }
        c = ch.next_sibling();
    }
    None
}

/// Rebuild all tabs from a saved [`Session`].
fn restore_session(tw: &Rc<TermWin>, session: &Session) {
    let storage = tw.ws.storage_dir(&dd_root());
    for tab in &session.tabs {
        let n = tw.shell_no.get() + 1;
        tw.shell_no.set(n);
        let paneroot = gtk::Box::new(gtk::Orientation::Vertical, 0);
        paneroot.set_hexpand(true);
        paneroot.set_vexpand(true);
        let mut pids = Vec::new();
        let (widget, first) = build_pane_widget(tw, &tab.root, &storage, &mut pids);
        paneroot.append(&widget);
        let title = if tab.title.is_empty() { format!("shell {n}") } else { tab.title.clone() };
        let name = add_page(tw, &title, None, &paneroot, true);
        tw.pids.borrow_mut().entry(name).or_default().extend(pids);
        if let Some(t) = first {
            t.grab_focus();
        }
    }
}

/// Build the widget tree for a saved pane node, collecting the pane pids. Returns the root widget and
/// the first terminal (to focus).
fn build_pane_widget(
    tw: &Rc<TermWin>,
    node: &PaneNode,
    storage: &std::path::Path,
    pids: &mut Vec<Rc<Cell<i32>>>,
) -> (gtk::Widget, Option<vte4::Terminal>) {
    match node {
        PaneNode::Leaf(pane) => {
            let history = pane
                .history_file
                .as_ref()
                .and_then(|f| std::fs::read_to_string(Session::history_path(storage, f)).ok());
            // Reuse the pane's saved slot (fresh one if the session predates slots), and restore ONLY if
            // that slot actually has a frozen checkpoint on disk.
            let slot = adopt_slot(tw, &pane.slot);
            let restore = slot_has_checkpoint(&tw.ws, &slot);
            let (term, pid) = make_terminal_ex(tw, pane.cwd.clone(), history, slot, restore);
            pids.push(pid);
            (term.clone().upcast(), Some(term))
        }
        PaneNode::Split { dir, ratio, a, b } => {
            let orient = if *dir == SplitDir::Horizontal {
                gtk::Orientation::Horizontal
            } else {
                gtk::Orientation::Vertical
            };
            let paned = gtk::Paned::new(orient);
            paned.set_resize_start_child(true);
            paned.set_resize_end_child(true);
            paned.set_hexpand(true);
            paned.set_vexpand(true);
            let (wa, fa) = build_pane_widget(tw, a, storage, pids);
            let (wb, fb) = build_pane_widget(tw, b, storage, pids);
            paned.set_start_child(Some(&wa));
            paned.set_end_child(Some(&wb));
            // Apply the saved split ratio once the paned has been allocated a size.
            let p = paned.clone();
            let r = *ratio;
            let horizontal = *dir == SplitDir::Horizontal;
            glib::timeout_add_local_once(std::time::Duration::from_millis(150), move || {
                let dim = if horizontal { p.width() } else { p.height() };
                if dim > 1 {
                    p.set_position((r * dim as f64).round() as i32);
                }
            });
            (paned.upcast(), fa.or(fb))
        }
    }
}

fn add_page(tw: &Rc<TermWin>, title: &str, icon: Option<&str>, content: &impl IsA<gtk::Widget>, closable: bool) -> String {
    let id = tw.counter.get();
    tw.counter.set(id + 1);
    let name = format!("p{id}");
    tw.stack.add_named(content, Some(&name));

    let bx = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    bx.add_css_class("tab");
    bx.set_hexpand(true);
    let inner = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    inner.set_hexpand(true);
    inner.set_halign(gtk::Align::Center);
    if let Some(ic) = icon {
        let il = gtk::Label::new(Some(ic));
        il.add_css_class("di");
        inner.append(&il);
    }
    let lbl = gtk::Label::new(Some(title));
    lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
    inner.append(&lbl);
    bx.append(&inner);
    if closable {
        let x = gtk::Button::from_icon_name("window-close-symbolic");
        x.add_css_class("tabx");
        let tw2 = tw.clone();
        let name2 = name.clone();
        x.connect_clicked(move |_| close_page(&tw2, &name2));
        bx.append(&x);
    }
    let click = gtk::GestureClick::new();
    let tw2 = tw.clone();
    let name2 = name.clone();
    click.connect_released(move |_, _, _, _| select_page(&tw2, &name2));
    bx.add_controller(click);

    tw.tabs.append(&bx);
    tw.entries.borrow_mut().push(TabEntry { name: name.clone(), button: bx });
    select_page(tw, &name);
    name
}

fn select_page(tw: &Rc<TermWin>, name: &str) {
    if !tw.entries.borrow().iter().any(|e| e.name == name) {
        return;
    }
    tw.stack.set_visible_child_name(name);
    for e in tw.entries.borrow().iter() {
        if e.name == name {
            e.button.add_css_class("on");
        } else {
            e.button.remove_css_class("on");
        }
    }
}

fn close_page(tw: &Rc<TermWin>, name: &str) {
    // Non-closable dashboard (first tab) stays.
    if tw.entries.borrow().first().map(|e| e.name.as_str()) == Some(name) {
        return;
    }
    for p in tw.pids.borrow_mut().remove(name).unwrap_or_default() {
        kill_pg(p.get());
    }
    if let Some(child) = tw.stack.child_by_name(name) {
        // A user-closed tab must forget its panes' frozen checkpoints so they don't restore next time.
        discard_page_slots(tw, &child);
        tw.stack.remove(&child);
    }
    let mut pos = None;
    {
        let mut es = tw.entries.borrow_mut();
        if let Some(i) = es.iter().position(|e| e.name == name) {
            tw.tabs.remove(&es[i].button);
            es.remove(i);
            pos = Some(i.min(es.len().saturating_sub(1)));
        }
    }
    if let Some(i) = pos {
        let next = tw.entries.borrow().get(i).map(|e| e.name.clone());
        if let Some(n) = next {
            select_page(tw, &n);
        }
    }
}

fn add_dashboard_tab(tw: &Rc<TermWin>) {
    let dash = build_dashboard(&tw.ws);
    add_page(tw, &tw.ws.name, Some("◧"), &dash, false);
}

fn add_terminal_tab(tw: &Rc<TermWin>) {
    let n = tw.shell_no.get() + 1;
    tw.shell_no.set(n);
    let paneroot = gtk::Box::new(gtk::Orientation::Vertical, 0);
    paneroot.set_hexpand(true);
    paneroot.set_vexpand(true);
    // OSC-7: open the new tab in the currently-focused shell's cwd. A brand-new tab gets a fresh slot
    // and never restores (nothing frozen for it yet).
    let (term, pid) = make_terminal_ex(tw, focused_cwd(tw), None, alloc_slot(tw), false);
    paneroot.append(&term);
    let name = add_page(tw, &format!("shell {n}"), None, &paneroot, true);
    tw.pids.borrow_mut().entry(name).or_default().push(pid);
    term.grab_focus();
}

/// Find the first VTE terminal in `w`'s subtree (used by the DD_TERM_SPLIT screenshot hook).
fn first_terminal_in(w: &gtk::Widget) -> Option<vte4::Terminal> {
    if let Some(t) = w.downcast_ref::<vte4::Terminal>() {
        return Some(t.clone());
    }
    let mut child = w.first_child();
    while let Some(c) = child {
        if let Some(t) = first_terminal_in(&c) {
            return Some(t);
        }
        child = c.next_sibling();
    }
    None
}

fn split_focused(tw: &Rc<TermWin>, orient: gtk::Orientation) {
    let Some(old) = tw.focused.borrow().clone() else { return };
    let Some(parent) = old.parent() else { return };
    let page = page_name_of(tw, old.upcast_ref::<gtk::Widget>());
    // OSC-7: split panes inherit the source pane's cwd. A fresh split gets a fresh slot; never restores.
    let split_cwd = old.current_directory_uri().and_then(|u| session::cwd_from_uri(&u));
    let (new, pid) = make_terminal_ex(tw, split_cwd, None, alloc_slot(tw), false);
    if let Some(name) = &page {
        tw.pids.borrow_mut().entry(name.clone()).or_default().push(pid);
    }
    let paned = gtk::Paned::new(orient);
    paned.set_resize_start_child(true);
    paned.set_resize_end_child(true);
    paned.set_hexpand(true);
    paned.set_vexpand(true);

    if let Some(bx) = parent.downcast_ref::<gtk::Box>() {
        bx.remove(&old);
        paned.set_start_child(Some(&old));
        paned.set_end_child(Some(&new));
        bx.append(&paned);
    } else if let Some(pp) = parent.downcast_ref::<gtk::Paned>() {
        let is_start = pp.start_child().as_ref() == Some(old.upcast_ref::<gtk::Widget>());
        if is_start {
            pp.set_start_child(gtk::Widget::NONE);
        } else {
            pp.set_end_child(gtk::Widget::NONE);
        }
        paned.set_start_child(Some(&old));
        paned.set_end_child(Some(&new));
        if is_start {
            pp.set_start_child(Some(&paned));
        } else {
            pp.set_end_child(Some(&paned));
        }
    } else {
        return;
    }
    new.grab_focus();
}

/// Walk up from `w` to the widget whose parent is the pages stack; return that stack child's name.
fn page_name_of(tw: &Rc<TermWin>, w: &gtk::Widget) -> Option<String> {
    let mut cur = w.clone();
    loop {
        let parent = cur.parent()?;
        if parent.downcast_ref::<gtk::Stack>().is_some() {
            return tw.stack.page(&cur).name().map(|s| s.to_string());
        }
        cur = parent;
    }
}

/// A shell exited → close its pane. If it's in a split, collapse the split (keep the sibling);
/// otherwise close the whole tab.
fn close_terminal_pane(tw: &Rc<TermWin>, term: &vte4::Terminal) {
    // During a window close we deliberately kill the shells AFTER freezing them — that kill fires this
    // handler, but the freeze must survive, so do nothing here.
    if tw.closing.get() {
        return;
    }
    // The shell exited (or a split is collapsing) → this pane is gone for good; drop its slot + any stale
    // frozen checkpoint so a reopen won't resurrect it. (If this closes the whole tab, close_page handles
    // the rest; this terminal is already out of the registry so there's no double work.)
    discard_terminal_slot(tw, term);
    let Some(parent) = term.parent() else { return };
    if let Some(paned) = parent.downcast_ref::<gtk::Paned>() {
        let is_start = paned.start_child().as_ref() == Some(term.upcast_ref::<gtk::Widget>());
        let sibling = if is_start { paned.end_child() } else { paned.start_child() };
        let Some(sibling) = sibling else {
            if let Some(n) = page_name_of(tw, term.upcast_ref()) {
                close_page(tw, &n);
            }
            return;
        };
        paned.set_start_child(gtk::Widget::NONE);
        paned.set_end_child(gtk::Widget::NONE);
        let Some(pparent) = paned.parent() else { return };
        if let Some(bx) = pparent.downcast_ref::<gtk::Box>() {
            bx.remove(paned);
            bx.append(&sibling);
        } else if let Some(pp) = pparent.downcast_ref::<gtk::Paned>() {
            let paned_is_start = pp.start_child().as_ref() == Some(paned.upcast_ref::<gtk::Widget>());
            if paned_is_start {
                pp.set_start_child(Some(&sibling));
            } else {
                pp.set_end_child(Some(&sibling));
            }
        }
        sibling.grab_focus();
    } else if let Some(n) = page_name_of(tw, term.upcast_ref()) {
        close_page(tw, &n);
    }
}

/// Build a terminal for checkpoint `slot`, optionally starting in `cwd` (OSC-7 new-tab-in-cwd) and
/// replaying `history` text (freeze/restore scrollback persistence) above the live shell. `restore`
/// resumes THIS slot's frozen process tree (decided by the caller from whether the slot has a MANIFEST).
fn make_terminal_ex(tw: &Rc<TermWin>, cwd: Option<String>, history: Option<String>, slot: String, restore: bool) -> (vte4::Terminal, Rc<Cell<i32>>) {
    let term = vte4::Terminal::new();
    let cfg = current_config();
    style_terminal(&term, &cfg);
    // Per-workspace scrollback cap wins over the global config default; otherwise keep the config's.
    if tw.ws.scrollback.is_some() {
        term.set_scrollback_lines(tw.ws.scrollback_lines());
    }
    register_terminal(&term);
    setup_hyperlinks(&term);
    {
        let tw = tw.clone();
        let t = term.clone();
        let fc = gtk::EventControllerFocus::new();
        fc.connect_enter(move |_| *tw.focused.borrow_mut() = Some(t.clone()));
        term.add_controller(fc);
    }
    // Gentler, more natural scrolling. macOS trackpad / high-res wheel deltas make VTE's default scroll
    // fly by many lines per flick; intercept in the capture phase and move the scrollback a damped,
    // clamped number of lines. When there is no scrollback to move (alt-screen apps like htop/less/vim),
    // fall through so VTE still maps the wheel to arrow keys.
    {
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        scroll.set_propagation_phase(gtk::PropagationPhase::Capture);
        let t = term.clone();
        scroll.connect_scroll(move |_, _dx, dy| {
            if let Some(adj) = t.vadjustment() {
                let max = adj.upper() - adj.page_size();
                if max <= adj.lower() {
                    return glib::Propagation::Proceed; // no scrollback (alt-screen) → let VTE handle it
                }
                let lines = (dy * 3.0).clamp(-5.0, 5.0);
                adj.set_value((adj.value() + lines).clamp(adj.lower(), max));
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        term.add_controller(scroll);
    }
    let pid = Rc::new(Cell::new(0));
    // Register this pane (terminal + its slot + pid) so the window's close handler can freeze it into its
    // own slot, and `save_session` can record which slot each pane owns.
    tw.panes.borrow_mut().push((term.downgrade(), slot.clone(), pid.clone()));
    let ddcli = ddcli_path();
    // DEBUG: DD_TERM_CMD overrides the whole command (isolate VTE-spawn vs ddcli); DD_TERM_DEBUG_LOG
    // captures ddcli's output to a file to diagnose the early exit.
    let testcmd = std::env::var("DD_TERM_CMD").ok();
    let dbg = std::env::var("DD_TERM_DEBUG_LOG").ok();
    let dbgcmd = dbg
        .as_ref()
        .map(|p| format!("exec '{}' workspace launch '{}' --slot '{}' > '{}' 2>&1", ddcli, tw.ws.name, slot, p));
    let cwd_arg = cwd.filter(|c| c.starts_with('/'));
    // Always pass this pane's `--slot`; add `--restore` when this slot has a frozen tree to resume, else
    // a `--cwd` for OSC-7 new-tab-in-cwd. (Restore ignores cwd — the checkpoint carries every cwd.)
    let mut launch_args: Vec<&str> =
        vec![ddcli.as_str(), "workspace", "launch", tw.ws.name.as_str(), "--slot", slot.as_str()];
    if restore {
        launch_args.push("--restore");
    } else if let Some(dir) = &cwd_arg {
        launch_args.push("--cwd");
        launch_args.push(dir.as_str());
    }
    let argv: Vec<&str> = if let Some(c) = &testcmd {
        vec!["/bin/sh", "-c", c.as_str()]
    } else if let Some(c) = &dbgcmd {
        vec!["/bin/sh", "-c", c.as_str()]
    } else {
        launch_args
    };
    // A CLEAN minimal env — NOT the full parent env. dd-term runs under the nix devshell, whose
    // DYLD_*/GTK/GI library-path vars would poison `ddcli`'s dynamic loader (and its forked engine),
    // crashing it at startup (SIGSEGV). Pass only what a shell needs.
    let mut env = vec![
        "TERM=xterm-256color".to_string(),
        "PATH=/Users/x/.local/bin:/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin".to_string(),
    ];
    for k in ["HOME", "USER", "LOGNAME", "LANG", "LC_ALL", "LC_CTYPE", "TMPDIR", "SSH_AUTH_SOCK"] {
        if let Ok(v) = std::env::var(k) {
            env.push(format!("{k}={v}"));
        }
    }
    let envv: Vec<&str> = env.iter().map(|s| s.as_str()).collect();
    // Replay saved scrollback/screen history (freeze/restore persistence) ABOVE the live shell, before
    // spawning, so the user's prior screen is visible the instant the window reopens.
    if let Some(text) = history {
        let bytes = session::replay_bytes(&text);
        if !bytes.is_empty() {
            term.feed(&bytes);
        }
    }
    // NOTE: we deliberately do NOT use VTE's spawn_async — on macOS it fork()s inside the multithreaded
    // GTK process and does non-async-signal-safe work before exec, which crashes the child before it
    // runs (every command "exits 11"). Instead spawn via posix_spawn (async-safe) onto a PTY we own.
    match spawn_on_pty(&term, &argv, &envv) {
        Ok((child, pty)) => {
            pid.set(child);
            // Keep the FOREIGN pty sized to the terminal grid — VTE doesn't resize a foreign pty itself,
            // so without this htop is malformed / half-height and doesn't reflow on window resize.
            let weak = term.downgrade();
            let mut last = (0, 0);
            glib::timeout_add_local(std::time::Duration::from_millis(300), move || {
                let Some(t) = weak.upgrade() else { return glib::ControlFlow::Break };
                let (c, r) = (t.column_count() as i32, t.row_count() as i32);
                if c > 0 && r > 0 && (c, r) != last {
                    let _ = pty.set_size(r, c);
                    last = (c, r);
                }
                glib::ControlFlow::Continue
            });
            // Shell exit → close this pane/tab (collapse a split, else close the tab). BUT a shell that dies
            // almost immediately means the LAUNCH failed (e.g. the host was momentarily saturated and the
            // engine couldn't start) — don't silently vanish the tab, which reads as "the shortcut did
            // nothing". Show the exit inline and keep the pane so the failure is visible and retryable.
            let tw2 = tw.clone();
            let te = term.clone();
            let born = std::time::Instant::now();
            glib::child_watch_add_local(glib::Pid(child), move |_pid, status| {
                if born.elapsed() < std::time::Duration::from_millis(2500) {
                    let code = (status >> 8) & 0xff;
                    te.feed(
                        format!("\r\n\x1b[31mshell exited immediately (status {code}) — launch failed; press ⌘T to retry\x1b[0m\r\n")
                            .as_bytes(),
                    );
                    return;
                }
                close_terminal_pane(&tw2, &te);
            });
            if let Ok(text) = std::env::var("DD_TERM_TYPE") {
                let t2 = term.clone();
                glib::timeout_add_local_once(std::time::Duration::from_millis(3000), move || {
                    t2.feed_child(format!("{text}\n").as_bytes());
                });
            }
        }
        Err(e) => term.feed(format!("\r\n\x1b[31mfailed to start shell: {e}\x1b[0m\r\n").as_bytes()),
    }
    (term, pid)
}

/// A URL matcher for auto-linking bare URLs (VTE turns matches into clickable regions). Explicit OSC-8
/// hyperlinks are handled separately (via `hyperlink_hover_uri`).
const URL_REGEX: &str = r"(?:https?://|www\.)[^\s<>\x22'`{}|\\^\[\]]+[^\s<>\x22'`{}|\\^\[\].,;:!?)]";

/// Wire URL auto-matching + click-to-open for a terminal: bare `http(s)://…` and explicit OSC-8
/// hyperlinks open in the macOS browser on Cmd+click (or a plain click on a match), with a pointer hover
/// cursor + a tooltip cue.
fn setup_hyperlinks(term: &vte4::Terminal) {
    let flags = PCRE2_UTF | PCRE2_NO_UTF_CHECK | PCRE2_MULTILINE | PCRE2_UCP | PCRE2_CASELESS;
    if let Ok(re) = vte4::Regex::for_match(URL_REGEX, flags) {
        let tag = term.match_add_regex(&re, 0);
        term.match_set_cursor_name(tag, "pointer");
    }
    term.set_mouse_autohide(true);

    // Hover cue: reflect the hovered OSC-8 link in the tooltip.
    term.connect_hyperlink_hover_uri_notify(|t| {
        let uri = t.hyperlink_hover_uri();
        t.set_tooltip_text(uri.as_deref());
    });

    // Click-to-open. Primary click over a URL match opens it; a modifier (Cmd/Ctrl) always opens the link
    // under the pointer even for explicit OSC-8 links.
    let click = gtk::GestureClick::new();
    click.set_button(1); // primary only
    let t = term.clone();
    click.connect_released(move |g, _n, x, y| {
        // Cmd/Ctrl-click opens the link under the pointer (an explicit OSC-8 hyperlink, else a regex URL
        // match). A modifier is required so a plain click / text selection is never hijacked.
        let state = g.current_event_state();
        let modified = state.contains(gdk::ModifierType::META_MASK) || state.contains(gdk::ModifierType::CONTROL_MASK);
        if !modified {
            return;
        }
        let uri = t.hyperlink_hover_uri().or_else(|| t.check_match_at(x, y).0);
        if let Some(uri) = uri {
            open_url(&uri);
        }
    });
    term.add_controller(click);
}

/// Open a URL in the macOS default browser (`open`). `www.`-prefixed bare matches get an `https://`.
fn open_url(url: &str) {
    let full = if url.starts_with("www.") { format!("https://{url}") } else { url.to_string() };
    let _ = std::process::Command::new("open").arg(&full).spawn();
}

/// The focused terminal's current directory (decoded from OSC 7's `file://` URI), for new-tab-in-cwd.
fn focused_cwd(tw: &Rc<TermWin>) -> Option<String> {
    let term = tw.focused.borrow().clone()?;
    let uri = term.current_directory_uri()?;
    session::cwd_from_uri(&uri)
}

/// Extract a terminal's whole scrollback + visible screen as plain text (for freeze/restore history).
/// Uses VTE's full text range (row 0 .. the scroll extent). Best-effort: returns "" if unavailable.
fn dump_terminal_history(term: &vte4::Terminal) -> String {
    // The vadjustment spans the whole buffer: value range [lower, upper); rows are 1:1 with it.
    let (first, last) = match term.vadjustment() {
        Some(adj) => (adj.lower() as i64, adj.upper() as i64),
        None => (0, term.row_count() as i64),
    };
    let (text, _len) = term.text_range_format(vte4::Format::Text, first, 0, last, -1);
    let raw = text.map(|g| g.to_string()).unwrap_or_default();
    // Cap the persisted history so a huge scrollback doesn't bloat the session on disk.
    session::clamp_history(&raw, 5000)
}

/// Dashboard: a sidebar (Overview + docker resources + Processes) over a stack. Overview shows the
/// workspace's real configuration now; the docker/htop panes populate once the per-workspace daemon
/// is wired — shown as a clear "not connected yet" state, not fake data.
fn build_dashboard(ws: &Workspace) -> gtk::Box {
    let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let side = gtk::Box::new(gtk::Orientation::Vertical, 2);
    side.add_css_class("dside");

    let names = ["Overview", "Containers", "Images", "Volumes", "Networks", "Processes", "Settings"];
    let pages = gtk::Stack::new();
    pages.set_hexpand(true);
    pages.set_vexpand(true);
    pages.set_transition_type(gtk::StackTransitionType::None);
    pages.add_named(&dash_overview(ws), Some("Overview"));
    pages.add_named(&dash_settings(ws), Some("Settings"));

    // Live panes fed by a background poller over the workspace daemon's Unix socket.
    let data = std::sync::Arc::new(std::sync::Mutex::new(DashData::default()));
    spawn_dashboard_poller(ws.name.clone(), shell_label(ws), data.clone());
    let (cpane, cbody) = live_table_pane(&["NAME", "IMAGE", "STATUS"]);
    let (ipane, ibody) = live_table_pane(&["REPOSITORY", "IMAGE ID", "SIZE"]);
    let (vpane, vbody) = live_table_pane(&["NAME", "DRIVER"]);
    let (npane, nbody) = live_table_pane(&["NAME", "DRIVER", "SCOPE"]);
    let (ppane, pbody) = live_proc_pane();
    pages.add_named(&cpane, Some("Containers"));
    pages.add_named(&ipane, Some("Images"));
    pages.add_named(&vpane, Some("Volumes"));
    pages.add_named(&npane, Some("Networks"));
    pages.add_named(&ppane, Some("Processes"));
    glib::timeout_add_local(std::time::Duration::from_millis(1500), move || {
        let d = data.lock().unwrap().clone();
        fill_table(&cbody, &d.containers, d.error.as_deref());
        fill_table(&ibody, &d.images, d.error.as_deref());
        fill_table(&vbody, &d.volumes, d.error.as_deref());
        fill_table(&nbody, &d.networks, d.error.as_deref());
        fill_proc_table(&pbody, &d.processes, d.error.as_deref());
        glib::ControlFlow::Continue
    });

    let labels: Rc<RefCell<Vec<gtk::Box>>> = Rc::new(RefCell::new(Vec::new()));
    for (i, name) in names.iter().enumerate() {
        let item = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        item.add_css_class("dsi");
        if i == 0 {
            item.add_css_class("on");
        }
        let l = gtk::Label::new(Some(name));
        l.set_xalign(0.0);
        l.set_hexpand(true);
        item.append(&l);
        let click = gtk::GestureClick::new();
        let pages2 = pages.clone();
        let name2 = name.to_string();
        let labels2 = labels.clone();
        click.connect_released(move |_, _, _, _| {
            pages2.set_visible_child_name(&name2);
            for (j, b) in labels2.borrow().iter().enumerate() {
                if names[j] == name2 {
                    b.add_css_class("on");
                } else {
                    b.remove_css_class("on");
                }
            }
        });
        item.add_controller(click);
        side.append(&item);
        labels.borrow_mut().push(item);
    }

    // Debug: DD_TERM_DASHPANE selects a dashboard pane for screenshotting.
    if let Ok(p) = std::env::var("DD_TERM_DASHPANE") {
        pages.set_visible_child_name(&p);
        for (j, b) in labels.borrow().iter().enumerate() {
            if names[j] == p {
                b.add_css_class("on");
            } else {
                b.remove_css_class("on");
            }
        }
    }

    // Resizable sidebar/content split (same feel as a terminal split), with a sensible initial width.
    let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
    paned.set_hexpand(true);
    paned.set_vexpand(true);
    paned.set_wide_handle(false); // thin 1-2px handle (match the terminal splits), not a fat grab bar
    paned.set_start_child(Some(&side));
    paned.set_end_child(Some(&pages));
    paned.set_position(190);
    paned.set_resize_start_child(false); // sidebar keeps its width when the window resizes
    paned.set_shrink_start_child(false);
    root.append(&paned);
    root
}

/// Editable workspace settings, reachable from the sidebar. Everything a workspace defines EXCEPT its
/// identity (name / image / arch — set once at creation) can be changed here: default shell, resource
/// caps, environment variables, bind mounts, and the docker socket. Saving rewrites `workspaces.conf`;
/// changes apply to newly-launched tabs (a running container can't be reconfigured live).
fn dash_settings(ws: &Workspace) -> gtk::ScrolledWindow {
    let form = Rc::new(build_form());

    // Pre-populate env + mount rows BEFORE their panes wrap the boxes, so existing entries show first.
    for (k, v) in &ws.env {
        add_env_row(&form);
        if let Some((ke, ve)) = form.env_rows.borrow().last() {
            ke.set_text(k);
            ve.set_text(v);
        }
    }
    for m in &ws.mounts {
        add_mount_row(&form);
        if let Some((h, c, ro)) = form.mount_rows.borrow().last() {
            h.set_text(&m.host);
            c.set_text(&m.container);
            ro.set_active(m.ro);
        }
    }

    let main = gtk::Box::new(gtk::Orientation::Vertical, 14);
    main.add_css_class("dmain");

    // Identity header — read-only (image/arch are creation-only).
    let head = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let nm = gtk::Label::new(Some("Settings"));
    nm.add_css_class("dashtitle");
    nm.set_xalign(0.0);
    nm.set_hexpand(true);
    head.append(&nm);
    head.append(&arch_chip(ws.arch));
    main.append(&head);
    let idl = gtk::Label::new(Some(&format!("{}  ·  image + architecture are fixed at creation", ws.image)));
    idl.add_css_class("fhint");
    idl.set_xalign(0.0);
    main.append(&idl);

    // Editable sections (reuse the new-workspace panes).
    main.append(&field("DEFAULT SHELL", &form.shell, Some("Blank = auto (bash -il, else sh -i).")));
    main.append(&pane_resources(&form));
    main.append(&pane_env(&form));
    main.append(&pane_mounts(&form));
    main.append(&pane_docker(&form));
    main.append(&pane_rendering(&form));
    main.append(&pane_network(&form));
    main.append(&pane_device(&form));

    // Apply the workspace's values AFTER the pane builders (which set their own defaults).
    form.name.set_text(&ws.name);
    form.image.set_text(&ws.image);
    form.os_linux.set(ws.arch != Arch::DarwinArm64);
    form.cpu_amd.set(ws.arch == Arch::Amd64);
    if let Some(s) = &ws.shell {
        form.shell.set_text(s);
    }
    if let Some(st) = &ws.storage {
        form.storage.set_text(&st.to_string_lossy());
    }
    form.cpus.set_value(ws.cpus.unwrap_or(0) as f64);
    form.mem.set_value(ws.memory_mb.unwrap_or(0) as f64);
    if let Some(sb) = ws.scrollback {
        form.scrollback.set_text(&sb.to_string());
    }
    form.docker.set_active(ws.docker_sock);
    form.gui.set_active(ws.gui);
    if let Some(vpn) = &ws.vpn {
        form.vpn.set_text(&vpn.to_spec());
    }
    if let Some(cuda) = &ws.cuda {
        form.cuda_on.set_active(true);
        form.cuda_name.set_text(&cuda.name);
        form.cuda_cc.set_text(&cuda.compute_capability);
        form.cuda_vram.set_text(&cuda.vram_mb.to_string());
    }

    // Save row.
    let saverow = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let status = gtk::Label::new(None);
    status.add_css_class("fhint");
    status.set_xalign(0.0);
    status.set_hexpand(true);
    let save = gtk::Button::with_label("Save changes");
    save.add_css_class("btn");
    save.add_css_class("primary");
    save.set_halign(gtk::Align::End);
    {
        let form = form.clone();
        let status = status.clone();
        save.connect_clicked(move |_| {
            if save_workspace(&form) {
                status.remove_css_class("err");
                status.set_text("Saved — applies to newly-opened tabs (⌘T) and future launches.");
            } else {
                status.add_css_class("err");
                status.set_text("Could not save — check the fields.");
            }
        });
    }
    saverow.append(&status);
    saverow.append(&save);
    main.append(&saverow);

    gtk::ScrolledWindow::builder().child(&main).hexpand(true).vexpand(true).build()
}

fn dash_overview(ws: &Workspace) -> gtk::ScrolledWindow {
    let main = gtk::Box::new(gtk::Orientation::Vertical, 10);
    main.add_css_class("dmain");

    let head = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let nm = gtk::Label::new(Some(&ws.name));
    nm.add_css_class("dashtitle");
    nm.set_xalign(0.0);
    head.append(&nm);
    head.append(&arch_chip(ws.arch));
    main.append(&head);

    let grid = gtk::Grid::new();
    grid.set_row_spacing(9);
    grid.set_column_spacing(18);
    let mut row = 0i32;
    let mut kv = |k: &str, v: String| {
        let kl = gtk::Label::new(Some(k));
        kl.add_css_class("kvk");
        kl.set_xalign(0.0);
        kl.set_valign(gtk::Align::Start);
        let vl = gtk::Label::new(Some(&v));
        vl.add_css_class("kvv");
        vl.set_xalign(0.0);
        vl.set_wrap(true);
        vl.set_selectable(true);
        grid.attach(&kl, 0, row, 1, 1);
        grid.attach(&vl, 1, row, 1, 1);
        row += 1;
    };
    kv("Image", ws.image.clone());
    kv("Architecture", ws.arch.as_str().to_string());
    kv("Storage", tilde(&ws.storage_dir(&dd_root())));
    kv("Shell", ws.shell.clone().unwrap_or_else(|| "auto (bash \u{2192} sh)".into()));
    kv("CPU cores", ws.cpus.map(|c| c.to_string()).unwrap_or_else(|| "unlimited".into()));
    kv("Memory", ws.memory_mb.map(|m| format!("{m} MB")).unwrap_or_else(|| "unlimited".into()));
    kv("Docker socket", if ws.docker_sock { "mounted (DOCKER_HOST set)".into() } else { "off".into() });
    kv("VPN egress", ws.vpn.as_ref().map(|v| v.to_spec()).unwrap_or_else(|| "direct".into()));
    kv(
        "CUDA device",
        ws.cuda
            .as_ref()
            .map(|c| format!("{} (cc {}, {} MB) \u{2192} host Metal", c.name, c.compute_capability, c.vram_mb))
            .unwrap_or_else(|| "none".into()),
    );
    if !ws.env.is_empty() {
        kv("Environment", ws.env.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("\n"));
    }
    if !ws.mounts.is_empty() {
        kv("Mounts", ws.mounts.iter().map(|m| format!("{} \u{2192} {} ({})", m.host, m.container, if m.ro { "ro" } else { "rw" })).collect::<Vec<_>>().join("\n"));
    }
    main.append(&grid);

    let tip = gtk::Label::new(Some("\u{2318}T opens a shell in this workspace. \u{2318}D splits."));
    tip.add_css_class("dhint");
    tip.set_xalign(0.0);
    tip.set_margin_top(6);
    main.append(&tip);

    gtk::ScrolledWindow::builder().child(&main).hexpand(true).vexpand(true).build()
}

fn dash_placeholder(note: &str) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 0);
    b.set_hexpand(true);
    b.set_vexpand(true);
    b.set_valign(gtk::Align::Center);
    b.set_halign(gtk::Align::Center);
    let l = gtk::Label::new(Some(note));
    l.add_css_class("dhint");
    l.set_justify(gtk::Justification::Center);
    b.append(&l);
    b
}

/// Latest snapshot of the workspace daemon's resources (rows are pre-formatted cell strings).
#[derive(Default, Clone)]
struct DashData {
    containers: Vec<Vec<String>>,
    images: Vec<Vec<String>>,
    volumes: Vec<Vec<String>>,
    networks: Vec<Vec<String>>,
    processes: Vec<Vec<String>>,
    error: Option<String>,
}

/// Background thread: ensure the workspace daemon, then poll it over its Unix socket every ~2s.
fn spawn_dashboard_poller(ws_name: String, shell: String, data: std::sync::Arc<std::sync::Mutex<DashData>>) {
    std::thread::spawn(move || {
        // `ddcli workspace daemon <name>` starts (idempotently) the isolated daemon + prints its socket.
        let sock = ddcli_command(&["workspace", "daemon", &ws_name])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| std::path::PathBuf::from(String::from_utf8_lossy(&o.stdout).trim().to_string()));
        loop {
            let mut d = DashData::default();
            match sock.as_ref().filter(|p| !p.as_os_str().is_empty()) {
                Some(s) => {
                    d.containers = query_containers(s);
                    d.images = query_images(s);
                    d.volumes = query_volumes(s);
                    d.networks = query_networks(s);
                }
                None => d.error = Some("workspace daemon unavailable".into()),
            }
            // Workspace processes = the launched shells + their guest subprocesses, read from the host
            // process table (they run in-process via dd-jit, NOT through the daemon).
            d.processes = workspace_processes(&ws_name, &shell);
            if let Ok(mut g) = data.lock() {
                *g = d;
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
    });
}

/// GET a JSON endpoint from the daemon over its Unix socket (HTTP/1.0, blocking).
fn daemon_get(sock: &std::path::Path, path: &str) -> Option<serde_json::Value> {
    use std::io::{Read, Write};
    let mut s = std::os::unix::net::UnixStream::connect(sock).ok()?;
    let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(3)));
    write!(s, "GET {path} HTTP/1.0\r\nHost: localhost\r\n\r\n").ok()?;
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let body = text.split("\r\n\r\n").nth(1)?;
    serde_json::from_str(body.trim()).ok()
}

fn arr(v: Option<serde_json::Value>) -> Vec<serde_json::Value> {
    v.and_then(|v| v.as_array().cloned()).unwrap_or_default()
}
fn s(v: &serde_json::Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

fn query_containers(sock: &std::path::Path) -> Vec<Vec<String>> {
    arr(daemon_get(sock, "/containers/json?all=1"))
        .iter()
        .map(|c| {
            let name = c
                .get("Names")
                .and_then(|n| n.as_array())
                .and_then(|a| a.first())
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .trim_start_matches('/')
                .to_string();
            vec![name, s(c, "Image"), s(c, "Status")]
        })
        .collect()
}
fn query_images(sock: &std::path::Path) -> Vec<Vec<String>> {
    arr(daemon_get(sock, "/images/json"))
        .iter()
        .map(|i| {
            let repo = i
                .get("RepoTags")
                .and_then(|n| n.as_array())
                .and_then(|a| a.first())
                .and_then(|x| x.as_str())
                .unwrap_or("<none>")
                .to_string();
            let id = s(i, "Id").trim_start_matches("sha256:").chars().take(12).collect::<String>();
            let size = i.get("Size").and_then(|x| x.as_i64()).map(|b| format!("{} MB", b / 1_000_000)).unwrap_or_default();
            vec![repo, id, size]
        })
        .collect()
}
fn query_volumes(sock: &std::path::Path) -> Vec<Vec<String>> {
    let v = daemon_get(sock, "/volumes");
    let list = v.and_then(|v| v.get("Volumes").cloned());
    arr(list).iter().map(|vo| vec![s(vo, "Name"), s(vo, "Driver")]).collect()
}
fn query_networks(sock: &std::path::Path) -> Vec<Vec<String>> {
    arr(daemon_get(sock, "/networks"))
        .iter()
        .map(|n| vec![s(n, "Name"), s(n, "Driver"), s(n, "Scope")])
        .collect()
}
/// The workspace's processes: every shell launched into this workspace (`ddcli workspace launch <name>`,
/// which runs the engine IN-PROCESS — so there is no `--rootfs` child to match) plus all of their
/// descendants (the guest's forks appear as host children of the launcher). Read from the host `ps`.
/// A friendly name for the workspace's shell — the basename of the configured shell, else "bash" (the
/// launch default). Used to name shell sessions in the Processes pane (e.g. `bash · up 3m`).
fn shell_label(ws: &Workspace) -> String {
    ws.shell
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|s| s.split_whitespace().next())
        .map(|first| first.rsplit('/').next().unwrap_or(first).to_string())
        .unwrap_or_else(|| "bash".to_string())
}

fn workspace_processes(ws_name: &str, shell: &str) -> Vec<Vec<String>> {
    let Ok(out) = std::process::Command::new("ps").args(["-axo", "pid=,ppid=,etime=,command="]).output() else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&out.stdout);
    filter_workspace_procs(&text, ws_name, shell)
}

/// Pure core of [`workspace_processes`] (unit-tested against a captured `ps` dump): given `ps -axo
/// pid=,ppid=,etime=,command=` output, return `[pid, ppid, name]` rows for the workspace's launcher
/// shells and everything under them. A launcher is a process whose command is `… workspace launch <name>`
/// with `<name>` as the FINAL argument (so `general` never matches `general-2`); descendants are found by
/// walking the ppid tree. Each shell is named by its `shell` binary + how long it has run (its `etime`) —
/// e.g. `bash · up 04:12` — which is meaningful and distinguishes sessions (the guest's own processes run
/// in-process and aren't individually visible host-side).
fn filter_workspace_procs(ps_text: &str, ws_name: &str, shell: &str) -> Vec<Vec<String>> {
    struct Proc {
        pid: String,
        ppid: String,
        etime: String,
        cmd: String,
    }
    let procs: Vec<Proc> = ps_text
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 4 {
                return None;
            }
            Some(Proc {
                pid: parts[0].to_string(),
                ppid: parts[1].to_string(),
                etime: parts[2].to_string(),
                cmd: parts[3..].join(" "),
            })
        })
        .collect();

    // Every process whose argv is `… workspace launch <name>`. A guest fork inherits the launcher's
    // argv, so this set includes BOTH the real launcher shells and their in-guest forks.
    let launch_pids: std::collections::HashSet<String> = procs
        .iter()
        .filter(|p| {
            let toks: Vec<&str> = p.cmd.split_whitespace().collect();
            toks.windows(2).any(|w| w == ["workspace", "launch"]) && toks.last() == Some(&ws_name)
        })
        .map(|p| p.pid.clone())
        .collect();
    // A launcher shell is a launch process whose PARENT is not itself a launcher (its parent is dd-term
    // or init); a launch process parented by another launcher is a guest fork, not a distinct shell.
    let mut keep: std::collections::HashSet<String> = launch_pids.clone();
    let roots: std::collections::HashSet<String> =
        procs.iter().filter(|p| launch_pids.contains(&p.pid) && !launch_pids.contains(&p.ppid)).map(|p| p.pid.clone()).collect();
    // Transitively add descendants (guest forks are host children of a launcher).
    loop {
        let mut added = false;
        for p in &procs {
            if !keep.contains(&p.pid) && keep.contains(&p.ppid) {
                keep.insert(p.pid.clone());
                added = true;
            }
        }
        if !added {
            break;
        }
    }

    procs
        .iter()
        .filter(|p| keep.contains(&p.pid))
        .map(|p| {
            let label = if roots.contains(&p.pid) {
                format!("{shell} · up {}", p.etime) // the shell binary + how long this session has run
            } else if p.cmd.contains(" --rootfs ") {
                guest_cmd(&p.cmd) // an engine subprocess exec'd with a real guest argv
            } else {
                "process".to_string() // a guest fork (retains the launcher's host argv)
            };
            vec![p.pid.clone(), p.ppid.clone(), label]
        })
        .collect()
}

/// Extract the guest command from an engine command line (the argv after `--rootfs <upper>`).
fn guest_cmd(cmd: &str) -> String {
    if let Some(i) = cmd.find(" --rootfs ") {
        let after = &cmd[i + " --rootfs ".len()..];
        if let Some(sp) = after.find(' ') {
            let guest = after[sp..].trim();
            if !guest.is_empty() {
                return guest.chars().take(140).collect();
            }
        }
    }
    cmd.rsplit('/').next().unwrap_or(cmd).chars().take(140).collect()
}

/// The Processes pane: a header + a body that [`fill_proc_table`] repopulates with a NAME column and
/// per-row Stop (SIGTERM) / Force-kill (SIGKILL) buttons. These act on the host launcher process — i.e.
/// the terminal shell session — because the workspace's guest processes run in-process (inside the dd-jit
/// engine) and aren't individually visible in the host process table.
fn live_proc_pane() -> (gtk::ScrolledWindow, gtk::Box) {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    outer.add_css_class("dmain");
    let head = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    head.add_css_class("trow");
    head.add_css_class("thead");
    for (i, c) in ["PID", "PROCESS", "SIGNAL"].iter().enumerate() {
        let l = gtk::Label::new(Some(c));
        l.set_xalign(if i == 2 { 1.0 } else { 0.0 });
        l.set_hexpand(i == 1);
        l.set_width_chars(if i == 1 { 24 } else { 10 });
        l.add_css_class("tcell");
        head.append(&l);
    }
    outer.append(&head);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.set_hexpand(true);
    outer.append(&body);
    let sc = gtk::ScrolledWindow::builder().child(&outer).hexpand(true).vexpand(true).build();
    (sc, body)
}

fn fill_proc_table(body: &gtk::Box, rows: &[Vec<String>], error: Option<&str>) {
    while let Some(c) = body.first_child() {
        body.remove(&c);
    }
    if let Some(e) = error {
        let l = gtk::Label::new(Some(e));
        l.add_css_class("dhint");
        l.set_margin_top(16);
        body.append(&l);
        return;
    }
    if rows.is_empty() {
        let l = gtk::Label::new(Some("— no shell sessions —"));
        l.add_css_class("dhint");
        l.set_margin_top(16);
        l.set_halign(gtk::Align::Start);
        body.append(&l);
        return;
    }
    for r in rows {
        let pid: i32 = r.first().and_then(|s| s.parse().ok()).unwrap_or(0);
        let name = r.get(2).cloned().unwrap_or_default();
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row.add_css_class("trow");
        row.add_css_class("tbody");
        let pl = gtk::Label::new(Some(&r[0]));
        pl.set_xalign(0.0);
        pl.set_width_chars(10);
        pl.add_css_class("tcell");
        let nl = gtk::Label::new(Some(&name));
        nl.set_xalign(0.0);
        nl.set_hexpand(true);
        nl.set_ellipsize(gtk::pango::EllipsizeMode::End);
        nl.add_css_class("tcell");
        row.append(&pl);
        row.append(&nl);
        // Per-row signal controls: graceful stop (SIGTERM) then force kill (SIGKILL).
        let stop = gtk::Button::from_icon_name("media-playback-stop-symbolic");
        stop.add_css_class("sigbtn");
        stop.set_tooltip_text(Some("Stop — send SIGTERM"));
        stop.set_valign(gtk::Align::Center);
        stop.connect_clicked(move |_| kill_pid(pid, libc::SIGTERM));
        let force = gtk::Button::from_icon_name("user-trash-symbolic");
        force.add_css_class("sigbtn");
        force.set_tooltip_text(Some("Force kill — send SIGKILL"));
        force.set_valign(gtk::Align::Center);
        force.connect_clicked(move |_| kill_pid(pid, libc::SIGKILL));
        row.append(&stop);
        row.append(&force);
        body.append(&row);
    }
}

/// Send `sig` to a shell session's whole process group (so the guest dies with the launcher), then to the
/// pid itself. Guarded so we never signal init/self.
fn kill_pid(pid: i32, sig: i32) {
    if pid > 1 {
        unsafe {
            libc::kill(-pid, sig); // the session's process group (launcher is a setsid leader → pgid==pid)
            libc::kill(pid, sig);
        }
    }
}

/// A scrolled table pane: a fixed header row + a body box that [`fill_table`] repopulates. Returns the
/// pane widget and the body box.
fn live_table_pane(headers: &[&str]) -> (gtk::ScrolledWindow, gtk::Box) {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    outer.add_css_class("dmain");
    let head = table_row(headers, "thead");
    outer.append(&head);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.set_hexpand(true);
    outer.append(&body);
    let sc = gtk::ScrolledWindow::builder().child(&outer).hexpand(true).vexpand(true).build();
    (sc, body)
}

fn table_row(cells: &[&str], css: &str) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.add_css_class("trow");
    row.add_css_class(css);
    for (i, c) in cells.iter().enumerate() {
        let l = gtk::Label::new(Some(c));
        l.set_xalign(0.0);
        l.set_hexpand(i == 0);
        l.set_width_chars(if i == 0 { 24 } else { 16 });
        l.set_ellipsize(gtk::pango::EllipsizeMode::End);
        l.add_css_class("tcell");
        row.append(&l);
    }
    row
}

fn fill_table(body: &gtk::Box, rows: &[Vec<String>], error: Option<&str>) {
    while let Some(c) = body.first_child() {
        body.remove(&c);
    }
    if let Some(e) = error {
        let l = gtk::Label::new(Some(e));
        l.add_css_class("dhint");
        l.set_margin_top(16);
        body.append(&l);
        return;
    }
    if rows.is_empty() {
        let l = gtk::Label::new(Some("— none —"));
        l.add_css_class("dhint");
        l.set_margin_top(16);
        l.set_halign(gtk::Align::Start);
        body.append(&l);
        return;
    }
    for r in rows {
        let cells: Vec<&str> = r.iter().map(|s| s.as_str()).collect();
        body.append(&table_row(&cells, "tbody"));
    }
}

fn style_terminal(term: &vte4::Terminal, cfg: &TermConfig) {
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
    term.set_cursor_blink_mode(if cfg.cursor_blink { vte4::CursorBlinkMode::On } else { vte4::CursorBlinkMode::Off });
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

/// Debug self-capture: with `DD_TERM_SHOT=<png>` (and `DD_TERM_VIEW=manager|terminal|newws` to pick the
/// surface), render this window to a PNG via GTK's own snapshot pipeline and exit — no OS screen-capture
/// permission needed. Used to verify the UI headlessly.
fn maybe_shot(window: &gtk::ApplicationWindow, tag: &str) {
    let Ok(path) = std::env::var("DD_TERM_SHOT") else { return };
    if std::env::var("DD_TERM_VIEW").unwrap_or_else(|_| "manager".into()) != tag {
        return;
    }
    let ms: u64 = std::env::var("DD_TERM_SHOT_MS").ok().and_then(|s| s.parse().ok()).unwrap_or(2500);
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
                let _ = tex.save_to_png(&path);
                eprintln!("[dd-term] wrote screenshot {path} ({w}x{h})");
            }
            _ => eprintln!("[dd-term] screenshot failed: no render node/renderer"),
        }
        std::process::exit(0);
    });
}

/// Spawn `argv` (with `env`) on a fresh PTY via `posix_spawn` — async-signal-safe, so it works inside
/// the multithreaded GTK process where fork+exec would crash on macOS — then hand the PTY master to
/// `term`. The child is its own session leader with the PTY as its controlling terminal (job control +
/// isatty work). Returns the child pid.
fn spawn_on_pty(term: &vte4::Terminal, argv: &[&str], env: &[&str]) -> std::io::Result<(i32, vte4::Pty)> {
    use std::ffi::{CStr, CString};
    // POSIX_SPAWN_SETSID (macOS): child becomes a session leader; opening the slave tty then makes it
    // the controlling terminal.
    const POSIX_SPAWN_SETSID: libc::c_short = 0x0400;
    unsafe {
        let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
        if master < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if libc::grantpt(master) != 0 || libc::unlockpt(master) != 0 {
            libc::close(master);
            return Err(std::io::Error::last_os_error());
        }
        // A sane initial winsize so full-screen apps (htop) aren't malformed before the first resize
        // sync; the real size is applied from the terminal grid right after (see the poller below).
        let iws = libc::winsize { ws_row: 40, ws_col: 120, ws_xpixel: 0, ws_ypixel: 0 };
        libc::ioctl(master, libc::TIOCSWINSZ, &iws);
        let sname = libc::ptsname(master);
        if sname.is_null() {
            libc::close(master);
            return Err(std::io::Error::new(std::io::ErrorKind::Other, "ptsname failed"));
        }
        let slave = CString::from(CStr::from_ptr(sname));

        let mut fa: libc::posix_spawn_file_actions_t = std::mem::zeroed();
        libc::posix_spawn_file_actions_init(&mut fa);
        // Open the slave as the child's stdin (no O_NOCTTY → becomes controlling tty for the session
        // leader), then dup to stdout/stderr; close the master in the child.
        libc::posix_spawn_file_actions_addopen(&mut fa, 0, slave.as_ptr(), libc::O_RDWR, 0);
        libc::posix_spawn_file_actions_adddup2(&mut fa, 0, 1);
        libc::posix_spawn_file_actions_adddup2(&mut fa, 0, 2);
        libc::posix_spawn_file_actions_addclose(&mut fa, master);

        let mut attr: libc::posix_spawnattr_t = std::mem::zeroed();
        libc::posix_spawnattr_init(&mut attr);
        libc::posix_spawnattr_setflags(&mut attr, POSIX_SPAWN_SETSID);

        let c_argv: Vec<CString> = argv.iter().map(|s| CString::new(*s).unwrap()).collect();
        let mut p_argv: Vec<*mut libc::c_char> = c_argv.iter().map(|c| c.as_ptr() as *mut _).collect();
        p_argv.push(std::ptr::null_mut());
        let c_env: Vec<CString> = env.iter().map(|s| CString::new(*s).unwrap()).collect();
        let mut p_env: Vec<*mut libc::c_char> = c_env.iter().map(|c| c.as_ptr() as *mut _).collect();
        p_env.push(std::ptr::null_mut());

        let mut pid: libc::pid_t = 0;
        let rc = libc::posix_spawn(&mut pid, p_argv[0], &fa, &attr, p_argv.as_ptr(), p_env.as_ptr());
        libc::posix_spawn_file_actions_destroy(&mut fa);
        libc::posix_spawnattr_destroy(&mut attr);
        if rc != 0 {
            libc::close(master);
            return Err(std::io::Error::from_raw_os_error(rc));
        }

        // Give the master to VTE (it takes ownership and drives the grid + resizes the tty).
        use std::os::fd::FromRawFd;
        let owned = std::os::fd::OwnedFd::from_raw_fd(master);
        let pty = vte4::Pty::foreign_sync(owned, gio::Cancellable::NONE)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
        term.set_pty(Some(&pty));
        Ok((pid, pty))
    }
}

fn kill_pg(pid: i32) {
    if pid > 0 {
        unsafe {
            libc::killpg(pid, libc::SIGHUP);
        }
    }
}

/// A `Command` for our own `ddcli` with a CLEAN environment. dd-term runs from the `.app`, whose
/// DYLD_*/GI library-path vars (pointing at the bundled nix GTK stack) poison `ddcli`'s dynamic loader and
/// crash it + its forked engine at startup. EVERY `ddcli` invocation from the GUI must therefore clear the
/// inherited env and pass only the essentials — the same discipline the terminal-launch spawn already uses.
/// Skipping this on the freeze-on-close checkpoint is why frozen workspaces silently lost their processes.
fn ddcli_command(args: &[&str]) -> std::process::Command {
    let mut c = std::process::Command::new(ddcli_path());
    c.args(args);
    c.env_clear();
    c.env("TERM", "xterm-256color");
    c.env("PATH", "/Users/x/.local/bin:/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin");
    for k in ["HOME", "USER", "LOGNAME", "LANG", "LC_ALL", "LC_CTYPE", "TMPDIR", "SSH_AUTH_SOCK"] {
        if let Ok(v) = std::env::var(k) {
            c.env(k, v);
        }
    }
    c
}

fn ddcli_path() -> String {
    if let Ok(home) = std::env::var("HOME") {
        let p = format!("{home}/.local/bin/ddcli");
        if std::path::Path::new(&p).exists() {
            return p;
        }
    }
    for p in ["/usr/local/bin/ddcli", "/opt/homebrew/bin/ddcli"] {
        if std::path::Path::new(p).exists() {
            return p.to_string();
        }
    }
    "ddcli".to_string()
}

fn dd_root() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    std::path::PathBuf::from(home).join(".dd")
}

/// Allocate a fresh, stable checkpoint slot for a new pane ("0", "1", …).
fn alloc_slot(tw: &Rc<TermWin>) -> String {
    let n = tw.slot_ctr.get();
    tw.slot_ctr.set(n + 1);
    n.to_string()
}

/// Reuse a pane's saved slot on restore (or allocate a fresh one for a slot-less legacy session). Keeps
/// the allocator ahead of any reused numeric slot so later new panes never collide with a restored one.
fn adopt_slot(tw: &Rc<TermWin>, saved: &Option<String>) -> String {
    match saved {
        Some(s) => {
            if let Ok(n) = s.parse::<u32>() {
                if n >= tw.slot_ctr.get() {
                    tw.slot_ctr.set(n + 1);
                }
            }
            s.clone()
        }
        None => alloc_slot(tw),
    }
}

/// True if this pane slot has a frozen checkpoint on disk (a written MANIFEST) to restore.
fn slot_has_checkpoint(ws: &Workspace, slot: &str) -> bool {
    ws.checkpoint_slot_dir(&dd_root(), slot).join("MANIFEST").exists()
}

/// Find the checkpoint slot registered for `term` (pruning dead registry entries as it scans).
fn slot_of_terminal(tw: &Rc<TermWin>, term: &vte4::Terminal) -> Option<String> {
    let mut found = None;
    tw.panes.borrow_mut().retain(|(w, slot, _)| match w.upgrade() {
        Some(t) if &t == term => {
            found = Some(slot.clone());
            true
        }
        Some(_) => true,
        None => false, // prune a dead pane
    });
    found
}

/// Collect every terminal in `w`'s widget subtree (used to discard slots when a tab/pane closes).
fn collect_terminals(w: &gtk::Widget, out: &mut Vec<vte4::Terminal>) {
    if let Some(t) = w.downcast_ref::<vte4::Terminal>() {
        out.push(t.clone());
    }
    let mut c = w.first_child();
    while let Some(ch) = c {
        collect_terminals(&ch, out);
        c = ch.next_sibling();
    }
}

/// A pane closed by the user (not a window close) → drop it from the registry and DISCARD its slot's
/// stale checkpoint, so a later reopen doesn't wrongly resurrect a shell the user deliberately closed.
fn discard_terminal_slot(tw: &Rc<TermWin>, term: &vte4::Terminal) {
    let mut removed = None;
    tw.panes.borrow_mut().retain(|(w, slot, _)| match w.upgrade() {
        Some(t) if &t == term => {
            removed = Some(slot.clone());
            false
        }
        Some(_) => true,
        None => false, // prune dead entries while we're here
    });
    if let Some(slot) = removed {
        let _ = std::fs::remove_dir_all(tw.ws.checkpoint_slot_dir(&dd_root(), &slot));
    }
}

/// Discard the slots of every terminal under a page's widget subtree (a whole tab being closed).
fn discard_page_slots(tw: &Rc<TermWin>, child: &gtk::Widget) {
    let mut terms = Vec::new();
    collect_terminals(child, &mut terms);
    for t in &terms {
        discard_terminal_slot(tw, t);
    }
}

fn workspaces_conf() -> std::path::PathBuf {
    dd_root().join("workspaces.conf")
}

fn tilde(p: &std::path::Path) -> String {
    let s = p.to_string_lossy().into_owned();
    if let Ok(home) = std::env::var("HOME") {
        if let Some(rest) = s.strip_prefix(&home) {
            return format!("~{rest}");
        }
    }
    s
}

// -------------------------------------------------------------------------------------------------
// macOS: force the whole app (incl. native title bars) to the dark appearance so nothing renders white.
// -------------------------------------------------------------------------------------------------
#[cfg(target_os = "macos")]
mod macshim {
    use objc2_app_kit::{NSAppearance, NSApplication};
    use objc2_foundation::{MainThreadMarker, NSString};
    /// Force the app AND every open window to the dark appearance (dark native title bars). Idempotent;
    /// call at startup and again on each window's realize (windows created after startup miss the app-only
    /// call otherwise).
    pub fn force_dark() {
        let mtm = unsafe { MainThreadMarker::new_unchecked() };
        let app = NSApplication::sharedApplication(mtm);
        unsafe {
            let name = NSString::from_str("NSAppearanceNameDarkAqua");
            let Some(dark) = NSAppearance::appearanceNamed(&name) else { return };
            app.setAppearance(Some(&dark));
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod macshim {
    pub fn force_dark() {}
}

#[cfg(test)]
mod tests {
    use super::filter_workspace_procs;

    // A captured `ps -axo pid=,ppid=,etime=,command=` slice: dd-term (43405) with two launcher shells for
    // the `general` workspace, one of which (43444) has a guest fork (90001); plus an orphaned launcher
    // (16020, ppid 1), an UNRELATED workspace launcher (`ubuntu-dev`), and noise that must be excluded.
    const PS: &str = "\
43405     1    01:00:00 ./target-mac/release/dd-term
43444 43405       04:12 /Users/x/.local/bin/ddcli workspace launch general
45125 43405       00:30 /Users/x/.local/bin/ddcli workspace launch general
90001 43444       00:05 /Users/x/.local/bin/ddcli workspace launch general
16020     1    02:03:04 /Users/x/.local/bin/ddcli workspace launch general
17980     1       10:00 /Users/x/.local/bin/ddcli workspace launch ubuntu-dev
55500     1       10:00 /usr/sbin/some-daemon --workspace launch generalizer
99999     1       00:01 grep workspace launch general";

    #[test]
    fn finds_launchers_and_their_forks() {
        let rows = filter_workspace_procs(PS, "general", "bash");
        let pids: Vec<&str> = rows.iter().map(|r| r[0].as_str()).collect();
        // Every `general` launcher + the guest fork under 43444, and nothing else.
        assert!(pids.contains(&"43444"), "missing launcher 43444");
        assert!(pids.contains(&"45125"), "missing launcher 45125");
        assert!(pids.contains(&"90001"), "missing guest fork 90001");
        assert!(pids.contains(&"16020"), "missing orphaned launcher 16020");
        assert!(!pids.contains(&"17980"), "must not match ubuntu-dev launcher");
        assert!(!pids.contains(&"55500"), "must not match `generalizer` substring");
        assert!(!pids.contains(&"43405"), "dd-term itself is not a workspace process");
        // The fork is a plain process; launchers are named by shell + uptime (from etime).
        let fork = rows.iter().find(|r| r[0] == "90001").unwrap();
        assert_eq!(fork[2], "process");
        let shell = rows.iter().find(|r| r[0] == "43444").unwrap();
        assert_eq!(shell[2], "bash · up 04:12");
    }

    #[test]
    fn exact_name_match_no_prefix_collision() {
        // `general` must never pull in `general-2`'s launcher.
        let ps = "100 1 00:10 /x/ddcli workspace launch general-2\n101 1 00:20 /x/ddcli workspace launch general";
        let rows = filter_workspace_procs(ps, "general", "fish");
        let pids: Vec<&str> = rows.iter().map(|r| r[0].as_str()).collect();
        assert_eq!(pids, vec!["101"]);
        assert_eq!(rows[0][2], "fish · up 00:20");
    }
}
