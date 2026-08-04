use sha2::Digest as _;
use std::path::{Path, PathBuf};

fn main() {
    let crate_root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let workspace = crate_root.join("../../..");
    let inputs = [
        workspace.join("Cargo.lock"),
        crate_root.join("Cargo.toml"),
        crate_root.join("build.rs"),
        crate_root.join("src/config"),
        crate_root.join("src/runtime"),
        workspace.join("src/containers"),
        workspace.join("src/workspaces/hl-ws"),
    ];
    let mut files = Vec::new();
    for input in inputs {
        collect(&input, &mut files);
    }
    files.sort();

    let mut digest = sha2::Sha256::new();
    for file in files {
        println!("cargo:rerun-if-changed={}", file.display());
        let relative = file.strip_prefix(&workspace).unwrap_or(&file);
        let bytes = std::fs::read(&file).unwrap_or_else(|error| {
            panic!("read runtime identity input {}: {error}", file.display())
        });
        digest.update(
            u64::try_from(relative.as_os_str().as_encoded_bytes().len())
                .expect("runtime identity path length fits u64")
                .to_le_bytes(),
        );
        digest.update(relative.as_os_str().as_encoded_bytes());
        digest.update(
            u64::try_from(bytes.len())
                .expect("runtime identity file length fits u64")
                .to_le_bytes(),
        );
        digest.update(bytes);
    }
    let identity = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    println!("cargo:rustc-env=HUSKLET_RUNTIME_BUILD_ID={identity}");
}

fn collect(path: &Path, output: &mut Vec<PathBuf>) {
    if path.is_file() {
        output.push(path.to_owned());
        return;
    }
    let mut entries = std::fs::read_dir(path)
        .unwrap_or_else(|error| {
            panic!(
                "read runtime identity directory {}: {error}",
                path.display()
            )
        })
        .map(|entry| entry.expect("read runtime identity entry").path())
        .collect::<Vec<_>>();
    entries.sort();
    for entry in entries {
        if entry
            .file_name()
            .is_some_and(|name| matches!(name.to_str(), Some("target" | ".git")))
        {
            continue;
        }
        collect(&entry, output);
    }
}
