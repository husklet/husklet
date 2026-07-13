#![allow(unused_imports, dead_code)]
//! The Workspaces page: a named image+arch dev environment you configure once and LAUNCH as a terminal.
//!
//! The model + persistence live in `hl_term::workspace` (a plain `~/.dd/workspaces.conf`); this view
//! is a thin CRUD over a [`WorkspaceStore`] plus a Launch that opens a VTE tab running
//! `ddcli workspace launch <name>` — a real interactive terminal inside the workspace's image.
//!
//! The page is a persistent notebook (built once in `ui::build`): page 0 is this config list, and each
//! Launch appends a terminal tab beside it (so launched shells survive the 2s state poll). `render` only
//! rebuilds page 0, and only when the workspace set actually changes (guarded by `ws_sig`) — otherwise a
//! poll would wipe half-typed input in the "New workspace" form.

use crate::ui::components::*;
use crate::ui::theme::*;
use crate::{AppModel, Msg};
use hl_term::workspace::{Arch, WorkspaceStore};
use gtk::prelude::*;
use relm4::ComponentSender;
use std::path::PathBuf;

/// The arch choices offered in the "New workspace" dropdown (index ↔ [`Arch`], stable order).
const ARCH_TOKENS: [&str; 3] = ["arm64", "amd64", "darwin-arm64"];

/// `~/.dd/workspaces.conf` — the same store the `ddcli workspace` subcommands read/write.
pub(crate) fn workspaces_conf() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".dd").join("workspaces.conf")
}

/// A signature of the configured workspace set, so `render` only rebuilds page 0 on a real change
/// (create/remove) and leaves a half-typed form alone across the 2s poll.
pub(crate) fn workspaces_sig() -> String {
    let store = WorkspaceStore::load(workspaces_conf());
    let mut s = String::from("ws|");
    for w in store.all() {
        s.push_str(&w.name);
        s.push('\t');
        s.push_str(w.arch.as_str());
        s.push('\t');
        s.push_str(&w.image);
        s.push('\n');
    }
    s
}

/// Fill the Workspaces config page (page 0 of the persistent notebook): the New-workspace form + one row
/// per configured workspace (Launch / Remove). `nb` is that notebook — Launch appends a terminal tab to it.
pub(crate) fn render_workspaces(
    page: &gtk::Box,
    nb: &gtk::Notebook,
    _m: &AppModel,
    sender: &ComponentSender<AppModel>,
) {
    clear_box(page);

    let title = gtk::Label::new(Some("Workspaces"));
    title.set_xalign(0.0);
    title.add_css_class("dd-h1");
    page.append(&title);

    let blurb = gtk::Label::new(Some(
        "A workspace is a named image + architecture you develop in. Launch one to get a terminal \
         inside that environment.",
    ));
    blurb.set_xalign(0.0);
    blurb.set_wrap(true);
    blurb.add_css_class("dd-sub");
    page.append(&blurb);

    // ---- New workspace form -------------------------------------------------
    let nh = gtk::Label::new(Some("New workspace"));
    nh.set_xalign(0.0);
    nh.add_css_class("dd-h2");
    page.append(&nh);

    let form = gtk::Box::new(gtk::Orientation::Vertical, 10);
    form.add_css_class("dd-step-card");

    let name_entry = gtk::Entry::new();
    name_entry.set_placeholder_text(Some("name (e.g. ubuntu-dev)"));
    name_entry.set_hexpand(true);
    let image_entry = gtk::Entry::new();
    image_entry.set_placeholder_text(Some("image (e.g. ubuntu:24.04)"));
    image_entry.set_hexpand(true);
    let arch_dd = gtk::DropDown::from_strings(&ARCH_TOKENS);
    arch_dd.add_css_class("dd-seg");
    arch_dd.set_tooltip_text(Some("Target architecture (amd64 runs via the jit86 translator)"));

    let fields = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    fields.append(&name_entry);
    fields.append(&image_entry);
    fields.append(&arch_dd);
    form.append(&fields);

    let create = gtk::Button::with_label("Create workspace");
    create.add_css_class("dd-btn");
    create.add_css_class("suggested-action");
    create.set_halign(gtk::Align::Start);
    {
        let s = sender.clone();
        let name_e = name_entry.clone();
        let image_e = image_entry.clone();
        let arch_e = arch_dd.clone();
        create.connect_clicked(move |_| {
            let name = name_e.text().as_str().trim().to_string();
            let image = image_e.text().as_str().trim().to_string();
            let arch = ARCH_TOKENS
                .get(arch_e.selected() as usize)
                .copied()
                .unwrap_or("arm64")
                .to_string();
            // Require both a name and an image; a blank form is a no-op (no panic, no bad entry).
            if !name.is_empty() && !image.is_empty() {
                s.input(Msg::CreateWorkspace(name, image, arch));
            }
        });
    }
    form.append(&create);
    page.append(&form);

    // ---- Configured workspaces ---------------------------------------------
    let lh = gtk::Label::new(Some("Configured"));
    lh.set_xalign(0.0);
    lh.add_css_class("dd-h2");
    page.append(&lh);

    let store = WorkspaceStore::load(workspaces_conf());
    if store.all().is_empty() {
        let empty = gtk::Label::new(Some("No workspaces yet — create one above."));
        empty.set_xalign(0.0);
        empty.add_css_class("dim-label");
        page.append(&empty);
        return;
    }

    for w in store.all() {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.add_css_class("dd-step-card");

        let texts = gtk::Box::new(gtk::Orientation::Vertical, 1);
        texts.set_hexpand(true);
        texts.set_valign(gtk::Align::Center);
        let t = gtk::Label::new(Some(&w.name));
        t.set_xalign(0.0);
        t.add_css_class("heading");
        let d = gtk::Label::new(Some(&format!("{} · {}", w.image, w.arch.as_str())));
        d.set_xalign(0.0);
        d.add_css_class("dim-label");
        d.add_css_class("caption");
        texts.append(&t);
        texts.append(&d);
        row.append(&texts);

        // Launch → a VTE tab running `ddcli workspace launch <name>` (bypasses Msg, exactly like the
        // container ＋-terminal button: a direct VTE spawn, not a daemon round-trip).
        let launch = gtk::Button::with_label("Launch");
        launch.add_css_class("dd-btn");
        launch.add_css_class("suggested-action");
        launch.set_valign(gtk::Align::Center);
        {
            let nb = nb.clone();
            let name = w.name.clone();
            launch.connect_clicked(move |_| {
                let ddcli = ddcli_bin().to_string_lossy().into_owned();
                let argv = ["workspace", "launch", name.as_str()];
                let mut full: Vec<&str> = vec![ddcli.as_str()];
                full.extend_from_slice(&argv);
                open_command_tab(&nb, &format!("ws: {name}"), &full);
            });
        }
        row.append(&launch);

        let remove = gtk::Button::with_label("Remove");
        remove.add_css_class("dd-btn");
        remove.add_css_class("dd-danger");
        remove.set_valign(gtk::Align::Center);
        {
            let s = sender.clone();
            let name = w.name.clone();
            remove.connect_clicked(move |_| s.input(Msg::RemoveWorkspace(name.clone())));
        }
        row.append(&remove);

        page.append(&row);
    }
}

/// Locate the bundled `ddcli` binary. A macOS app launched from Finder/launchd has a minimal PATH, so
/// (mirroring `install::resolve_cli`) prefer an explicit override, then the installed/dev app bundle's
/// `Contents/Resources`, then a sibling of this executable; fall back to the bare name (PATH) for dev.
fn ddcli_bin() -> PathBuf {
    if let Some(p) = std::env::var_os("DD_CLI_BIN") {
        return PathBuf::from(p);
    }
    let names = ["ddcli", "dd"];
    for n in names {
        let p = PathBuf::from("/Applications/dd.app/Contents/Resources").join(n);
        if p.exists() {
            return p;
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(contents) = exe.parent().and_then(|p| p.parent()) {
            for n in names {
                let c = contents.join("Resources").join(n);
                if c.exists() {
                    return c;
                }
            }
        }
        if let Some(dir) = exe.parent() {
            for n in names {
                let c = dir.join(n);
                if c.exists() {
                    return c;
                }
            }
        }
    }
    PathBuf::from("ddcli")
}
