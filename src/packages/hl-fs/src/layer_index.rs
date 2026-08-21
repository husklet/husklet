//! A complete name index for one immutable, content-addressed layer tree.
//!
//! An unpacked OCI chain is immutable and named by its chain digest, so a full
//! enumeration of its paths is a durable fact rather than a memo of observed
//! lookups. Absence from a verified index is therefore proof of absence: a
//! negative lookup against the layer resolves with no syscalls and no staleness
//! risk. The producer (image unpack) and the consumer (the path resolver) live
//! in crates that do not depend on each other, so the format lives here.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use sha2::{Digest as _, Sha256};

const MAGIC: &[u8; 8] = b"HLLIDX01";
const DIGEST_LEN: usize = 32;
/// Refuse absurd sidecars rather than allocating from a corrupt length field.
const MAX_BYTES: u64 = 512 * 1024 * 1024;

/// Flags describing facts that hold for the whole layer.
const FLAG_HAS_WHITEOUTS: u8 = 1 << 0;
const FLAG_HAS_OPAQUE: u8 = 1 << 1;

/// The node kinds a layer entry can have, encoded stably on disk.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Kind {
    File = 0,
    Directory = 1,
    Symlink = 2,
    Fifo = 3,
    Socket = 4,
    BlockDevice = 5,
    CharDevice = 6,
    Unknown = 7,
}

impl Kind {
    const fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            0 => Self::File,
            1 => Self::Directory,
            2 => Self::Symlink,
            3 => Self::Fifo,
            4 => Self::Socket,
            5 => Self::BlockDevice,
            6 => Self::CharDevice,
            7 => Self::Unknown,
            _ => return None,
        })
    }

    #[cfg(unix)]
    const fn of(mode: u32) -> Self {
        match mode & 0o170_000 {
            0o100_000 => Self::File,
            0o040_000 => Self::Directory,
            0o120_000 => Self::Symlink,
            0o010_000 => Self::Fifo,
            0o140_000 => Self::Socket,
            0o060_000 => Self::BlockDevice,
            0o020_000 => Self::CharDevice,
            _ => Self::Unknown,
        }
    }
}

/// One indexed name and the metadata a `stat` of it would report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    pub kind: Kind,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub size: u64,
    pub mtime_seconds: i64,
    pub mtime_nanoseconds: u32,
    /// Populated only for symlinks.
    pub link: Vec<u8>,
    /// This name is an overlay whiteout victim recorded inside this layer.
    pub whiteout: bool,
    /// This directory carries an overlay opaque marker inside this layer.
    pub opaque: bool,
}

/// A complete enumeration of one layer's names, keyed by layer-relative path.
///
/// Keys carry no leading separator: `usr/lib/libc.so`, never `/usr/lib/libc.so`.
#[derive(Clone, Debug, Default)]
pub struct LayerIndex {
    entries: BTreeMap<Vec<u8>, Entry>,
    has_whiteouts: bool,
    has_opaque: bool,
}

impl LayerIndex {
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether any name in this layer is an overlay whiteout or opaque marker.
    ///
    /// Unpacked OCI chains resolve every marker by deletion, so this is false
    /// for them and the resolver can skip both marker probes outright.
    #[must_use]
    pub const fn has_markers(&self) -> bool {
        self.has_whiteouts || self.has_opaque
    }

    /// Look a layer-relative path up. `None` is proof the layer has no such name.
    #[must_use]
    pub fn get(&self, path: &[u8]) -> Option<&Entry> {
        self.entries.get(path)
    }

    /// Whether the layer whites this layer-relative path out.
    #[must_use]
    pub fn is_whiteout(&self, path: &[u8]) -> bool {
        self.has_whiteouts && self.entries.get(path).is_some_and(|entry| entry.whiteout)
    }

    /// Whether the layer marks this layer-relative directory opaque.
    #[must_use]
    pub fn is_opaque(&self, path: &[u8]) -> bool {
        self.has_opaque && self.entries.get(path).is_some_and(|entry| entry.opaque)
    }

    /// Walk a materialized layer tree and record every name it contains.
    ///
    /// # Errors
    /// Returns an error when the tree cannot be traversed or read.
    #[cfg(unix)]
    pub fn build(root: &Path) -> io::Result<Self> {
        let mut index = Self::default();
        index.walk(root, &[])?;
        Ok(index)
    }

    /// Refuses, naming the metadata this host does not carry.
    ///
    /// The entry point stays so that it and its consumers are one width -- `hl-images`'
    /// `snapshot::index::publish` calls it ungated -- but it cannot answer here. Every entry the
    /// Unix walk records is a POSIX `mode`, `uid`, `gid` and `mtime` read through `MetadataExt`,
    /// and those fields are the whole comparison an overlay diff makes. An index built without
    /// them would answer a lookup with a wrong entry rather than with a miss, and the sidecar's
    /// contract is that a miss is never a wrong answer.
    ///
    /// # Errors
    /// Always [`io::ErrorKind::Unsupported`].
    #[cfg(not(unix))]
    pub fn build(root: &Path) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "cannot index {}: a layer index is a POSIX enumeration -- mode, uid, gid and mtime per \
                 name -- and this host's metadata carries none of them",
                root.display()
            ),
        ))
    }

    #[cfg(unix)]
    fn walk(&mut self, directory: &Path, prefix: &[u8]) -> io::Result<()> {
        use std::os::unix::ffi::OsStrExt as _;
        use std::os::unix::fs::MetadataExt as _;

        let mut children = Vec::new();
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.as_bytes().to_vec();
            let mut path = prefix.to_vec();
            if !path.is_empty() {
                path.push(b'/');
            }
            path.extend_from_slice(&name);
            let metadata = entry.path().symlink_metadata()?;
            let kind = Kind::of(metadata.mode());
            let link = if kind == Kind::Symlink {
                std::fs::read_link(entry.path())?.as_os_str().as_bytes().to_vec()
            } else {
                Vec::new()
            };
            // Overlay markers name a victim in the same directory; record the
            // victim's name rather than the marker's so a lookup finds it.
            let (key, whiteout, opaque) = if name == b".wh..wh..opq" {
                (prefix.to_vec(), false, true)
            } else if let Some(victim) = name.strip_prefix(b".wh.".as_slice()).filter(|it| !it.is_empty()) {
                let mut key = prefix.to_vec();
                if !key.is_empty() {
                    key.push(b'/');
                }
                key.extend_from_slice(victim);
                (key, true, false)
            } else {
                (path.clone(), false, false)
            };
            self.has_whiteouts |= whiteout;
            self.has_opaque |= opaque;
            let existing = self.entries.entry(key).or_insert(Entry {
                kind,
                mode: metadata.mode(),
                uid: metadata.uid(),
                gid: metadata.gid(),
                size: metadata.size(),
                mtime_seconds: metadata.mtime(),
                mtime_nanoseconds: u32::try_from(metadata.mtime_nsec().max(0)).unwrap_or(0),
                link,
                whiteout,
                opaque,
            });
            existing.whiteout |= whiteout;
            existing.opaque |= opaque;
            if kind == Kind::Directory {
                children.push((entry.path(), path));
            }
        }
        for (path, prefix) in children {
            self.walk(&path, &prefix)?;
        }
        Ok(())
    }

    /// Encode the index with a trailing SHA-256 over its own body.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(64 * self.entries.len() + 32);
        bytes.extend_from_slice(MAGIC);
        let mut flags = 0u8;
        if self.has_whiteouts {
            flags |= FLAG_HAS_WHITEOUTS;
        }
        if self.has_opaque {
            flags |= FLAG_HAS_OPAQUE;
        }
        bytes.push(flags);
        bytes.extend_from_slice(&[0, 0, 0]);
        bytes.extend_from_slice(&(self.entries.len() as u64).to_le_bytes());
        for (path, entry) in &self.entries {
            bytes.extend_from_slice(&(path.len() as u16).to_le_bytes());
            bytes.extend_from_slice(path);
            bytes.push(entry.kind as u8);
            bytes.push(u8::from(entry.whiteout) | (u8::from(entry.opaque) << 1));
            bytes.extend_from_slice(&entry.mode.to_le_bytes());
            bytes.extend_from_slice(&entry.uid.to_le_bytes());
            bytes.extend_from_slice(&entry.gid.to_le_bytes());
            bytes.extend_from_slice(&entry.size.to_le_bytes());
            bytes.extend_from_slice(&entry.mtime_seconds.to_le_bytes());
            bytes.extend_from_slice(&entry.mtime_nanoseconds.to_le_bytes());
            bytes.extend_from_slice(&(entry.link.len() as u16).to_le_bytes());
            bytes.extend_from_slice(&entry.link);
        }
        let digest = <[u8; DIGEST_LEN]>::from(Sha256::digest(&bytes));
        bytes.extend_from_slice(&digest);
        bytes
    }

    /// Decode an index, rejecting any body whose trailing digest does not match.
    ///
    /// # Errors
    /// Returns `InvalidData` for a bad magic, a digest mismatch, or a truncated body.
    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        let invalid = |reason: &str| io::Error::new(io::ErrorKind::InvalidData, format!("layer index: {reason}"));
        if bytes.len() < 20 + DIGEST_LEN {
            return Err(invalid("truncated header"));
        }
        let (body, digest) = bytes.split_at(bytes.len() - DIGEST_LEN);
        if <[u8; DIGEST_LEN]>::from(Sha256::digest(body)) != digest {
            return Err(invalid("digest mismatch"));
        }
        if &body[..8] != MAGIC {
            return Err(invalid("unknown magic"));
        }
        let flags = body[8];
        let count = u64::from_le_bytes(body[12..20].try_into().map_err(|_| invalid("bad count"))?);
        let mut cursor = Cursor {
            bytes: &body[20..],
            offset: 0,
        };
        let mut entries = BTreeMap::new();
        for _ in 0..count {
            let path = cursor.bytes16().ok_or_else(|| invalid("truncated path"))?.to_vec();
            let kind = Kind::from_byte(cursor.byte().ok_or_else(|| invalid("truncated kind"))?)
                .ok_or_else(|| invalid("unknown kind"))?;
            let markers = cursor.byte().ok_or_else(|| invalid("truncated markers"))?;
            let entry = Entry {
                kind,
                mode: cursor.u32().ok_or_else(|| invalid("truncated mode"))?,
                uid: cursor.u32().ok_or_else(|| invalid("truncated uid"))?,
                gid: cursor.u32().ok_or_else(|| invalid("truncated gid"))?,
                size: cursor.u64().ok_or_else(|| invalid("truncated size"))?,
                mtime_seconds: cursor.u64().ok_or_else(|| invalid("truncated mtime"))? as i64,
                mtime_nanoseconds: cursor.u32().ok_or_else(|| invalid("truncated mtime nanos"))?,
                link: cursor.bytes16().ok_or_else(|| invalid("truncated link"))?.to_vec(),
                whiteout: markers & 1 != 0,
                opaque: markers & 2 != 0,
            };
            entries.insert(path, entry);
        }
        if !cursor.finished() {
            return Err(invalid("trailing bytes"));
        }
        Ok(Self {
            entries,
            has_whiteouts: flags & FLAG_HAS_WHITEOUTS != 0,
            has_opaque: flags & FLAG_HAS_OPAQUE != 0,
        })
    }

    /// Read and verify an index sidecar.
    ///
    /// # Errors
    /// Returns an error when the sidecar is missing, oversized, or fails verification.
    pub fn load(path: &Path) -> io::Result<Self> {
        let metadata = std::fs::metadata(path)?;
        if metadata.len() > MAX_BYTES {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "layer index: oversized"));
        }
        Self::decode(&std::fs::read(path)?)
    }
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl Cursor<'_> {
    fn finished(&self) -> bool {
        self.offset == self.bytes.len()
    }
    fn take(&mut self, count: usize) -> Option<&[u8]> {
        let end = self.offset.checked_add(count)?;
        let slice = self.bytes.get(self.offset..end)?;
        self.offset = end;
        Some(slice)
    }
    fn byte(&mut self) -> Option<u8> {
        self.take(1).map(|slice| slice[0])
    }
    fn u32(&mut self) -> Option<u32> {
        self.take(4)
            .and_then(|slice| slice.try_into().ok())
            .map(u32::from_le_bytes)
    }
    fn u64(&mut self) -> Option<u64> {
        self.take(8)
            .and_then(|slice| slice.try_into().ok())
            .map(u64::from_le_bytes)
    }
    fn bytes16(&mut self) -> Option<&[u8]> {
        let length = self
            .take(2)
            .and_then(|slice| slice.try_into().ok())
            .map(u16::from_le_bytes)?;
        // Reborrow so the returned slice does not hold the cursor mutably.
        let end = self.offset.checked_add(usize::from(length))?;
        let slice = self.bytes.get(self.offset..end)?;
        self.offset = end;
        Some(slice)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::{Kind, LayerIndex};

    fn fixture() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("usr/lib")).unwrap();
        std::fs::write(root.path().join("usr/lib/libc.so"), b"content").unwrap();
        std::os::unix::fs::symlink("libc.so", root.path().join("usr/lib/libc.so.6")).unwrap();
        root
    }

    #[test]
    fn build_enumerates_every_name_and_records_kinds() {
        let root = fixture();
        let index = LayerIndex::build(root.path()).unwrap();
        assert_eq!(index.len(), 4);
        assert_eq!(index.get(b"usr").unwrap().kind, Kind::Directory);
        assert_eq!(index.get(b"usr/lib/libc.so").unwrap().kind, Kind::File);
        assert_eq!(index.get(b"usr/lib/libc.so").unwrap().size, 7);
        let link = index.get(b"usr/lib/libc.so.6").unwrap();
        assert_eq!(link.kind, Kind::Symlink);
        assert_eq!(link.link, b"libc.so");
        assert!(!index.has_markers());
    }

    #[test]
    fn absence_from_the_index_is_reported_as_absence() {
        let index = LayerIndex::build(fixture().path()).unwrap();
        assert!(index.get(b"usr/lib/absent.so").is_none());
        assert!(index.get(b"etc/ld.so.cache").is_none());
        assert!(index.get(b"/usr").is_none(), "keys carry no leading separator");
    }

    #[test]
    fn round_trip_preserves_entries_and_marker_flags() {
        let root = fixture();
        std::fs::write(root.path().join("usr/.wh.gone"), b"").unwrap();
        std::fs::write(root.path().join("usr/lib/.wh..wh..opq"), b"").unwrap();
        let index = LayerIndex::build(root.path()).unwrap();
        assert!(index.has_markers());
        assert!(index.is_whiteout(b"usr/gone"));
        assert!(index.is_opaque(b"usr/lib"));

        let decoded = LayerIndex::decode(&index.encode()).unwrap();
        assert!(decoded.is_whiteout(b"usr/gone"));
        assert!(decoded.is_opaque(b"usr/lib"));
        assert_eq!(decoded.len(), index.len());
        assert_eq!(decoded.get(b"usr/lib/libc.so.6"), index.get(b"usr/lib/libc.so.6"));
    }

    #[test]
    fn a_corrupted_body_fails_verification_rather_than_decoding() {
        let index = LayerIndex::build(fixture().path()).unwrap();
        let mut bytes = index.encode();
        let middle = bytes.len() / 2;
        bytes[middle] ^= 0xff;
        assert!(LayerIndex::decode(&bytes).is_err());

        let mut truncated = index.encode();
        truncated.truncate(truncated.len() - 8);
        assert!(LayerIndex::decode(&truncated).is_err());
        assert!(LayerIndex::decode(b"").is_err());
    }
}
