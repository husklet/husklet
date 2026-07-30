//! ABI conformance gate for the guest GL/EGL shim objects — now SPLIT across the two staged cdylibs,
//! matching real Mesa's library layout:
//!
//!   * `libGLESv2.so.2` exports the `gl*` core render set (364 entry points) in ITS OWN dynamic symbol
//!     table — so libepoxy (GTK's GL loader) `dlsym`s core `gl*` directly from it, and
//!   * `libEGL.so.1`   exports the `egl*` set (44) + the one shared-state accessor `hl_shim_state_ptr`
//!     (and NO `gl*` — they are `local:`, hidden by the version script), so an EGL app binds its
//!     lifecycle here.
//!
//! Together the two objects cover the whole `gl*`+`egl*` surface with no leakage and no duplication of
//! the exported names. This test:
//!   1. reads the two committed goldens (`abi_symbols_gl.txt` = 364, `abi_symbols_egl.txt` = 44),
//!   2. cross-checks the generator's SOURCE — the shim manifest's `GL`/`EGL` rows equal those goldens
//!      (so the generated surface can't drift), and
//!   3. `nm -D`s each STAGED `.so` (produced by this crate's `build.rs` during the test build) and asserts
//!      its exported dynamic symbols EQUAL its golden exactly, that libEGL leaks NO `gl*`, and that
//!      libGLESv2 leaks NO `egl*`.
//!
//! ABI GOLDEN SPLIT (intentional, correct): the old single golden pinned `libEGL == 402` (`gl*`+`egl*`).
//! It is now split `libEGL == 44` (`egl*`) + `libGLESv2 == 364` (`gl*`) — same union, distributed to
//! match Mesa so libepoxy stops aborting.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

const GOLDEN_GL: &str = "shim/egl/tests/golden/abi_symbols_gl.txt";
const GOLDEN_EGL: &str = "shim/egl/tests/golden/abi_symbols_egl.txt";
const MANIFEST: &str = "shim/egl/registry/gles2_egl.manifest";
const GOLDEN_GBM: &str = "shim/gbm.symbols";
const EXPECTED_GL: usize = 422;
const EXPECTED_EGL: usize = 44;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The staged shim dir for the host arch, e.g. `~/.hl/gl/aarch64` — where `build.rs` installs the two
/// cdylibs (the exact artifacts the guest e2e apps load via `LD_LIBRARY_PATH`).
fn staged_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let arch = match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        "x86_64" => "x86_64",
        other => panic!("unsupported host arch for the ABI test: {other}"),
    };
    Path::new(&home).join(".hl").join("gl").join(arch)
}

fn read_golden(path: &Path) -> BTreeSet<String> {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read golden {}: {e}", path.display()))
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// Names in the shim manifest whose `LIB` column (col 1) is `lib` (col 2 is the entry-point name).
fn manifest_names(path: &Path, lib: &str) -> BTreeSet<String> {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read manifest {}: {e}", path.display()))
        .lines()
        .filter(|l| !l.trim_start().starts_with('#') && !l.trim().is_empty())
        .filter_map(|l| {
            let mut f = l.split('\t');
            let col_lib = f.next()?;
            let name = f.next()?;
            (col_lib == lib).then(|| name.to_string())
        })
        .collect()
}

/// The exported (`nm -D --defined-only`) symbols of a staged `.so`, filtered to the API prefix `pred`.
fn exports(so: &Path, pred: impl Fn(&str) -> bool) -> BTreeSet<String> {
    assert!(
        so.exists(),
        "staged {} missing — run the crate build first so build.rs stages the shims",
        so.display()
    );
    let out = Command::new("nm")
        .args(["-D", "--defined-only"])
        .arg(so)
        .output()
        .unwrap_or_else(|e| panic!("run nm on {}: {e}", so.display()));
    assert!(out.status.success(), "nm -D failed on {}", so.display());
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_whitespace().nth(2)) // "<addr> T <name>"
        .filter(|s| pred(s))
        .map(str::to_string)
        .collect()
}

fn needed(so: &Path) -> BTreeSet<String> {
    let out = Command::new("readelf")
        .args(["-d"])
        .arg(so)
        .output()
        .unwrap_or_else(|e| panic!("run readelf on {}: {e}", so.display()));
    assert!(
        out.status.success(),
        "readelf -d failed on {}",
        so.display()
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|line| line.contains("(NEEDED)"))
        .filter_map(|line| {
            line.split_once('[')?
                .1
                .split_once(']')
                .map(|(name, _)| name)
        })
        .map(str::to_owned)
        .collect()
}

fn imports(so: &Path, symbol: &str) -> bool {
    let out = Command::new("nm")
        .args(["-D", "--undefined-only"])
        .arg(so)
        .output()
        .unwrap_or_else(|e| panic!("run nm on {}: {e}", so.display()));
    assert!(
        out.status.success(),
        "nm -D --undefined-only failed on {}",
        so.display()
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .any(|line| line.split_whitespace().last() == Some(symbol))
}

/// Is `s` a core `gl*` name (and NOT an `egl*` name — `egl*` starts with `e`, so this never overlaps)?
fn is_gl(s: &str) -> bool {
    s.starts_with("gl")
}
fn is_egl(s: &str) -> bool {
    s.starts_with("egl")
}

#[test]
fn shim_export_surface_matches_the_split_golden_abi() {
    let golden_gl = read_golden(&manifest_dir().join(GOLDEN_GL));
    let golden_egl = read_golden(&manifest_dir().join(GOLDEN_EGL));
    assert_eq!(
        golden_gl.len(),
        EXPECTED_GL,
        "golden {GOLDEN_GL} has an unexpected count"
    );
    assert_eq!(
        golden_egl.len(),
        EXPECTED_EGL,
        "golden {GOLDEN_EGL} has an unexpected count"
    );

    // (a) the generator's SOURCE: manifest GL/EGL rows == the respective goldens (surface can't drift).
    let manifest = manifest_dir().join(MANIFEST);
    assert_eq!(
        manifest_names(&manifest, "GL"),
        golden_gl,
        "manifest GL rows differ from the gl golden"
    );
    assert_eq!(
        manifest_names(&manifest, "EGL"),
        golden_egl,
        "manifest EGL rows differ from the egl golden"
    );

    let dir = staged_dir();
    let libgles = dir.join("libGLESv2.so.2");
    let libegl = dir.join("libEGL.so.1");
    let libgbm = dir.join("libgbm.so.1");
    assert_eq!(
        exports(&libegl, |symbol| symbol
            == "hl_shim_external_buffers_enabled"),
        ["hl_shim_external_buffers_enabled".to_owned()]
            .into_iter()
            .collect(),
        "GBM capability gate must be dynamically visible from libEGL"
    );
    assert_eq!(
        exports(&libgbm, |symbol| symbol.starts_with("gbm_")),
        read_golden(&manifest_dir().join(GOLDEN_GBM)),
        "libgbm.so.1 exported ABI differs from its golden"
    );
    assert!(
        needed(&libgbm).contains("libEGL.so.1"),
        "libgbm must load its capability owner before querying external-buffer support"
    );
    assert!(
        imports(&libgbm, "hl_shim_external_buffers_enabled"),
        "libgbm must bind the EGL-owned capability directly, not discover it opportunistically"
    );

    // (b) libGLESv2.so.2 exports EXACTLY the gl* golden, and leaks NO egl*.
    let gles_gl = exports(&libgles, is_gl);
    let gles_egl = exports(&libgles, is_egl);
    let missing: Vec<_> = golden_gl.difference(&gles_gl).collect();
    let extra: Vec<_> = gles_gl.difference(&golden_gl).collect();
    assert!(
        missing.is_empty(),
        "libGLESv2.so.2: gl* golden symbols missing: {missing:?}"
    );
    assert!(
        extra.is_empty(),
        "libGLESv2.so.2: exports gl* not in the golden: {extra:?}"
    );
    assert_eq!(
        gles_gl.len(),
        EXPECTED_GL,
        "libGLESv2.so.2: exported gl* count drifted"
    );
    assert!(
        gles_egl.is_empty(),
        "libGLESv2.so.2 must not export any egl*: {gles_egl:?}"
    );

    // (c) libEGL.so.1 exports EXACTLY the egl* golden, and leaks NO gl*.
    let egl_egl = exports(&libegl, is_egl);
    let egl_gl = exports(&libegl, is_gl);
    let missing: Vec<_> = golden_egl.difference(&egl_egl).collect();
    let extra: Vec<_> = egl_egl.difference(&golden_egl).collect();
    assert!(
        missing.is_empty(),
        "libEGL.so.1: egl* golden symbols missing: {missing:?}"
    );
    assert!(
        extra.is_empty(),
        "libEGL.so.1: exports egl* not in the golden: {extra:?}"
    );
    assert_eq!(
        egl_egl.len(),
        EXPECTED_EGL,
        "libEGL.so.1: exported egl* count drifted"
    );
    assert!(
        egl_gl.is_empty(),
        "libEGL.so.1 must not export any gl* (they live in libGLESv2): {egl_gl:?}"
    );
}
