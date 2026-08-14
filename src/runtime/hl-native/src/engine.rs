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
use image::resolve_layered_guest;
mod layout;
use layout::validate_elf_image;

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
    pub unsafe fn create(config: EngineConfig<'_>) -> Result<Self, i32> {
        // SAFETY: forwarded unchanged; the hook does not observe raw inputs.
        unsafe { Self::create_after_pinning(config, || {}) }
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
            // ET_EXEC images are always stored away from their fixed guest link addresses. This keeps
            // host address ownership independent of executable-specific assumptions; the C projection
            // layer translates every guest-visible address back to the ELF link range.
            flags: u32::from(kind == 1),
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
        os::{fd::AsRawFd, unix::fs::PermissionsExt as _},
        path::PathBuf,
    };

    #[cfg(feature = "native-test-hooks")]
    use std::time::{Duration, Instant};

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
            if let Err(error) = deliver(-process) {
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error);
                }
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
        use std::io::BufRead as _;
        use std::process::Stdio;

        let mut command = std::process::Command::new("/bin/sh");
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
                .unwrap()
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
            option_names: &[],
            option_values: &[],
            standard_fds: [standard.as_raw_fd(); 3],
            provider_fd: -1,
        };
        // SAFETY: all descriptors and borrowed slices remain live through create;
        // the bridge copies configuration and imports its own descriptor handles.
        let engine = unsafe { Engine::create(config) }.unwrap();
        (engine, standard)
    }

    #[test]
    fn pathname_replacement_cannot_change_pinned_initial_image() {
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
                standard_fds: [standard.as_raw_fd(); 3],
                provider_fd: -1,
            };
            // SAFETY: every borrowed string and descriptor remains live through create.
            assert!(unsafe { Engine::create(config) }.is_err());
        }
    }

    #[test]
    fn unlinked_pinned_image_can_reexec_proc_self_exe_on_both_isas() {
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
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(root.path().join("bin")).unwrap();
            let source = root.path().join("self.c");
            let main_path = root.path().join("bin/main");
            let second_path = root.path().join("next");
            let dac_path = root.path().join("dac");
            let busy_path = root.path().join("busy");
            std::fs::write(&source, SOURCE).unwrap();
            for (identity, output) in [(1, &main_path), (2, &second_path), (3, &dac_path), (4, &busy_path)] {
                let compile = std::process::Command::new(compiler)
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
            let root = tempfile::tempdir().unwrap();
            let source = root.path().join("authority.c");
            std::fs::write(&source, SOURCE).unwrap();
            let compile = |arguments: &[&str], input: &std::path::Path, output: &std::path::Path| {
                let result = std::process::Command::new(compiler)
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

    fn resolved(mut roots: Vec<std::fs::File>, path: &str) -> Option<String> {
        let mut file = resolve_layered_guest(std::path::Path::new(path), &roots).ok()??;
        let mut value = String::new();
        file.read_to_string(&mut value).ok()?;
        roots.clear();
        Some(value)
    }

    #[test]
    fn interpreter_union_authority_handles_layers_links_and_deletions() {
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
    fn pinned_interpreter_survives_ancestor_replacement() {
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
    fn executable_plan_requires_generic_displacement() {
        let plan = inspect(&image()).unwrap();
        assert_eq!(plan.kind, 1);
        assert_eq!(plan.flags, 1);
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

    #[test]
    fn concurrent_exit_reads_only_coherent_publications() {
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
                    let mut values = Vec::with_capacity(50_000);
                    for _ in 0..50_000 {
                        values.push(engine.exit());
                    }
                    engine.request(2, 0).unwrap();
                    for _ in 0..1_000_000 {
                        let value = engine.exit();
                        values.push(value);
                        if value != initial {
                            break;
                        }
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
    fn checkpoint_control_transaction_serializes_readiness_and_acknowledgement() {
        const CHILD: &str = "HL_NATIVE_CHECKPOINT_TRANSACTION_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "engine::tests::checkpoint_control_transaction_serializes_readiness_and_acknowledgement",
                    "--nocapture",
                ])
                .env(CHILD, "1");
            let mut child = IsolatedTestChild::spawn(command).unwrap();
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    assert!(status.success(), "checkpoint transaction child failed: {status}");
                    return;
                }
                if Instant::now() >= deadline {
                    panic!("checkpoint transaction child exceeded 15 seconds");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        for isa in [1, 2] {
            let (mut engine, _standard) = create_engine(isa);
            let (_broker, transport) = crate::CheckpointTransport::create().unwrap();
            engine.configure_checkpoint(&transport).unwrap();
            let argument = CString::new("guest").unwrap();

            // SAFETY: these test-feature-only functions own a process-global
            // deterministic barrier and take no caller-provided pointers.
            assert_eq!(unsafe { crate::bindings::hl_c_backend_checkpoint_test_arm() }, 1);
            std::thread::scope(|scope| {
                let request = scope.spawn(|| engine.request(4, 0));
                let deadline = Instant::now() + Duration::from_secs(5);
                // SAFETY: the test barrier owns its process-global state and takes no pointers.
                while unsafe { crate::bindings::hl_c_backend_checkpoint_test_phase() } != 2 {
                    assert!(
                        Instant::now() < deadline,
                        "ISA {isa} request did not acquire checkpoint transaction"
                    );
                    std::thread::yield_now();
                }
                let running = scope.spawn(|| engine.run(&[argument.as_ptr()]));
                // SAFETY: the test barrier owns its process-global state and takes no pointers.
                while unsafe { crate::bindings::hl_c_backend_checkpoint_test_phase() } != 3 {
                    assert!(
                        Instant::now() < deadline,
                        "ISA {isa} guest process did not reach checkpoint control"
                    );
                    std::thread::yield_now();
                }
                let _generation = transport.bump();
                // SAFETY: phase 2 proves the request owns the transaction lock;
                // release lets it consume the sole readiness byte and complete
                // the full command/ack exchange before run may inspect it.
                unsafe { crate::bindings::hl_c_backend_checkpoint_test_release() };
                assert_eq!(
                    request.join().unwrap(),
                    Err(12),
                    "ISA {isa} zero-executor acknowledgement changed"
                );
                // SAFETY: the test barrier owns its process-global state and takes no pointers.
                assert_eq!(unsafe { crate::bindings::hl_c_backend_checkpoint_test_phase() }, 6);
                let deadline = Instant::now() + Duration::from_secs(5);
                // SAFETY: the test barrier owns its process-global state and takes no pointers.
                while unsafe { crate::bindings::hl_c_backend_checkpoint_test_phase() } != 7 {
                    assert!(
                        Instant::now() < deadline,
                        "ISA {isa} run did not cross serialized readiness"
                    );
                    std::thread::yield_now();
                }
                engine.request(2, 0).unwrap();
                assert_eq!(
                    running.join().unwrap(),
                    Ok(()),
                    "ISA {isa} run failed after checkpoint ack"
                );
            });
            // SAFETY: the child has joined every user of the feature-only hook.
            unsafe { crate::bindings::hl_c_backend_checkpoint_test_reset() };
        }
    }
}
