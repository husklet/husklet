#![allow(unsafe_code)]

use std::{
    ffi::{c_char, c_int, c_uint, c_void},
    fs::File,
    io::{Read, Seek, SeekFrom},
    ptr::NonNull,
};

use crate::bindings::{self, Backend, SyscallDispatch};

pub const STATUS_OK: i32 = 0;

/// Borrowed, low-level creation arguments for the native engine.
///
/// The safe high-level container adapter owns the strings, arrays and image
/// plan. This package deliberately does not depend on application domain types.
#[derive(Clone, Copy)]
pub struct EngineConfig<'a> {
    pub isa: u32,
    pub rootfs: Option<&'a std::ffi::CStr>,
    pub executable_host: Option<&'a std::ffi::CStr>,
    pub executable_fd: i32,
    pub option_names: &'a [*const c_char],
    pub option_values: &'a [*const c_char],
    pub standard_fds: [i32; 3],
    pub provider_fd: i32,
    pub syscall_context: *mut c_void,
    pub syscall_dispatch: Option<SyscallDispatch>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
struct Plan {
    abi: u32,
    size: u32,
    architecture: u32,
    kind: u32,
    link_start: u64,
    link_end: u64,
    has_interpreter: u32,
    flags: u32,
    interpreter_identity: u64,
}

/// Validated information derived from an ELF program-header table before it
/// is projected into the stable native ABI plan.
struct ProgramLayout {
    load_start: u64,
    load_end: u64,
    interpreter: Option<Vec<u8>>,
    entry_is_executable: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Exit {
    pub kind: u32,
    pub status: i32,
    pub detail: u64,
}

/// Unique owner of a native engine instance.
pub struct Engine(NonNull<Backend>);

// SAFETY: the C lifecycle contract permits request from another thread while
// run is active. The handle remains uniquely owned and destroy joins the active
// run before releasing the engine allocation.
unsafe impl Send for Engine {}
// SAFETY: every shared operation is implemented by the C engine's synchronized
// request/read interface; mutation of engine state is not exposed through Rust.
unsafe impl Sync for Engine {}

impl Engine {
    /// Creates an engine through the stable C bridge.
    ///
    /// # Safety
    /// `image_plan`, option pointers and callback state must satisfy the C ABI.
    /// Borrowed create inputs need only remain valid for this call; C copies
    /// configuration. Callback context must remain valid until this value drops.
    pub unsafe fn create(config: EngineConfig<'_>) -> Result<Self, i32> {
        if config.option_names.len() != config.option_values.len() {
            return Err(STATUS_OK.wrapping_add(1));
        }
        let count = c_uint::try_from(config.option_names.len()).map_err(|_| 1)?;
        let image_plan = Plan::inspect(&config)?;
        let mut output = std::ptr::null_mut();
        // SAFETY: the caller guarantees that the raw option and callback
        // pointers satisfy the documented C ABI. All Rust-owned arrays and
        // strings are borrowed through this call, and `output` is writable.
        let status = unsafe {
            bindings::hl_c_backend_create(
                config.isa,
                config.rootfs.map_or(std::ptr::null(), std::ffi::CStr::as_ptr),
                config.executable_host.map_or(std::ptr::null(), std::ffi::CStr::as_ptr),
                config.executable_fd,
                (&raw const image_plan).cast(),
                count,
                config.option_names.as_ptr(),
                config.option_values.as_ptr(),
                config.standard_fds.as_ptr(),
                config.provider_fd,
                config.syscall_context,
                config.syscall_dispatch,
                &raw mut output,
            )
        };
        if status != STATUS_OK {
            return Err(status);
        }
        NonNull::new(output).map(Self).ok_or(1)
    }

    pub fn run(&self, arguments: &[*const c_char]) -> Result<(), i32> {
        let count = c_int::try_from(arguments.len()).map_err(|_| 1)?;
        // SAFETY: `self` owns a live backend, the pointer array is readable for
        // `count` entries during the call, and C does not retain the array.
        let status = unsafe { bindings::hl_c_backend_run(self.0.as_ptr(), count, arguments.as_ptr()) };
        (status == STATUS_OK).then_some(()).ok_or(status)
    }

    pub fn request(&self, request: u32, signal: i32) -> Result<(), i32> {
        // SAFETY: `self` owns a live backend and the C request entry point is
        // synchronized with both run and destruction by the engine contract.
        let status = unsafe { bindings::hl_c_backend_request(self.0.as_ptr(), request, signal) };
        (status == STATUS_OK).then_some(()).ok_or(status)
    }

    #[must_use]
    pub fn exit(&self) -> Exit {
        // SAFETY: `self` owns a live backend; this accessor only copies the
        // completed engine's immutable exit kind.
        let kind = unsafe { bindings::hl_c_backend_exit_kind(self.0.as_ptr()) };
        // SAFETY: `self` owns a live backend; this accessor only copies the
        // completed engine's immutable exit status.
        let status = unsafe { bindings::hl_c_backend_exit_status(self.0.as_ptr()) };
        // SAFETY: `self` owns a live backend; this accessor only copies the
        // completed engine's immutable exit detail.
        let detail = unsafe { bindings::hl_c_backend_exit_detail(self.0.as_ptr()) };
        Exit { kind, status, detail }
    }
}

impl Plan {
    fn inspect(config: &EngineConfig<'_>) -> Result<Self, i32> {
        let mut file = open_main_image(config)?;
        let image_length = file.metadata().map_err(|_| 1)?.len();
        if image_length < 64 {
            return Err(1);
        }
        let mut header = [0_u8; 64];
        file.read_exact(&mut header).map_err(|_| 1)?;
        if &header[..7] != b"\x7fELF\x02\x01\x01" || !matches!(header[7], 0 | 3) {
            return Err(1);
        }
        let word16 = |offset| u16::from_le_bytes(header[offset..offset + 2].try_into().expect("fixed header"));
        let word32 = |offset| u32::from_le_bytes(header[offset..offset + 4].try_into().expect("fixed header"));
        let word64 = |offset| u64::from_le_bytes(header[offset..offset + 8].try_into().expect("fixed header"));
        let kind = match word16(16) {
            2 => 1,
            3 => 2,
            _ => return Err(1),
        };
        let machine = match config.isa {
            1 => 0xb7,
            2 => 0x3e,
            _ => return Err(1),
        };
        if word16(18) != machine {
            return Err(1);
        }
        if word32(20) != 1 || word16(52) != 64 {
            return Err(1);
        }
        let entry = word64(24);
        if config.isa == 1 && !entry.is_multiple_of(4) {
            return Err(1);
        }
        let layout = inspect_program_headers(
            &mut file,
            image_length,
            entry,
            word64(32),
            u64::from(word16(54)),
            word16(56),
        )?;
        if !layout.entry_is_executable {
            return Err(1);
        }
        let link_start = layout.load_start & !0xfff;
        let span = layout.load_end.checked_sub(link_start).ok_or(1)?;
        let link_end = link_start
            .checked_add(span.checked_add(0xffff).ok_or(1)? & !0xffff)
            .ok_or(1)?;
        let interpreter_identity = layout.interpreter.as_deref().map_or(0, |path| {
            path.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
                (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
            })
        });
        Ok(Self {
            abi: 1,
            size: u32::try_from(std::mem::size_of::<Plan>()).expect("small ABI struct"),
            architecture: config.isa,
            kind,
            link_start,
            link_end,
            has_interpreter: u32::from(layout.interpreter.is_some()),
            flags: 0,
            interpreter_identity,
        })
    }
}

fn open_main_image(config: &EngineConfig<'_>) -> Result<File, i32> {
    if config.executable_fd >= 0 {
        #[cfg(unix)]
        {
            use std::os::fd::FromRawFd;
            // SAFETY: `dup` accepts any integer descriptor and reports invalid
            // descriptors with a negative return value, which is checked.
            let descriptor = unsafe { libc::dup(config.executable_fd) };
            if descriptor < 0 {
                return Err(1);
            }
            // SAFETY: successful `dup` returned a fresh owned descriptor, so
            // transferring that ownership to `File` is unique and balanced.
            return Ok(unsafe { File::from_raw_fd(descriptor) });
        }
        #[cfg(not(unix))]
        return Err(3);
    }
    let path = config.executable_host.ok_or(1)?;
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        File::open(std::ffi::OsStr::from_bytes(path.to_bytes())).map_err(|_| 1)
    }
    #[cfg(not(unix))]
    return Err(3);
}

fn inspect_program_headers(
    file: &mut File,
    image_length: u64,
    entry: u64,
    phoff: u64,
    phentsize: u64,
    phnum: u16,
) -> Result<ProgramLayout, i32> {
    const PROGRAM_HEADER_SIZE: u64 = 56;
    const MAX_PROGRAM_HEADERS: u16 = 1024;
    const MAX_LOAD_SEGMENTS: u16 = 128;
    if phentsize != PROGRAM_HEADER_SIZE || phnum == 0 || phnum > MAX_PROGRAM_HEADERS {
        return Err(1);
    }
    let table_size = phentsize.checked_mul(u64::from(phnum)).ok_or(1)?;
    if phoff.checked_add(table_size).is_none_or(|end| end > image_length) {
        return Err(1);
    }
    let mut first = u64::MAX;
    let mut last = 0_u64;
    let mut interpreter = None;
    let mut loads = 0_u16;
    let mut entry_is_executable = false;
    for index in 0..phnum {
        let offset = phoff
            .checked_add(u64::from(index).checked_mul(phentsize).ok_or(1)?)
            .ok_or(1)?;
        file.seek(SeekFrom::Start(offset)).map_err(|_| 1)?;
        let mut program = [0_u8; 56];
        file.read_exact(&mut program).map_err(|_| 1)?;
        let u32_at = |offset| u32::from_le_bytes(program[offset..offset + 4].try_into().expect("program header"));
        let u64_at = |offset| u64::from_le_bytes(program[offset..offset + 8].try_into().expect("program header"));
        match u32_at(0) {
            1 => {
                loads = loads.checked_add(1).ok_or(1)?;
                if loads > MAX_LOAD_SEGMENTS {
                    return Err(1);
                }
                let file_offset = u64_at(8);
                let start = u64_at(16);
                let file_size = u64_at(32);
                let memory_size = u64_at(40);
                let alignment = u64_at(48);
                if file_size > memory_size
                    || (file_size != 0 && file_offset.checked_add(file_size).is_none_or(|end| end > image_length))
                    || (alignment > 1 && (!alignment.is_power_of_two() || start % alignment != file_offset % alignment))
                {
                    return Err(1);
                }
                let end = start.checked_add(memory_size).ok_or(1)?;
                first = first.min(start);
                last = last.max(end);
                entry_is_executable |= u32_at(4) & 1 != 0 && entry >= start && entry < end;
            }
            3 => {
                if interpreter.is_some() {
                    return Err(1);
                }
                interpreter = Some(read_interpreter(file, u64_at(8), u64_at(32))?);
            }
            _ => {}
        }
    }
    if first == u64::MAX {
        return Err(1);
    }
    Ok(ProgramLayout {
        load_start: first,
        load_end: last,
        interpreter,
        entry_is_executable,
    })
}

fn read_interpreter(file: &mut File, offset: u64, encoded_size: u64) -> Result<Vec<u8>, i32> {
    let size = usize::try_from(encoded_size).map_err(|_| 1)?;
    if size == 0 || size > 4096 {
        return Err(1);
    }
    let mut path = vec![0; size];
    file.seek(SeekFrom::Start(offset)).map_err(|_| 1)?;
    file.read_exact(&mut path).map_err(|_| 1)?;
    if path.last() != Some(&0) || path[..path.len() - 1].contains(&0) {
        return Err(1);
    }
    path.pop();
    Ok(path)
}

impl Drop for Engine {
    fn drop(&mut self) {
        // SAFETY: `Engine` is the unique owner of this live backend pointer and
        // Drop runs exactly once; destroy also joins any active run.
        unsafe { bindings::hl_c_backend_destroy(self.0.as_ptr()) };
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::{EngineConfig, Plan};
    use std::{
        ffi::c_void,
        fs::OpenOptions,
        io::{Seek, SeekFrom, Write},
        os::fd::AsRawFd,
        path::PathBuf,
    };

    fn put16(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put64(bytes: &mut [u8], offset: usize, value: u64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn image() -> Vec<u8> {
        let mut bytes = vec![0; 4096];
        bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        put16(&mut bytes, 16, 2);
        put16(&mut bytes, 18, 0xb7);
        put32(&mut bytes, 20, 1);
        put64(&mut bytes, 24, 0x40_0100);
        put64(&mut bytes, 32, 64);
        put16(&mut bytes, 52, 64);
        put16(&mut bytes, 54, 56);
        put16(&mut bytes, 56, 1);
        put32(&mut bytes, 64, 1);
        put32(&mut bytes, 68, 5);
        put64(&mut bytes, 72, 0);
        put64(&mut bytes, 80, 0x40_0000);
        put64(&mut bytes, 88, 0x40_0000);
        put64(&mut bytes, 96, 4096);
        put64(&mut bytes, 104, 4096);
        put64(&mut bytes, 112, 4096);
        bytes
    }

    fn inspect(bytes: &[u8]) -> Result<Plan, i32> {
        let path = PathBuf::from(format!(
            "/var/tmp/hl-native-elf-inspect-{}-{:x}",
            std::process::id(),
            bytes.as_ptr() as usize
        ));
        let mut file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        file.write_all(bytes).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        let config = EngineConfig {
            isa: 1,
            rootfs: None,
            executable_host: None,
            executable_fd: file.as_raw_fd(),
            option_names: &[],
            option_values: &[],
            standard_fds: [-1; 3],
            provider_fd: -1,
            syscall_context: std::ptr::null_mut::<c_void>(),
            syscall_dispatch: None,
        };
        let result = Plan::inspect(&config);
        std::fs::remove_file(path).unwrap();
        result
    }

    #[test]
    fn executable_markers_cannot_change_generic_plan() {
        let plain = image();
        let mut marked = plain.clone();
        marked[0x260..0x26e].copy_from_slice(b"\xff Go buildinf:");
        marked[0x340..0x348].copy_from_slice(b"v8_blob_");
        assert_eq!(inspect(&plain), inspect(&marked));
    }

    #[test]
    fn malformed_load_segment_is_rejected_before_native_loader() {
        let mut bytes = image();
        put64(&mut bytes, 96, 4097);
        assert!(inspect(&bytes).is_err(), "p_filesz larger than p_memsz was accepted");

        let mut bytes = image();
        put64(&mut bytes, 72, 4096);
        assert!(
            inspect(&bytes).is_err(),
            "PT_LOAD bytes outside the image were accepted"
        );

        let mut bytes = image();
        put64(&mut bytes, 24, 0x40_1000);
        assert!(
            inspect(&bytes).is_err(),
            "entry outside an executable segment was accepted"
        );
    }
}
