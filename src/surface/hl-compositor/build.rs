#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};

fn main() {
    #[cfg(target_os = "linux")]
    link_runtime_xkbcommon();
}

/// Some runtime/CI images install `libxkbcommon.so.0` without the unversioned development symlink the
/// linker expects for `-lxkbcommon`. Make that already-installed runtime library discoverable inside
/// Cargo's build output; do not write into the source tree or a system directory.
#[cfg(target_os = "linux")]
fn link_runtime_xkbcommon() {
    let Some(runtime) = runtime_library() else {
        return;
    };
    let out =
        PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR")).join("native");
    std::fs::create_dir_all(&out).expect("create native link directory");
    let link = out.join("libxkbcommon.so");
    if !link.exists() {
        std::os::unix::fs::symlink(&runtime, &link).expect("link installed libxkbcommon runtime");
    }
    println!("cargo:rustc-link-search=native={}", out.display());
}

#[cfg(target_os = "linux")]
fn runtime_library() -> Option<PathBuf> {
    let triples = ["aarch64-linux-gnu", "x86_64-linux-gnu"];
    for root in ["/usr/lib", "/lib"] {
        for triple in triples {
            let candidate = Path::new(root).join(triple).join("libxkbcommon.so.0");
            if candidate.exists() {
                return Some(candidate);
            }
        }
        let candidate = Path::new(root).join("libxkbcommon.so.0");
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}
