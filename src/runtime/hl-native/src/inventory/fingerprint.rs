// A content fingerprint over the native C sources.
//
// The value is compiled into the shared object, exported as `cargo:rustc-env`, and therefore
// folded into the Cargo fingerprint of every Rust artifact built against this crate. Two
// silent failures depend on it. The engine is dlopened rather than linked, so a Rust binary's
// bytes are unchanged by nearly every C edit and "the binary did not change" is not evidence
// of a stale build; and a shared object taken from another target directory, feature set, or
// moment runs the previous engine without a word. Threading one value through both makes each
// of those a loud, named failure.
//
// Self-contained on purpose: the Cargo build script and the freshness test both `include!`
// this file, so it must not name anything outside `std`.
//
// 128-bit FNV-1a. This detects accidental staleness and makes no adversarial claim.

/// Hashes every file under `directory`, path and content, in a deterministic order.
///
/// Only the entry names are hashed, never the walk root: the build script starts from a path
/// relative to the package and the freshness test from an absolute one, and both must arrive at
/// the same value. It also keeps the value independent of where the checkout lives, which the
/// reproducibility gate requires.
#[must_use]
pub fn native_fingerprint(directory: &std::path::Path) -> String {
    const OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    let mut state = OFFSET;
    fingerprint_directory(directory, &mut state);
    format!("f{state:032x}")
}

fn fingerprint_directory(directory: &std::path::Path, state: &mut u128) {
    let mut entries = std::fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()))
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|error| panic!("enumerate {}: {error}", directory.display()));
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        fingerprint_bytes(entry.file_name().to_string_lossy().as_bytes(), state);
        if path.is_dir() {
            fingerprint_directory(&path, state);
        } else if path.is_file() {
            let bytes = std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            fingerprint_bytes(&bytes, state);
        }
    }
}

fn fingerprint_bytes(bytes: &[u8], state: &mut u128) {
    const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;
    for byte in bytes.iter().copied().chain([0xff]) {
        *state = (*state ^ u128::from(byte)).wrapping_mul(PRIME);
    }
}
