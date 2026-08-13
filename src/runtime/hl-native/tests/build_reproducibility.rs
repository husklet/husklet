use std::{
    fmt::Write,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use sha2::{Digest, Sha256};

fn build(workspace: &Path, target: &Path) -> Vec<u8> {
    let status = Command::new(env!("CARGO"))
        .args([
            "build",
            "--release",
            "--offline",
            "--package",
            "hl-native",
            "--manifest-path",
        ])
        .arg(workspace.join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", target)
        .status()
        .expect("run independent native build");
    assert!(status.success(), "independent native build failed: {status}");

    let filename = if cfg!(target_os = "macos") {
        "libhl_native_engine.dylib"
    } else if cfg!(target_os = "windows") {
        "hl_native_engine.dll"
    } else {
        "libhl_native_engine.so"
    };
    let artifact = find_artifact(target, filename).expect("native shared-library artifact");
    fs::read(artifact).expect("read native shared-library artifact")
}

fn find_artifact(directory: &Path, filename: &str) -> Option<PathBuf> {
    let mut entries = fs::read_dir(directory).ok()?.collect::<Result<Vec<_>, _>>().ok()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_artifact(&path, filename) {
                return Some(found);
            }
        } else if entry.file_name() == filename {
            return Some(path);
        }
    }
    None
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().fold(String::with_capacity(64), |mut hash, byte| {
        write!(hash, "{byte:02x}").expect("writing to a string cannot fail");
        hash
    })
}

#[test]
#[ignore = "expensive independent-build gate run by the release workflow"]
fn native_shared_library_is_reproducible_across_out_directories() {
    let package = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = package.ancestors().nth(3).expect("workspace root");
    let temporary = tempfile::tempdir().expect("temporary build root");
    let first = build(workspace, &temporary.path().join("first"));
    let second = build(workspace, &temporary.path().join("second"));
    let first_hash = sha256(&first);
    let second_hash = sha256(&second);
    eprintln!("first={first_hash}\nsecond={second_hash}");
    assert!(
        first == second,
        "native shared library depends on its Cargo OUT_DIR: {first_hash} != {second_hash}"
    );
}
