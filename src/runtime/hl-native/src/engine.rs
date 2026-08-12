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
pub struct Create<'a> {
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

#[derive(Clone, Copy)]
#[repr(C)]
struct MainImagePlan {
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Exit {
    pub kind: u32,
    pub status: i32,
    pub detail: u64,
}

/// Unique owner of a native engine instance.
pub struct Engine(NonNull<Backend>);

// The C lifecycle contract permits request from another thread while run is
// active. The handle remains uniquely owned and destroy joins the active run.
unsafe impl Send for Engine {}
unsafe impl Sync for Engine {}

impl Engine {
    /// Creates an engine through the stable C bridge.
    ///
    /// # Safety
    /// `image_plan`, option pointers and callback state must satisfy the C ABI.
    /// Borrowed create inputs need only remain valid for this call; C copies
    /// configuration. Callback context must remain valid until this value drops.
    pub unsafe fn create(config: Create<'_>) -> Result<Self, i32> {
        if config.option_names.len() != config.option_values.len() {
            return Err(STATUS_OK.wrapping_add(1));
        }
        let count = c_uint::try_from(config.option_names.len()).map_err(|_| 1)?;
        let image_plan = inspect_main_image(&config)?;
        let mut output = std::ptr::null_mut();
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
        let status = unsafe { bindings::hl_c_backend_run(self.0.as_ptr(), count, arguments.as_ptr()) };
        (status == STATUS_OK).then_some(()).ok_or(status)
    }

    pub fn request(&self, request: u32, signal: i32) -> Result<(), i32> {
        let status = unsafe { bindings::hl_c_backend_request(self.0.as_ptr(), request, signal) };
        (status == STATUS_OK).then_some(()).ok_or(status)
    }

    #[must_use]
    pub fn exit(&self) -> Exit {
        Exit {
            kind: unsafe { bindings::hl_c_backend_exit_kind(self.0.as_ptr()) },
            status: unsafe { bindings::hl_c_backend_exit_status(self.0.as_ptr()) },
            detail: unsafe { bindings::hl_c_backend_exit_detail(self.0.as_ptr()) },
        }
    }

    #[must_use]
    pub fn translation_count(&self) -> u64 {
        unsafe { bindings::hl_c_backend_translation_count(self.0.as_ptr()) }
    }
}

fn inspect_main_image(config: &Create<'_>) -> Result<MainImagePlan, i32> {
    let mut file = if config.executable_fd >= 0 {
        #[cfg(unix)]
        {
            use std::os::fd::FromRawFd;
            let descriptor = unsafe { libc::dup(config.executable_fd) };
            if descriptor < 0 {
                return Err(1);
            }
            unsafe { File::from_raw_fd(descriptor) }
        }
        #[cfg(not(unix))]
        return Err(3);
    } else {
        let path = config.executable_host.ok_or(1)?;
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            File::open(std::ffi::OsStr::from_bytes(path.to_bytes())).map_err(|_| 1)?
        }
        #[cfg(not(unix))]
        return Err(3);
    };
    let mut header = [0_u8; 64];
    file.read_exact(&mut header).map_err(|_| 1)?;
    if &header[..6] != b"\x7fELF\x02\x01" {
        return Err(1);
    }
    let word16 = |offset| u16::from_le_bytes(header[offset..offset + 2].try_into().expect("fixed header"));
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
    let phoff = word64(32);
    let phentsize = u64::from(word16(54));
    let phnum = word16(56);
    if phentsize < 56 || phnum == 0 {
        return Err(1);
    }
    let mut first = u64::MAX;
    let mut last = 0_u64;
    let mut interpreter = None;
    for index in 0..phnum {
        file.seek(SeekFrom::Start(phoff + u64::from(index) * phentsize))
            .map_err(|_| 1)?;
        let mut program = [0_u8; 56];
        file.read_exact(&mut program).map_err(|_| 1)?;
        let u32_at = |offset| u32::from_le_bytes(program[offset..offset + 4].try_into().expect("program header"));
        let u64_at = |offset| u64::from_le_bytes(program[offset..offset + 8].try_into().expect("program header"));
        match u32_at(0) {
            1 => {
                let start = u64_at(16);
                let end = start.checked_add(u64_at(40)).ok_or(1)?;
                first = first.min(start);
                last = last.max(end);
            }
            3 => {
                let size = usize::try_from(u64_at(32)).map_err(|_| 1)?;
                if size == 0 || size > 4096 {
                    return Err(1);
                }
                let mut path = vec![0; size];
                file.seek(SeekFrom::Start(u64_at(8))).map_err(|_| 1)?;
                file.read_exact(&mut path).map_err(|_| 1)?;
                if path.last() == Some(&0) {
                    path.pop();
                }
                interpreter = Some(path);
            }
            _ => {}
        }
    }
    let link_start = first & !0xfff;
    let span = last.checked_sub(link_start).ok_or(1)?;
    let link_end = link_start
        .checked_add(span.checked_add(0xffff).ok_or(1)? & !0xffff)
        .ok_or(1)?;
    let interpreter_identity = interpreter.as_deref().map_or(0, |path| {
        path.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
        })
    });
    Ok(MainImagePlan {
        abi: 1,
        size: u32::try_from(std::mem::size_of::<MainImagePlan>()).expect("small ABI struct"),
        architecture: config.isa,
        kind,
        link_start,
        link_end,
        has_interpreter: u32::from(interpreter.is_some()),
        flags: 0,
        interpreter_identity,
    })
}

impl Drop for Engine {
    fn drop(&mut self) {
        unsafe { bindings::hl_c_backend_destroy(self.0.as_ptr()) };
    }
}
