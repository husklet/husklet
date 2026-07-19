//! The one sha256 helper in `hl-images`. Both the registry (layer/config blob digests) and the build
//! cache (step-key hashing) need sha256; this module owns the value and streaming implementation so those
//! call sites don't each re-derive it. Hashing and gzip decoding run in-process.

use crate::Error;
use sha2::{Digest as _, Sha256};
use std::fmt::{self, Display, Formatter};
use std::io::Read;
use std::path::Path;

/// A computed SHA-256 content digest.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Sha256Digest([u8; 32]);

impl Sha256Digest {
    /// Computes the digest of bytes already in memory.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        Self::from_hasher(hasher)
    }

    /// Finishes an incrementally populated SHA-256 hasher.
    pub(crate) fn from_hasher(hasher: Sha256) -> Self {
        Self(hasher.finalize().into())
    }

    /// Computes the digest of a reader without buffering its complete contents.
    pub(crate) fn read(mut reader: impl Read) -> std::io::Result<Self> {
        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        Ok(Self::from_hasher(hasher))
    }

    /// Computes the digest of a file without reading it wholly into memory.
    pub(crate) fn file(path: &Path) -> Result<Self, Error> {
        let file = std::fs::File::open(path)
            .map_err(|error| Error::Digest(format!("open {}: {error}", path.display())))?;
        Self::read(file).map_err(|error| Error::Digest(format!("read {}: {error}", path.display())))
    }

    /// Computes the digest of the decompressed contents of a gzip file.
    pub(crate) fn gzip_file(path: &Path) -> Result<Self, Error> {
        let file = std::fs::File::open(path)
            .map_err(|error| Error::Digest(format!("open {}: {error}", path.display())))?;
        let decoder = flate2::read::GzDecoder::new(std::io::BufReader::new(file));
        Self::read(decoder)
            .map_err(|error| Error::Digest(format!("gunzip {}: {error}", path.display())))
    }

    /// Encodes the OCI digest representation used by registry descriptors.
    pub fn oci(self) -> String {
        format!("sha256:{self}")
    }
}

impl Display for Sha256Digest {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        // Standard sha256("") and sha256("abc").
        assert_eq!(
            Sha256Digest::from_bytes(b"").to_string(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            Sha256Digest::from_bytes(b"abc").to_string(),
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
            Sha256Digest::file(&raw).unwrap().oci(),
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
        assert_eq!(
            Sha256Digest::gzip_file(&gz).unwrap(),
            Sha256Digest::file(&raw).unwrap()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
