use std::env;

#[path = "src/retained_platform.rs"]
mod retained_platform;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_C_EXECUTION");
    println!("cargo:rustc-check-cfg=cfg(hl_retained_c)");
    println!("cargo:rustc-check-cfg=cfg(hl_retained_c_default)");
    if env::var_os("CARGO_FEATURE_C_EXECUTION").is_none() {
        return;
    }
    let target_os = env::var("CARGO_CFG_TARGET_OS").expect("Cargo supplies CARGO_CFG_TARGET_OS");
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").expect("Cargo supplies CARGO_CFG_TARGET_ARCH");
    if !retained_platform::supported(&target_os, &target_arch) {
        println!(
            "cargo:warning=native C engine unavailable for {target_arch}-{target_os}; production execution is disabled"
        );
        return;
    }
    println!("cargo:rustc-cfg=hl_retained_c");
    if retained_platform::production_default(&target_os, &target_arch) {
        println!("cargo:rustc-cfg=hl_retained_c_default");
    }
}
