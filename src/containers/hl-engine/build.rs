use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_C_EXECUTION");
    if env::var_os("CARGO_FEATURE_C_EXECUTION").is_none() {
        return;
    }

    let source = PathBuf::from(env::var_os("HL_C_ENGINE_SOURCE").expect("c-execution requires HL_C_ENGINE_SOURCE"));
    let build = PathBuf::from(env::var_os("HL_C_ENGINE_BUILD").expect("c-execution requires HL_C_ENGINE_BUILD"));
    println!("cargo:rerun-if-env-changed=HL_C_ENGINE_SOURCE");
    println!("cargo:rerun-if-env-changed=HL_C_ENGINE_BUILD");
    println!("cargo:rerun-if-changed=c_backend/shim.c");

    cc::Build::new()
        .file("c_backend/shim.c")
        .include(source.join("include"))
        .define("_GNU_SOURCE", None)
        .opt_level(2)
        .compile("hl_c_backend_shim");

    let target = build.join("CMakeFiles/life_target_aarch64.dir/src/core/target/aarch64.c.o");
    let lifecycle = build.join("CMakeFiles/prod_life_aarch64.dir/src/core/lifecycle.c.o");
    println!("cargo:rustc-link-arg={}", target.display());
    println!("cargo:rustc-link-arg={}", lifecycle.display());
    println!("cargo:rustc-link-arg=-Wl,--start-group");
    for library in ["hl-engine", "hl-translator", "hl-linux-abi", "hl-host-linux"] {
        println!(
            "cargo:rustc-link-arg={}",
            build.join(format!("lib{library}.a")).display()
        );
    }
    println!("cargo:rustc-link-arg=-Wl,--end-group");
    println!("cargo:rustc-link-arg=-latomic");
    println!("cargo:rustc-link-arg=-lgcc");
    println!("cargo:rustc-link-arg=-lc");
    for library in ["atomic", "dl", "m", "pthread"] {
        println!("cargo:rustc-link-lib={library}");
    }
}
