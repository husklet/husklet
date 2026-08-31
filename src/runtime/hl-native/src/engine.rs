#![allow(unsafe_code)]

use std::{
    ffi::{c_char, c_int, c_uint},
    fs::File,
    io::{Seek, SeekFrom},
    ptr::NonNull,
};

use crate::bindings::{self, Backend};

#[cfg(unix)]
mod image;
#[cfg(unix)]
use image::pin_guest_image;
#[cfg(all(test, unix))]
use image::{resolve_layered_guest, resolve_through_merged_directory_symlink};
mod layout;
use layout::validate_elf_image;

pub use crate::bindings::{EngineBoxConfig, EngineNetworkInterface, EnginePublishRule};

pub const STATUS_OK: i32 = 0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Load(crate::LoadKind),
    Status(i32),
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Load(kind) => write!(formatter, "native library load failed: {kind:?}"),
            Self::Status(status) => write!(formatter, "native engine status {status}"),
        }
    }
}

impl std::error::Error for Error {}

/// Low-level creation arguments for the native engine.
///
/// Strings, arrays, image descriptors, and standard descriptors are borrowed
/// for the duration of creation. `provider_fd`, when nonnegative, transfers
/// ownership to create even though provider transport is currently unsupported.
#[derive(Clone, Copy)]
pub struct EngineConfig<'a> {
    pub isa: u32,
    pub rootfs: Option<&'a std::ffi::CStr>,
    pub executable_host: Option<&'a std::ffi::CStr>,
    pub executable_fd: i32,
    pub option_names: &'a [*const c_char],
    pub option_values: &'a [*const c_char],
    pub box_config: Option<&'a EngineBoxConfig>,
    pub standard_fds: [i32; 3],
    pub provider_fd: i32,
}

type Plan = crate::bindings::MainImagePlan;

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
// SAFETY: the shared Rust surface is limited to run (C lifecycle-gated), request
// (C lifecycle-locked), and exit (one bridge-locked snapshot). Checkpoint
// configuration and destruction require exclusive Rust access.
unsafe impl Sync for Engine {}

impl Engine {
    #[cfg(unix)]
    pub fn configure_checkpoint(&mut self, transport: &crate::CheckpointTransport) -> Result<(), i32> {
        let status = transport.configure(self.0.as_ptr());
        (status == STATUS_OK).then_some(()).ok_or(status)
    }
    /// Creates an engine through the stable C bridge.
    ///
    /// # Safety
    /// Option pointers must satisfy the C ABI.
    /// Borrowed create inputs need only remain valid for this call; C copies
    /// configuration.
    pub unsafe fn create(config: EngineConfig<'_>) -> Result<Self, Error> {
        if let Err(error) = crate::loader::api() {
            consume_provider(config.provider_fd);
            return Err(Error::Load(error.kind()));
        }
        // SAFETY: forwarded unchanged; the hook does not observe raw inputs.
        unsafe { Self::create_after_pinning(config, || {}) }.map_err(Error::Status)
    }

    unsafe fn create_after_pinning(config: EngineConfig<'_>, after_pin: impl FnOnce()) -> Result<Self, i32> {
        if config.option_names.len() != config.option_values.len() {
            return Err(STATUS_OK.wrapping_add(1));
        }
        let count = c_uint::try_from(config.option_names.len()).map_err(|_| 1)?;
        #[cfg(unix)]
        let pinned_executable = open_main_image(&config)?;
        #[cfg(unix)]
        let config = {
            use std::os::fd::AsRawFd as _;
            EngineConfig {
                executable_fd: pinned_executable.as_raw_fd(),
                ..config
            }
        };
        after_pin();
        let image_plan = Plan::inspect(&config)?;
        #[cfg(unix)]
        let interpreter_image = Plan::interpreter(&config)?
            .map(|path| pin_guest_image(&config, &path))
            .transpose()?;
        #[cfg(unix)]
        if let Some(image) = interpreter_image.as_deref() {
            validate_elf_image(&mut std::io::Cursor::new(image), image.len() as u64, config.isa)?;
        }
        #[cfg(not(unix))]
        let interpreter_image: Option<Vec<u8>> = None;
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
                &raw const image_plan,
                interpreter_image
                    .as_deref()
                    .map_or(std::ptr::null(), |image| image.as_ptr().cast()),
                interpreter_image.as_deref().map_or(0, <[u8]>::len),
                count,
                config.option_names.as_ptr(),
                config.option_values.as_ptr(),
                config.box_config.map_or(std::ptr::null(), std::ptr::from_ref),
                config.standard_fds.as_ptr(),
                config.provider_fd,
                std::ptr::null_mut(),
                None,
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

    /// The container-namespace pid of the guest process this engine launched.
    ///
    /// `None` until the launched process has published its container identity, and again once it has
    /// been reaped. A checkpoint image names each captured member by exactly this number and a restore
    /// re-forks it under the same one, so it is the only identity of a launched guest that survives a
    /// whole-image capture.
    #[must_use]
    pub fn guest_pid(&self) -> Option<std::num::NonZeroI32> {
        // SAFETY: `self` owns a live backend. The C side reads one atomically published field of a
        // shared mapping under the engine lock and retains nothing.
        std::num::NonZeroI32::new(unsafe { bindings::hl_c_backend_guest_pid(self.0.as_ptr()) })
    }

    #[must_use]
    pub fn exit(&self) -> Exit {
        let mut result = bindings::EngineExit {
            abi: 5,
            size: u32::try_from(std::mem::size_of::<bindings::EngineExit>()).expect("small ABI struct"),
            kind: 0,
            guest_status: 0,
            detail: 0,
        };
        // SAFETY: `self` owns a live backend and `result` is writable. The C
        // bridge publishes and copies the complete record under one lock, so a
        // concurrent run cannot race or expose a torn group of fields.
        let status = unsafe { bindings::hl_c_backend_exit(self.0.as_ptr(), &raw mut result) };
        debug_assert_eq!(status, STATUS_OK);
        Exit {
            kind: result.kind,
            status: result.guest_status,
            detail: result.detail,
        }
    }
}

fn consume_provider(descriptor: i32) {
    if descriptor < 0 {
        return;
    }
    #[cfg(unix)]
    {
        // SAFETY: every nonnegative provider descriptor transfers to create, including loader failure.
        unsafe { libc::close(descriptor) };
    }
    #[cfg(windows)]
    {
        unsafe extern "C" {
            fn _close(descriptor: i32) -> i32;
        }
        // SAFETY: the bridge contract defines provider_fd as a C-runtime descriptor on Windows.
        unsafe { _close(descriptor) };
    }
}

impl Plan {
    fn inspect(config: &EngineConfig<'_>) -> Result<Self, i32> {
        let mut file = open_main_image(config)?;
        file.seek(SeekFrom::Start(0)).map_err(|_| 1)?;
        let image_length = file.metadata().map_err(|_| 1)?.len();
        let (kind, layout) = validate_elf_image(&mut file, image_length, config.isa)?;
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
            flags: main_image_flags(kind),
            interpreter_identity,
        })
    }

    #[cfg(unix)]
    fn interpreter(config: &EngineConfig<'_>) -> Result<Option<Vec<u8>>, i32> {
        let mut file = open_main_image(config)?;
        file.seek(SeekFrom::Start(0)).map_err(|_| 1)?;
        let image_length = file.metadata().map_err(|_| 1)?.len();
        validate_elf_image(&mut file, image_length, config.isa).map(|(_, layout)| layout.interpreter)
    }
}

fn main_image_flags(kind: u32) -> u32 {
    // Linux can give an ET_EXEC the same fixed address it has under native exec, using
    // MAP_FIXED_NOREPLACE so an engine mapping is never overwritten. Besides avoiding a second address
    // domain, this is required by the x86 transliterator. Hosts whose low image range is unavailable keep
    // the generic displaced projection used by the interpreter and checkpoint restore.
    if cfg!(target_os = "linux") {
        0
    } else {
        u32::from(kind == 1)
    }
}

fn open_main_image(config: &EngineConfig<'_>) -> Result<File, i32> {
    if config.executable_fd >= 0 {
        #[cfg(unix)]
        {
            use std::os::fd::BorrowedFd;
            // SAFETY: the descriptor is borrowed only for this duplication
            // call; invalid descriptors are reported by `try_clone_to_owned`.
            let descriptor = unsafe { BorrowedFd::borrow_raw(config.executable_fd) }
                .try_clone_to_owned()
                .map_err(|_| 1)?;
            return Ok(File::from(descriptor));
        }
        #[cfg(not(unix))]
        return Err(3);
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let path = config.executable_host.ok_or(1)?;
        File::open(std::ffi::OsStr::from_bytes(path.to_bytes())).map_err(|_| 1)
    }
    #[cfg(not(unix))]
    return Err(3);
}

/// Inspect one already-resolved host executable and return its guest interpreter path.
/// This performs the same strict ELF validation used by engine creation.
#[cfg(unix)]
pub fn executable_interpreter(path: &std::path::Path, isa: u32) -> Result<Option<Vec<u8>>, Error> {
    let mut file = File::open(path).map_err(|_| Error::Status(1))?;
    let length = file.metadata().map_err(|_| Error::Status(1))?.len();
    validate_elf_image(&mut file, length, isa)
        .map(|(_, layout)| layout.interpreter)
        .map_err(Error::Status)
}

#[cfg(unix)]
fn launch_roots(config: &EngineConfig<'_>) -> Result<Vec<File>, i32> {
    use std::os::unix::{ffi::OsStrExt as _, fs::OpenOptionsExt as _};
    let mut roots = Vec::new();
    let open = |path: &std::ffi::OsStr| {
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .map_err(|_| 1)
    };
    if let Some(root) = config.rootfs {
        roots.push(open(std::ffi::OsStr::from_bytes(root.to_bytes()))?);
    }
    for (&name, &value) in config.option_names.iter().zip(config.option_values) {
        if name.is_null() || value.is_null() {
            return Err(1);
        }
        // SAFETY: EngineConfig guarantees live NUL-terminated option strings.
        let name = unsafe { std::ffi::CStr::from_ptr(name) };
        if name.to_bytes() != b"HL_LOWER" {
            continue;
        }
        // SAFETY: same EngineConfig string contract as the name above.
        for record in unsafe { std::ffi::CStr::from_ptr(value) }
            .to_bytes()
            .split(|byte| *byte == b'\n')
            .filter(|record| !record.is_empty())
        {
            roots.push(open(std::ffi::OsStr::from_bytes(record))?);
        }
    }
    Ok(roots)
}

#[cfg(unix)]
enum EntryKind {
    Directory,
    Regular,
    Symlink(std::path::PathBuf),
}

#[cfg(unix)]
fn entry_is_opaque(root: &File, parts: &[std::ffi::OsString]) -> bool {
    open_components(root, parts, true).ok().is_some_and(|directory| {
        entry_mode(&directory, std::ffi::OsStr::new(".wh..wh..opq"))
            .ok()
            .flatten()
            .is_some()
    })
}

#[cfg(unix)]
fn layered_entry(parts: &[std::ffi::OsString], roots: &[File]) -> std::io::Result<Option<(usize, EntryKind)>> {
    let Some((leaf, parent)) = parts.split_last() else {
        return Ok(None);
    };
    let mut whiteout = std::ffi::OsString::from(".wh.");
    whiteout.push(leaf);
    for (index, root) in roots.iter().enumerate() {
        let directory = match open_components(root, parent, true) {
            Ok(directory) => directory,
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => continue,
            Err(error) => return Err(error),
        };
        if entry_mode(&directory, &whiteout)?.is_some() {
            return Ok(None);
        }
        if let Some(mode) = entry_mode(&directory, leaf)? {
            let kind = if mode & libc::S_IFMT == libc::S_IFLNK {
                EntryKind::Symlink(read_link(&directory, leaf)?)
            } else if mode & libc::S_IFMT == libc::S_IFDIR {
                EntryKind::Directory
            } else if mode & libc::S_IFMT == libc::S_IFREG {
                EntryKind::Regular
            } else {
                return Ok(None);
            };
            return Ok(Some((index, kind)));
        }
        if entry_mode(&directory, std::ffi::OsStr::new(".wh..wh..opq"))?.is_some() {
            return Ok(None);
        }
    }
    Ok(None)
}

#[cfg(unix)]
fn open_components(root: &File, parts: &[std::ffi::OsString], directory: bool) -> std::io::Result<File> {
    use std::os::{
        fd::{AsRawFd as _, FromRawFd as _},
        unix::ffi::OsStrExt as _,
    };
    // SAFETY: root is live and fcntl receives no pointer arguments.
    let duplicate = unsafe { libc::fcntl(root.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: fcntl returned a new owned descriptor.
    let mut current = unsafe { File::from_raw_fd(duplicate) };
    for (index, part) in parts.iter().enumerate() {
        let name = std::ffi::CString::new(part.as_bytes())
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
        let final_part = index + 1 == parts.len();
        let flags = libc::O_RDONLY
            | libc::O_NOFOLLOW
            | libc::O_CLOEXEC
            | if !final_part || directory { libc::O_DIRECTORY } else { 0 };
        // SAFETY: current is live and name is NUL-terminated for this call.
        let descriptor = unsafe { libc::openat(current.as_raw_fd(), name.as_ptr(), flags) };
        if descriptor < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: openat returned a new owned descriptor.
        current = unsafe { File::from_raw_fd(descriptor) };
    }
    Ok(current)
}

#[cfg(unix)]
fn entry_mode(directory: &File, name: &std::ffi::OsStr) -> std::io::Result<Option<libc::mode_t>> {
    use std::{
        mem::MaybeUninit,
        os::{fd::AsRawFd as _, unix::ffi::OsStrExt as _},
    };
    let name =
        std::ffi::CString::new(name.as_bytes()).map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let mut metadata = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: inputs are live and metadata is writable for one stat.
    let status = unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            metadata.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if status == 0 {
        // SAFETY: successful fstatat initialized metadata.
        return Ok(Some(unsafe { metadata.assume_init().st_mode }));
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ENOENT) {
        Ok(None)
    } else {
        Err(error)
    }
}

#[cfg(unix)]
fn read_link(directory: &File, name: &std::ffi::OsStr) -> std::io::Result<std::path::PathBuf> {
    use std::os::{
        fd::AsRawFd as _,
        unix::ffi::{OsStrExt as _, OsStringExt as _},
    };
    let name =
        std::ffi::CString::new(name.as_bytes()).map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let mut bytes = vec![0_u8; 4096];
    // SAFETY: inputs stay live and bytes is writable for its initialized length.
    let count = unsafe {
        libc::readlinkat(
            directory.as_raw_fd(),
            name.as_ptr(),
            bytes.as_mut_ptr().cast(),
            bytes.len(),
        )
    };
    if count < 0 || count as usize == bytes.len() {
        return Err(if count < 0 {
            std::io::Error::last_os_error()
        } else {
            std::io::Error::from_raw_os_error(libc::ENAMETOOLONG)
        });
    }
    bytes.truncate(count as usize);
    Ok(std::path::PathBuf::from(std::ffi::OsString::from_vec(bytes)))
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
    use super::{Engine, EngineConfig, Exit, Plan, resolve_layered_guest};
    use std::{
        ffi::CString,
        fs::{File, OpenOptions},
        io::{Read as _, Seek, SeekFrom, Write},
        os::{
            fd::{AsRawFd, FromRawFd, OwnedFd},
            unix::fs::PermissionsExt as _,
        },
    };

    #[cfg(feature = "native-test-hooks")]
    use std::time::{Duration, Instant};

    #[cfg(all(feature = "native-test-hooks", target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn transliteration_lowering_and_nonwrapping_body_owners_are_exact() {
        let _serial = engine_test_lock();
        let hook = crate::loader::tests()
            .expect("native test bridge")
            .x86_64_translit_displaced;
        for scenario in 1..=88 {
            // SAFETY: the hook accepts one bounded scalar selector and owns no external state.
            assert_eq!(unsafe { hook(scenario) }, 0, "scenario {scenario}");
        }
    }

    #[cfg(all(feature = "native-test-hooks", target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn unresolved_constant_jcc_ibtc_lifecycle_is_exact() {
        let _serial = engine_test_lock();
        let hook = crate::loader::tests()
            .expect("native test bridge")
            .x86_64_translit_displaced;
        for scenario in 89..=100 {
            // SAFETY: the hook accepts one bounded scalar selector and isolates mutable engine state in a child.
            assert_eq!(unsafe { hook(scenario) }, 0, "JCC IBTC scenario {scenario}");
        }
    }

    #[cfg(all(feature = "native-test-hooks", target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn unresolved_direct_jmp_ibtc_lifecycle_is_exact() {
        let _serial = engine_test_lock();
        let hook = crate::loader::tests()
            .expect("native test bridge")
            .x86_64_translit_displaced;
        for scenario in 101..=111 {
            // SAFETY: the hook accepts one bounded scalar selector and isolates mutable engine state in a child.
            assert_eq!(unsafe { hook(scenario) }, 0, "direct JMP IBTC scenario {scenario}");
        }
    }

    #[cfg(all(feature = "native-test-hooks", target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn translated_fallthrough_chains_preserve_fs_transactions_and_irq_polling() {
        let _serial = engine_test_lock();
        let hook = crate::loader::tests()
            .expect("native test bridge")
            .x86_64_translit_displaced;
        for scenario in 198..=199 {
            // SAFETY: the hook accepts one bounded scalar selector and isolates mutable engine state in a child.
            assert_eq!(unsafe { hook(scenario) }, 0, "fallthrough IBTC scenario {scenario}");
        }
    }

    #[cfg(all(feature = "native-test-hooks", target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn indirect_jumps_and_calls_fill_and_hit_the_shared_target_cache() {
        let _serial = engine_test_lock();
        let hook = crate::loader::tests()
            .expect("native test bridge")
            .x86_64_translit_displaced;
        for scenario in [200, 202, 203, 204] {
            // SAFETY: the hook accepts one bounded scalar selector and isolates mutable engine state in a child.
            assert_eq!(unsafe { hook(scenario) }, 0, "indirect IBTC scenario {scenario}");
        }
        // Scenario 201 is the unresolved-indirect marker-5 hit arm. Keep it
        // explicit so an independently-added selector cannot hide it again.
        assert_eq!(unsafe { hook(201) }, 0, "indirect IBTC scenario 201");
    }

    #[cfg(all(feature = "native-test-hooks", target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn translation_reuses_the_authoritative_decoder_bytes() {
        let _serial = engine_test_lock();
        let hook = crate::loader::tests()
            .expect("native test bridge")
            .x86_64_translit_displaced;
        // SAFETY: scenario 205 owns bounded mappings and restores all process-global test state.
        assert_eq!(unsafe { hook(205) }, 0, "single-fetch scenario 205");
    }

    #[test]
    fn transliteration_exported_selectors_do_not_overlap() {
        let _serial = engine_test_lock();
        let source = include_str!("native/translator/guest/x86_64/translit.inc");
        let start = source
            .find("HL_API int hl_x86_64_translit_displaced_test(uint32_t scenario) {")
            .expect("x86 transliteration test export");
        let mut depth = 0_i32;
        let mut selectors = std::collections::BTreeMap::<u32, &str>::new();
        for line in source[start..].lines() {
            depth += line.matches('{').count() as i32;
            depth -= line.matches('}').count() as i32;
            if line.starts_with("    if (scenario") {
                let mut values = Vec::new();
                if let Some(rest) = line.split("scenario >= ").nth(1) {
                    let low = rest.split_whitespace().next().unwrap().parse::<u32>().unwrap();
                    let high = line
                        .split("scenario <= ")
                        .nth(1)
                        .unwrap()
                        .split(|character: char| !character.is_ascii_digit())
                        .next()
                        .unwrap()
                        .parse::<u32>()
                        .unwrap();
                    values.extend(low..=high);
                } else {
                    for rest in line.split("scenario == ").skip(1) {
                        values.push(
                            rest.split(|character: char| !character.is_ascii_digit())
                                .next()
                                .unwrap()
                                .parse::<u32>()
                                .unwrap(),
                        );
                    }
                }
                for selector in values {
                    assert!(
                        selectors.insert(selector, line).is_none(),
                        "duplicate exported transliteration selector {selector}"
                    );
                }
            }
            if depth == 0 {
                break;
            }
        }
        for selector in [
            190, 192, 198, 199, 200, 201, 202, 203, 204, 205, 206, 207, 208, 209, 210, 211, 212, 213, 214, 215, 216,
        ] {
            assert!(selectors.contains_key(&selector), "unbound selector {selector}");
        }
    }

    #[cfg(all(feature = "native-test-hooks", target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn cache_census_prefix_has_exact_signal_stage_ownership() {
        let _serial = engine_test_lock();
        let hook = crate::loader::tests()
            .expect("native test bridge")
            .x86_64_translit_displaced;
        assert_eq!(unsafe { hook(206) }, 0, "cache census signal stages");
    }

    #[cfg(all(feature = "native-test-hooks", target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn helper_address_relocation_forms_are_authenticated() {
        let _serial = engine_test_lock();
        let hook = crate::loader::tests()
            .expect("native test bridge")
            .x86_64_translit_displaced;
        // SAFETY: selector 207 checks fixed local instruction bytes and owns no external state.
        assert_eq!(unsafe { hook(207) }, 0, "helper relocation scenario 207");
    }

    #[cfg(all(feature = "native-test-hooks", target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn jcc_shared_guard_preserves_cache_lifecycle_and_signal_contracts() {
        let _serial = engine_test_lock();
        let hook = crate::loader::tests()
            .expect("native test bridge")
            .x86_64_translit_displaced;
        for selector in 208..=216 {
            // SAFETY: each selector runs in a fixture-owned subprocess and arena.
            assert_eq!(unsafe { hook(selector) }, 0, "JCC shared guard scenario {selector}");
        }
    }

    #[cfg(all(feature = "native-test-hooks", target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn persistent_cache_codegen_modes_are_canonical() {
        let _serial = engine_test_lock();
        let hook = crate::loader::tests()
            .expect("native test bridge")
            .x86_64_translit_displaced;
        // SAFETY: scenario 150 mutates only the child-local option table and restores every entry.
        assert_eq!(unsafe { hook(150) }, 0, "persistent-cache codegen-mode scenario 150");
    }

    #[cfg(all(feature = "native-test-hooks", target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn ret_target_cache_simulation_is_observational_and_exact() {
        let _serial = engine_test_lock();
        let hook = crate::loader::tests()
            .expect("native test bridge")
            .x86_64_translit_displaced;
        let mut failures = Vec::new();
        for scenario in (112..=126)
            .chain(131..=134)
            .chain(std::iter::once(130))
            .chain(128..=129)
            .chain(std::iter::once(127))
            .chain(std::iter::once(135))
            .chain(std::iter::once(136))
        {
            // SAFETY: the hook accepts one bounded selector and forks before touching simulation state.
            let status = unsafe { hook(scenario) };
            if status != 0 {
                failures.push((scenario, status));
            }
        }
        assert!(failures.is_empty(), "RET IBTC scenarios failed: {failures:?}");
    }

    #[cfg(all(feature = "native-test-hooks", target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn unresolved_direct_call_ibtc_lifecycle_is_exact() {
        let _serial = engine_test_lock();
        let hook = crate::loader::tests()
            .expect("native test bridge")
            .x86_64_translit_displaced;
        for scenario in 137..=147 {
            // SAFETY: the hook accepts one bounded scalar selector and isolates mutable engine state in a child.
            assert_eq!(unsafe { hook(scenario) }, 0, "direct CALL IBTC scenario {scenario}");
        }
    }

    #[cfg(feature = "native-test-hooks")]
    struct IsolatedTestChild(Option<std::process::Child>);

    #[cfg(feature = "native-test-hooks")]
    impl IsolatedTestChild {
        fn spawn(mut command: std::process::Command) -> std::io::Result<Self> {
            use std::os::unix::process::CommandExt as _;

            // SAFETY: setsid is async-signal-safe and touches no Rust-owned state.
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() < 0 {
                        Err(std::io::Error::last_os_error())
                    } else {
                        Ok(())
                    }
                });
            }
            command.spawn().map(|child| Self(Some(child)))
        }

        fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
            self.0.as_mut().expect("live isolated child").try_wait()
        }

        fn terminate(&mut self) -> std::io::Result<()> {
            self.terminate_with(|group| {
                // SAFETY: spawn made the child a session and process-group leader, so the
                // negative identifier is confined to this test's descendants.
                if unsafe { libc::kill(group, libc::SIGKILL) } < 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            })
        }

        fn terminate_with(&mut self, deliver: impl FnOnce(i32) -> std::io::Result<()>) -> std::io::Result<()> {
            let Some(child) = self.0.as_mut() else {
                return Ok(());
            };
            let process = i32::try_from(child.id())
                .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "child PID exceeds i32"))?;
            if let Err(error) = deliver(-process)
                && error.raw_os_error() != Some(libc::ESRCH)
            {
                return Err(error);
            }
            // Once delivery succeeds (or the group is already absent), retaining the
            // numeric PID would let a later Drop signal an unrelated, reused group.
            let mut child = self.0.take().expect("live isolated child");
            child.wait().map(drop)
        }
    }

    #[cfg(feature = "native-test-hooks")]
    impl Drop for IsolatedTestChild {
        fn drop(&mut self) {
            let _ = self.terminate();
        }
    }

    #[cfg(feature = "native-test-hooks")]
    #[test]
    fn isolated_test_child_termination_is_retryable_idempotent_and_closes_descendant_descriptors() {
        let _serial = engine_test_lock();
        use std::io::BufRead as _;
        use std::process::Stdio;

        let mut command = std::process::Command::new("sh");
        command.args(["-c", "sleep 60 & echo $!; wait"]).stdout(Stdio::piped());
        let mut child = IsolatedTestChild::spawn(command).unwrap();
        let output = child.0.as_mut().unwrap().stdout.take().unwrap();
        let mut output = std::io::BufReader::new(output);
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        let descendant = line.trim().parse::<i32>().unwrap();
        // SAFETY: signal zero only probes the PID printed by the live test child.
        assert_eq!(
            unsafe { libc::kill(descendant, 0) },
            0,
            "descendant was not live before cleanup"
        );

        let mut deliveries = 0;
        assert_eq!(
            child
                .terminate_with(|_| {
                    deliveries += 1;
                    Err(std::io::Error::from_raw_os_error(libc::EPERM))
                })
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EPERM)
        );
        child
            .terminate_with(|group| {
                deliveries += 1;
                // SAFETY: the helper supplied the negative identifier of its isolated group.
                if unsafe { libc::kill(group, libc::SIGKILL) } < 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            })
            .unwrap();
        child
            .terminate_with(|_| {
                deliveries += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(deliveries, 2, "termination was not retryable and idempotent");
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            sender
                .send(output.bytes().collect::<std::io::Result<Vec<_>>>())
                .unwrap();
        });
        let closed = receiver.recv_timeout(Duration::from_secs(2));
        if closed.is_err() {
            // SAFETY: this is the exact PID reported by the deliberately spawned descendant.
            unsafe { libc::kill(descendant, libc::SIGKILL) };
        }
        assert_eq!(
            closed.unwrap().unwrap(),
            Vec::<u8>::new(),
            "a descendant retained the inherited descriptor"
        );
    }

    /// A coordinator must find a peer that has made itself a session leader.
    ///
    /// This is the POSITIVE half of checkpoint membership, and it is the half no in-memory broker test
    /// can state: the engine emulates the guest's `setsid(2)` with the host's, so a guest that leads a
    /// session -- every `PostgreSQL` backend, every shell job -- has its own host session id. While peer
    /// enumeration also required a matching session, a live cluster produced ZERO peers, the coordinator
    /// published a one-process manifest, and the eight real members that arrived afterwards were refused
    /// at `REGISTER_READY` because the capture they belonged to had already finished. The negative
    /// ("an unregistered publisher is refused") was tested; this direction was not.
    #[cfg(all(feature = "native-test-hooks", target_os = "linux"))]
    #[test]
    fn a_peer_that_leads_its_own_session_is_still_enumerated_as_a_peer() {
        let _serial = engine_test_lock();
        let mut ready = [-1; 2];
        let mut release = [-1; 2];
        // SAFETY: both arrays name writable storage for two new descriptors each.
        assert_eq!(unsafe { libc::pipe(ready.as_mut_ptr()) }, 0);
        // SAFETY: as above.
        assert_eq!(unsafe { libc::pipe(release.as_mut_ptr()) }, 0);

        // SAFETY: the child touches only async-signal-safe calls on inherited descriptors -- no
        // allocation, no lock, no Rust destructor walk -- because it is forked out of a multi-threaded
        // test binary and any other work can deadlock against a lock held at fork time.
        let child = unsafe { libc::fork() };
        assert!(child >= 0, "fork failed");
        if child == 0 {
            // SAFETY: child side; every descriptor below is inherited and owned here.
            unsafe {
                libc::setsid();
                let byte = [1_u8];
                libc::write(ready[1], byte.as_ptr().cast(), 1);
                let mut block = [0_u8; 1];
                libc::read(release[0], block.as_mut_ptr().cast(), 1);
                libc::_exit(0);
            }
        }
        // SAFETY: the parent has no further use for these ends.
        unsafe {
            libc::close(ready[1]);
            libc::close(release[0]);
        }
        let mut byte = [0_u8; 1];
        // SAFETY: byte is writable and the descriptor is owned here.
        let observed = unsafe { libc::read(ready[0], byte.as_mut_ptr().cast(), 1) };

        // SAFETY: `child` is an exact live PID owned by this test.
        let session = unsafe { libc::getsid(child) };
        // SAFETY: the hook reads only this process's own /proc view.
        let enumerated = unsafe { crate::bindings::hl_c_backend_host_process_peer_enumerated_test(child) };

        // SAFETY: releasing and reaping the exact child forked above.
        unsafe {
            let byte = [1_u8];
            libc::write(release[1], byte.as_ptr().cast(), 1);
            let mut status = 0;
            libc::waitpid(child, &raw mut status, 0);
            libc::close(ready[0]);
            libc::close(release[1]);
        }

        assert_eq!(observed, 1, "child never reached setsid");
        assert_eq!(
            session, child,
            "the peer was not its own session leader, so it proves nothing"
        );
        assert_eq!(enumerated, 1, "a session-leading peer was not enumerated");
    }

    #[cfg(feature = "native-test-hooks")]
    #[test]
    fn host_force_stop_kills_exact_activation_group_and_preserves_unrelated_process() {
        let _serial = engine_test_lock();
        use std::io::BufRead as _;
        use std::process::Stdio;

        let mut command = std::process::Command::new("sh");
        command.args(["-c", "sleep 60 & echo $!; wait"]).stdout(Stdio::piped());
        let mut activation = IsolatedTestChild::spawn(command).unwrap();
        let leader = i32::try_from(activation.0.as_ref().unwrap().id()).unwrap();
        let output = activation.0.as_mut().unwrap().stdout.take().unwrap();
        let mut output = std::io::BufReader::new(output);
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        let descendant = line.trim().parse::<i32>().unwrap();
        let mut unrelated = std::process::Command::new("sleep").arg("60").spawn().unwrap();
        let unrelated_pid = i32::try_from(unrelated.id()).unwrap();

        // SAFETY: these are exact live PIDs created and still owned by this test.
        unsafe {
            assert_eq!(libc::getpgid(leader), leader);
            assert_eq!(libc::getpgid(descendant), leader);
            assert_eq!(libc::kill(unrelated_pid, 0), 0);
            assert_eq!(crate::bindings::hl_c_backend_host_process_force_test(leader), 0);
        }
        activation.0.as_mut().unwrap().wait().unwrap();
        activation.0 = None;
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            // SAFETY: signal zero probes only the exact descendant PID printed above.
            if unsafe { libc::kill(descendant, 0) } != 0
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "force-stopped descendant {descendant} remained live"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            unrelated.try_wait().unwrap().is_none(),
            "force stop killed unrelated PID {unrelated_pid}"
        );
        unrelated.kill().unwrap();
        unrelated.wait().unwrap();
        assert_eq!(
            output.bytes().collect::<std::io::Result<Vec<_>>>().unwrap(),
            Vec::<u8>::new()
        );
    }

    #[cfg(feature = "native-test-hooks")]
    #[test]
    fn production_force_stop_is_safe_before_session_ready_and_while_waiting() {
        let _serial = engine_test_lock();
        struct ResumeActivation;
        impl Drop for ResumeActivation {
            fn drop(&mut self) {
                // SAFETY: the test hook only releases the deliberately paused child.
                unsafe { crate::bindings::hl_c_backend_activation_ready_pause(0) };
            }
        }

        let mut unrelated = std::process::Command::new("sleep").arg("60").spawn().unwrap();
        // SAFETY: this arms a test-only pause before the activation child's setsid handshake.
        unsafe { crate::bindings::hl_c_backend_activation_ready_pause(1) };
        let resume = ResumeActivation;
        let (engine, _standard) = create_engine(1);
        let argument = CString::new("guest").unwrap();
        std::thread::scope(|scope| {
            let running = scope.spawn(|| engine.run(&[argument.as_ptr()]));
            engine.request(2, 0).unwrap();
            drop(resume);
            running.join().unwrap().unwrap();
        });
        assert!(
            unrelated.try_wait().unwrap().is_none(),
            "pre-session force stop killed an unrelated process"
        );

        for _ in 0..32 {
            let (engine, _standard) = create_engine(1);
            std::thread::scope(|scope| {
                let running = scope.spawn(|| engine.run(&[argument.as_ptr()]));
                std::thread::yield_now();
                engine.request(2, 0).unwrap();
                running.join().unwrap().unwrap();
            });
            assert!(
                unrelated.try_wait().unwrap().is_none(),
                "force/wait race killed an unrelated process"
            );
        }
        unrelated.kill().unwrap();
        unrelated.wait().unwrap();
    }

    /// Whether the Linux cross compiler these guest images need is actually installed.
    ///
    /// The dev shell provides both cross compilers, and when they are present the coverage MUST run:
    /// it is the only exercise of guest process re-exec on the host the product ships on, so skipping
    /// it there would delete the coverage rather than defer it. Outside the shell the compiler is
    /// genuinely absent and a hard panic reddens a gate for a missing tool rather than for a defect.
    ///
    /// The notice goes to the real stderr descriptor rather than through `eprintln!`, because the test
    /// harness captures Rust-level output and prints it only for a FAILING test -- which would make an
    /// unrun arm indistinguishable from a passing one. A test that quietly does nothing is worse than
    /// one that fails, so the skip names the test and the ISA it left uncovered where it can be seen.
    #[allow(unsafe_code)]
    fn guest_compiler_present(name: &str, test: &str, isa: u32) -> bool {
        if matches!(guest_compiler(name).arg("--version").output(), Ok(result) if result.status.success()) {
            return true;
        }
        let notice = format!(
            "SKIP {test}: ISA {isa} left UNCOVERED -- `{name}` is not installed. \
             Run inside `nix develop`, which provides both Linux cross compilers.\n"
        );
        // SAFETY: a write of an owned, initialized buffer to the process's stderr descriptor. It
        // borrows nothing beyond the call, and a short or failed write is not an error worth acting on.
        unsafe {
            libc::write(2, notice.as_ptr().cast(), notice.len());
        }
        false
    }

    fn guest_compiler(name: &str) -> std::process::Command {
        let mut command = std::process::Command::new(name);
        // The Nix Darwin shell exports host linker flags such as `-lintl`.
        // Linux cross-linkers must not inherit flags for Darwin libraries.
        command.env_remove("NIX_LDFLAGS").env_remove("NIX_LDFLAGS_FOR_BUILD");
        command
    }

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
        // `b .`: a valid AArch64 process that remains live until force-stop.
        bytes[0x100..0x104].copy_from_slice(&0x1400_0000_u32.to_le_bytes());
        bytes
    }

    #[cfg(all(not(feature = "native-test-hooks"), target_os = "linux", target_arch = "x86_64"))]
    fn product_jcc_ibtc_image() -> Vec<u8> {
        let mut bytes = image();
        put16(&mut bytes, 18, 0x3e);
        // The first execution misses because the taken target is not published. The target
        // returns to the already-published source once, so ON subsequently hits while OFF
        // consumes and suppresses a second miss. Both arms execute identical probe bytes.
        bytes[0x100..0x106].copy_from_slice(&[0x31, 0xc0, 0x74, 0x0c, 0x0f, 0x0b]);
        bytes[0x110..0x122].copy_from_slice(&[
            0xff, 0xc1, 0x83, 0xf9, 0x02, 0x7c, 0xe9, 0x0f, 0xa2, 0xb8, 0x3c, 0, 0, 0, 0x31, 0xff, 0x0f, 0x05,
        ]);
        bytes
    }

    #[cfg(all(not(feature = "native-test-hooks"), target_os = "linux", target_arch = "x86_64"))]
    fn run_product_diagnostic(
        bytes: Vec<u8>,
        disabled: bool,
        translit: bool,
    ) -> std::collections::BTreeMap<String, u64> {
        let mut executable = tempfile::tempfile().unwrap();
        executable.write_all(&bytes).unwrap();
        executable.seek(SeekFrom::Start(0)).unwrap();
        let mut output = tempfile::tempfile().unwrap();
        let pcache = tempfile::tempdir().unwrap();
        let names = [
            CString::new("HL_TRANSLIT").unwrap(),
            CString::new("HL_C_DIAGNOSTICS").unwrap(),
            CString::new("HL_PCACHE").unwrap(),
            CString::new("HL_PCACHE_DIR").unwrap(),
            CString::new("HL_TRANSLIT_JCC_IBTC_DISABLE").unwrap(),
        ];
        let one = CString::new("1").unwrap();
        let zero = CString::new("0").unwrap();
        let pcache_path = CString::new(pcache.path().to_str().unwrap()).unwrap();
        let option_names = names[..if disabled { 5 } else { 4 }]
            .iter()
            .map(|name| name.as_ptr())
            .collect::<Vec<_>>();
        let mut option_values = vec![
            if translit { one.as_ptr() } else { zero.as_ptr() },
            one.as_ptr(),
            one.as_ptr(),
            pcache_path.as_ptr(),
        ];
        if disabled {
            option_values.push(one.as_ptr());
        }
        let config = EngineConfig {
            isa: 2,
            rootfs: None,
            executable_host: None,
            executable_fd: executable.as_raw_fd(),
            option_names: &option_names,
            option_values: &option_values,
            box_config: None,
            standard_fds: [output.as_raw_fd(); 3],
            provider_fd: -1,
        };
        // SAFETY: descriptors, C strings and borrowed slices remain live through create;
        // the bridge imports its own descriptors and copies the launch options.
        let engine = unsafe { Engine::create(config) }.unwrap();
        let argument = CString::new("jcc-ibtc-product-proof").unwrap();
        assert_eq!(engine.run(&[argument.as_ptr()]), Ok(()));
        drop(engine);
        assert_eq!(
            pcache.path().read_dir().unwrap().count(),
            0,
            "diagnostics persisted code"
        );
        output.seek(SeekFrom::Start(0)).unwrap();
        let mut stderr = String::new();
        output.read_to_string(&mut stderr).unwrap();
        let records = stderr
            .lines()
            .filter_map(|line| line.strip_prefix("[diag] backend-shape "))
            .collect::<Vec<_>>();
        assert_eq!(records.len(), 1, "unexpected diagnostic output: {stderr}");
        let mut expected = vec![
            "version",
            "available",
            "crossings",
            "translated_entries",
            "interpreted_entries",
            "translated_steps",
            "interpreted_steps",
            "mixed_sse_executed",
            "mixed_sse_executed_transitions",
            "mixed_sse_disabled_boundaries",
            "jcc_ibtc_enabled",
            "jcc_ibtc_emitted",
            "jcc_ibtc_hits",
            "jcc_ibtc_misses",
            "jcc_ibtc_irq",
            "jcc_ibtc_fills",
            "jcc_ibtc_suppressed",
            "jcc_ibtc_invalid_refusals",
            "direct_jmp_ibtc_enabled",
            "direct_jmp_ibtc_emitted",
            "direct_jmp_ibtc_hits",
            "direct_jmp_ibtc_misses",
            "direct_jmp_ibtc_irq",
            "direct_jmp_ibtc_fills",
            "direct_jmp_ibtc_suppressed",
            "direct_jmp_ibtc_invalid_refusals",
            "direct_call_ibtc_emitted",
            "direct_call_ibtc_hits",
            "direct_call_ibtc_misses",
            "direct_call_ibtc_irq",
            "direct_call_ibtc_fills",
            "direct_call_ibtc_invalid_refusals",
            "ret_ibtc_attempts",
            "ret_ibtc_hits",
            "ret_ibtc_key_misses",
            "ret_ibtc_null_misses",
            "ret_ibtc_irq",
            "ret_ibtc_fills",
            "ret_ibtc_collisions",
            "ret_ibtc_unmapped",
            "ret_ibtc_invalid_refusals",
            "ret_fast_ibtc_hits",
            "ret_fast_ibtc_misses",
            "ret_fast_ibtc_irq",
            "ret_fast_ibtc_fills",
            "ret_fast_ibtc_invalid_refusals",
            "executed_form_total",
            "executed_form_unique",
            "executed_form_overflow",
        ];
        for rank in 0..16 {
            expected.push(Box::leak(format!("executed_form{rank}_key").into_boxed_str()));
            expected.push(Box::leak(format!("executed_form{rank}_count").into_boxed_str()));
        }
        let mut fields = std::collections::BTreeMap::new();
        for token in records[0].split_whitespace() {
            let (name, value) = token.split_once('=').expect("well-formed diagnostic token");
            assert!(expected.contains(&name), "unexpected diagnostic token {name}: {stderr}");
            let value = value.parse::<u64>().expect("decimal diagnostic value");
            assert!(
                fields.insert(name.to_owned(), value).is_none(),
                "duplicate {name}: {stderr}"
            );
        }
        for name in &expected {
            assert!(fields.contains_key(*name), "missing {name}: {stderr}");
        }
        assert_eq!(fields.len(), expected.len());
        fields
    }

    #[cfg(all(not(feature = "native-test-hooks"), target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn production_nohooks_jcc_ibtc_diagnostics_proves_on_and_off() {
        let _serial = engine_test_lock();
        let on = run_product_diagnostic(product_jcc_ibtc_image(), false, true);
        assert_eq!(on["version"], 7);
        assert_eq!(on["available"], 1);
        assert_eq!(on["jcc_ibtc_enabled"], 1);
        assert_eq!(on["jcc_ibtc_emitted"], 1);
        assert_eq!(on["jcc_ibtc_hits"], 1);
        assert_eq!(on["jcc_ibtc_misses"], 1);
        assert_eq!(on["jcc_ibtc_irq"], 0);
        assert_eq!(on["jcc_ibtc_fills"], 1);
        assert_eq!(on["jcc_ibtc_suppressed"], 0);
        assert_eq!(on["jcc_ibtc_invalid_refusals"], 0);
        assert_eq!(on["executed_form_overflow"], 0);
        assert!(on["executed_form_total"] > 0);
        assert!(on["executed_form_unique"] > 0);
        assert!(on["executed_form_total"] >= on["executed_form_unique"]);

        let off = run_product_diagnostic(product_jcc_ibtc_image(), true, true);
        assert_eq!(off["version"], 7);
        assert_eq!(off["available"], 1);
        assert_eq!(off["jcc_ibtc_enabled"], 0, "{off:?}");
        assert_eq!(off["jcc_ibtc_emitted"], 1);
        assert_eq!(off["jcc_ibtc_hits"], 0);
        assert_eq!(off["jcc_ibtc_misses"], 2);
        assert_eq!(off["jcc_ibtc_irq"], 0);
        assert_eq!(off["jcc_ibtc_fills"], 0);
        assert_eq!(off["jcc_ibtc_suppressed"], 2);
        assert_eq!(off["jcc_ibtc_invalid_refusals"], 0);
    }

    #[cfg(all(not(feature = "native-test-hooks"), target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn production_nohooks_executed_forms_aggregate_across_guest_fork() {
        let _serial = engine_test_lock();
        let mut bytes = image();
        put16(&mut bytes, 18, 0x3e);
        bytes[0x100..0x126].copy_from_slice(&[
            0xb8, 0x39, 0, 0, 0, 0x0f, 0x05, 0x85, 0xc0, 0x74, 0x10, 0x89, 0xc7, 0x31, 0xf6, 0x31, 0xd2, 0x45, 0x31,
            0xd2, 0xb8, 0x3d, 0, 0, 0, 0x0f, 0x05, 0x0f, 0xa2, 0xb8, 0x3c, 0, 0, 0, 0x31, 0xff, 0x0f, 0x05,
        ]);
        let fields = run_product_diagnostic(bytes, false, true);
        assert_eq!(fields["available"], 1);
        assert_eq!(fields["executed_form_overflow"], 0);
        assert!(fields["executed_form_total"] >= 2, "{fields:?}");
        assert!(fields["executed_form_unique"] >= 1, "{fields:?}");
        assert_eq!(fields["executed_form0_count"], 2, "{fields:?}");
    }

    #[cfg(all(not(feature = "native-test-hooks"), target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn production_nohooks_deferred_fault_does_not_publish_executed_form() {
        let _serial = engine_test_lock();
        let mut bytes = image();
        put16(&mut bytes, 18, 0x3e);
        bytes[0x100..0x134].copy_from_slice(&[
            0xb8, 0x39, 0, 0, 0, 0x0f, 0x05, 0x85, 0xc0, 0x74, 0x1b, 0x89, 0xc7, 0x31, 0xf6, 0x31, 0xd2, 0x45, 0x31,
            0xd2, 0xb8, 0x3d, 0, 0, 0, 0x0f, 0x05, 0x0f, 0xa2, 0xb8, 0x3c, 0, 0, 0, 0x31, 0xff, 0x0f, 0x05, 0x31, 0xc0,
            0x0f, 0xae, 0x00, 0xb8, 0x3c, 0, 0, 0, 0x31, 0xff, 0x0f, 0x05,
        ]);
        let fields = run_product_diagnostic(bytes, false, true);
        assert_eq!(fields["executed_form_total"], 1, "{fields:?}");
        assert_eq!(fields["executed_form_unique"], 1, "{fields:?}");
        assert_eq!(fields["executed_form0_count"], 1, "{fields:?}");
    }

    #[cfg(all(not(feature = "native-test-hooks"), target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn production_nohooks_executed_form_table_probes_collisions_then_overflows() {
        let _serial = engine_test_lock();
        let mut bytes = image();
        bytes.resize(0x1_0000, 0);
        put16(&mut bytes, 18, 0x3e);
        let image_size = bytes.len() as u64;
        put64(&mut bytes, 96, image_size);
        put64(&mut bytes, 104, image_size);
        let mut cursor = 0x100;
        for form in 0..4097_u32 {
            if form & 1 != 0 {
                bytes[cursor] = 0x66;
                cursor += 1;
            }
            if form & 2 != 0 {
                bytes[cursor] = 0xf3;
                cursor += 1;
            }
            if form & 4 != 0 {
                bytes[cursor] = 0xf2;
                cursor += 1;
            }
            bytes[cursor] = 0x40 | ((form >> 3) & 15) as u8;
            bytes[cursor + 1] = 0x0f;
            bytes[cursor + 2] = 0x1f;
            bytes[cursor + 3] = 0xc0 | ((form >> 7) & 63) as u8;
            cursor += 4;
        }
        bytes[cursor..cursor + 9].copy_from_slice(&[0xb8, 0x3c, 0, 0, 0, 0x31, 0xff, 0x0f, 0x05]);
        let fields = run_product_diagnostic(bytes, false, false);
        assert_eq!(fields["executed_form_total"], 4100, "{fields:?}");
        assert_eq!(fields["executed_form_unique"], 4096, "{fields:?}");
        assert_eq!(fields["executed_form_overflow"], 4, "{fields:?}");
    }

    #[cfg(all(not(feature = "native-test-hooks"), target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn production_nohooks_async_irq_excludes_staged_rep_completion() {
        let _serial = engine_test_lock();
        const SOURCE: &str = r#"
.global _start
.text
_start:
    lea action(%rip), %rsi
    mov $10, %edi
    xor %edx, %edx
    mov $8, %r10d
    mov $13, %eax
    syscall
    mov $39, %eax
    syscall
    mov %rax, %r12
    mov $57, %eax
    syscall
    test %eax, %eax
    jz child
    mov %eax, %r13d
    lea left(%rip), %rsi
    lea right(%rip), %rdi
    mov $134217728, %ecx
    cld
    repe cmpsb
    mov %r13d, %edi
    xor %esi, %esi
    xor %edx, %edx
    xor %r10d, %r10d
    mov $61, %eax
    syscall
    xor %eax, %eax
    cpuid
    mov $60, %eax
    xor %edi, %edi
    syscall
child:
    lea delay(%rip), %rdi
    xor %esi, %esi
    mov $35, %eax
    syscall
    mov %r12d, %edi
    mov $SIGNAL, %esi
    mov $62, %eax
    syscall
    mov $60, %eax
    xor %edi, %edi
    syscall
handler:
    ret
restorer:
    mov $15, %eax
    syscall
.data
.align 8
action:
    .quad handler
    .quad 0x04000000
    .quad restorer
    .quad 0
delay:
    .quad 0
    .quad 1000000
.bss
.align 16
left: .skip 134217728
right: .skip 134217728
"#;
        if !guest_compiler_present(
            "x86_64-linux-gnu-gcc",
            "production_nohooks_async_irq_excludes_staged_rep_completion",
            2,
        ) {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("irq.S");
        std::fs::write(&source, SOURCE).unwrap();
        let run = |signal: u8| {
            let output = root.path().join(format!("irq-{signal}"));
            let compile = guest_compiler("x86_64-linux-gnu-gcc")
                .args(["-static", "-nostdlib", "-no-pie"])
                .arg(format!("-DSIGNAL={signal}"))
                .arg(&source)
                .arg("-o")
                .arg(&output)
                .output()
                .unwrap();
            assert!(compile.status.success(), "{}", String::from_utf8_lossy(&compile.stderr));
            run_product_diagnostic(std::fs::read(output).unwrap(), false, false)
        };
        let control = run(0);
        let interrupted = run(10);
        assert_eq!(
            control["executed_form_total"],
            interrupted["executed_form_total"] + 1,
            "control={control:?} interrupted={interrupted:?}"
        );
    }

    fn x86_tcsetsf_image(termios: &[u8; 36]) -> Vec<u8> {
        let mut bytes = image();
        put16(&mut bytes, 18, 0x3e);
        // ioctl(0, TCSETSF, 0x400200), then exit_group(ioctl_result).
        bytes[0x100..0x11c].copy_from_slice(&[
            0xb8, 0x10, 0, 0, 0, 0x31, 0xff, 0xbe, 0x04, 0x54, 0, 0, 0xba, 0, 0x02, 0x40, 0, 0x0f, 0x05, 0x89, 0xc7,
            0xb8, 0x3c, 0, 0, 0, 0x0f, 0x05,
        ]);
        bytes[0x200..0x224].copy_from_slice(termios);
        bytes
    }

    fn aarch64_tcsetsf_image(termios: &[u8; 36]) -> Vec<u8> {
        let mut bytes = image();
        // ioctl(0, TCSETSF, 0x400200), then exit_group(ioctl_result). The ADR is relative to
        // 0x400108 and names the image at 0x400200.
        for (offset, instruction) in [
            0xd280_0000_u32, // mov x0, #0
            0xd28a_8081,     // mov x1, #0x5404
            0x1000_07c2,     // adr x2, +0xf8
            0xd280_03a8,     // mov x8, #29 (ioctl)
            0xd400_0001,     // svc #0
            0xd280_0bc8,     // mov x8, #94 (exit_group)
            0xd400_0001,     // svc #0
        ]
        .into_iter()
        .enumerate()
        {
            bytes[0x100 + offset * 4..0x104 + offset * 4].copy_from_slice(&instruction.to_le_bytes());
        }
        bytes[0x200..0x224].copy_from_slice(termios);
        bytes
    }

    fn engine_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn every_engine_test_serializes_process_global_native_state() {
        let _serial = engine_test_lock();
        let source = include_str!("engine.rs");
        let marker = ["#[", "test]"].concat();
        let mut tests = 0;
        for test in source.split(&marker).skip(1) {
            let body = test.split_once('{').expect("test function body").1.trim_start();
            assert!(
                body.starts_with("let _serial = engine_test_lock();"),
                "engine test does not acquire the process-global native-state guard: {}",
                test.lines().find(|line| line.contains("fn ")).unwrap_or("unnamed test")
            );
            tests += 1;
        }
        assert!(tests >= 37, "engine test census unexpectedly shrank to {tests}");
    }

    #[test]
    fn guest_tcsetsf_publishes_flush_across_the_launch_fork_on_both_isas() {
        let _serial = engine_test_lock();
        let mut master = -1;
        let mut slave = -1;
        // SAFETY: both descriptor outputs are writable; no name, termios override or size is requested.
        assert_eq!(
            unsafe {
                libc::openpty(
                    &raw mut master,
                    &raw mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0
        );
        // SAFETY: successful openpty transferred two uniquely owned descriptors.
        let (_master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
        let mut attributes = std::mem::MaybeUninit::<libc::termios>::uninit();
        // SAFETY: the slave is live and the output points at writable storage.
        assert_eq!(
            unsafe { libc::tcgetattr(slave.as_raw_fd(), attributes.as_mut_ptr()) },
            0
        );
        // SAFETY: successful tcgetattr initialized the structure; Linux's guest image is its first 36 bytes.
        let attributes = unsafe { attributes.assume_init() };
        let mut image = [0_u8; 36];
        // SAFETY: both objects are live and non-overlapping for exactly the guest image width.
        unsafe { std::ptr::copy_nonoverlapping((&raw const attributes).cast::<u8>(), image.as_mut_ptr(), image.len()) };

        for (isa, guest) in [(1, aarch64_tcsetsf_image(&image)), (2, x86_tcsetsf_image(&image))] {
            crate::terminal_termios_flush_register(slave.as_raw_fd()).expect("register terminal before launch fork");
            let before = crate::terminal_termios_flush_generation(slave.as_raw_fd());
            let mut executable = tempfile::tempfile().unwrap();
            executable.write_all(&guest).unwrap();
            executable.seek(SeekFrom::Start(0)).unwrap();
            let config = EngineConfig {
                isa,
                rootfs: None,
                executable_host: None,
                executable_fd: executable.as_raw_fd(),
                option_names: &[],
                option_values: &[],
                box_config: None,
                standard_fds: [slave.as_raw_fd(); 3],
                provider_fd: -1,
            };
            // SAFETY: all borrowed descriptors remain live through creation and run.
            let engine = unsafe { Engine::create(config) }.unwrap();
            let argument = CString::new("guest").unwrap();
            assert_eq!(engine.run(&[argument.as_ptr()]), Ok(()));
            assert_eq!(engine.exit().status, 0, "ISA {isa} guest TCSETSF syscall failed");
            assert!(
                crate::terminal_termios_flush_generation(slave.as_raw_fd()) > before,
                "ISA {isa} guest ioctl returned without publishing flush provenance"
            );
            crate::terminal_termios_flush_unregister(slave.as_raw_fd());
        }
    }

    #[test]
    fn flush_provenance_survives_capacity_and_concurrent_fork_publishers() {
        let _serial = engine_test_lock();
        let mut terminals = Vec::new();
        for _ in 0..(64 + 8) {
            let mut master = -1;
            let mut slave = -1;
            // SAFETY: writable descriptor outputs are supplied and checked.
            assert_eq!(
                unsafe {
                    libc::openpty(
                        &raw mut master,
                        &raw mut slave,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    )
                },
                0
            );
            // SAFETY: successful openpty returned uniquely owned descriptors.
            terminals.push(unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) });
        }
        for (_, slave) in terminals.iter().take(64) {
            crate::terminal_termios_flush_register(slave.as_raw_fd()).expect("one stable active-terminal slot");
        }
        assert!(
            crate::terminal_termios_flush_register(terminals[64].1.as_raw_fd()).is_none(),
            "capacity exhaustion silently evicted an active terminal"
        );
        let first_before = crate::terminal_termios_flush_generation(terminals[0].1.as_raw_fd());
        let other_before = crate::terminal_termios_flush_generation(terminals[1].1.as_raw_fd());
        let _ = crate::terminal_termios_flush_mark_test(terminals[0].1.as_raw_fd(), 0x5404);
        let per_publish = crate::terminal_termios_flush_generation(terminals[0].1.as_raw_fd()) - first_before;
        assert!(
            matches!(per_publish, 1 | 2),
            "one target-local publication per compiled guest ISA"
        );
        for _ in 1..65 {
            let _ = crate::terminal_termios_flush_mark_test(terminals[0].1.as_raw_fd(), 0x5404);
        }
        assert_eq!(
            crate::terminal_termios_flush_generation(terminals[0].1.as_raw_fd()),
            first_before + 65 * per_publish
        );
        assert_eq!(
            crate::terminal_termios_flush_generation(terminals[1].1.as_raw_fd()),
            other_before,
            "another terminal inherited the repeated flushes"
        );
        let mut children = Vec::new();
        for _ in 0..8 {
            let slave = &terminals[0].1;
            // SAFETY: the child calls one allocation-free native publisher and exits without unwinding.
            let child = unsafe { libc::fork() };
            assert!(child >= 0);
            if child == 0 {
                let _ = crate::terminal_termios_flush_mark_test(slave.as_raw_fd(), 0x5404);
                // SAFETY: leave the post-fork child without touching inherited Rust state.
                unsafe { libc::_exit(0) };
            }
            children.push(child);
        }
        for child in children {
            let mut status = 0;
            // SAFETY: every PID is an unreaped child created above.
            assert_eq!(unsafe { libc::waitpid(child, &raw mut status, 0) }, child);
            assert!(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0);
        }
        assert_eq!(
            crate::terminal_termios_flush_generation(terminals[0].1.as_raw_fd()),
            first_before + (65 + 8) * per_publish,
            "concurrent fork publishers lost an atomic increment"
        );

        let mut ready = [0; 2];
        let mut release = [0; 2];
        // SAFETY: both arrays provide writable storage for fresh pipe descriptors.
        assert_eq!(unsafe { libc::pipe(ready.as_mut_ptr()) }, 0);
        assert_eq!(unsafe { libc::pipe(release.as_mut_ptr()) }, 0);
        // SAFETY: the child performs only async-signal-safe descriptor operations and the native publisher.
        let stale = unsafe { libc::fork() };
        assert!(stale >= 0);
        if stale == 0 {
            let mut byte = [1_u8];
            // SAFETY: inherited pipe ends remain live in this child.
            unsafe {
                libc::write(ready[1], byte.as_ptr().cast(), 1);
                libc::read(release[0], byte.as_mut_ptr().cast(), 1);
            }
            let _ = crate::terminal_termios_flush_mark_test(terminals[0].1.as_raw_fd(), 0x5404);
            // SAFETY: leave the post-fork child without touching inherited Rust state.
            unsafe { libc::_exit(0) };
        }
        let mut byte = [0_u8];
        // SAFETY: inherited pipe ends remain live in this parent.
        assert_eq!(unsafe { libc::read(ready[0], byte.as_mut_ptr().cast(), 1) }, 1);
        crate::terminal_termios_flush_unregister(terminals[0].1.as_raw_fd());
        crate::terminal_termios_flush_register(terminals[0].1.as_raw_fd())
            .expect("reuse released slot with a new epoch");
        let replacement = crate::terminal_termios_flush_generation(terminals[0].1.as_raw_fd());
        // SAFETY: releasing the child requires one byte on its inherited pipe.
        assert_eq!(unsafe { libc::write(release[1], byte.as_ptr().cast(), 1) }, 1);
        let mut status = 0;
        // SAFETY: `stale` is an unreaped child created above.
        assert_eq!(unsafe { libc::waitpid(stale, &raw mut status, 0) }, stale);
        assert_eq!(
            crate::terminal_termios_flush_generation(terminals[0].1.as_raw_fd()),
            replacement,
            "a pre-unregister child published into a reused slot"
        );

        #[cfg(feature = "native-test-hooks")]
        {
            let descriptor = terminals[0].1.as_raw_fd();
            assert_eq!(crate::terminal_termios_flush_mark_test(descriptor, u64::MAX), 1);
            let publisher = std::thread::spawn(move || {
                let _ = crate::terminal_termios_flush_mark_test(descriptor, 0x5404);
            });
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while crate::terminal_termios_flush_mark_test(descriptor, u64::MAX - 1) != 2 {
                assert!(
                    std::time::Instant::now() < deadline,
                    "publisher never reached the pre-CAS barrier"
                );
                std::thread::yield_now();
            }
            crate::terminal_termios_flush_unregister(descriptor);
            crate::terminal_termios_flush_register(descriptor).expect("reuse the slot while a stale CAS is paused");
            let replacement = crate::terminal_termios_flush_generation(descriptor);
            assert_eq!(crate::terminal_termios_flush_mark_test(descriptor, u64::MAX - 2), 3);
            publisher.join().unwrap();
            assert_eq!(
                crate::terminal_termios_flush_generation(descriptor),
                replacement,
                "a publisher that loaded the old tag incremented the replacement counter"
            );
        }
        for descriptor in ready.into_iter().chain(release) {
            // SAFETY: each descriptor was created by pipe and is no longer needed.
            unsafe { libc::close(descriptor) };
        }
        for (_, slave) in terminals.iter().take(64) {
            crate::terminal_termios_flush_unregister(slave.as_raw_fd());
        }
    }

    fn dynamic_image(interpreter: &[u8], isa: u32) -> Vec<u8> {
        let mut bytes = image();
        if isa == 2 {
            put16(&mut bytes, 18, 0x3e);
            bytes[0x100..0x102].copy_from_slice(&[0xeb, 0xfe]);
        }
        put16(&mut bytes, 56, 2);
        put32(&mut bytes, 120, 3);
        put64(&mut bytes, 128, 0x200);
        put64(&mut bytes, 152, u64::try_from(interpreter.len() + 1).unwrap());
        bytes[0x200..0x200 + interpreter.len()].copy_from_slice(interpreter);
        bytes[0x200 + interpreter.len()] = 0;
        bytes
    }

    fn exiting_interpreter(isa: u32) -> Vec<u8> {
        let mut bytes = image();
        put16(&mut bytes, 16, 3);
        put64(&mut bytes, 24, 0x100);
        put64(&mut bytes, 80, 0);
        put64(&mut bytes, 88, 0);
        if isa == 1 {
            bytes[0x100..0x104].copy_from_slice(&0xd280_0000_u32.to_le_bytes());
            bytes[0x104..0x108].copy_from_slice(&0xd280_0ba8_u32.to_le_bytes());
            bytes[0x108..0x10c].copy_from_slice(&0xd400_0001_u32.to_le_bytes());
        } else {
            put16(&mut bytes, 18, 0x3e);
            bytes[0x100..0x109].copy_from_slice(&[0x31, 0xff, 0xb8, 0x3c, 0, 0, 0, 0x0f, 0x05]);
        }
        bytes
    }

    fn create_engine(isa: u32) -> (Engine, std::fs::File) {
        create_engine_with_options(isa, &[], &[])
    }

    #[cfg(feature = "native-test-hooks")]
    #[test]
    fn requesting_stop_after_native_finish_is_idempotent() {
        let _serial = engine_test_lock();
        for isa in [1, 2] {
            let create = || {
                let mut executable = tempfile::tempfile().unwrap();
                executable.write_all(&exiting_interpreter(isa)).unwrap();
                executable.seek(SeekFrom::Start(0)).unwrap();
                let standard = OpenOptions::new().read(true).write(true).open("/dev/null").unwrap();
                let config = EngineConfig {
                    isa,
                    rootfs: None,
                    executable_host: None,
                    executable_fd: executable.as_raw_fd(),
                    option_names: &[],
                    option_values: &[],
                    box_config: None,
                    standard_fds: [standard.as_raw_fd(); 3],
                    provider_fd: -1,
                };
                // SAFETY: all borrowed descriptors remain live until engine construction returns.
                unsafe { Engine::create(config) }.unwrap()
            };
            let engine = create();
            let bystander = create();
            let api = crate::loader::tests().unwrap();
            // SAFETY: the backend remains live until the run and all phase operations finish.
            unsafe { (api.engine_finish_test_arm)(engine.0.as_ptr()) };
            let argument = CString::new("guest").unwrap();
            std::thread::scope(|scope| {
                let (finished, completion) = std::sync::mpsc::channel();
                scope.spawn(move || {
                    let bystander_argument = CString::new("guest").unwrap();
                    finished.send(bystander.run(&[bystander_argument.as_ptr()])).unwrap();
                });
                let bystander_result = completion.recv_timeout(std::time::Duration::from_millis(250));
                // SAFETY: reads the phase associated with `engine`, retaining no pointer.
                let after_bystander = unsafe { (api.engine_finish_test_phase)(engine.0.as_ptr()) };
                if bystander_result.is_err() {
                    // If a regression let the bystander consume the arm, release it before failing so
                    // the scoped thread can join instead of turning one assertion into a suite hang.
                    // SAFETY: `engine` remains live through this scope and is the hook's armed target.
                    unsafe { (api.engine_finish_test_release)(engine.0.as_ptr()) };
                }
                assert_eq!(bystander_result.unwrap(), Ok(()));
                assert_eq!(
                    after_bystander, 1,
                    "ISA {isa} bystander consumed another engine's finish arm"
                );

                let running = scope.spawn(|| engine.run(&[argument.as_ptr()]));
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                // SAFETY: the phase query reads one atomic and retains nothing.
                while unsafe { (api.engine_finish_test_phase)(engine.0.as_ptr()) } != 2 {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "ISA {isa} did not reach native FINISHED"
                    );
                    std::thread::yield_now();
                }
                let stopped = engine.request(2, 0);
                // SAFETY: releases the run thread parked by this test's arm above.
                unsafe { (api.engine_finish_test_release)(engine.0.as_ptr()) };
                assert_eq!(stopped, Ok(()), "ISA {isa} terminal stop was not idempotent");
                assert_eq!(running.join().unwrap(), Ok(()));
            });
        }
    }

    #[cfg(feature = "native-test-hooks")]
    #[test]
    fn destroying_stays_busy_and_a_terminate_race_to_finished_succeeds() {
        let _serial = engine_test_lock();
        let api = crate::loader::tests().unwrap();
        // SAFETY: the test export owns its complete synthetic engine and retains no pointer.
        assert_eq!(unsafe { (api.engine_request_state_test)(0) }, 14);
        // SAFETY: scenario one supplies a terminating host callback that publishes FINISHED before
        // returning INVALID_ARGUMENT, reproducing the post-terminate race without a live process.
        assert_eq!(unsafe { (api.engine_request_state_test)(1) }, 0);
    }

    fn create_engine_with_options(
        isa: u32,
        option_names: &[*const std::ffi::c_char],
        option_values: &[*const std::ffi::c_char],
    ) -> (Engine, std::fs::File) {
        let mut executable = tempfile::tempfile().unwrap();
        let mut bytes = image();
        if isa == 2 {
            put16(&mut bytes, 18, 0x3e);
            // `jmp .`: the x86-64 counterpart of the AArch64 live loop.
            bytes[0x100..0x102].copy_from_slice(&[0xeb, 0xfe]);
        }
        executable.write_all(&bytes).unwrap();
        executable.seek(SeekFrom::Start(0)).unwrap();
        let standard = OpenOptions::new().read(true).write(true).open("/dev/null").unwrap();
        let config = EngineConfig {
            isa,
            rootfs: None,
            executable_host: None,
            executable_fd: executable.as_raw_fd(),
            option_names,
            option_values,
            box_config: None,
            standard_fds: [standard.as_raw_fd(); 3],
            provider_fd: -1,
        };
        // SAFETY: all descriptors and borrowed slices remain live through create;
        // the bridge copies configuration and imports its own descriptor handles.
        let engine = unsafe { Engine::create(config) }.unwrap();
        (engine, standard)
    }

    #[test]
    fn armed_running_guest_reaches_checkpoint_broker() {
        let _serial = engine_test_lock();
        const CHILD: &str = "HL_NATIVE_ARMED_CHECKPOINT_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "engine::tests::armed_running_guest_reaches_checkpoint_broker",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(CHILD, "1")
                .spawn()
                .unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    assert!(status.success(), "armed checkpoint child failed: {status}");
                    return;
                }
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("armed checkpoint child exceeded 15 seconds");
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }

        for isa in [1, 2] {
            let (mut engine, _standard) = create_engine(isa);
            let (broker, transport) = crate::CheckpointTransport::create().unwrap();
            engine.configure_checkpoint(&transport).unwrap();
            let argument = CString::new("guest").unwrap();
            std::thread::scope(|scope| {
                let running = scope.spawn(|| engine.run(&[argument.as_ptr()]));
                std::thread::sleep(std::time::Duration::from_millis(500));
                let _generation = transport.bump();
                let signal = crate::CheckpointTransport::interrupt_signal(isa);
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                let channel = loop {
                    engine.request(4, signal).unwrap();
                    if let Some(channel) = broker.accept(std::time::Duration::from_millis(100)) {
                        break Some(channel);
                    }
                    if std::time::Instant::now() >= deadline {
                        break None;
                    }
                };
                let _ = engine.request(2, 0);
                let _ = running.join().unwrap();
                assert!(channel.is_some(), "ISA {isa} did not publish a checkpoint channel");
            });
        }
    }

    #[test]
    fn pathname_replacement_cannot_change_pinned_initial_image() {
        let _serial = engine_test_lock();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("guest");
        std::fs::write(&path, image()).unwrap();
        let executable = CString::new(path.to_str().unwrap()).unwrap();
        let standard = OpenOptions::new().read(true).write(true).open("/dev/null").unwrap();
        let config = EngineConfig {
            isa: 1,
            rootfs: None,
            executable_host: Some(&executable),
            executable_fd: -1,
            option_names: &[],
            option_values: &[],
            box_config: None,
            standard_fds: [standard.as_raw_fd(); 3],
            provider_fd: -1,
        };
        let displaced = directory.path().join("displaced");
        // SAFETY: all pointers and descriptors remain live through creation. The hook runs only after
        // the engine has pinned the path's file description and replaces the directory entry, not that
        // open description.
        let engine = unsafe {
            Engine::create_after_pinning(config, || {
                std::fs::rename(&path, &displaced).unwrap();
                std::fs::write(&path, b"not an ELF image").unwrap();
            })
        };
        assert!(engine.is_ok(), "creation reopened the replaced executable pathname");
    }

    #[test]
    fn interpreter_replacement_between_create_and_run_cannot_change_image() {
        let _serial = engine_test_lock();
        for isa in [1, 2] {
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(root.path().join("bin")).unwrap();
            std::fs::create_dir_all(root.path().join("lib")).unwrap();
            let main = root.path().join("bin/main");
            let interpreter = root.path().join("lib/ld-test.so");
            std::fs::write(&main, dynamic_image(b"/lib/ld-test.so", isa)).unwrap();
            std::fs::write(&interpreter, exiting_interpreter(isa)).unwrap();
            let main = CString::new(main.to_str().unwrap()).unwrap();
            let root_path = CString::new(root.path().to_str().unwrap()).unwrap();
            let standard = OpenOptions::new().read(true).write(true).open("/dev/null").unwrap();
            let config = EngineConfig {
                isa,
                rootfs: Some(&root_path),
                executable_host: Some(&main),
                executable_fd: -1,
                option_names: &[],
                option_values: &[],
                box_config: None,
                standard_fds: [standard.as_raw_fd(); 3],
                provider_fd: -1,
            };
            // SAFETY: every borrowed string and descriptor remains live through create.
            let engine = unsafe { Engine::create(config) }.unwrap();
            std::fs::remove_file(root.path().join("bin/main")).unwrap();
            std::fs::remove_dir(root.path().join("bin")).unwrap();
            std::fs::remove_file(&interpreter).unwrap();
            std::fs::remove_dir(root.path().join("lib")).unwrap();
            let argument = CString::new("/bin/main").unwrap();
            engine.run(&[argument.as_ptr()]).unwrap();
            assert_eq!(engine.exit().status, 0);
        }
    }

    #[test]
    fn malformed_pinned_interpreter_is_rejected_during_create() {
        let _serial = engine_test_lock();
        for isa in [1, 2] {
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(root.path().join("bin")).unwrap();
            std::fs::create_dir_all(root.path().join("lib")).unwrap();
            let main = root.path().join("bin/main");
            let interpreter = root.path().join("lib/ld-test.so");
            std::fs::write(&main, dynamic_image(b"/lib/ld-test.so", isa)).unwrap();
            let mut malformed = exiting_interpreter(isa);
            put64(&mut malformed, 32, u64::MAX - 32);
            std::fs::write(&interpreter, malformed).unwrap();
            let main = CString::new(main.to_str().unwrap()).unwrap();
            let root_path = CString::new(root.path().to_str().unwrap()).unwrap();
            let standard = OpenOptions::new().read(true).write(true).open("/dev/null").unwrap();
            let config = EngineConfig {
                isa,
                rootfs: Some(&root_path),
                executable_host: Some(&main),
                executable_fd: -1,
                option_names: &[],
                option_values: &[],
                box_config: None,
                standard_fds: [standard.as_raw_fd(); 3],
                provider_fd: -1,
            };
            // SAFETY: every borrowed string and descriptor remains live through create.
            assert!(unsafe { Engine::create(config) }.is_err());
        }
    }

    #[test]
    fn unlinked_pinned_image_can_reexec_proc_self_exe_on_both_isas() {
        let _serial = engine_test_lock();
        const SOURCE: &str = r#"
#include <sys/syscall.h>
#include <unistd.h>
#include <errno.h>
#include <fcntl.h>
#include <string.h>
int main(int argc, char **argv) {
#if IMAGE_ID == 1
    char *next[] = { (char *)"/next", (char *)"stage-b", 0 };
    syscall(SYS_execve, next[0], next, (char *[]){ 0 });
    return errno;
#elif IMAGE_ID == 2
    if (argc == 2 && !strcmp(argv[1], "stage-b")) {
        unlink("/next");
        char *next[] = { (char *)"/proc/self/exe", (char *)"verify-b", 0 };
        syscall(SYS_execve, next[0], next, (char *[]){ 0 });
        return errno;
    }

    return argc == 2 && !strcmp(argv[1], "verify-b") ? 0 : 92;
#elif IMAGE_ID == 3
    char *next[] = { (char *)"/proc/self/exe", (char *)"again", 0 };
    syscall(SYS_execve, next[0], next, (char *[]){ 0 });
    return errno == EACCES ? 0 : errno;
#else
    if (argc == 2) return !strcmp(argv[1], "verify-a") ? 0 : 96;
    int held = open("/next", O_WRONLY);
    if (held < 0) return 94;
    char *next[] = { (char *)"/next", (char *)"stage-b", 0 };
    syscall(SYS_execve, next[0], next, (char *[]){ 0 });
    if (errno != ETXTBSY) return 95;
    close(held);
    char *self[] = { (char *)"/proc/self/exe", (char *)"verify-a", 0 };
    syscall(SYS_execve, self[0], self, (char *[]){ 0 });
    return errno;
#endif
}
"#;
        for (isa, compiler) in [(1, "aarch64-linux-gnu-gcc"), (2, "x86_64-linux-gnu-gcc")] {
            if !guest_compiler_present(
                compiler,
                "unlinked_pinned_image_can_reexec_proc_self_exe_on_both_isas",
                isa,
            ) {
                continue;
            }
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(root.path().join("bin")).unwrap();
            let source = root.path().join("self.c");
            let main_path = root.path().join("bin/main");
            let second_path = root.path().join("next");
            let dac_path = root.path().join("dac");
            let busy_path = root.path().join("busy");
            std::fs::write(&source, SOURCE).unwrap();
            for (identity, output) in [(1, &main_path), (2, &second_path), (3, &dac_path), (4, &busy_path)] {
                let compile = guest_compiler(compiler)
                    .args(["-static", "-no-pie", "-O2"])
                    .arg(format!("-DIMAGE_ID={identity}"))
                    .arg(&source)
                    .arg("-o")
                    .arg(output)
                    .output()
                    .unwrap_or_else(|error| panic!("{compiler} is required for ISA {isa}: {error}"));
                assert!(
                    compile.status.success(),
                    "{compiler} failed: {}",
                    String::from_utf8_lossy(&compile.stderr)
                );
            }
            let root_path = CString::new(root.path().to_str().unwrap()).unwrap();
            let run = |host: &std::path::Path, guest: &str, after_create: &dyn Fn()| {
                let executable = CString::new(host.to_str().unwrap()).unwrap();
                let standard = OpenOptions::new().read(true).write(true).open("/dev/null").unwrap();
                let config = EngineConfig {
                    isa,
                    rootfs: Some(&root_path),
                    executable_host: Some(&executable),
                    executable_fd: -1,
                    option_names: &[],
                    option_values: &[],
                    box_config: None,
                    standard_fds: [standard.as_raw_fd(); 3],
                    provider_fd: -1,
                };
                // SAFETY: every borrowed string and descriptor remains live through create.
                let engine = unsafe { Engine::create(config) }.unwrap();
                after_create();
                let argument = CString::new(guest).unwrap();
                engine.run(&[argument.as_ptr()]).unwrap();
                engine.exit().status
            };
            assert_eq!(
                run(&busy_path, "/busy", &|| {}),
                0,
                "ISA {isa} rotated authority after failed exec"
            );
            assert_eq!(
                run(&main_path, "/bin/main", &|| {
                    std::fs::remove_file(&main_path).unwrap();
                    std::fs::remove_dir(root.path().join("bin")).unwrap();
                }),
                0,
                "ISA {isa} did not rotate and re-exec image B"
            );
            let mut permissions = std::fs::metadata(&dac_path).unwrap().permissions();
            permissions.set_mode(0o644);
            std::fs::set_permissions(&dac_path, permissions).unwrap();
            assert_eq!(
                run(&dac_path, "/dac", &|| std::fs::remove_file(&dac_path).unwrap()),
                0,
                "ISA {isa} self authority lost execute DAC metadata"
            );
        }
    }

    #[test]
    fn failed_prepared_exec_never_publishes_candidate_authority() {
        let _serial = engine_test_lock();
        const SOURCE: &str = r#"
#include <sys/syscall.h>
#include <unistd.h>
#include <errno.h>
#include <fcntl.h>
#include <string.h>
#define STRINGIFY_INNER(value) #value
#define STRINGIFY(value) STRINGIFY_INNER(value)
int main(int argc, char **argv) {
#ifdef CANDIDATE
    return 99;
#else
    if (argc == 2) return !strcmp(argv[1], "verify-a-" STRINGIFY(SCENARIO)) ? 0 : 98;
#if SCENARIO == 1
    int held = open("/candidate", O_WRONLY);
    if (held < 0) return 91;
    const char *path = "/candidate";
    int expected = ETXTBSY;
#elif SCENARIO == 2
    const char *path = "/denied";
    int expected = EACCES;
#elif SCENARIO == 3
    const char *path = "/malformed";
    int expected = ENOEXEC;
#elif SCENARIO == 4
    const char *path = "/script";
    int expected = ENOENT;
#else
    const char *path = "/dynamic";
    int expected = ENOENT;
#endif
    char *candidate[] = { (char *)path, 0 };
    syscall(SYS_execve, path, candidate, (char *[]){ 0 });
    if (errno != expected) return 20 + errno;
    char *self[] = { (char *)"/proc/self/exe", (char *)"verify-a-" STRINGIFY(SCENARIO), 0 };
    syscall(SYS_execve, self[0], self, (char *[]){ 0 });
    return errno;
#endif
}
"#;
        use std::os::unix::fs::PermissionsExt as _;
        for (isa, compiler) in [(1, "aarch64-linux-gnu-gcc"), (2, "x86_64-linux-gnu-gcc")] {
            if !guest_compiler_present(
                compiler,
                "failed_prepared_exec_never_publishes_candidate_authority",
                isa,
            ) {
                continue;
            }
            let root = tempfile::tempdir().unwrap();
            let source = root.path().join("authority.c");
            std::fs::write(&source, SOURCE).unwrap();
            let compile = |arguments: &[&str], input: &std::path::Path, output: &std::path::Path| {
                let result = guest_compiler(compiler)
                    .args(arguments)
                    .arg(input)
                    .arg("-o")
                    .arg(output)
                    .output()
                    .unwrap();
                assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
            };
            let candidate = root.path().join("candidate");
            compile(&["-static", "-no-pie", "-O2", "-DCANDIDATE"], &source, &candidate);
            std::fs::copy(&candidate, root.path().join("denied")).unwrap();
            let mut denied = std::fs::metadata(root.path().join("denied")).unwrap().permissions();
            denied.set_mode(0o644);
            std::fs::set_permissions(root.path().join("denied"), denied).unwrap();
            std::fs::write(root.path().join("malformed"), b"not an executable\n").unwrap();
            std::fs::write(root.path().join("script"), b"#!/missing-interpreter\n").unwrap();
            for path in [root.path().join("malformed"), root.path().join("script")] {
                let mut permissions = std::fs::metadata(&path).unwrap().permissions();
                permissions.set_mode(0o755);
                std::fs::set_permissions(path, permissions).unwrap();
            }
            std::fs::write(root.path().join("dynamic"), dynamic_image(b"/missing-loader", isa)).unwrap();
            let mut dynamic_permissions = std::fs::metadata(root.path().join("dynamic")).unwrap().permissions();
            dynamic_permissions.set_mode(0o755);
            std::fs::set_permissions(root.path().join("dynamic"), dynamic_permissions).unwrap();
            let root_path = CString::new(root.path().to_str().unwrap()).unwrap();
            for scenario in 1..=5 {
                let main = root.path().join(format!("main-{scenario}"));
                compile(
                    &["-static", "-no-pie", "-O2", &format!("-DSCENARIO={scenario}")],
                    &source,
                    &main,
                );
                let executable = CString::new(main.to_str().unwrap()).unwrap();
                let standard = OpenOptions::new().read(true).write(true).open("/dev/null").unwrap();
                let config = EngineConfig {
                    isa,
                    rootfs: Some(&root_path),
                    executable_host: Some(&executable),
                    executable_fd: -1,
                    option_names: &[],
                    option_values: &[],
                    box_config: None,
                    standard_fds: [standard.as_raw_fd(); 3],
                    provider_fd: -1,
                };
                // SAFETY: borrowed strings and descriptors remain live through creation.
                let engine = unsafe { Engine::create(config) }.unwrap();
                let argument = CString::new(format!("/main-{scenario}")).unwrap();
                engine.run(&[argument.as_ptr()]).unwrap();
                assert_eq!(engine.exit().status, 0, "ISA {isa} scenario {scenario}");
            }
        }
    }

    /// A bind-mounted host directory must enumerate, not merely open files by name.
    ///
    /// A volume is its own jail root: `openat`, `read` and `write` on a path under it route to the
    /// host source, but the layered namespace walk that decides descriptor provenance knows only the
    /// image layers, and the mount point exists in those as the empty placeholder
    /// `vol_mkmountpoint()` materializes. The two therefore describe different directory objects, and
    /// `openat(O_DIRECTORY)` rejects the disagreement -- so listing a mounted directory failed with
    /// `EAGAIN` while reading a file inside it succeeded, and a subdirectory present only in the host
    /// source was `ENOENT` to `openat` while `cat` of a file inside it worked.
    ///
    /// This is the product's flagship path -- a developer mounts a project directory and runs `ls` --
    /// and no test in the repository listed a mounted directory before this one. The guest runs the
    /// two syscalls directly rather than through a shell so the failure is an errno, not a message.
    #[test]
    fn a_bind_mounted_directory_enumerates_its_host_entries() {
        let _serial = engine_test_lock();
        const SOURCE: &str = r#"
#include <errno.h>
#include <fcntl.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

/* linux_dirent64: d_ino, d_off, d_reclen, d_type, then the NUL-terminated name at offset 19. */
struct record {
    unsigned long long object;
    long long offset;
    unsigned short size;
    unsigned char type;
    char name[];
};

/* An exit status is 8 bits, so each failing step gets a 48-wide band and clamps its errno into it. */
static int reported(int base) { return base + (errno < 47 ? errno : 47); }

int main(void) {
    int mount = open("/work", O_RDONLY | O_DIRECTORY);
    if (mount < 0) return reported(10);
    char buffer[8192];
    long used = syscall(SYS_getdents64, mount, buffer, sizeof buffer);
    if (used < 0) return reported(60);
    int alpha = 0, nested = 0;
    for (long at = 0; at + 19 <= used;) {
        struct record *entry = (struct record *)(buffer + at);
        if (entry->size < 24) return 3;
        if (!strcmp(entry->name, "alpha.txt")) alpha = 1;
        if (!strcmp(entry->name, "nested")) nested = 1;
        at += entry->size;
    }
    if (!alpha) return 4;
    if (!nested) return 5;
    /* A directory that exists only in the host source, never in the image placeholder. */
    int child = open("/work/nested", O_RDONLY | O_DIRECTORY);
    if (child < 0) return reported(110);
    long child_used = syscall(SYS_getdents64, child, buffer, sizeof buffer);
    if (child_used < 0) return reported(160);
    for (long at = 0; at + 19 <= child_used;) {
        struct record *entry = (struct record *)(buffer + at);
        if (entry->size < 24) return 6;
        if (!strcmp(entry->name, "deep.txt")) return 0;
        at += entry->size;
    }
    return 7;
}
"#;
        /// Turn the guest's exit status back into the syscall and errno it stands for, because a bare
        /// number here would say nothing about which half of the mechanism broke.
        fn explain(status: i32) -> String {
            match status {
                0 => "success".to_owned(),
                3 | 6 => "a getdents64 record was shorter than linux_dirent64's fixed header".to_owned(),
                4 => "the mount listed without `alpha.txt`".to_owned(),
                5 => "the mount listed without `nested`".to_owned(),
                7 => "`/work/nested` listed without `deep.txt`".to_owned(),
                10..=57 => format!("openat(\"/work\", O_DIRECTORY) failed with errno {}", status - 10),
                60..=107 => format!("getdents64 on the mount failed with errno {}", status - 60),
                110..=157 => {
                    format!(
                        "openat(\"/work/nested\", O_DIRECTORY) failed with errno {}",
                        status - 110
                    )
                }
                160..=207 => format!("getdents64 on `/work/nested` failed with errno {}", status - 160),
                other => format!("the guest exited {other}"),
            }
        }

        for (isa, compiler) in [(1, "aarch64-linux-gnu-gcc"), (2, "x86_64-linux-gnu-gcc")] {
            if !guest_compiler_present(compiler, "a_bind_mounted_directory_enumerates_its_host_entries", isa) {
                continue;
            }
            // The rootfs and the mounted source are separate host trees, exactly as a workspace mounts
            // a project directory into an image: nothing the guest sees under `/work` exists in the
            // image, and the placeholder the engine creates at the mount point is a different inode.
            let root = tempfile::tempdir().unwrap();
            let project = tempfile::tempdir().unwrap();
            std::fs::write(project.path().join("alpha.txt"), "hello from the host").unwrap();
            std::fs::create_dir(project.path().join("nested")).unwrap();
            std::fs::write(project.path().join("nested/deep.txt"), "nested").unwrap();
            let source = root.path().join("listing.c");
            let guest = root.path().join("listing");
            std::fs::write(&source, SOURCE).unwrap();
            let compile = guest_compiler(compiler)
                .args(["-static", "-no-pie", "-O2"])
                .arg(&source)
                .arg("-o")
                .arg(&guest)
                .output()
                .unwrap_or_else(|error| panic!("{compiler} is required for ISA {isa}: {error}"));
            assert!(
                compile.status.success(),
                "{compiler} failed: {}",
                String::from_utf8_lossy(&compile.stderr)
            );
            let root_path = CString::new(root.path().to_str().unwrap()).unwrap();
            let executable = CString::new(guest.to_str().unwrap()).unwrap();
            let volumes = CString::new("HL_VOLUMES").unwrap();
            let specification = CString::new(format!("/work:{}", project.path().display())).unwrap();
            let standard = OpenOptions::new().read(true).write(true).open("/dev/null").unwrap();
            let config = EngineConfig {
                isa,
                rootfs: Some(&root_path),
                executable_host: Some(&executable),
                executable_fd: -1,
                option_names: &[volumes.as_ptr()],
                option_values: &[specification.as_ptr()],
                box_config: None,
                standard_fds: [standard.as_raw_fd(); 3],
                provider_fd: -1,
            };
            // SAFETY: every borrowed string and descriptor remains live through create.
            let engine = unsafe { Engine::create(config) }.unwrap();
            let argument = CString::new("/listing").unwrap();
            engine.run(&[argument.as_ptr()]).unwrap();
            let status = engine.exit().status;
            assert_eq!(status, 0, "ISA {isa}: {}", explain(status));
        }
    }

    fn resolved(mut roots: Vec<std::fs::File>, path: &str) -> Option<String> {
        let mut file = resolve_layered_guest(std::path::Path::new(path), &roots).ok()??;
        let mut value = String::new();
        file.read_to_string(&mut value).ok()?;
        roots.clear();
        Some(value)
    }

    #[test]
    fn interpreter_union_authority_handles_layers_links_and_deletions() {
        let _serial = engine_test_lock();
        use std::os::unix::fs::symlink;
        let upper = tempfile::tempdir().unwrap();
        let lower = tempfile::tempdir().unwrap();
        let lower_second = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(lower.path().join("lib/real")).unwrap();
        std::fs::create_dir_all(lower_second.path().join("lib")).unwrap();
        std::fs::write(lower.path().join("lib/real/loader"), "lower").unwrap();
        std::fs::write(lower_second.path().join("lib/fallback"), "second").unwrap();
        symlink("real/loader", lower.path().join("lib/relative")).unwrap();
        symlink("/lib/real/loader", lower.path().join("lib/absolute")).unwrap();
        let roots = || {
            vec![
                File::open(upper.path()).unwrap(),
                File::open(lower.path()).unwrap(),
                File::open(lower_second.path()).unwrap(),
            ]
        };
        assert_eq!(resolved(roots(), "/lib/real/loader").as_deref(), Some("lower"));
        assert_eq!(resolved(roots(), "/lib/relative").as_deref(), Some("lower"));
        assert_eq!(resolved(roots(), "/lib/absolute").as_deref(), Some("lower"));
        assert_eq!(resolved(roots(), "/lib/fallback").as_deref(), Some("second"));
        std::fs::write(lower.path().join("lib/fallback"), "first").unwrap();
        assert_eq!(resolved(roots(), "/lib/fallback").as_deref(), Some("first"));

        std::fs::create_dir_all(upper.path().join("lib/real")).unwrap();
        std::fs::write(upper.path().join("lib/real/loader"), "upper").unwrap();
        assert_eq!(resolved(roots(), "/lib/real/loader").as_deref(), Some("upper"));
        std::fs::remove_file(upper.path().join("lib/real/loader")).unwrap();
        std::fs::write(upper.path().join("lib/real/.wh.loader"), "").unwrap();
        assert!(resolved(roots(), "/lib/real/loader").is_none());
        std::fs::remove_file(upper.path().join("lib/real/.wh.loader")).unwrap();
        std::fs::write(upper.path().join("lib/real/.wh..wh..opq"), "").unwrap();
        assert!(resolved(roots(), "/lib/real/loader").is_none());
        std::fs::create_dir_all(lower.path().join("lib/sub")).unwrap();
        std::fs::write(lower.path().join("lib/sub/loader"), "hidden").unwrap();
        std::fs::create_dir_all(upper.path().join("lib/sub")).unwrap();
        std::fs::write(upper.path().join("lib/.wh..wh..opq"), "").unwrap();
        assert!(resolved(roots(), "/lib/sub/loader").is_none());
        assert!(resolved(roots(), "/lib/.wh..wh..opq").is_none());
        assert!(resolved(roots(), "/lib/real/.wh.loader").is_none());
        assert!(resolved(roots(), "/../../proc/self/fd/0").is_none());
    }

    #[test]
    fn upper_non_directory_ancestor_masks_lower_directory() {
        let _serial = engine_test_lock();
        let upper = tempfile::tempdir().unwrap();
        let lower = tempfile::tempdir().unwrap();
        std::fs::write(upper.path().join("lib"), "not a directory").unwrap();
        std::fs::create_dir_all(lower.path().join("lib")).unwrap();
        std::fs::write(lower.path().join("lib/loader"), "must stay hidden").unwrap();
        let roots = vec![File::open(upper.path()).unwrap(), File::open(lower.path()).unwrap()];
        let error = resolve_layered_guest(std::path::Path::new("/lib/loader"), &roots).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::ENOTDIR));
    }

    #[test]
    fn upper_merged_directory_keeps_lower_symlink_children_reachable() {
        let _serial = engine_test_lock();
        use std::os::unix::fs::symlink;
        let upper = tempfile::tempdir().unwrap();
        let lower = tempfile::tempdir().unwrap();
        std::fs::create_dir(upper.path().join("lib")).unwrap();
        std::fs::create_dir_all(lower.path().join("usr/lib")).unwrap();
        std::fs::write(lower.path().join("usr/lib/loader"), "lower loader").unwrap();
        symlink("usr/lib", lower.path().join("lib")).unwrap();
        let roots = vec![File::open(upper.path()).unwrap(), File::open(lower.path()).unwrap()];
        let error = resolve_layered_guest(std::path::Path::new("/lib/loader"), &roots).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::ENOTDIR));
        let mut loader = super::resolve_through_merged_directory_symlink(std::path::Path::new("/lib/loader"), &roots)
            .unwrap()
            .unwrap();
        let mut contents = String::new();
        loader.read_to_string(&mut contents).unwrap();
        assert_eq!(contents, "lower loader");
    }

    #[test]
    fn pinned_interpreter_survives_ancestor_replacement() {
        let _serial = engine_test_lock();
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("lib/live")).unwrap();
        std::fs::write(root.path().join("lib/live/loader"), "original").unwrap();
        let roots = vec![File::open(root.path()).unwrap()];
        let mut pinned = resolve_layered_guest(std::path::Path::new("/lib/live/loader"), &roots)
            .unwrap()
            .unwrap();
        std::fs::rename(root.path().join("lib/live"), root.path().join("lib/displaced")).unwrap();
        std::fs::create_dir_all(root.path().join("lib/live")).unwrap();
        std::fs::write(root.path().join("lib/live/loader"), "replacement").unwrap();
        let mut value = String::new();
        pinned.read_to_string(&mut value).unwrap();
        assert_eq!(value, "original");
    }

    fn inspect(bytes: &[u8]) -> Result<Plan, i32> {
        let path = std::env::temp_dir().join(format!(
            "hl-native-elf-inspect-{}-{:x}",
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
            box_config: None,
            standard_fds: [-1; 3],
            provider_fd: -1,
        };
        let result = Plan::inspect(&config);
        std::fs::remove_file(path).unwrap();
        result
    }

    #[test]
    fn executable_markers_cannot_change_generic_plan() {
        let _serial = engine_test_lock();
        let plain = image();
        let mut marked = plain.clone();
        marked[0x260..0x26e].copy_from_slice(b"\xff Go buildinf:");
        marked[0x340..0x348].copy_from_slice(b"v8_blob_");
        assert_eq!(inspect(&plain), inspect(&marked));
    }

    #[test]
    fn executable_plan_uses_the_host_placement_policy() {
        let _serial = engine_test_lock();
        let plan = inspect(&image()).unwrap();
        assert_eq!(plan.kind, 1);
        assert_eq!(plan.flags, u32::from(!cfg!(target_os = "linux")));
    }

    #[test]
    fn malformed_load_segment_is_rejected_before_native_loader() {
        let _serial = engine_test_lock();
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

    #[test]
    fn concurrent_exit_reads_only_coherent_publications() {
        let _serial = engine_test_lock();
        for isa in [1, 2] {
            let (engine, _standard) = create_engine(isa);
            let argument = CString::new("guest").unwrap();
            let initial = Exit {
                kind: 0,
                status: 0,
                detail: 0,
            };
            let observed = std::thread::scope(|scope| {
                let running = scope.spawn(|| engine.run(&[argument.as_ptr()]));
                let reading = scope.spawn(|| {
                    let mut values = vec![initial];
                    for _ in 0..50_000 {
                        let value = engine.exit();
                        if !values.contains(&value) {
                            values.push(value);
                        }
                    }
                    engine.request(2, 0).unwrap();
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                    loop {
                        let value = engine.exit();
                        if !values.contains(&value) {
                            values.push(value);
                        }
                        if value != initial {
                            break;
                        }
                        assert!(
                            std::time::Instant::now() < deadline,
                            "ISA {isa} did not publish an exit within five seconds"
                        );
                        std::thread::yield_now();
                    }
                    values
                });
                let values = reading.join().unwrap();
                running.join().unwrap().unwrap();
                values
            });
            let published = engine.exit();
            assert!(
                observed.contains(&published),
                "ISA {isa} test never observed publication"
            );
            assert!(
                observed.into_iter().all(|value| value == initial || value == published),
                "ISA {isa} reader observed a partially published exit record"
            );
            drop(engine);
        }
    }

    #[cfg(feature = "native-test-hooks")]
    #[test]
    fn fork_child_prunes_foreign_checkpoint_descriptors_before_fd_reuse() {
        let _serial = engine_test_lock();
        const CHILD: &str = "HL_NATIVE_CHECKPOINT_PRUNE_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "engine::tests::fork_child_prunes_foreign_checkpoint_descriptors_before_fd_reuse",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(CHILD, "1");
            let mut child = IsolatedTestChild::spawn(command).unwrap();
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    assert!(status.success(), "checkpoint prune child failed: {status}");
                    return;
                }
                assert!(Instant::now() < deadline, "checkpoint prune child exceeded 15 seconds");
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        // SAFETY: the test hook creates, forks, verifies, and closes its own descriptors.
        assert_eq!(
            unsafe { crate::bindings::hl_c_backend_checkpoint_test_prune_foreign_descriptors() },
            1
        );
    }

    #[cfg(feature = "native-test-hooks")]
    #[test]
    fn checkpoint_configuration_adopt_failures_preserve_descriptor_ownership() {
        let _serial = engine_test_lock();
        const CHILD: &str = "HL_NATIVE_CHECKPOINT_ADOPT_FAILURE_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "engine::tests::checkpoint_configuration_adopt_failures_preserve_descriptor_ownership",
                    "--nocapture",
                ])
                .env(CHILD, "1");
            let status = command.status().unwrap();
            assert!(status.success(), "checkpoint adoption child failed: {status}");
            return;
        }

        let descriptor_directory = if cfg!(target_os = "linux") {
            "/proc/self/fd"
        } else {
            "/dev/fd"
        };
        for position in 1..=4 {
            let (mut engine, _standard) = create_engine(1);
            let (_broker, transport) = crate::CheckpointTransport::create().unwrap();
            let descriptors_before = std::fs::read_dir(descriptor_directory).unwrap().count();
            // SAFETY: the feature-only hook affects only this thread's next configure transaction.
            unsafe { crate::bindings::hl_c_backend_checkpoint_test_fail_private_adopt(position) };
            // SAFETY: this feature-only observation hook takes no pointers and only reads the
            // checkpoint test ledger while no configure transaction is active.
            let private_before = unsafe { crate::bindings::hl_c_backend_checkpoint_test_private_descriptor_count() };
            assert!(
                engine.configure_checkpoint(&transport).is_err(),
                "position {position} unexpectedly succeeded"
            );
            let descriptors_after = std::fs::read_dir(descriptor_directory).unwrap().count();
            // SAFETY: the failed configure transaction has returned, so this feature-only hook
            // only reads the settled checkpoint test ledger and does not alias mutable state.
            let private_after = unsafe { crate::bindings::hl_c_backend_checkpoint_test_private_descriptor_count() };
            assert_eq!(
                descriptors_after, descriptors_before,
                "position {position} leaked a descriptor"
            );
            assert_eq!(
                private_after, private_before,
                "position {position} changed private ownership"
            );
            engine.configure_checkpoint(&transport).unwrap();
        }
    }
}
