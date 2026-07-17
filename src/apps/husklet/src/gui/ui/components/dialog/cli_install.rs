#![allow(unused_imports, dead_code)]
use gtk::prelude::*;

/// Install the `hl` CLI and show a small window with a shell picker + matching PATH instructions.
pub fn show_cli_install(parent: &gtk::ApplicationWindow) {
    let result = crate::install::install_cli();
    let ok = result.is_ok();
    let on_path = result.as_ref().map(|(_, p)| *p).unwrap_or(false);
    let cmd = result
        .as_ref()
        .ok()
        .and_then(|(link, _)| link.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "hl".to_string());
    let status_text = match &result {
        Ok((link, _)) => format!("Installed to {}", link.display()),
        Err(e) => format!("Couldn't install: {e}"),
    };

    let heading = gtk::Label::new(Some("hl command-line tool"));
    heading.set_xalign(0.0);
    heading.add_css_class("hl-onboard-head");
    let status = gtk::Label::new(Some(&status_text));
    status.set_xalign(0.0);
    status.set_wrap(true);
    status.add_css_class("hl-sub");

    // Shell picker.
    let dropdown = gtk::DropDown::from_strings(&["zsh", "bash", "fish"]);
    dropdown.set_selected(detect_shell_index());
    let shell_lbl = gtk::Label::new(Some("Shell"));
    shell_lbl.add_css_class("dim-label");
    let shell_row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    shell_row.append(&shell_lbl);
    shell_row.append(&dropdown);
    shell_row.set_visible(ok && !on_path);

    // Per-shell instructions.
    let instr = gtk::Label::new(None);
    instr.set_xalign(0.0);
    instr.set_wrap(true);
    instr.set_selectable(true);
    instr.add_css_class("hl-cli-msg");
    instr.set_visible(ok);
    if ok {
        instr.set_label(&shell_instr(dropdown.selected(), on_path, &cmd));
        let instr2 = instr.clone();
        let cmd2 = cmd.clone();
        dropdown.connect_selected_notify(move |d| {
            instr2.set_label(&shell_instr(d.selected(), on_path, &cmd2))
        });
    }

    let done = gtk::Button::with_label("Done");
    done.add_css_class("hl-btn");
    done.add_css_class("suggested-action");
    let btnrow = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    btnrow.set_halign(gtk::Align::End);
    btnrow.set_margin_top(8);
    btnrow.append(&done);

    let v = gtk::Box::new(gtk::Orientation::Vertical, 10);
    v.set_margin_top(20);
    v.set_margin_bottom(18);
    v.set_margin_start(22);
    v.set_margin_end(22);
    v.append(&heading);
    v.append(&status);
    v.append(&shell_row);
    v.append(&instr);
    v.append(&btnrow);

    let win = gtk::Window::builder()
        .title("Install hl CLI")
        .modal(true)
        .resizable(false)
        .default_width(460)
        .child(&v)
        .build();
    win.set_transient_for(Some(parent));
    let w = win.clone();
    done.connect_clicked(move |_| w.close());
    win.present();
}

pub(crate) fn detect_shell_index() -> u32 {
    let sh = std::env::var("SHELL").unwrap_or_default();
    if sh.contains("fish") {
        2
    } else if sh.contains("bash") {
        1
    } else {
        0 // zsh (macOS default) or unknown
    }
}

pub(crate) fn shell_instr(idx: u32, on_path: bool, cmd: &str) -> String {
    if on_path {
        return format!("~/.local/bin is already on your PATH.\nJust run:  {cmd}");
    }
    match idx {
        1 => "Add to ~/.bashrc, then restart your terminal:\n\nexport PATH=\"$HOME/.local/bin:$PATH\"",
        2 => "Run once (fish):\n\nfish_add_path ~/.local/bin",
        _ => "Add to ~/.zshrc, then restart your terminal:\n\nexport PATH=\"$HOME/.local/bin:$PATH\"",
    }
    .to_string()
}
