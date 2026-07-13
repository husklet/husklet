//! Headless screenshot: run a real shell session through the full pipeline (LocalPty → Vt → CpuRenderer)
//! and write a PNG — the self-inspection tool that proves the terminal renders correctly with no GUI.
//!
//!   cargo run -p hl-term --example screenshot -- /tmp/out.png
//!
//! Defaults to `dd-term.png` in the current directory.

use hl_term::pty::local::LocalPty;
use hl_term::{CpuRenderer, PtyBackend, Vt};
use std::time::{Duration, Instant};

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "dd-term.png".to_string());
    let (cols, rows) = (80usize, 24usize);

    // A little "dev session": colors, a prompt, box drawing (DEC line-drawing), a table, cursor.
    let script = r#"
printf '\033[1;32mdd-term\033[0m:\033[1;34m~/project\033[0m$ ls --color\r\n'
printf '\033[1;34msrc\033[0m  \033[1;34mtests\033[0m  \033[0;32mREADME.md\033[0m  Cargo.toml\r\n'
printf '\r\n'
printf '\033(0lqqqqqqqqqqqqqqqqqqqqqqqqqwqqqqqqqqx\033(B\r\n'
printf '\033(0x\033(B \033[33mworkspace\033[0m           \033(0x\033(B arch  \033(0x\033(B\r\n'
printf '\033(0tqqqqqqqqqqqqqqqqqqqqqqqqqnqqqqqqqqu\033(B\r\n'
printf '\033(0x\033(B ubuntu:24.04           \033(0x\033(B arm64 \033(0x\033(B\r\n'
printf '\033(0x\033(B alpine (x86_64 on arm) \033(0x\033(B amd64 \033(0x\033(B\r\n'
printf '\033(0mqqqqqqqqqqqqqqqqqqqqqqqqqvqqqqqqqqj\033(B\r\n'
printf '\r\n'
printf '\033[1;32mdd-term\033[0m:\033[1;34m~/project\033[0m$ '
"#;

    let sh = if std::path::Path::new("/bin/bash").exists() { "/bin/bash" } else { "/bin/sh" };
    let mut pty = LocalPty::spawn(&[sh, "-c", script], cols as u16, rows as u16, &[("TERM", "xterm-256color")])
        .expect("spawn shell");

    let mut vt = Vt::new(cols, rows);
    let fd = pty.master_fd().unwrap();
    let mut buf = [0u8; 8192];
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut exited = false;
    loop {
        let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        let pr = unsafe { libc::poll(&mut pfd, 1, 20) };
        if pr > 0 && pfd.revents & libc::POLLIN != 0 {
            let n = pty.read(&mut buf).unwrap_or(0);
            if n > 0 {
                vt.advance_bytes(&buf[..n]);
                continue;
            }
        }
        if exited || Instant::now() > deadline {
            break;
        }
        if pty.try_wait().is_some() {
            exited = true;
        }
    }

    let png = CpuRenderer::default().render_png(vt.grid());
    std::fs::write(&out, &png).expect("write png");
    let (w, h) = {
        let img = CpuRenderer::default().render(vt.grid());
        (img.width, img.height)
    };
    eprintln!("wrote {out} ({w}x{h}px, title={:?})", vt.title);
    // Also echo the grid as text so the render can be sanity-checked without opening the image.
    for r in 0..rows {
        let line = vt.grid().row_text(r);
        if !line.is_empty() {
            eprintln!("| {line}");
        }
    }
}
