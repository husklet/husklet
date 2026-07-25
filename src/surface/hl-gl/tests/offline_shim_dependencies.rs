use std::process::Command;

#[test]
fn nested_shim_dependencies_resolve_offline_with_an_empty_cargo_home() {
    let gl = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = gl.join("../../..");
    let vendor = root.join("third_party/rust/shim-deps");
    let cargo_home =
        std::env::temp_dir().join(format!("hl-offline-shim-cargo-home-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cargo_home);
    std::fs::create_dir(&cargo_home).unwrap();

    for manifest in [
        gl.join("shim/egl/Cargo.toml"),
        root.join("src/surface/hl-vulkan/shim/vulkan/Cargo.toml"),
    ] {
        let output = Command::new(env!("CARGO"))
            .args([
                "metadata",
                "--offline",
                "--format-version",
                "1",
                "--manifest-path",
            ])
            .arg(&manifest)
            .arg("--config")
            .arg("source.crates-io.replace-with=\"vendored-sources\"")
            .arg("--config")
            .arg(format!("source.vendored-sources.directory={vendor:?}"))
            .env("CARGO_HOME", &cargo_home)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{} did not resolve from the checked-in source: {}",
            manifest.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    std::fs::remove_dir_all(cargo_home).unwrap();
}
