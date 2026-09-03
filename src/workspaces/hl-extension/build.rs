mod build_support;

fn main() {
    let manifest = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    println!("cargo:rerun-if-env-changed=HL_EXTENSION_SPEC_GENERATE");
    for path in build_support::watched(&manifest) {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    if std::env::var_os("HL_EXTENSION_SPEC_GENERATE").as_deref() == Some(std::ffi::OsStr::new("1")) {
        return;
    }
    if let Err(error) = build_support::verify(&manifest) {
        panic!(
            "{error}; regenerate with `HL_EXTENSION_SPEC_GENERATE=1 cargo run -p hl-extension --bin hl-extension-spec -- --write`"
        );
    }
}
