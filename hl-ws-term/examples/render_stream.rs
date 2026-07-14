//! Render a raw terminal byte stream (from stdin) into a PNG — used to screenshot the output of a
//! *real* launched session (e.g. the bytes captured from `ddcli workspace launch`).
//!
//!   some-command | cargo run -q -p hl-term --example render_stream -- out.png 80 24

use hl_ws_term::{CpuRenderer, Vt};
use std::io::Read;

fn main() {
    let mut args = std::env::args().skip(1);
    let out = args.next().unwrap_or_else(|| "stream.png".to_string());
    let cols: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(80);
    let rows: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(24);

    let mut bytes = Vec::new();
    std::io::stdin().read_to_end(&mut bytes).expect("read stdin");

    let mut vt = Vt::new(cols, rows);
    vt.advance_bytes(&bytes);

    let png = CpuRenderer::default().render_png(vt.grid());
    std::fs::write(&out, &png).expect("write png");
    eprintln!("rendered {} bytes → {out} ({cols}x{rows} cells)", bytes.len());
}
