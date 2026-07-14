//! hl-display build script: on macOS, compile the small Mach IPC shim (`src/mach_bridge.c`) that
//! receives the engine's IOSurface send-right (GPU rung 2 handle bridge) and link the frameworks it
//! needs. No-op on other platforms (the portable compositor core builds without it).

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=src/mach_bridge.c");
    let on_mac = env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos");
    if !on_mac {
        return;
    }
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let obj = out.join("hl_mach_bridge.o");
    let lib = out.join("libhl_mach_bridge.a");
    let cc = env::var("CC").unwrap_or_else(|_| "cc".into());
    let ar = env::var("AR").unwrap_or_else(|_| "ar".into());
    let ok = Command::new(&cc)
        .args(["-O2", "-Wall", "-c", "src/mach_bridge.c", "-o"])
        .arg(&obj)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
        && Command::new(&ar)
            .arg("rcs")
            .arg(&lib)
            .arg(&obj)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
    if !ok {
        panic!("failed to compile hl-display mach_bridge.c");
    }
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=hl_mach_bridge");
    println!("cargo:rustc-link-lib=framework=IOSurface");
    println!("cargo:rustc-link-lib=framework=CoreFoundation");
}
