//! The one sha256 helper in `hl-images`. Both the registry (layer/config blob digests) and the build
//! cache (step-key hashing) need sha256; this module owns the single implementation so those call sites
//! don't each re-derive it. Hashing is done in-process with the `sha2` crate (already in the workspace
//! lock via `hl-cli`), so we no longer shell out to `sha256sum` — one fewer external tool to depend on.
//! The only subprocess left is `gzip -dc` in [`sha256_gz_file`], and only for *decompression* (there is
//! no in-tree gzip decoder); its stdout is streamed straight into the hasher.

use crate::Error;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;

/// Lowercase-hex sha256 of `bytes`, WITHOUT the `sha256:` prefix. Always 64 hex chars.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex(&h.finalize())
}

/// `sha256:<hex>` of a file's raw contents, streamed (never read whole into memory).
pub(crate) fn sha256_file(path: &Path) -> Result<String, Error> {
    let mut f = std::fs::File::open(path)
        .map_err(|e| Error::Digest(format!("open {}: {e}", path.display())))?;
    let mut h = Sha256::new();
    std::io::copy(&mut f, &mut h)
        .map_err(|e| Error::Digest(format!("read {}: {e}", path.display())))?;
    Ok(prefixed(&h.finalize()))
}

/// `sha256:<hex>` of the DECOMPRESSED contents of a gzip file (an OCI `diff_id`). Decompression runs
/// in-process via `flate2` (pure-Rust miniz_oxide backend), streamed into the hasher — no subprocess.
pub(crate) fn sha256_gz_file(path: &Path) -> Result<String, Error> {
    let f = std::fs::File::open(path)
        .map_err(|e| Error::Digest(format!("open {}: {e}", path.display())))?;
    let mut dec = flate2::read::GzDecoder::new(std::io::BufReader::new(f));
    let mut h = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = dec
            .read(&mut buf)
            .map_err(|e| Error::Digest(format!("gunzip {}: {e}", path.display())))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(prefixed(&h.finalize()))
}

/// `sha256:` + [`hex`] of a raw 32-byte digest.
fn prefixed(digest: &[u8]) -> String {
    format!("sha256:{}", hex(digest))
}

/// Lowercase-hex encode of raw bytes.
fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        s.push(char::from_digit((b & 0x0f) as u32, 16).unwrap());
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        // Standard sha256("") and sha256("abc").
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn file_and_gz() {
        let dir = std::env::temp_dir().join(format!("hl-digest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let raw = dir.join("f");
        std::fs::write(&raw, b"abc").unwrap();
        assert_eq!(
            sha256_file(&raw).unwrap(),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // gzip the file, then confirm the gz helper hashes the DECOMPRESSED bytes.
        let gz = dir.join("f.gz");
        let st = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("gzip -c '{}' > '{}'", raw.display(), gz.display()))
            .status()
            .unwrap();
        assert!(st.success());
        assert_eq!(sha256_gz_file(&gz).unwrap(), sha256_file(&raw).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
