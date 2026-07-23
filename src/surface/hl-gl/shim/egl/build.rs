//! Generates the guest `libEGL.so.1` and `libGLESv2.so.2` shim entry-point surface.

#[path = "build/binding.rs"]
mod binding;
#[path = "build/census.rs"]
mod census;
#[path = "build/implemented.rs"]
mod implemented;
#[path = "build/staging.rs"]
mod staging;

use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(BuildEnvironment::get("CARGO_MANIFEST_DIR"))
        .join("registry/gles2_egl.manifest");
    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rustc-check-cfg=cfg(gles_client)");

    let text = std::fs::read_to_string(&manifest)
        .unwrap_or_else(|error| panic!("read {}: {error}", manifest.display()));

    staging::emit_role_link_args();

    let output = census::Census::generate(&text);
    let path = PathBuf::from(BuildEnvironment::get("OUT_DIR")).join("generated_entrypoints.rs");
    std::fs::write(path, output).unwrap();
}

struct BuildEnvironment;

impl BuildEnvironment {
    fn get(name: &str) -> String {
        std::env::var(name).unwrap_or_else(|_| panic!("env {name} not set"))
    }
}
