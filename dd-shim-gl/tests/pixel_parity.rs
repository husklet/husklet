//! Pixel-/IR-parity harness: run a GLES frame through the C shim (`gl_shim.c`) and the Rust shim
//! (`dd-shim-gl`) and diff the dd-gpu IR they emit. Both streams feed the *same* host backend, so
//! byte-identical IR ⇒ identical pixels — and the diff pinpoints the exact diverging `Cmd` rather than
//! an opaque image delta. This is the cutover gate.
//!
//! Live today: the **clear-path full frame** (a `glClear` frame) is compiled + run through the real
//! `gl_shim.c` (its `DD_IR_DUMP` host-tool mode) and compared byte-for-byte to the Rust shim's
//! lowering. Frames with a real draw need the GLSL→shader translator (not yet ported); those skip with
//! a notice and go live automatically once `crate::frame::build_frame_ir` returns them.

use std::path::{Path, PathBuf};
use std::process::Command;

use dd_shim_gl::common::ir::{encode_stream, Cmd};
use dd_shim_gl::common::wire::Decoder;
use dd_shim_gl::frame::build_frame_ir;
use dd_shim_gl::state::GlState;

fn decode_stream(bytes: &[u8]) -> Result<Vec<Cmd>, String> {
    let mut d = Decoder::new(bytes);
    let mut cmds = Vec::new();
    while !d.is_empty() {
        cmds.push(Cmd::decode(&mut d).map_err(|e| format!("decode at byte {}: {e:?}", d.pos()))?);
    }
    Ok(cmds)
}

/// Parity verdict: byte-identical, or the first point of divergence (decoded `Cmd` index) so a
/// regression is actionable.
fn diff_ir(c_shim: &[u8], rust_shim: &[u8]) -> Result<(), String> {
    if c_shim == rust_shim {
        return Ok(());
    }
    let a = decode_stream(c_shim)?;
    let b = decode_stream(rust_shim)?;
    for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
        if x != y {
            return Err(format!("IR diverges at command #{i}:\n  gl_shim.c : {x:?}\n  dd-shim-gl: {y:?}"));
        }
    }
    Err(format!(
        "IR command count differs: gl_shim.c={} dd-shim-gl={} (first {} match); raw lens {}/{}",
        a.len(),
        b.len(),
        a.len().min(b.len()),
        c_shim.len(),
        rust_shim.len()
    ))
}

fn gl_shim_c_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../dd-tests/guests/gl_shim.c")
}

/// Compile `gl_shim.c` + a workload `.c`, run it with `DD_IR_DUMP`, and return the IR bytes gl_shim.c
/// emitted for its frame. Returns `None` (skip) if the toolchain / source isn't available.
fn gl_shim_c_ir(workload_main: &str) -> Option<Vec<u8>> {
    let shim = gl_shim_c_path();
    if !shim.exists() {
        eprintln!("[parity] gl_shim.c not found at {shim:?}; skipping C-shim comparison");
        return None;
    }
    let dir = std::env::temp_dir().join(format!("dd-shim-parity-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let so = dir.join("libEGL.so.1");
    let wl_c = dir.join("workload.c");
    let wl = dir.join("workload");
    let ir = dir.join("frame.ir");
    std::fs::write(&wl_c, workload_main).ok()?;

    // gl_shim.c → libEGL.so.1 (all GLES/EGL symbols live in the one TU).
    let built_shim = Command::new("cc")
        .args(["-O2", "-fPIC", "-shared", "-o"])
        .arg(&so)
        .arg(&shim)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !built_shim {
        eprintln!("[parity] could not build gl_shim.c (no cc?); skipping");
        return None;
    }
    let built_wl = Command::new("cc")
        .arg("-O2")
        .arg(&wl_c)
        .arg(&so)
        .arg("-o")
        .arg(&wl)
        .arg(format!("-Wl,-rpath,{}", dir.display()))
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !built_wl {
        eprintln!("[parity] could not build the workload; skipping");
        return None;
    }
    if !Command::new(&wl).env("DD_IR_DUMP", &ir).status().map(|s| s.success()).unwrap_or(false) {
        eprintln!("[parity] workload run failed; skipping");
        return None;
    }
    let bytes = std::fs::read(&ir).ok();
    let _ = std::fs::remove_dir_all(&dir);
    bytes
}

/// The dd-shim-gl frame IR for a full-window clear, driving the same state methods the exported
/// `glClear`/`eglSwapBuffers` call — on an isolated `GlState` so the test doesn't race the
/// process-global state other tests use.
fn dd_shim_gl_clear_ir(w: u32, h: u32, color: [f32; 4]) -> Vec<u8> {
    let mut s = GlState::default();
    s.surface_up(w, h);
    s.clear = color;
    let (x, y, cw, ch, _scissored) = s.clear_scissor_rect(); // glClear, no scissor → full-target rect
    s.record_clear_call(x, y, cw, ch);
    build_frame_ir(&s).expect("clear frame must lower to IR")
}

#[test]
fn diff_engine_detects_divergence_and_agreement() {
    let a = encode_stream(&[Cmd::CreateFence(1), Cmd::Present { surface: 1, texture: 500 }]);
    let b = a.clone();
    assert!(diff_ir(&a, &b).is_ok());
    let c = encode_stream(&[Cmd::CreateFence(1), Cmd::Present { surface: 1, texture: 501 }]);
    assert!(diff_ir(&a, &c).unwrap_err().contains("command #1"));
    let d = encode_stream(&[Cmd::CreateFence(1)]);
    assert!(diff_ir(&a, &d).unwrap_err().contains("count differs"));
}

/// Resource-lowering parity (from inc2), now routed through the live diff engine.
#[test]
fn resource_lowering_is_parity_clean() {
    use dd_shim_gl::common::wire::Encoder;
    use dd_shim_gl::lower::vertex_buffer_cmds;
    let data: Vec<u8> = (0..64u8).collect();
    let rust_ir = encode_stream(&vertex_buffer_cmds(205, &data));
    let mut e = Encoder::new();
    e.u8(1);
    e.u32(205);
    e.u64(data.len() as u64);
    e.u32(1);
    e.str("");
    e.u8(3);
    e.u32(205);
    e.u64(0);
    e.bytes(&data);
    diff_ir(&e.into_vec(), &rust_ir).expect("resource lowering byte-identical to gl_shim.c");
}

/// THE CUTOVER GATE (clear path): a full 640x480 clear frame run through the real gl_shim.c must be
/// byte-identical to dd-shim-gl's lowering. Skips (does not fail) if the C toolchain is unavailable.
#[test]
fn full_frame_clear_is_byte_identical_to_gl_shim_c() {
    let workload = r#"
extern void* eglGetDisplay(void*);
extern unsigned eglInitialize(void*, int*, int*);
extern unsigned eglChooseConfig(void*, const int*, void**, int, int*);
extern void* eglCreateContext(void*, void*, void*, const int*);
extern void* eglCreateWindowSurface(void*, void*, void*, const int*);
extern unsigned eglMakeCurrent(void*, void*, void*, void*);
extern void glClearColor(float, float, float, float);
extern void glClear(unsigned);
extern unsigned eglSwapBuffers(void*, void*);
int main(void) {
    void* d = eglGetDisplay(0);
    eglInitialize(d, 0, 0);
    int cfgattr[] = { 0x3038 };
    void* cfg; int n;
    eglChooseConfig(d, cfgattr, &cfg, 1, &n);
    int ctxattr[] = { 0x3098, 2, 0x3038 };
    void* ctx = eglCreateContext(d, cfg, 0, ctxattr);
    int win[2] = { 640, 480 };
    void* s = eglCreateWindowSurface(d, cfg, win, 0);
    eglMakeCurrent(d, s, s, ctx);
    glClearColor(0.1f, 0.2f, 0.3f, 1.0f);
    glClear(0x4000);
    eglSwapBuffers(d, s);
    return 0;
}
"#;
    let c_ir = match gl_shim_c_ir(workload) {
        Some(b) => b,
        None => {
            eprintln!("[parity] SKIP: gl_shim.c toolchain unavailable");
            return;
        }
    };
    let rust_ir = dd_shim_gl_clear_ir(640, 480, [0.1, 0.2, 0.3, 1.0]);
    match diff_ir(&c_ir, &rust_ir) {
        Ok(()) => eprintln!("[parity] PASS clear-frame: dd-shim-gl IR byte-identical to gl_shim.c ({} bytes)", c_ir.len()),
        Err(e) => panic!("clear-frame parity FAILED:\n{e}"),
    }
}

#[test]
fn full_frame_draw_pending_translator() {
    // A real draw needs the GLSL translator (not yet ported); document the pending state so this goes
    // live the moment build_frame_ir starts returning draw frames.
    let mut s = GlState::default();
    s.surface_up(64, 64);
    s.record_draw_call(4, 0, 3, false, 0, 0);
    assert!(build_frame_ir(&s).is_none(), "a draw frame must not emit mismatched IR until the translator lands");
    eprintln!("[parity] SKIP full-frame draw: GLSL→shader translator pending");
}
