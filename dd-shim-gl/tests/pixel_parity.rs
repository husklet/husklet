//! Pixel-/IR-parity harness: run a GLES frame through the C shim (`gl_shim.c`) and the Rust shim
//! (`dd-shim-gl`) and diff the dd-gpu IR they emit. Both streams feed the *same* host backend, so
//! byte-identical IR ⇒ identical pixels — and the diff pinpoints the exact diverging `Cmd` rather than
//! an opaque image delta. This is the cutover gate.
//!
//! Two comparison modes, both live:
//! * `full_frame_clear_*` drives an isolated `GlState` (unit-level) against the real gl_shim.c dump.
//! * `full_frame_textured_triangle_*` is the flagship **black-box** test: it compiles the SAME GLES
//!   workload against BOTH shims' `.so` (gl_shim.c's `libEGL.so.1` and dd-shim-gl's cdylib), runs each
//!   with `DD_IR_DUMP`, and asserts the emitted IR byte-streams are identical.

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

/// Parity verdict: byte-identical, or the first point of divergence (decoded `Cmd` index).
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

/// Locate dd-shim-gl's built cdylib (target/<profile>/libdd_shim_gl.so), next to the test binary.
fn dd_shim_gl_so() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?; // target/<profile>/deps/<test>
    let so = exe.parent()?.parent()?.join("libdd_shim_gl.so");
    so.exists().then_some(so)
}

/// Build gl_shim.c into a `libEGL.so.1` in `dir`. Returns the path, or None to skip.
fn build_gl_shim_so(dir: &Path) -> Option<PathBuf> {
    let src = gl_shim_c_path();
    if !src.exists() {
        return None;
    }
    let so = dir.join("libEGL.so.1");
    let ok = Command::new("cc")
        .args(["-O2", "-fPIC", "-shared", "-Wl,-soname,libEGL.so.1", "-o"])
        .arg(&so)
        .arg(&src)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    ok.then_some(so)
}

/// Compile `workload` against `shim_so` (staged as libEGL.so.1) and run it with DD_IR_DUMP; return the
/// emitted IR bytes. `tag` disambiguates the temp dir. None on any toolchain failure (skip).
fn run_workload_against(shim_so: &Path, workload: &str, tag: &str) -> Option<Vec<u8>> {
    let dir = std::env::temp_dir().join(format!("dd-shim-parity-{}-{tag}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let egl = dir.join("libEGL.so.1");
    std::fs::copy(shim_so, &egl).ok()?;
    let wl_c = dir.join("workload.c");
    let wl = dir.join("workload");
    let ir = dir.join("frame.ir");
    std::fs::write(&wl_c, workload).ok()?;
    let built = Command::new("cc")
        .arg("-O2")
        .arg(&wl_c)
        .arg(&egl)
        .arg("-o")
        .arg(&wl)
        .arg(format!("-Wl,-rpath,{}", dir.display()))
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !built {
        eprintln!("[parity] workload compile failed ({tag}); skipping");
        return None;
    }
    if !Command::new(&wl).env("DD_IR_DUMP", &ir).status().map(|s| s.success()).unwrap_or(false) {
        eprintln!("[parity] workload run failed ({tag}); skipping");
        return None;
    }
    let bytes = std::fs::read(&ir).ok();
    let _ = std::fs::remove_dir_all(&dir);
    bytes
}

fn dd_shim_gl_clear_ir(w: u32, h: u32, color: [f32; 4]) -> Vec<u8> {
    let mut s = GlState::default();
    s.surface_up(w, h);
    s.clear = color;
    let (x, y, cw, ch, _scissored) = s.clear_scissor_rect();
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
}

/// Cutover gate (clear path): a 640x480 clear frame through the real gl_shim.c, byte-identical.
#[test]
fn full_frame_clear_is_byte_identical_to_gl_shim_c() {
    let workload = CLEAR_WORKLOAD;
    let dir = std::env::temp_dir().join(format!("dd-shim-clear-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let so = match build_gl_shim_so(&dir) {
        Some(s) => s,
        None => {
            eprintln!("[parity] SKIP clear: gl_shim.c toolchain unavailable");
            return;
        }
    };
    let c_ir = match run_workload_against(&so, workload, "clear-c") {
        Some(b) => b,
        None => return,
    };
    let _ = std::fs::remove_dir_all(&dir);
    let rust_ir = dd_shim_gl_clear_ir(640, 480, [0.1, 0.2, 0.3, 1.0]);
    match diff_ir(&c_ir, &rust_ir) {
        Ok(()) => eprintln!("[parity] PASS clear-frame: byte-identical ({} bytes)", c_ir.len()),
        Err(e) => panic!("clear-frame parity FAILED:\n{e}"),
    }
}

/// THE CUTOVER GATE (flagship): a textured-triangle frame (shader compile + VBO + texture + uniforms +
/// a draw) compiled against BOTH shims and run; the emitted IR must be byte-identical. This is the
/// green light for a flagged live glmark2 cutover test.
#[test]
fn full_frame_textured_triangle_is_byte_identical() {
    let dir = std::env::temp_dir().join(format!("dd-shim-tt-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let gl_shim_so = match build_gl_shim_so(&dir) {
        Some(s) => s,
        None => {
            eprintln!("[parity] SKIP textured-triangle: gl_shim.c toolchain unavailable");
            return;
        }
    };
    let rust_so = match dd_shim_gl_so() {
        Some(s) => s,
        None => {
            eprintln!("[parity] SKIP textured-triangle: dd-shim-gl cdylib not found");
            return;
        }
    };
    let c_ir = match run_workload_against(&gl_shim_so, TEXTURED_TRIANGLE_WORKLOAD, "tt-c") {
        Some(b) => b,
        None => return,
    };
    let rust_ir = match run_workload_against(&rust_so, TEXTURED_TRIANGLE_WORKLOAD, "tt-rust") {
        Some(b) => b,
        None => return,
    };
    let _ = std::fs::remove_dir_all(&dir);
    match diff_ir(&c_ir, &rust_ir) {
        Ok(()) => eprintln!("[parity] PASS textured-triangle: dd-shim-gl IR byte-identical to gl_shim.c ({} bytes)", c_ir.len()),
        Err(e) => panic!("textured-triangle parity FAILED:\n{e}"),
    }
}

/// THE REAL-WORKLOAD GATE: a glmark2-es2-ish frame — clear + TWO textured draws with rotating uniform
/// state — takes gl_shim.c's `replay` path (per-draw ids + segmented render pass). Compiled against
/// BOTH shims; the full multi-draw IR must be byte-identical. This green closes IR parity for real apps.
#[test]
fn full_frame_multi_draw_replay_is_byte_identical() {
    let dir = std::env::temp_dir().join(format!("dd-shim-md-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let gl_shim_so = match build_gl_shim_so(&dir) {
        Some(s) => s,
        None => {
            eprintln!("[parity] SKIP multi-draw: gl_shim.c toolchain unavailable");
            return;
        }
    };
    let rust_so = match dd_shim_gl_so() {
        Some(s) => s,
        None => {
            eprintln!("[parity] SKIP multi-draw: dd-shim-gl cdylib not found");
            return;
        }
    };
    let c_ir = match run_workload_against(&gl_shim_so, MULTI_DRAW_WORKLOAD, "md-c") {
        Some(b) => b,
        None => return,
    };
    let rust_ir = match run_workload_against(&rust_so, MULTI_DRAW_WORKLOAD, "md-rust") {
        Some(b) => b,
        None => return,
    };
    let _ = std::fs::remove_dir_all(&dir);
    match diff_ir(&c_ir, &rust_ir) {
        Ok(()) => eprintln!("[parity] PASS multi-draw replay: dd-shim-gl IR byte-identical to gl_shim.c ({} bytes)", c_ir.len()),
        Err(e) => panic!("multi-draw replay parity FAILED:\n{e}"),
    }
}

/// The wl_egl_window path glmark2/Chrome use: `wl_egl_window_create` (our libwayland-egl) → the magic
/// window struct → `eglCreateWindowSurface`. Compiled against both shims; IR must be byte-identical.
#[test]
fn full_frame_wl_egl_window_is_byte_identical() {
    let dir = std::env::temp_dir().join(format!("dd-shim-wl-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let gl_shim_so = match build_gl_shim_so(&dir) {
        Some(s) => s,
        None => {
            eprintln!("[parity] SKIP wl_egl_window: gl_shim.c toolchain unavailable");
            return;
        }
    };
    let rust_so = match dd_shim_gl_so() {
        Some(s) => s,
        None => {
            eprintln!("[parity] SKIP wl_egl_window: dd-shim-gl cdylib not found");
            return;
        }
    };
    let c_ir = match run_workload_against(&gl_shim_so, WL_EGL_WORKLOAD, "wl-c") {
        Some(b) => b,
        None => return,
    };
    let rust_ir = match run_workload_against(&rust_so, WL_EGL_WORKLOAD, "wl-rust") {
        Some(b) => b,
        None => return,
    };
    let _ = std::fs::remove_dir_all(&dir);
    match diff_ir(&c_ir, &rust_ir) {
        Ok(()) => eprintln!("[parity] PASS wl_egl_window: byte-identical ({} bytes)", c_ir.len()),
        Err(e) => panic!("wl_egl_window parity FAILED:\n{e}"),
    }
}

/// Exercises the newly-ported uniform setters — a mat3 uniform (16-byte MSL column re-stride) + an
/// ivec2 — which real apps (glmark2 normal matrices, GTK) use. A stub would drop them; here the full IR
/// (incl. the uniform buffer bytes) must be byte-identical to gl_shim.c.
#[test]
fn full_frame_mat3_uniform_is_byte_identical() {
    let dir = std::env::temp_dir().join(format!("dd-shim-u3-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let gl_shim_so = match build_gl_shim_so(&dir) {
        Some(s) => s,
        None => {
            eprintln!("[parity] SKIP mat3-uniform: gl_shim.c toolchain unavailable");
            return;
        }
    };
    let rust_so = match dd_shim_gl_so() {
        Some(s) => s,
        None => return,
    };
    let c_ir = match run_workload_against(&gl_shim_so, MAT3_UNIFORM_WORKLOAD, "u3-c") {
        Some(b) => b,
        None => return,
    };
    let rust_ir = match run_workload_against(&rust_so, MAT3_UNIFORM_WORKLOAD, "u3-rust") {
        Some(b) => b,
        None => return,
    };
    let _ = std::fs::remove_dir_all(&dir);
    match diff_ir(&c_ir, &rust_ir) {
        Ok(()) => eprintln!("[parity] PASS mat3-uniform: byte-identical ({} bytes)", c_ir.len()),
        Err(e) => panic!("mat3-uniform parity FAILED:\n{e}"),
    }
}

const MAT3_UNIFORM_WORKLOAD: &str = r#"
extern void* eglGetDisplay(void*);
extern unsigned eglInitialize(void*, int*, int*);
extern unsigned eglChooseConfig(void*, const int*, void**, int, int*);
extern void* eglCreateContext(void*, void*, void*, const int*);
extern void* eglCreateWindowSurface(void*, void*, void*, const int*);
extern unsigned eglMakeCurrent(void*, void*, void*, void*);
extern unsigned eglSwapBuffers(void*, void*);
extern unsigned glCreateShader(unsigned);
extern void glShaderSource(unsigned, int, const char* const*, const int*);
extern void glCompileShader(unsigned);
extern unsigned glCreateProgram(void);
extern void glAttachShader(unsigned, unsigned);
extern void glLinkProgram(unsigned);
extern void glUseProgram(unsigned);
extern void glGenBuffers(int, unsigned*);
extern void glBindBuffer(unsigned, unsigned);
extern void glBufferData(unsigned, long, const void*, unsigned);
extern int glGetAttribLocation(unsigned, const char*);
extern void glEnableVertexAttribArray(unsigned);
extern void glVertexAttribPointer(unsigned, int, unsigned, unsigned char, int, const void*);
extern int glGetUniformLocation(unsigned, const char*);
extern void glUniformMatrix3fv(int, int, unsigned char, const float*);
extern void glUniform2iv(int, int, const int*);
extern void glDrawArrays(unsigned, int, int);
static const char* VS =
  "attribute vec2 aPos;\n"
  "uniform mat3 uNormal;\n"
  "uniform ivec2 uSel;\n"
  "varying vec2 vN;\n"
  "void main(){ vN = (uNormal * vec3(aPos, 1.0)).xy + vec2(uSel); gl_Position = vec4(aPos, 0.0, 1.0); }\n";
static const char* FS =
  "precision mediump float;\n"
  "varying vec2 vN;\n"
  "void main(){ gl_FragColor = vec4(vN, 0.0, 1.0); }\n";
int main(void) {
    void* d = eglGetDisplay(0); eglInitialize(d, 0, 0);
    int ca[] = { 0x3038 }; void* cfg; int n; eglChooseConfig(d, ca, &cfg, 1, &n);
    int cta[] = { 0x3098, 2, 0x3038 }; void* ctx = eglCreateContext(d, cfg, 0, cta);
    int win[2] = { 128, 128 }; void* s = eglCreateWindowSurface(d, cfg, win, 0); eglMakeCurrent(d, s, s, ctx);
    unsigned vs = glCreateShader(0x8B31); glShaderSource(vs, 1, &VS, 0); glCompileShader(vs);
    unsigned fs = glCreateShader(0x8B30); glShaderSource(fs, 1, &FS, 0); glCompileShader(fs);
    unsigned p = glCreateProgram(); glAttachShader(p, vs); glAttachShader(p, fs); glLinkProgram(p); glUseProgram(p);
    float v[] = { 0,0, 1,0, 0,1 }; unsigned b; glGenBuffers(1, &b); glBindBuffer(0x8892, b);
    glBufferData(0x8892, (long)sizeof(v), v, 0x88E4);
    int a = glGetAttribLocation(p, "aPos"); glEnableVertexAttribArray((unsigned)a);
    glVertexAttribPointer((unsigned)a, 2, 0x1406, 0, 8, (void*)0);
    float m3[9] = { 1,0,0, 0,1,0, 0,0,1 };
    glUniformMatrix3fv(glGetUniformLocation(p, "uNormal"), 1, 0, m3);
    int sel[2] = { 3, 7 };
    glUniform2iv(glGetUniformLocation(p, "uSel"), 1, sel);
    glDrawArrays(0x0004, 0, 3);
    eglSwapBuffers(d, s); return 0;
}
"#;

// glmark2's window path: wl_egl_window_create → eglCreateWindowSurface → clear + textured-less draw.
const WL_EGL_WORKLOAD: &str = r#"
extern void* wl_egl_window_create(void*, int, int);
extern void* eglGetDisplay(void*);
extern unsigned eglInitialize(void*, int*, int*);
extern unsigned eglChooseConfig(void*, const int*, void**, int, int*);
extern void* eglCreateContext(void*, void*, void*, const int*);
extern void* eglCreateWindowSurface(void*, void*, void*, const int*);
extern unsigned eglMakeCurrent(void*, void*, void*, void*);
extern unsigned eglSwapBuffers(void*, void*);
extern unsigned glCreateShader(unsigned);
extern void glShaderSource(unsigned, int, const char* const*, const int*);
extern void glCompileShader(unsigned);
extern unsigned glCreateProgram(void);
extern void glAttachShader(unsigned, unsigned);
extern void glLinkProgram(unsigned);
extern void glUseProgram(unsigned);
extern void glGenBuffers(int, unsigned*);
extern void glBindBuffer(unsigned, unsigned);
extern void glBufferData(unsigned, long, const void*, unsigned);
extern int glGetAttribLocation(unsigned, const char*);
extern void glEnableVertexAttribArray(unsigned);
extern void glVertexAttribPointer(unsigned, int, unsigned, unsigned char, int, const void*);
extern int glGetUniformLocation(unsigned, const char*);
extern void glUniformMatrix4fv(int, int, unsigned char, const float*);
extern void glClearColor(float, float, float, float);
extern void glClear(unsigned);
extern void glDrawArrays(unsigned, int, int);
static const char* VS = "attribute vec2 aPos;uniform mat4 uMVP;void main(){gl_Position=uMVP*vec4(aPos,0.0,1.0);}\n";
static const char* FS = "precision mediump float;void main(){gl_FragColor=vec4(1.0,0.5,0.0,1.0);}\n";
int main(void) {
    void* win = wl_egl_window_create(0, 300, 200);
    void* d = eglGetDisplay(0); eglInitialize(d, 0, 0);
    int ca[] = { 0x3038 }; void* cfg; int n; eglChooseConfig(d, ca, &cfg, 1, &n);
    int cta[] = { 0x3098, 2, 0x3038 }; void* ctx = eglCreateContext(d, cfg, 0, cta);
    void* s = eglCreateWindowSurface(d, cfg, win, 0); eglMakeCurrent(d, s, s, ctx);
    unsigned vs = glCreateShader(0x8B31); glShaderSource(vs, 1, &VS, 0); glCompileShader(vs);
    unsigned fs = glCreateShader(0x8B30); glShaderSource(fs, 1, &FS, 0); glCompileShader(fs);
    unsigned p = glCreateProgram(); glAttachShader(p, vs); glAttachShader(p, fs); glLinkProgram(p); glUseProgram(p);
    float v[] = { 0,0, 1,0, 0,1 }; unsigned b; glGenBuffers(1, &b); glBindBuffer(0x8892, b);
    glBufferData(0x8892, (long)sizeof(v), v, 0x88E4);
    int a = glGetAttribLocation(p, "aPos"); glEnableVertexAttribArray((unsigned)a);
    glVertexAttribPointer((unsigned)a, 2, 0x1406, 0, 8, (void*)0);
    float m[16] = { 1,0,0,0, 0,1,0,0, 0,0,1,0, 0,0,0,1 };
    int u = glGetUniformLocation(p, "uMVP"); glUniformMatrix4fv(u, 1, 0, m);
    glClearColor(0.1f, 0.1f, 0.1f, 1.0f); glClear(0x4000); glDrawArrays(0x0004, 0, 3);
    eglSwapBuffers(d, s); return 0;
}
"#;

const CLEAR_WORKLOAD: &str = r#"
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

// A textured triangle, no glClear (→ gl_shim.c's single-draw path). Shader compile + VBO + 2x2 texture
// + a mat4 uniform + a sampler + glDrawArrays.
const TEXTURED_TRIANGLE_WORKLOAD: &str = r#"
extern void* eglGetDisplay(void*);
extern unsigned eglInitialize(void*, int*, int*);
extern unsigned eglChooseConfig(void*, const int*, void**, int, int*);
extern void* eglCreateContext(void*, void*, void*, const int*);
extern void* eglCreateWindowSurface(void*, void*, void*, const int*);
extern unsigned eglMakeCurrent(void*, void*, void*, void*);
extern unsigned eglSwapBuffers(void*, void*);
extern unsigned glCreateShader(unsigned);
extern void glShaderSource(unsigned, int, const char* const*, const int*);
extern void glCompileShader(unsigned);
extern unsigned glCreateProgram(void);
extern void glAttachShader(unsigned, unsigned);
extern void glLinkProgram(unsigned);
extern void glUseProgram(unsigned);
extern void glGenBuffers(int, unsigned*);
extern void glBindBuffer(unsigned, unsigned);
extern void glBufferData(unsigned, long, const void*, unsigned);
extern int glGetAttribLocation(unsigned, const char*);
extern void glEnableVertexAttribArray(unsigned);
extern void glVertexAttribPointer(unsigned, int, unsigned, unsigned char, int, const void*);
extern void glGenTextures(int, unsigned*);
extern void glActiveTexture(unsigned);
extern void glBindTexture(unsigned, unsigned);
extern void glTexImage2D(unsigned, int, int, int, int, int, unsigned, unsigned, const void*);
extern void glTexParameteri(unsigned, unsigned, int);
extern int glGetUniformLocation(unsigned, const char*);
extern void glUniformMatrix4fv(int, int, unsigned char, const float*);
extern void glUniform1i(int, int);
extern void glDrawArrays(unsigned, int, int);

static const char* VS =
  "attribute vec2 aPos;\n"
  "attribute vec2 aTex;\n"
  "uniform mat4 uMVP;\n"
  "varying vec2 vTex;\n"
  "void main(){\n"
  "  gl_Position = uMVP * vec4(aPos, 0.0, 1.0);\n"
  "  vTex = aTex;\n"
  "}\n";
static const char* FS =
  "precision mediump float;\n"
  "uniform sampler2D uTex;\n"
  "varying vec2 vTex;\n"
  "void main(){\n"
  "  gl_FragColor = texture2D(uTex, vTex);\n"
  "}\n";

int main(void) {
    void* d = eglGetDisplay(0);
    eglInitialize(d, 0, 0);
    int cfgattr[] = { 0x3038 };
    void* cfg; int n;
    eglChooseConfig(d, cfgattr, &cfg, 1, &n);
    int ctxattr[] = { 0x3098, 2, 0x3038 };
    void* ctx = eglCreateContext(d, cfg, 0, ctxattr);
    int win[2] = { 256, 256 };
    void* s = eglCreateWindowSurface(d, cfg, win, 0);
    eglMakeCurrent(d, s, s, ctx);

    unsigned vs = glCreateShader(0x8B31);
    glShaderSource(vs, 1, &VS, 0);
    glCompileShader(vs);
    unsigned fs = glCreateShader(0x8B30);
    glShaderSource(fs, 1, &FS, 0);
    glCompileShader(fs);
    unsigned prog = glCreateProgram();
    glAttachShader(prog, vs);
    glAttachShader(prog, fs);
    glLinkProgram(prog);
    glUseProgram(prog);

    float verts[] = { 0,0, 0,0,  1,0, 1,0,  0,1, 0,1 };
    unsigned vbo; glGenBuffers(1, &vbo);
    glBindBuffer(0x8892, vbo);
    glBufferData(0x8892, (long)sizeof(verts), verts, 0x88E4);

    int aPos = glGetAttribLocation(prog, "aPos");
    int aTex = glGetAttribLocation(prog, "aTex");
    glEnableVertexAttribArray((unsigned)aPos);
    glVertexAttribPointer((unsigned)aPos, 2, 0x1406, 0, 16, (const void*)0);
    glEnableVertexAttribArray((unsigned)aTex);
    glVertexAttribPointer((unsigned)aTex, 2, 0x1406, 0, 16, (const void*)8);

    unsigned char pix[16] = { 255,0,0,255, 0,255,0,255, 0,0,255,255, 255,255,0,255 };
    unsigned tex; glGenTextures(1, &tex);
    glActiveTexture(0x84C0);
    glBindTexture(0x0DE1, tex);
    glTexImage2D(0x0DE1, 0, 0x1908, 2, 2, 0, 0x1908, 0x1401, pix);
    glTexParameteri(0x0DE1, 0x2801, 0x2601);
    glTexParameteri(0x0DE1, 0x2800, 0x2601);

    float mvp[16] = { 1,0,0,0, 0,1,0,0, 0,0,1,0, 0,0,0,1 };
    int uMVP = glGetUniformLocation(prog, "uMVP");
    glUniformMatrix4fv(uMVP, 1, 0, mvp);
    int uTex = glGetUniformLocation(prog, "uTex");
    glUniform1i(uTex, 0);

    glDrawArrays(0x0004, 0, 3);
    eglSwapBuffers(d, s);
    return 0;
}
"#;

// A clear + two textured draws with rotating uniform state → gl_shim.c's replay path.
const MULTI_DRAW_WORKLOAD: &str = r#"
extern void* eglGetDisplay(void*);
extern unsigned eglInitialize(void*, int*, int*);
extern unsigned eglChooseConfig(void*, const int*, void**, int, int*);
extern void* eglCreateContext(void*, void*, void*, const int*);
extern void* eglCreateWindowSurface(void*, void*, void*, const int*);
extern unsigned eglMakeCurrent(void*, void*, void*, void*);
extern unsigned eglSwapBuffers(void*, void*);
extern unsigned glCreateShader(unsigned);
extern void glShaderSource(unsigned, int, const char* const*, const int*);
extern void glCompileShader(unsigned);
extern unsigned glCreateProgram(void);
extern void glAttachShader(unsigned, unsigned);
extern void glLinkProgram(unsigned);
extern void glUseProgram(unsigned);
extern void glGenBuffers(int, unsigned*);
extern void glBindBuffer(unsigned, unsigned);
extern void glBufferData(unsigned, long, const void*, unsigned);
extern int glGetAttribLocation(unsigned, const char*);
extern void glEnableVertexAttribArray(unsigned);
extern void glVertexAttribPointer(unsigned, int, unsigned, unsigned char, int, const void*);
extern void glGenTextures(int, unsigned*);
extern void glActiveTexture(unsigned);
extern void glBindTexture(unsigned, unsigned);
extern void glTexImage2D(unsigned, int, int, int, int, int, unsigned, unsigned, const void*);
extern void glTexParameteri(unsigned, unsigned, int);
extern int glGetUniformLocation(unsigned, const char*);
extern void glUniformMatrix4fv(int, int, unsigned char, const float*);
extern void glUniform1i(int, int);
extern void glClearColor(float, float, float, float);
extern void glClear(unsigned);
extern void glEnable(unsigned);
extern void glBlendFunc(unsigned, unsigned);
extern void glDrawArrays(unsigned, int, int);

static const char* VS =
  "attribute vec2 aPos;\n"
  "attribute vec2 aTex;\n"
  "uniform mat4 uMVP;\n"
  "varying vec2 vTex;\n"
  "void main(){ gl_Position = uMVP * vec4(aPos, 0.0, 1.0); vTex = aTex; }\n";
static const char* FS =
  "precision mediump float;\n"
  "uniform sampler2D uTex;\n"
  "varying vec2 vTex;\n"
  "void main(){ gl_FragColor = texture2D(uTex, vTex); }\n";

int main(void) {
    void* d = eglGetDisplay(0);
    eglInitialize(d, 0, 0);
    int cfgattr[] = { 0x3038 };
    void* cfg; int n;
    eglChooseConfig(d, cfgattr, &cfg, 1, &n);
    int ctxattr[] = { 0x3098, 2, 0x3038 };
    void* ctx = eglCreateContext(d, cfg, 0, ctxattr);
    int win[2] = { 320, 240 };
    void* s = eglCreateWindowSurface(d, cfg, win, 0);
    eglMakeCurrent(d, s, s, ctx);

    unsigned vs = glCreateShader(0x8B31); glShaderSource(vs, 1, &VS, 0); glCompileShader(vs);
    unsigned fs = glCreateShader(0x8B30); glShaderSource(fs, 1, &FS, 0); glCompileShader(fs);
    unsigned prog = glCreateProgram();
    glAttachShader(prog, vs); glAttachShader(prog, fs); glLinkProgram(prog); glUseProgram(prog);

    float verts[] = { 0,0, 0,0,  1,0, 1,0,  0,1, 0,1 };
    unsigned vbo; glGenBuffers(1, &vbo);
    glBindBuffer(0x8892, vbo);
    glBufferData(0x8892, (long)sizeof(verts), verts, 0x88E4);
    int aPos = glGetAttribLocation(prog, "aPos");
    int aTex = glGetAttribLocation(prog, "aTex");
    glEnableVertexAttribArray((unsigned)aPos);
    glVertexAttribPointer((unsigned)aPos, 2, 0x1406, 0, 16, (const void*)0);
    glEnableVertexAttribArray((unsigned)aTex);
    glVertexAttribPointer((unsigned)aTex, 2, 0x1406, 0, 16, (const void*)8);

    unsigned char pix[16] = { 255,0,0,255, 0,255,0,255, 0,0,255,255, 255,255,0,255 };
    unsigned tex; glGenTextures(1, &tex);
    glActiveTexture(0x84C0); glBindTexture(0x0DE1, tex);
    glTexImage2D(0x0DE1, 0, 0x1908, 2, 2, 0, 0x1908, 0x1401, pix);
    glTexParameteri(0x0DE1, 0x2801, 0x2601);
    int uMVP = glGetUniformLocation(prog, "uMVP");
    int uTex = glGetUniformLocation(prog, "uTex");
    glUniform1i(uTex, 0);

    /* frame: clear + two draws with different transforms */
    glEnable(0x0BE2);            /* GL_BLEND */
    glBlendFunc(0x0302, 0x0303); /* SRC_ALPHA, ONE_MINUS_SRC_ALPHA */
    glClearColor(0.2f, 0.2f, 0.2f, 1.0f);
    glClear(0x4000);
    float mvp1[16] = { 1,0,0,0, 0,1,0,0, 0,0,1,0,  0.0f,0,0,1 };
    glUniformMatrix4fv(uMVP, 1, 0, mvp1);
    glDrawArrays(0x0004, 0, 3);
    float mvp2[16] = { 1,0,0,0, 0,1,0,0, 0,0,1,0, -0.5f,0,0,1 };
    glUniformMatrix4fv(uMVP, 1, 0, mvp2);
    glDrawArrays(0x0004, 0, 3);
    eglSwapBuffers(d, s);
    return 0;
}
"#;
