//! A full visual demo: an iTerm2-style split-pane terminal window with three workspace panes, each
//! running a real shell session, composited into a single PNG — the whole dd-term pipeline
//! (Layout → LocalPty → Vt → CpuRenderer) with no GPU/display.
//!
//!   cargo run -p hl-term --example demo -- /tmp/demo.png

use hl_ws_term::layout::{Dir, Layout, Rect};
use hl_ws_term::pty::local::LocalPty;
use hl_ws_term::render::CpuRenderer;
use hl_ws_term::{PtyBackend, Vt};
use std::time::{Duration, Instant};

const SCALE: u32 = 2;
const CELL: u32 = 8 * SCALE; // 16px cells
const TITLE_H: u32 = 22;
const PAD: u32 = 3;

struct Canvas {
    w: u32,
    h: u32,
    px: Vec<u8>,
}
impl Canvas {
    fn new(w: u32, h: u32, bg: (u8, u8, u8)) -> Canvas {
        let mut px = vec![0u8; (w * h * 4) as usize];
        for i in 0..(w * h) as usize {
            px[i * 4] = bg.0;
            px[i * 4 + 1] = bg.1;
            px[i * 4 + 2] = bg.2;
            px[i * 4 + 3] = 0xff;
        }
        Canvas { w, h, px }
    }
    fn fill(&mut self, x: u32, y: u32, w: u32, h: u32, c: (u8, u8, u8)) {
        for yy in y..(y + h).min(self.h) {
            for xx in x..(x + w).min(self.w) {
                let i = ((yy * self.w + xx) * 4) as usize;
                self.px[i] = c.0;
                self.px[i + 1] = c.1;
                self.px[i + 2] = c.2;
                self.px[i + 3] = 0xff;
            }
        }
    }
    fn border(&mut self, x: u32, y: u32, w: u32, h: u32, c: (u8, u8, u8)) {
        self.fill(x, y, w, 1, c);
        self.fill(x, y + h - 1, w, 1, c);
        self.fill(x, y, 1, h, c);
        self.fill(x + w - 1, y, 1, h, c);
    }
    /// Blit an RGBA image at (ox, oy), clipped to the canvas.
    fn blit(&mut self, ox: u32, oy: u32, img: &hl_ws_term::render::Image) {
        for row in 0..img.height {
            if oy + row >= self.h {
                break;
            }
            for col in 0..img.width {
                if ox + col >= self.w {
                    break;
                }
                let si = ((row * img.width + col) * 4) as usize;
                let di = (((oy + row) * self.w + (ox + col)) * 4) as usize;
                self.px[di..di + 4].copy_from_slice(&img.rgba[si..si + 4]);
            }
        }
    }
}

/// Run a scripted shell session sized `cols x rows` and return the resulting terminal grid.
fn session(script: &str, cols: usize, rows: usize) -> Vt {
    let sh = if std::path::Path::new("/bin/bash").exists() { "/bin/bash" } else { "/bin/sh" };
    let mut pty = LocalPty::spawn(&[sh, "-c", script], cols as u16, rows as u16, &[("TERM", "xterm-256color")])
        .expect("spawn");
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
    vt
}

/// Render a short title string into a small RGBA image (green on the tab background).
fn title_img(text: &str, cols: usize) -> hl_ws_term::render::Image {
    let mut vt = Vt::new(cols.max(text.len() + 1), 1);
    vt.advance_bytes(b"\x1b[1;32m");
    vt.advance_bytes(text.as_bytes());
    vt.advance_bytes(b"\x1b[?25l"); // no cursor in the tab
    let r = CpuRenderer { scale: SCALE, bg_default: (0x2b, 0x2b, 0x2b), ..CpuRenderer::default() };
    r.render(vt.grid())
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "demo.png".to_string());
    let (cw, ch) = (1200u32, 720u32);
    let mut canvas = Canvas::new(cw, ch, (0x0d, 0x0d, 0x10));

    // Three panes: left half = "ubuntu-dev"; right column split into "alpine-x86" (top) and "logs".
    let mut lay = Layout::new(0);
    lay.split(0, Dir::Vertical, 1, 0.5);
    lay.split(1, Dir::Horizontal, 2, 0.5);
    let area = Rect { x: 0.0, y: 0.0, w: cw as f32, h: ch as f32 };
    let rects = lay.rects(area, 8.0);

    let panes: [(&str, &str, (u8, u8, u8), &str); 3] = [
        (
            "ubuntu-dev  -  ubuntu:24.04 (arm64)",
            "workspace",
            (0x1e, 0x1e, 0x28),
            "printf '\\033[1;32mdev\\033[0m:\\033[1;34m~/project\\033[0m$ ls --color\\r\\n'; \
             printf '\\033[1;34msrc\\033[0m  \\033[1;34mtests\\033[0m  \\033[0;32mREADME.md\\033[0m  Cargo.toml\\r\\n'; \
             printf '\\033[1;32mdev\\033[0m:\\033[1;34m~/project\\033[0m$ uname -m && cat /etc/os-release 2>/dev/null | head -1\\r\\n'; \
             printf 'aarch64\\r\\n'; printf 'PRETTY_NAME=\"Ubuntu 24.04 LTS\"\\r\\n'; \
             printf '\\033[1;32mdev\\033[0m:\\033[1;34m~/project\\033[0m$ cargo build\\r\\n'; \
             printf '   \\033[1;32mCompiling\\033[0m dd-term-core v0.1.0\\r\\n'; \
             printf '    \\033[1;32mFinished\\033[0m dev target(s) in 0.6s\\r\\n'; \
             printf '\\033[1;32mdev\\033[0m:\\033[1;34m~/project\\033[0m$ \\033[?25h'",
        ),
        (
            "alpine-x86  -  alpine (x86_64 on arm, jit86)",
            "workspace",
            (0x28, 0x1e, 0x1e),
            "printf '\\033[36m~ \\033[0m# uname -m\\r\\n'; printf 'x86_64\\r\\n'; \
             printf '\\033[36m~ \\033[0m# apk add python3\\r\\n'; \
             printf '\\033[32mOK:\\033[0m 6 MiB in 15 packages\\r\\n'; \
             printf '\\033[36m~ \\033[0m# python3 -c \"print(2**64)\"\\r\\n'; \
             printf '18446744073709551616\\r\\n'; printf '\\033[36m~ \\033[0m# '",
        ),
        (
            "logs  -  docker events",
            "stream",
            (0x16, 0x22, 0x18),
            "printf '\\033[90m10:31:02\\033[0m \\033[32mstart \\033[0mubuntu-dev\\r\\n'; \
             printf '\\033[90m10:31:04\\033[0m \\033[33mpull  \\033[0malpine:latest\\r\\n'; \
             printf '\\033[90m10:31:20\\033[0m \\033[32mstart \\033[0malpine-x86\\r\\n'; \
             printf '\\033[90m10:33:57\\033[0m \\033[36mexec  \\033[0mubuntu-dev bash\\r\\n'",
        ),
    ];

    for (pid, rect) in rects {
        let (title, _kind, bg, script) = panes[pid as usize];
        let (px, py, pw, ph) = (rect.x as u32, rect.y as u32, rect.w as u32, rect.h as u32);
        // Title tab.
        canvas.fill(px, py, pw, TITLE_H, (0x2b, 0x2b, 0x2b));
        let t = title_img(&format!(" {title}"), (pw / CELL) as usize);
        canvas.blit(px, py + (TITLE_H - CELL) / 2, &t);
        // Terminal area.
        let tx = px + PAD;
        let ty = py + TITLE_H + PAD;
        let tw = pw.saturating_sub(PAD * 2);
        let th = ph.saturating_sub(TITLE_H + PAD * 2);
        let cols = (tw / CELL).max(1) as usize;
        let rows = (th / CELL).max(1) as usize;
        canvas.fill(tx, ty, cols as u32 * CELL, rows as u32 * CELL, bg);
        let vt = session(script, cols, rows);
        let r = CpuRenderer { scale: SCALE, bg_default: bg, ..CpuRenderer::default() };
        canvas.blit(tx, ty, &r.render(vt.grid()));
        // Pane border (highlight the focused pane).
        let bc = if pid == lay.focused() { (0x4a, 0x9e, 0xff) } else { (0x33, 0x33, 0x3a) };
        canvas.border(px, py, pw, ph, bc);
    }

    let png = hl_ws_term::png::encode_rgba(canvas.w, canvas.h, &canvas.px);
    std::fs::write(&out, &png).expect("write");
    eprintln!("wrote {out} ({}x{}px, {} panes)", canvas.w, canvas.h, panes.len());
}
