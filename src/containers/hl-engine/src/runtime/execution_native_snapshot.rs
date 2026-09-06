//! Stopped-process architectural capture for the staged native x86 checkpoint reader.
//!
//! Publication remains behind the native checkpoint eligibility gate.  The transaction here is the
//! complete, fail-closed publication primitive that gate will call once the lifecycle is admitted.
#![allow(dead_code)]

use crate::composition::{CheckpointSink, CompositionError};
use sha2::{Digest as _, Sha256};
use std::io;
#[cfg(target_os = "linux")]
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};
#[cfg(target_arch = "x86_64")]
use std::time::{Duration, Instant};

const MAGIC: &[u8; 8] = b"HLNXREG\0";
const VERSION: u16 = 1;
const ELF_MACHINE_X86_64: u16 = 62;
pub(super) const RECORD_SIZE: usize = 256;
const REGISTER_COUNT: usize = 27;
const REGISTER_OFFSET: usize = 32;
const MEMORY_MAGIC: &[u8; 8] = b"HLNXMEM\0";
const MEMORY_VERSION: u16 = 2;
const MEMORY_HEADER_SIZE: usize = 32;
const MEMORY_ENTRY_SIZE: usize = 96;
const MAX_MAPPINGS: usize = 4096;
const MAX_CAPTURE_BYTES: usize = 1 << 30;
const MAX_PATH_BYTES: usize = 4096;
#[cfg(target_arch = "x86_64")]
const STOP_DEADLINE: Duration = Duration::from_secs(2);

pub(super) const REGISTER_OBJECT: &str = "native/registers.x86-v1";
pub(super) const MEMORY_OBJECT: &str = "native/memory.x86-v1";
const MANIFEST_MAGIC: &[u8; 16] = b"HLNATIVE-X86-V1\0";
const MANIFEST_SIZE: usize = 160;

fn native_manifest(registers: &[u8], memory: &[u8]) -> Vec<u8> {
    let mut out = vec![0; MANIFEST_SIZE];
    out[..16].copy_from_slice(MANIFEST_MAGIC);
    out[16..48].copy_from_slice(&object_name(REGISTER_OBJECT));
    out[48..56].copy_from_slice(&(registers.len() as u64).to_le_bytes());
    out[56..88].copy_from_slice(&Sha256::digest(registers));
    out[88..120].copy_from_slice(&object_name(MEMORY_OBJECT));
    out[120..128].copy_from_slice(&(memory.len() as u64).to_le_bytes());
    out[128..160].copy_from_slice(&Sha256::digest(memory));
    out
}

fn object_name(name: &str) -> [u8; 32] {
    let mut field = [0; 32];
    field[..name.len()].copy_from_slice(name.as_bytes());
    field
}

pub(super) fn validate_native_objects(
    manifest: &[u8],
    object: impl Fn(&str) -> Option<Vec<u8>>,
) -> Result<(), InvalidNativeImage> {
    if manifest.len() != MANIFEST_SIZE
        || &manifest[..16] != MANIFEST_MAGIC
        || manifest[16..48] != object_name(REGISTER_OBJECT)
        || manifest[88..120] != object_name(MEMORY_OBJECT)
    {
        return Err(InvalidNativeImage::Manifest);
    }
    let registers = object(REGISTER_OBJECT).ok_or(InvalidNativeImage::Missing)?;
    let memory = object(MEMORY_OBJECT).ok_or(InvalidNativeImage::Missing)?;
    for (size_at, digest_at, bytes) in [(48, 56, registers.as_slice()), (120, 128, memory.as_slice())] {
        let size = u64::from_le_bytes(manifest[size_at..size_at + 8].try_into().expect("manifest field"));
        if size != bytes.len() as u64 || manifest[digest_at..digest_at + 32] != Sha256::digest(bytes)[..] {
            return Err(InvalidNativeImage::Digest);
        }
    }
    X86RegisterRecord::decode(&registers).map_err(|_| InvalidNativeImage::Registers)?;
    NativeMemoryImage::decode(&memory).map_err(|_| InvalidNativeImage::Memory)?;
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum InvalidNativeImage {
    Manifest,
    Missing,
    Digest,
    Registers,
    Memory,
}

/// Atomically stages a complete stopped-process image.  `commit_until` is the only publication
/// point; every earlier failure best-effort aborts the owned transaction before returning.
#[cfg(target_os = "linux")]
pub(super) fn publish_stopped_native(
    sink: &dyn CheckpointSink,
    pid: libc::pid_t,
    deadline: Instant,
) -> Result<(), CompositionError> {
    if Instant::now() >= deadline {
        return Err(CompositionError::DeadlineExceeded);
    }
    let transaction = sink.begin_until(deadline)?;
    struct Thaw(libc::pid_t);
    impl Drop for Thaw {
        fn drop(&mut self) {
            unsafe {
                libc::kill(self.0, libc::SIGCONT);
            }
        }
    }
    let _thaw = Thaw(pid);
    let result = (|| {
        let registers = capture(pid)
            .map_err(|_| CompositionError::RuntimeConstruction)?
            .encode();
        let memory = capture_stopped_memory(pid, deadline)
            .and_then(|image| {
                image
                    .encode()
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, format!("{error:?}")))
            })
            .map_err(|error| {
                if error.kind() == io::ErrorKind::TimedOut {
                    CompositionError::DeadlineExceeded
                } else {
                    CompositionError::RuntimeConstruction
                }
            })?;
        if Instant::now() >= deadline {
            return Err(CompositionError::DeadlineExceeded);
        }
        let manifest = native_manifest(&registers, &memory);
        validate_native_objects(&manifest, |name| match name {
            REGISTER_OBJECT => Some(registers.to_vec()),
            MEMORY_OBJECT => Some(memory.clone()),
            _ => None,
        })
        .map_err(|_| CompositionError::RuntimeConstruction)?;
        sink.put_until(transaction, REGISTER_OBJECT, &registers, deadline)?;
        sink.put_until(transaction, MEMORY_OBJECT, &memory, deadline)?;
        sink.put_until(
            transaction,
            crate::runtime::checkpoint::image_envelope::OBJECT,
            &crate::runtime::checkpoint::image_envelope::Reader::NativeX86V1.encode(),
            deadline,
        )?;
        sink.commit_until(transaction, &manifest, deadline)
    })();
    if result.is_err() {
        let _ = sink.abort_until(transaction, deadline);
    }
    result
}

/// Canonical `native-x86-v1` architectural state. Register order is Linux x86-64
/// `user_regs_struct`: r15..gs, exactly as returned by `NT_PRSTATUS`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct X86RegisterRecord {
    pub(super) signal_mask: u64,
    pub(super) registers: [u64; REGISTER_COUNT],
}

/// One canonical Linux VMA. File mappings name immutable input below the
/// process root; anonymous private mappings instead own `bytes` exactly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct NativeMapping {
    pub(super) start: u64,
    pub(super) end: u64,
    pub(super) offset: u64,
    pub(super) protection: u8,
    pub(super) device_major: u32,
    pub(super) device_minor: u32,
    pub(super) inode: u64,
    pub(super) kernel_special: bool,
    pub(super) root_relative: Option<Vec<u8>>,
    pub(super) file_digest: Option<[u8; 32]>,
    pub(super) bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct NativeMemoryImage {
    pub(super) mappings: Vec<NativeMapping>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum InvalidMemoryImage {
    Size,
    Magic,
    Version,
    Reserved,
    Count,
    Overflow,
    Order,
    Protection,
    Kind,
    Path,
    Payload,
}

impl NativeMemoryImage {
    pub(super) fn encode(&self) -> Result<Vec<u8>, InvalidMemoryImage> {
        if self.mappings.len() > MAX_MAPPINGS {
            return Err(InvalidMemoryImage::Count);
        }
        validate_mappings(&self.mappings)?;
        let variable = self.mappings.iter().try_fold(0usize, |total, mapping| {
            total
                .checked_add(mapping.root_relative.as_ref().map_or(0, Vec::len))
                .and_then(|n| n.checked_add(mapping.bytes.len()))
                .ok_or(InvalidMemoryImage::Overflow)
        })?;
        let fixed = self
            .mappings
            .len()
            .checked_mul(MEMORY_ENTRY_SIZE)
            .and_then(|n| n.checked_add(MEMORY_HEADER_SIZE))
            .ok_or(InvalidMemoryImage::Overflow)?;
        let total = fixed.checked_add(variable).ok_or(InvalidMemoryImage::Overflow)?;
        if variable > MAX_CAPTURE_BYTES || total > u32::MAX as usize {
            return Err(InvalidMemoryImage::Overflow);
        }
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(MEMORY_MAGIC);
        out.extend_from_slice(&MEMORY_VERSION.to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes());
        out.extend_from_slice(&(self.mappings.len() as u32).to_le_bytes());
        out.extend_from_slice(&(total as u64).to_le_bytes());
        out.extend_from_slice(&[0; 8]);
        for mapping in &self.mappings {
            let path_len = mapping.root_relative.as_ref().map_or(0, Vec::len);
            out.extend_from_slice(&mapping.start.to_le_bytes());
            out.extend_from_slice(&mapping.end.to_le_bytes());
            out.extend_from_slice(&mapping.offset.to_le_bytes());
            out.extend_from_slice(&mapping.inode.to_le_bytes());
            out.extend_from_slice(&mapping.device_major.to_le_bytes());
            out.extend_from_slice(&mapping.device_minor.to_le_bytes());
            out.push(mapping.protection);
            out.push(if mapping.kernel_special {
                2
            } else {
                u8::from(mapping.root_relative.is_some())
            });
            out.extend_from_slice(&[0; 6]);
            out.extend_from_slice(&(path_len as u32).to_le_bytes());
            out.extend_from_slice(&(mapping.bytes.len() as u64).to_le_bytes());
            out.extend_from_slice(&[0; 4]);
            out.extend_from_slice(mapping.file_digest.as_ref().unwrap_or(&[0; 32]));
        }
        for mapping in &self.mappings {
            if let Some(path) = &mapping.root_relative {
                out.extend_from_slice(path);
            }
            out.extend_from_slice(&mapping.bytes);
        }
        Ok(out)
    }

    pub(super) fn decode(input: &[u8]) -> Result<Self, InvalidMemoryImage> {
        if input.len() < MEMORY_HEADER_SIZE {
            return Err(InvalidMemoryImage::Size);
        }
        if &input[..8] != MEMORY_MAGIC {
            return Err(InvalidMemoryImage::Magic);
        }
        if le_u16(input, 8)? != MEMORY_VERSION {
            return Err(InvalidMemoryImage::Version);
        }
        if le_u16(input, 10)? != 0 || input[24..32].iter().any(|byte| *byte != 0) {
            return Err(InvalidMemoryImage::Reserved);
        }
        let count = usize::try_from(le_u32(input, 12)?).map_err(|_| InvalidMemoryImage::Count)?;
        if count > MAX_MAPPINGS {
            return Err(InvalidMemoryImage::Count);
        }
        let declared = usize::try_from(le_u64(input, 16)?).map_err(|_| InvalidMemoryImage::Overflow)?;
        if declared != input.len() {
            return Err(InvalidMemoryImage::Size);
        }
        let fixed = count
            .checked_mul(MEMORY_ENTRY_SIZE)
            .and_then(|n| n.checked_add(MEMORY_HEADER_SIZE))
            .ok_or(InvalidMemoryImage::Overflow)?;
        if fixed > input.len() {
            return Err(InvalidMemoryImage::Size);
        }
        let mut cursor = fixed;
        let mut mappings = Vec::with_capacity(count);
        for index in 0..count {
            let at = MEMORY_HEADER_SIZE + index * MEMORY_ENTRY_SIZE;
            if input[at + 42..at + 48]
                .iter()
                .chain(&input[at + 60..at + 64])
                .any(|byte| *byte != 0)
            {
                return Err(InvalidMemoryImage::Reserved);
            }
            let kind = input[at + 41];
            if kind > 2 {
                return Err(InvalidMemoryImage::Kind);
            }
            let path_len = usize::try_from(le_u32(input, at + 48)?).map_err(|_| InvalidMemoryImage::Overflow)?;
            let byte_len = usize::try_from(le_u64(input, at + 52)?).map_err(|_| InvalidMemoryImage::Overflow)?;
            let next = cursor
                .checked_add(path_len)
                .and_then(|n| n.checked_add(byte_len))
                .ok_or(InvalidMemoryImage::Overflow)?;
            if next > input.len() {
                return Err(InvalidMemoryImage::Size);
            }
            let root_relative = (kind == 1).then(|| input[cursor..cursor + path_len].to_vec());
            if kind != 1 && path_len != 0 {
                return Err(InvalidMemoryImage::Kind);
            }
            cursor += path_len;
            let bytes = input[cursor..cursor + byte_len].to_vec();
            cursor += byte_len;
            mappings.push(NativeMapping {
                start: le_u64(input, at)?,
                end: le_u64(input, at + 8)?,
                offset: le_u64(input, at + 16)?,
                inode: le_u64(input, at + 24)?,
                device_major: le_u32(input, at + 32)?,
                device_minor: le_u32(input, at + 36)?,
                protection: input[at + 40],
                kernel_special: kind == 2,
                root_relative,
                file_digest: (kind == 1).then(|| input[at + 64..at + 96].try_into().expect("digest field")),
                bytes,
            });
        }
        if cursor != input.len() {
            return Err(InvalidMemoryImage::Payload);
        }
        validate_mappings(&mappings)?;
        Ok(Self { mappings })
    }
}

fn validate_mappings(mappings: &[NativeMapping]) -> Result<(), InvalidMemoryImage> {
    let mut previous_end = 0;
    let mut copied = 0usize;
    for mapping in mappings {
        let length = mapping
            .end
            .checked_sub(mapping.start)
            .ok_or(InvalidMemoryImage::Order)?;
        if length == 0 || mapping.start < previous_end {
            return Err(InvalidMemoryImage::Order);
        }
        previous_end = mapping.end;
        if mapping.protection & !7 != 0 {
            return Err(InvalidMemoryImage::Protection);
        }
        if mapping.kernel_special {
            if mapping.root_relative.is_some()
                || !mapping.bytes.is_empty()
                || mapping.offset != 0
                || mapping.inode != 0
                || mapping.device_major != 0
                || mapping.device_minor != 0
                || mapping.file_digest.is_some()
            {
                return Err(InvalidMemoryImage::Kind);
            }
            continue;
        }
        match &mapping.root_relative {
            Some(path) => {
                if !canonical_relative(path)
                    || path.len() > MAX_PATH_BYTES
                    || path.contains(&0)
                    || !mapping.bytes.is_empty()
                    || mapping.file_digest.is_none()
                {
                    return Err(InvalidMemoryImage::Path);
                }
            }
            None => {
                if mapping.offset != 0
                    || mapping.inode != 0
                    || mapping.device_major != 0
                    || mapping.device_minor != 0
                    || mapping.file_digest.is_some()
                {
                    return Err(InvalidMemoryImage::Kind);
                }
                if mapping.bytes.len() as u64 != length {
                    return Err(InvalidMemoryImage::Payload);
                }
                copied = copied
                    .checked_add(mapping.bytes.len())
                    .ok_or(InvalidMemoryImage::Overflow)?;
            }
        }
    }
    if copied > MAX_CAPTURE_BYTES {
        return Err(InvalidMemoryImage::Overflow);
    }
    Ok(())
}

fn canonical_relative(path: &[u8]) -> bool {
    !path.is_empty()
        && path[0] != b'/'
        && path
            .split(|byte| *byte == b'/')
            .all(|part| !part.is_empty() && part != b"." && part != b"..")
}

fn le_u16(bytes: &[u8], at: usize) -> Result<u16, InvalidMemoryImage> {
    Ok(u16::from_le_bytes(
        bytes
            .get(at..at + 2)
            .ok_or(InvalidMemoryImage::Size)?
            .try_into()
            .expect("field"),
    ))
}
fn le_u32(bytes: &[u8], at: usize) -> Result<u32, InvalidMemoryImage> {
    Ok(u32::from_le_bytes(
        bytes
            .get(at..at + 4)
            .ok_or(InvalidMemoryImage::Size)?
            .try_into()
            .expect("field"),
    ))
}
fn le_u64(bytes: &[u8], at: usize) -> Result<u64, InvalidMemoryImage> {
    Ok(u64::from_le_bytes(
        bytes
            .get(at..at + 8)
            .ok_or(InvalidMemoryImage::Size)?
            .try_into()
            .expect("field"),
    ))
}

/// Captures the stable VMA view of an already-admitted, already-stopped process.
/// It neither attaches nor resumes the process, so stop and cleanup ownership stay
/// with the coordinator that admitted it.
#[cfg(target_os = "linux")]
pub(super) fn capture_stopped_memory(pid: libc::pid_t, deadline: Instant) -> io::Result<NativeMemoryImage> {
    if pid <= 1 || pid == unsafe { libc::getpid() } || !process_is_stopped(pid)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "memory capture requires another stopped process",
        ));
    }
    check_deadline(deadline)?;
    let maps_path = format!("/proc/{pid}/maps");
    let before = std::fs::read(&maps_path)?;
    if before.len() > MAX_MAPPINGS * MAX_PATH_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "process map table exceeds bound",
        ));
    }
    let root = PathBuf::from(format!("/proc/{pid}/root"));
    let specifications = parse_maps(&before, &root, deadline)?;
    let memory = std::fs::File::open(format!("/proc/{pid}/mem"))?;
    let mut copied = 0usize;
    let mut mappings = Vec::with_capacity(specifications.len());
    for mut mapping in specifications {
        check_deadline(deadline)?;
        if !mapping.kernel_special && mapping.root_relative.is_none() {
            let length = usize::try_from(mapping.end - mapping.start)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "mapping length exceeds host"))?;
            copied = copied
                .checked_add(length)
                .filter(|total| *total <= MAX_CAPTURE_BYTES)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "anonymous memory exceeds capture bound"))?;
            mapping.bytes = read_process_mem_exact(&memory, mapping.start, length, deadline)?;
        }
        mappings.push(mapping);
    }
    check_deadline(deadline)?;
    revalidate_file_mappings(&mappings, &root, deadline)?;
    ensure_same_maps(&before, &std::fs::read(maps_path)?)?;
    validate_mappings(&mappings)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, format!("invalid VMA table: {error:?}")))?;
    Ok(NativeMemoryImage { mappings })
}

#[cfg(target_os = "linux")]
fn revalidate_file_mappings(mappings: &[NativeMapping], root: &Path, deadline: Instant) -> io::Result<()> {
    for mapping in mappings {
        let (Some(path), Some(expected)) = (&mapping.root_relative, mapping.file_digest) else {
            continue;
        };
        check_deadline(deadline)?;
        let path = std::str::from_utf8(path)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "non-UTF-8 mapping path"))?;
        let file = std::fs::File::open(root.join(path))?;
        let metadata = file.metadata()?;
        if metadata.ino() != mapping.inode
            || libc::major(metadata.dev()) as u32 != mapping.device_major
            || libc::minor(metadata.dev()) as u32 != mapping.device_minor
            || hash_file_range(&file, mapping.offset, mapping.end - mapping.start, deadline)? != expected
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "file mapping content identity changed",
            ));
        }
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub(super) fn capture_stopped_memory(
    _pid: libc::pid_t,
    _deadline: std::time::Instant,
) -> io::Result<NativeMemoryImage> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "native memory capture requires Linux",
    ))
}

#[cfg(target_os = "linux")]
fn parse_maps(input: &[u8], root: &Path, deadline: Instant) -> io::Result<Vec<NativeMapping>> {
    let text =
        std::str::from_utf8(input).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "non-UTF-8 process maps"))?;
    let mut mappings = Vec::new();
    for line in text.lines() {
        if mappings.len() == MAX_MAPPINGS {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "too many process mappings"));
        }
        // Linux escapes whitespace in map pathnames, so tokenizing all fields is
        // lossless and avoids `splitn` consuming its limit on alignment spaces.
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() < 5 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "malformed process map"));
        }
        let (start, end) = fields[0]
            .split_once('-')
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "malformed mapping range"))?;
        let start = u64::from_str_radix(start, 16)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad mapping start"))?;
        let end =
            u64::from_str_radix(end, 16).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad mapping end"))?;
        let perms = fields[1].as_bytes();
        if perms.len() != 4 || !matches!(perms[3], b'p') {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "shared or malformed mapping is not capturable",
            ));
        }
        let protection =
            u8::from(perms[0] == b'r') | (u8::from(perms[1] == b'w') << 1) | (u8::from(perms[2] == b'x') << 2);
        if !matches!(perms[0], b'r' | b'-') || !matches!(perms[1], b'w' | b'-') || !matches!(perms[2], b'x' | b'-') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "malformed mapping permissions",
            ));
        }
        let offset = u64::from_str_radix(fields[2], 16)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad mapping offset"))?;
        let (major, minor) = fields[3]
            .split_once(':')
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad mapping device"))?;
        let device_major = u32::from_str_radix(major, 16)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad device major"))?;
        let device_minor = u32::from_str_radix(minor, 16)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad device minor"))?;
        let inode = fields[4]
            .parse::<u64>()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad mapping inode"))?;
        if fields.len() > 6 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unescaped whitespace in mapping path",
            ));
        }
        let encoded_path = fields.get(5).copied().unwrap_or("");
        let decoded_path = decode_maps_path(encoded_path.as_bytes())?;
        let path = std::str::from_utf8(&decoded_path)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "non-UTF-8 mapping path"))?;
        let kernel_special = matches!(path, "[vdso]" | "[vvar]" | "[vvar_vclock]" | "[vsyscall]");
        let root_relative = if path.starts_with('/') {
            if path.ends_with(" (deleted)") {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "deleted file mapping has no immutable identity",
                ));
            }
            let relative = path.strip_prefix('/').expect("absolute path");
            if !canonical_relative(relative.as_bytes()) || relative.as_bytes().len() > MAX_PATH_BYTES {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "mapping path exceeds bound"));
            }
            let file = std::fs::File::open(root.join(relative))?;
            let metadata = file.metadata()?;
            if metadata.ino() != inode
                || libc::major(metadata.dev()) as u32 != device_major
                || libc::minor(metadata.dev()) as u32 != device_minor
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "file mapping identity changed",
                ));
            }
            Some((
                relative.as_bytes().to_vec(),
                hash_file_range(&file, offset, end - start, deadline)?,
            ))
        } else {
            if inode != 0 || device_major != 0 || device_minor != 0 || offset != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "anonymous mapping carries file identity",
                ));
            }
            None
        };
        let (root_relative, file_digest) =
            root_relative.map_or((None, None), |(path, digest)| (Some(path), Some(digest)));
        mappings.push(NativeMapping {
            start,
            end,
            offset,
            protection,
            device_major,
            device_minor,
            inode,
            kernel_special,
            root_relative,
            file_digest,
            bytes: Vec::new(),
        });
    }
    Ok(mappings)
}

#[cfg(target_os = "linux")]
fn decode_maps_path(encoded: &[u8]) -> io::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(encoded.len());
    let mut at = 0;
    while at < encoded.len() {
        if encoded[at] != b'\\' {
            out.push(encoded[at]);
            at += 1;
            continue;
        }
        let escape = encoded
            .get(at + 1..at + 4)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "malformed maps escape"))?;
        let byte = match escape {
            b"040" => b' ',
            b"011" => b'\t',
            b"012" => b'\n',
            b"134" => b'\\',
            _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "unknown maps escape")),
        };
        out.push(byte);
        at += 4;
    }
    Ok(out)
}

#[cfg(target_os = "linux")]
fn hash_file_range(file: &std::fs::File, offset: u64, mapping_len: u64, deadline: Instant) -> io::Result<[u8; 32]> {
    let available = file.metadata()?.len().saturating_sub(offset).min(mapping_len);
    let mut hash = Sha256::new();
    let mut buffer = vec![0u8; (1 << 20).min(usize::try_from(available).unwrap_or(1 << 20))];
    let mut done = 0u64;
    while done < available {
        check_deadline(deadline)?;
        let count = buffer
            .len()
            .min(usize::try_from(available - done).unwrap_or(buffer.len()));
        let read = file.read_at(&mut buffer[..count], offset + done)?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "file mapping changed while hashing",
            ));
        }
        hash.update(&buffer[..read]);
        done += read as u64;
    }
    Ok(hash.finalize().into())
}

#[cfg(target_os = "linux")]
fn read_process_mem_exact(memory: &std::fs::File, start: u64, length: usize, deadline: Instant) -> io::Result<Vec<u8>> {
    let mut bytes = vec![0u8; length];
    let mut done = 0usize;
    while done < length {
        check_deadline(deadline)?;
        let count = (length - done).min(1 << 20);
        let read = memory.read_at(&mut bytes[done..done + count], start + done as u64)?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "unreadable anonymous process mapping",
            ));
        }
        done += read;
    }
    Ok(bytes)
}

#[cfg(target_os = "linux")]
fn read_process_exact(pid: libc::pid_t, start: u64, length: usize, deadline: Instant) -> io::Result<Vec<u8>> {
    const CHUNK: usize = 1 << 20;
    let mut bytes = vec![0_u8; length];
    let mut done = 0usize;
    while done < length {
        check_deadline(deadline)?;
        let count = (length - done).min(CHUNK);
        let mut local = libc::iovec {
            iov_base: bytes[done..done + count].as_mut_ptr().cast(),
            iov_len: count,
        };
        let remote_address = start
            .checked_add(done as u64)
            .and_then(|address| usize::try_from(address).ok())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "mapping address overflow"))?;
        let remote = libc::iovec {
            iov_base: remote_address as *mut libc::c_void,
            iov_len: count,
        };
        let read = unsafe { libc::process_vm_readv(pid, &raw mut local, 1, &raw const remote, 1, 0) };
        if read < 0 {
            return Err(io::Error::last_os_error());
        }
        if read as usize != count {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "partial process memory read",
            ));
        }
        done += count;
    }
    Ok(bytes)
}

#[cfg(target_os = "linux")]
fn check_deadline(deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "native memory capture deadline elapsed",
        ))
    } else {
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn ensure_same_maps(before: &[u8], after: &[u8]) -> io::Result<()> {
    if before == after {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "process map table changed during capture",
        ))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum InvalidRecord {
    Size,
    Magic,
    Version,
    Architecture,
    DeclaredSize,
    Reserved,
}

impl X86RegisterRecord {
    pub(super) fn encode(&self) -> [u8; RECORD_SIZE] {
        let mut bytes = [0_u8; RECORD_SIZE];
        bytes[..8].copy_from_slice(MAGIC);
        bytes[8..10].copy_from_slice(&VERSION.to_le_bytes());
        bytes[10..12].copy_from_slice(&ELF_MACHINE_X86_64.to_le_bytes());
        bytes[12..16].copy_from_slice(&(RECORD_SIZE as u32).to_le_bytes());
        bytes[16..24].copy_from_slice(&self.signal_mask.to_le_bytes());
        for (index, value) in self.registers.iter().enumerate() {
            let start = REGISTER_OFFSET + index * 8;
            bytes[start..start + 8].copy_from_slice(&value.to_le_bytes());
        }
        bytes
    }

    pub(super) fn decode(bytes: &[u8]) -> Result<Self, InvalidRecord> {
        let bytes: &[u8; RECORD_SIZE] = bytes.try_into().map_err(|_| InvalidRecord::Size)?;
        if &bytes[..8] != MAGIC {
            return Err(InvalidRecord::Magic);
        }
        if u16::from_le_bytes(bytes[8..10].try_into().expect("fixed field")) != VERSION {
            return Err(InvalidRecord::Version);
        }
        if u16::from_le_bytes(bytes[10..12].try_into().expect("fixed field")) != ELF_MACHINE_X86_64 {
            return Err(InvalidRecord::Architecture);
        }
        if u32::from_le_bytes(bytes[12..16].try_into().expect("fixed field")) as usize != RECORD_SIZE {
            return Err(InvalidRecord::DeclaredSize);
        }
        if bytes[24..32].iter().chain(&bytes[248..]).any(|byte| *byte != 0) {
            return Err(InvalidRecord::Reserved);
        }
        let mut registers = [0; REGISTER_COUNT];
        for (index, value) in registers.iter_mut().enumerate() {
            let start = REGISTER_OFFSET + index * 8;
            *value = u64::from_le_bytes(bytes[start..start + 8].try_into().expect("fixed field"));
        }
        Ok(Self {
            signal_mask: u64::from_le_bytes(bytes[16..24].try_into().expect("fixed field")),
            registers,
        })
    }
}

#[cfg(target_arch = "x86_64")]
pub(super) fn capture(pid: libc::pid_t) -> io::Result<X86RegisterRecord> {
    capture_with(pid, || Ok(()))
}

#[cfg(not(target_arch = "x86_64"))]
pub(super) fn capture(_pid: libc::pid_t) -> io::Result<X86RegisterRecord> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "native-x86-v1 register capture requires a Linux x86-64 host",
    ))
}

#[cfg(target_arch = "x86_64")]
fn capture_with(pid: libc::pid_t, after_stop: impl FnOnce() -> io::Result<()>) -> io::Result<X86RegisterRecord> {
    if pid <= 1 || pid == unsafe { libc::getpid() } {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "capture target must be another live process",
        ));
    }
    ptrace(libc::PTRACE_SEIZE, pid, 0, 0)?;
    let mut guard = TraceGuard {
        pid,
        was_group_stopped: false,
        ptrace_stopped: false,
    };
    ptrace(libc::PTRACE_INTERRUPT, pid, 0, 0)?;
    guard.was_group_stopped = wait_for_ptrace_stop(pid, STOP_DEADLINE)?;
    guard.ptrace_stopped = true;
    after_stop()?;

    let mut raw: libc::user_regs_struct = unsafe { std::mem::zeroed() };
    let mut iov = libc::iovec {
        iov_base: (&raw mut raw).cast(),
        iov_len: std::mem::size_of_val(&raw),
    };
    ptrace(
        libc::PTRACE_GETREGSET,
        pid,
        libc::NT_PRSTATUS as usize,
        (&raw mut iov) as usize,
    )?;
    if iov.iov_len != std::mem::size_of_val(&raw) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "short x86-64 NT_PRSTATUS register set",
        ));
    }
    let mut signal_mask = 0_u64;
    ptrace(
        libc::PTRACE_GETSIGMASK,
        pid,
        std::mem::size_of_val(&signal_mask),
        (&raw mut signal_mask) as usize,
    )?;
    let registers = unsafe { std::ptr::read_unaligned((&raw const raw).cast::<[u64; REGISTER_COUNT]>()) };
    drop(guard);
    Ok(X86RegisterRecord { signal_mask, registers })
}

#[cfg(target_arch = "x86_64")]
fn process_is_stopped(pid: libc::pid_t) -> io::Result<bool> {
    let status = std::fs::read(format!("/proc/{pid}/status"))?;
    let state = status
        .split(|byte| *byte == b'\n')
        .find(|line| line.starts_with(b"State:"))
        .and_then(|line| line.get(7).copied())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing process state"))?;
    Ok(matches!(state, b'T' | b't'))
}

#[cfg(target_arch = "x86_64")]
fn wait_for_ptrace_stop(pid: libc::pid_t, timeout: Duration) -> io::Result<bool> {
    let deadline = Instant::now() + timeout;
    loop {
        // Observe death without collecting it. The process owner must retain the exit status.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let observed = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &raw mut info,
                libc::WEXITED | libc::WSTOPPED | libc::WNOHANG | libc::WNOWAIT | libc::__WALL,
            )
        };
        if observed < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        } else if unsafe { info.si_pid() } == pid
            && matches!(info.si_code, libc::CLD_EXITED | libc::CLD_KILLED | libc::CLD_DUMPED)
        {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "tracee exited before register capture",
            ));
        }

        let mut status = 0;
        let waited = unsafe { libc::waitpid(pid, &raw mut status, libc::__WALL | libc::WNOHANG) };
        if waited < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if waited == pid && libc::WIFSTOPPED(status) {
            let event = status >> 16;
            if event != libc::PTRACE_EVENT_STOP {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "unexpected ptrace stop"));
            }
            // A seized tracee reports PTRACE_EVENT_STOP/SIGTRAP for PTRACE_INTERRUPT.
            // A pre-existing group-stop reports the actual stopping signal instead.
            return Ok(libc::WSTOPSIG(status) != libc::SIGTRAP);
        }
        if waited == pid {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "unexpected tracee event"));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "timed out waiting for ptrace stop",
            ));
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[cfg(target_arch = "x86_64")]
fn ptrace(request: libc::c_uint, pid: libc::pid_t, address: usize, data: usize) -> io::Result<()> {
    let result = unsafe { libc::ptrace(request, pid, address as *mut libc::c_void, data as *mut libc::c_void) };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(target_arch = "x86_64")]
struct TraceGuard {
    pid: libc::pid_t,
    was_group_stopped: bool,
    ptrace_stopped: bool,
}

#[cfg(target_arch = "x86_64")]
impl Drop for TraceGuard {
    fn drop(&mut self) {
        // Cleanup must not wait: a destructor cannot safely depend on an untrusted tracee making
        // another transition. A stopped tracee can be detached; otherwise this best-effort detach
        // either succeeds immediately or the kernel releases the relationship when the task exits.
        let signal = if self.was_group_stopped {
            libc::SIGSTOP as usize
        } else {
            0
        };
        if self.ptrace_stopped {
            let _ = ptrace(libc::PTRACE_DETACH, self.pid, 0, signal);
        } else {
            let _ = ptrace(libc::PTRACE_DETACH, self.pid, 0, 0);
        }
    }
}

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::num::NonZeroU64;
    use std::os::fd::RawFd;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    #[derive(Default)]
    struct AtomicSink {
        state: Mutex<(BTreeMap<String, Vec<u8>>, BTreeMap<String, Vec<u8>>, usize)>,
        fail_at: Option<&'static str>,
    }

    impl CheckpointSink for AtomicSink {
        fn replace(&self, _: &[u8]) -> Result<(), CompositionError> {
            unreachable!()
        }
        fn begin_until(&self, _: Instant) -> Result<NonZeroU64, CompositionError> {
            Ok(NonZeroU64::new(1).unwrap())
        }
        fn put_until(&self, _: NonZeroU64, name: &str, bytes: &[u8], _: Instant) -> Result<(), CompositionError> {
            if self.fail_at == Some(name) {
                return Err(CompositionError::RuntimeConstruction);
            }
            self.state.lock().unwrap().1.insert(name.to_owned(), bytes.to_vec());
            Ok(())
        }
        fn abort_until(&self, _: NonZeroU64, _: Instant) -> Result<(), CompositionError> {
            let mut state = self.state.lock().unwrap();
            state.1.clear();
            state.2 += 1;
            Ok(())
        }
        fn commit_until(&self, _: NonZeroU64, manifest: &[u8], _: Instant) -> Result<(), CompositionError> {
            if self.fail_at == Some("commit") {
                return Err(CompositionError::RuntimeConstruction);
            }
            let mut state = self.state.lock().unwrap();
            let staging = std::mem::take(&mut state.1);
            state.0 = staging;
            state.0.insert("MANIFEST".into(), manifest.to_vec());
            Ok(())
        }
    }

    fn stopped_child() -> libc::pid_t {
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            loop {
                unsafe { libc::pause() };
            }
        }
        assert_eq!(unsafe { libc::kill(pid, libc::SIGSTOP) }, 0);
        wait_until_stopped(pid);
        pid
    }

    #[test]
    fn native_transaction_publishes_only_a_complete_validated_generation() {
        let pid = stopped_child();
        let sink = AtomicSink::default();
        publish_stopped_native(&sink, pid, Instant::now() + Duration::from_secs(10)).unwrap();
        let state = sink.state.lock().unwrap();
        assert_eq!(state.0.len(), 4);
        assert_eq!(
            crate::runtime::checkpoint::image_envelope::Reader::decode(&state.0["IMAGE"]),
            Ok(crate::runtime::checkpoint::image_envelope::Reader::NativeX86V1),
        );
        assert_eq!(
            validate_native_objects(&state.0["MANIFEST"], |name| state.0.get(name).cloned()),
            Ok(())
        );
        let mut tampered = state.0.clone();
        tampered.get_mut(MEMORY_OBJECT).unwrap()[0] ^= 1;
        assert_eq!(
            validate_native_objects(&state.0["MANIFEST"], |name| tampered.get(name).cloned()),
            Err(InvalidNativeImage::Digest)
        );
        assert_eq!(
            validate_native_objects(&state.0["MANIFEST"], |name| (name != REGISTER_OBJECT)
                .then(|| state.0[name].clone())),
            Err(InvalidNativeImage::Missing)
        );
        drop(state);
        kill_and_reap(pid);
    }

    #[test]
    fn object_and_commit_failures_abort_without_partial_publication_and_thaw_tracee() {
        for failure in [MEMORY_OBJECT, "commit"] {
            let pid = stopped_child();
            let sink = AtomicSink {
                fail_at: Some(failure),
                ..AtomicSink::default()
            };
            assert!(publish_stopped_native(&sink, pid, Instant::now() + Duration::from_secs(10)).is_err());
            let state = sink.state.lock().unwrap();
            assert!(state.0.is_empty());
            assert!(state.1.is_empty());
            assert_eq!(state.2, 1);
            drop(state);
            wait_until_running(pid);
            kill_and_reap(pid);
        }
    }

    #[test]
    fn canonical_codec_is_exact_and_rejects_every_structural_mutation() {
        let record = X86RegisterRecord {
            signal_mask: 0x1122_3344_5566_7788,
            registers: std::array::from_fn(|index| 0xfeed_0000_0000_0000 | index as u64),
        };
        let encoded = record.encode();
        assert_eq!(&encoded[..8], b"HLNXREG\0");
        assert_eq!(&encoded[8..10], &1_u16.to_le_bytes());
        assert_eq!(&encoded[10..12], &62_u16.to_le_bytes());
        assert_eq!(&encoded[12..16], &256_u32.to_le_bytes());
        assert_eq!(X86RegisterRecord::decode(&encoded), Ok(record));
        for (at, expected) in [
            (0, InvalidRecord::Magic),
            (8, InvalidRecord::Version),
            (10, InvalidRecord::Architecture),
            (12, InvalidRecord::DeclaredSize),
            (24, InvalidRecord::Reserved),
            (255, InvalidRecord::Reserved),
        ] {
            let mut changed = encoded;
            changed[at] ^= 1;
            assert_eq!(X86RegisterRecord::decode(&changed), Err(expected), "offset {at}");
        }
        assert_eq!(X86RegisterRecord::decode(&encoded[..255]), Err(InvalidRecord::Size));
    }

    #[test]
    fn live_child_register_sentinel_and_signal_mask_are_captured_without_leaving_it_stopped() {
        let (pid, ready) = sentinel_child(false);
        wait_byte(ready);
        let record = capture(pid).unwrap();
        assert_eq!(record.registers[0], 0x1515_1515_1515_1515, "r15 sentinel");
        assert_ne!(record.signal_mask & (1 << (libc::SIGUSR1 - 1)), 0);
        wait_until_running(pid);
        kill_and_reap(pid);
    }

    #[test]
    fn failed_capture_thaws_running_child_and_preserves_an_existing_group_stop() {
        let (running, ready) = sentinel_child(false);
        wait_byte(ready);
        let error = capture_with(running, || Err(io::Error::other("injected after stop"))).unwrap_err();
        assert_eq!(error.to_string(), "injected after stop");
        wait_until_running(running);
        kill_and_reap(running);

        let (stopped, ready) = sentinel_child(true);
        wait_byte(ready);
        wait_until_stopped(stopped);
        capture(stopped).unwrap();
        wait_until_stopped(stopped);
        unsafe { libc::kill(stopped, libc::SIGCONT) };
        kill_and_reap(stopped);
    }

    #[test]
    fn exit_observation_does_not_reap_the_process_owners_child() {
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            unsafe { libc::_exit(23) }
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match wait_for_ptrace_stop(pid, Duration::from_millis(10)) {
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(error) if error.raw_os_error() == Some(libc::ECHILD) && Instant::now() < deadline => {
                    std::thread::yield_now();
                }
                other => panic!("unexpected exit observation: {other:?}"),
            }
        }
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(pid, &raw mut status, 0) }, pid);
        assert!(libc::WIFEXITED(status));
        assert_eq!(libc::WEXITSTATUS(status), 23);
    }

    #[test]
    fn seized_child_exit_is_observed_without_consuming_its_status() {
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            loop {
                unsafe { libc::pause() };
            }
        }
        ptrace(libc::PTRACE_SEIZE, pid, 0, 0).unwrap();
        assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
        let error = wait_for_ptrace_stop(pid, Duration::from_secs(2)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(pid, &raw mut status, 0) }, pid);
        assert!(libc::WIFSIGNALED(status));
        assert_eq!(libc::WTERMSIG(status), libc::SIGKILL);
    }

    #[test]
    fn memory_codec_rejects_malformed_lengths_counts_and_overflow() {
        let image = NativeMemoryImage {
            mappings: vec![NativeMapping {
                start: 0x1000,
                end: 0x2000,
                offset: 0,
                protection: 3,
                device_major: 0,
                device_minor: 0,
                inode: 0,
                kernel_special: false,
                root_relative: None,
                file_digest: None,
                bytes: vec![0x5a; 0x1000],
            }],
        };
        let encoded = image.encode().unwrap();
        assert_eq!(NativeMemoryImage::decode(&encoded), Ok(image));
        for (at, value, expected) in [
            (0, b'X', InvalidMemoryImage::Magic),
            (8, 3, InvalidMemoryImage::Version),
            (10, 1, InvalidMemoryImage::Reserved),
            (24, 1, InvalidMemoryImage::Reserved),
            (MEMORY_HEADER_SIZE + 41, 2, InvalidMemoryImage::Kind),
        ] {
            let mut changed = encoded.clone();
            changed[at] = value;
            assert_eq!(NativeMemoryImage::decode(&changed), Err(expected));
        }
        let mut count = encoded.clone();
        count[12..16].copy_from_slice(&(MAX_MAPPINGS as u32 + 1).to_le_bytes());
        assert_eq!(NativeMemoryImage::decode(&count), Err(InvalidMemoryImage::Count));
        let mut overflow = encoded.clone();
        overflow[MEMORY_HEADER_SIZE + 52..MEMORY_HEADER_SIZE + 60].copy_from_slice(&u64::MAX.to_le_bytes());
        assert_eq!(NativeMemoryImage::decode(&overflow), Err(InvalidMemoryImage::Overflow));
        assert_eq!(
            NativeMemoryImage::decode(&encoded[..encoded.len() - 1]),
            Err(InvalidMemoryImage::Size)
        );
    }

    #[test]
    fn memory_codec_rejects_noncanonical_file_paths() {
        for path in [b"/absolute".as_slice(), b"a//b", b"a/./b", b"a/../b", b"a/\0b"] {
            let image = NativeMemoryImage {
                mappings: vec![NativeMapping {
                    start: 0x1000,
                    end: 0x2000,
                    offset: 0,
                    protection: 1,
                    device_major: 1,
                    device_minor: 2,
                    inode: 3,
                    kernel_special: false,
                    root_relative: Some(path.to_vec()),
                    file_digest: Some([7; 32]),
                    bytes: vec![],
                }],
            };
            assert_eq!(image.encode(), Err(InvalidMemoryImage::Path), "{path:?}");
        }
    }

    #[test]
    fn proc_maps_path_escapes_are_strict_and_canonical() {
        assert_eq!(decode_maps_path(br"a\040b\011c\012d\134e").unwrap(), b"a b\tc\nd\\e");
        for malformed in [br"a\".as_slice(), br"a\04", br"a\041", br"a\000"] {
            assert_eq!(
                decode_maps_path(malformed).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
    }

    #[test]
    fn mapped_file_digest_changes_with_the_mapped_range() {
        let file = tempfile::tempfile().unwrap();
        file.write_all_at(b"before", 0).unwrap();
        let first = hash_file_range(&file, 0, 6, Instant::now() + Duration::from_secs(1)).unwrap();
        file.write_all_at(b"after!", 0).unwrap();
        let second = hash_file_range(&file, 0, 6, Instant::now() + Duration::from_secs(1)).unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn file_mapping_revalidation_rejects_in_place_content_change() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("mapped file");
        std::fs::write(&path, b"before").unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let metadata = file.metadata().unwrap();
        let mapping = NativeMapping {
            start: 0x1000,
            end: 0x1006,
            offset: 0,
            protection: 1,
            device_major: libc::major(metadata.dev()) as u32,
            device_minor: libc::minor(metadata.dev()) as u32,
            inode: metadata.ino(),
            kernel_special: false,
            root_relative: Some(b"mapped file".to_vec()),
            file_digest: Some(hash_file_range(&file, 0, 6, Instant::now() + Duration::from_secs(1)).unwrap()),
            bytes: vec![],
        };
        std::fs::write(path, b"after!").unwrap();
        assert_eq!(
            revalidate_file_mappings(&[mapping], root.path(), Instant::now() + Duration::from_secs(1))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn stopped_child_prot_none_memory_is_captured_losslessly() {
        let mut pipe = [0; 2];
        assert_eq!(unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC) }, 0);
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            unsafe {
                libc::close(pipe[0]);
                let page = libc::mmap(
                    std::ptr::null_mut(),
                    4096,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                    -1,
                    0,
                );
                assert_ne!(page, libc::MAP_FAILED);
                std::ptr::write_bytes(page.cast::<u8>(), 0x6e, 4096);
                assert_eq!(libc::mprotect(page, 4096, libc::PROT_NONE), 0);
                let address = page as u64;
                libc::write(pipe[1], (&raw const address).cast(), 8);
                libc::raise(libc::SIGSTOP);
                libc::pause();
            }
        }
        unsafe { libc::close(pipe[1]) };
        let mut address = 0u64;
        assert_eq!(unsafe { libc::read(pipe[0], (&raw mut address).cast(), 8) }, 8);
        unsafe { libc::close(pipe[0]) };
        wait_until_stopped(pid);
        let image = capture_stopped_memory(pid, Instant::now() + Duration::from_secs(2)).unwrap();
        assert_eq!(image_bytes(&image, address, 16), vec![0x6e; 16]);
        assert!(
            image
                .mappings
                .iter()
                .any(|mapping| mapping.start <= address && address < mapping.end && mapping.protection == 0)
        );
        kill_and_reap(pid);
    }

    #[test]
    fn stopped_child_heap_and_stack_are_captured_and_a_later_mutation_changes_only_the_new_image() {
        let (pid, ready) = memory_sentinel_child();
        let mut addresses = [0_u64; 2];
        assert_eq!(
            unsafe { libc::read(ready, addresses.as_mut_ptr().cast(), std::mem::size_of_val(&addresses)) },
            std::mem::size_of_val(&addresses) as isize
        );
        unsafe { libc::close(ready) };
        wait_until_stopped(pid);
        let first = capture_stopped_memory(pid, Instant::now() + Duration::from_secs(2)).unwrap();
        assert_eq!(image_bytes(&first, addresses[0], 16), vec![0x48; 16], "heap sentinel");
        assert_eq!(image_bytes(&first, addresses[1], 16), vec![0x53; 16], "stack sentinel");

        let replacement = [0x4d_u8; 16];
        let local = libc::iovec {
            iov_base: replacement.as_ptr().cast_mut().cast(),
            iov_len: replacement.len(),
        };
        let remote = libc::iovec {
            iov_base: addresses[0] as usize as *mut libc::c_void,
            iov_len: replacement.len(),
        };
        assert_eq!(
            unsafe { libc::process_vm_writev(pid, &raw const local, 1, &raw const remote, 1, 0) },
            16
        );
        let second = capture_stopped_memory(pid, Instant::now() + Duration::from_secs(2)).unwrap();
        assert_eq!(
            image_bytes(&first, addresses[0], 16),
            vec![0x48; 16],
            "first image mutated by alias"
        );
        assert_eq!(image_bytes(&second, addresses[0], 16), vec![0x4d; 16]);
        assert!(process_is_stopped(pid).unwrap(), "capture stole stop ownership");
        unsafe { libc::kill(pid, libc::SIGCONT) };
        kill_and_reap(pid);
    }

    #[test]
    fn stopped_capture_refuses_deadlines_and_running_targets_without_changing_ownership() {
        let (pid, ready) = sentinel_child(false);
        wait_byte(ready);
        let running = capture_stopped_memory(pid, Instant::now() + Duration::from_secs(1)).unwrap_err();
        assert_eq!(running.kind(), io::ErrorKind::InvalidInput);
        assert!(!process_is_stopped(pid).unwrap());
        unsafe { libc::kill(pid, libc::SIGSTOP) };
        wait_until_stopped(pid);
        let deadline = capture_stopped_memory(pid, Instant::now()).unwrap_err();
        assert_eq!(deadline.kind(), io::ErrorKind::TimedOut);
        assert!(process_is_stopped(pid).unwrap());
        unsafe { libc::kill(pid, libc::SIGCONT) };
        kill_and_reap(pid);
    }

    #[test]
    fn map_races_and_malformed_or_shared_tables_are_refused() {
        assert!(ensure_same_maps(b"one", b"one").is_ok());
        assert_eq!(
            ensure_same_maps(b"one", b"two").unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        let root = Path::new("/");
        assert_eq!(
            parse_maps(b"not-a-map\n", root, Instant::now() + Duration::from_secs(1))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        let shared = b"1000-2000 rw-s 00000000 00:00 0\n";
        assert_eq!(
            parse_maps(shared, root, Instant::now() + Duration::from_secs(1))
                .unwrap_err()
                .kind(),
            io::ErrorKind::Unsupported
        );
    }

    fn image_bytes(image: &NativeMemoryImage, address: u64, length: usize) -> Vec<u8> {
        let mapping = image
            .mappings
            .iter()
            .find(|mapping| mapping.start <= address && address + length as u64 <= mapping.end)
            .expect("sentinel mapping");
        let offset = (address - mapping.start) as usize;
        mapping.bytes[offset..offset + length].to_vec()
    }

    fn memory_sentinel_child() -> (libc::pid_t, RawFd) {
        let mut pipe = [0; 2];
        assert_eq!(unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC) }, 0);
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            unsafe {
                libc::close(pipe[0]);
                let heap = libc::malloc(16).cast::<u8>();
                assert!(!heap.is_null());
                std::ptr::write_bytes(heap, 0x48, 16);
                let stack = [0x53_u8; 16];
                let addresses = [heap as u64, stack.as_ptr() as u64];
                libc::write(pipe[1], addresses.as_ptr().cast(), std::mem::size_of_val(&addresses));
                libc::raise(libc::SIGSTOP);
                loop {
                    std::hint::black_box(std::ptr::read_volatile(heap));
                    std::hint::black_box(std::ptr::read_volatile(stack.as_ptr()));
                    libc::pause();
                }
            }
        }
        unsafe { libc::close(pipe[1]) };
        (pid, pipe[0])
    }

    fn sentinel_child(stop: bool) -> (libc::pid_t, RawFd) {
        let mut pipe = [0; 2];
        assert_eq!(unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC) }, 0);
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            unsafe {
                libc::close(pipe[0]);
                let mut mask: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&raw mut mask);
                libc::sigaddset(&raw mut mask, libc::SIGUSR1);
                libc::sigprocmask(libc::SIG_BLOCK, &raw const mask, std::ptr::null_mut());
                std::arch::asm!("mov r15, {sentinel}", sentinel = in(reg) 0x1515_1515_1515_1515_u64);
                libc::write(pipe[1], b"R".as_ptr().cast(), 1);
                if stop {
                    libc::raise(libc::SIGSTOP);
                }
                loop {
                    libc::pause();
                }
            }
        }
        unsafe {
            libc::close(pipe[1]);
        }
        (pid, pipe[0])
    }

    fn wait_byte(fd: RawFd) {
        let mut byte = 0;
        assert_eq!(unsafe { libc::read(fd, (&raw mut byte as *mut u8).cast(), 1) }, 1);
        unsafe {
            libc::close(fd);
        }
    }

    fn wait_until_stopped(pid: libc::pid_t) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if process_is_stopped(pid).unwrap_or(false) {
                return;
            }
            std::thread::yield_now();
        }
        panic!("child {pid} did not stop");
    }

    fn wait_until_running(pid: libc::pid_t) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if !process_is_stopped(pid).unwrap_or(true) {
                let stable_until = Instant::now() + Duration::from_millis(50);
                while Instant::now() < stable_until {
                    assert!(!process_is_stopped(pid).unwrap(), "child {pid} stopped after detach");
                    std::thread::yield_now();
                }
                return;
            }
            std::thread::yield_now();
        }
        panic!("child {pid} did not resume");
    }

    fn kill_and_reap(pid: libc::pid_t) {
        unsafe {
            libc::kill(pid, libc::SIGKILL);
            libc::waitpid(pid, std::ptr::null_mut(), 0);
        }
    }
}
