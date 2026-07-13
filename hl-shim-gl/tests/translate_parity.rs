//! Translator byte-parity: assert dd-shim-gl's Rust GLSL→MSL translator produces output IDENTICAL to
//! gl_shim.c's `translate()`, using gl_shim.c's own `-DDD_TR_TOOL gl_tr` tool as the oracle. Runs over
//! the committed `shader_translate/*.glsl` corpus (chrome/Skia shaders that exercise uniforms,
//! matrices, samplers, builtins, local decls). Skips (does not fail) if `cc` isn't available.

use std::path::{Path, PathBuf};
use std::process::Command;

fn shader_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../hl-jit-darwin/testdata/guests/shader_translate")
}
fn gl_shim_c() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../hl-jit-darwin/testdata/guests/gl_shim.c")
}

/// Build gl_shim.c's translator tool (`cc -DDD_TR_TOOL`). Returns the binary path, or None to skip.
fn build_gl_tr(dir: &Path) -> Option<PathBuf> {
    let src = gl_shim_c();
    if !src.exists() {
        return None;
    }
    let bin = dir.join("gl_tr");
    let ok = Command::new("cc")
        .arg("-DDD_TR_TOOL")
        .arg(&src)
        .arg("-o")
        .arg(&bin)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    ok.then_some(bin)
}

fn gl_tr_output(bin: &Path, vert: &Path, frag: &Path) -> Option<String> {
    let out = Command::new(bin).arg(vert).arg(frag).output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

#[test]
fn translator_matches_gl_shim_c_over_the_shader_corpus() {
    let dir = std::env::temp_dir().join(format!("dd-shim-tr-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let gl_tr = match build_gl_tr(&dir) {
        Some(b) => b,
        None => {
            eprintln!("[translate-parity] SKIP: cannot build gl_tr (no cc / no gl_shim.c)");
            return;
        }
    };

    let sdir = shader_dir();
    // Discover every <name>.vert.glsl / <name>.frag.glsl pair.
    let mut names: Vec<String> = std::fs::read_dir(&sdir)
        .expect("shader_translate dir")
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|f| f.strip_suffix(".vert.glsl").map(|s| s.to_string()))
        .collect();
    names.sort();
    assert!(!names.is_empty(), "no shader pairs found in {sdir:?}");

    let mut checked = 0;
    for name in &names {
        let vert = sdir.join(format!("{name}.vert.glsl"));
        let frag = sdir.join(format!("{name}.frag.glsl"));
        if !frag.exists() {
            continue;
        }
        let golden = match gl_tr_output(&gl_tr, &vert, &frag) {
            Some(g) => g,
            None => {
                eprintln!("[translate-parity] gl_tr failed on {name}; skipping that pair");
                continue;
            }
        };
        let vs = std::fs::read_to_string(&vert).unwrap();
        let fs = std::fs::read_to_string(&frag).unwrap();
        let got = hl_shim_gl::translate::translate(&vs, &fs);
        if got != golden {
            // Pinpoint the first differing line for an actionable failure.
            let gl: Vec<&str> = golden.lines().collect();
            let rl: Vec<&str> = got.lines().collect();
            let mut first = None;
            for i in 0..gl.len().max(rl.len()) {
                if gl.get(i) != rl.get(i) {
                    first = Some(i);
                    break;
                }
            }
            let i = first.unwrap_or(0);
            panic!(
                "translator diverges from gl_shim.c on '{name}' at line {i}:\n  gl_shim.c : {:?}\n  dd-shim-gl: {:?}\n--- full gl_shim.c ---\n{golden}\n--- full dd-shim-gl ---\n{got}",
                gl.get(i),
                rl.get(i)
            );
        }
        checked += 1;
    }
    let _ = std::fs::remove_dir_all(&dir);
    eprintln!("[translate-parity] PASS: {checked} shader pairs byte-identical to gl_shim.c translate()");
    assert!(checked > 0, "no shader pairs were actually compared");
}
