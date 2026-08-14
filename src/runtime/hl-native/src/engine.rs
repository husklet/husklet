#![allow(unsafe_code)]

use std::{
    ffi::{c_char, c_int, c_uint},
    fs::File,
    io::{Read, Seek, SeekFrom},
    ptr::NonNull,
};

use crate::bindings::{self, Backend};

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
                &raw const image_plan,
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
        let layout = ProgramLayout::inspect(
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
            // ET_EXEC images are always stored away from their fixed guest link addresses. This keeps
            // host address ownership independent of executable-specific assumptions; the C projection
            // layer translates every guest-visible address back to the ELF link range.
            flags: u32::from(kind == 1),
            interpreter_identity,
        })
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

impl ProgramLayout {
    fn inspect(
        file: &mut File,
        image_length: u64,
        entry: u64,
        phoff: u64,
        phentsize: u64,
        phnum: u16,
    ) -> Result<Self, i32> {
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
                        || (alignment > 1
                            && (!alignment.is_power_of_two() || start % alignment != file_offset % alignment))
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
        Ok(Self {
            load_start: first,
            load_end: last,
            interpreter,
            entry_is_executable,
        })
    }
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
    use super::{Engine, EngineConfig, Exit, Plan};
    use std::{
        ffi::CString,
        fs::OpenOptions,
        io::{Seek, SeekFrom, Write},
        os::fd::AsRawFd,
        path::PathBuf,
    };

    #[cfg(feature = "native-test-hooks")]
    use std::time::{Duration, Instant};

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
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "engine::tests::checkpoint_control_transaction_serializes_readiness_and_acknowledgement",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    assert!(status.success(), "checkpoint transaction child failed: {status}");
                    return;
                }
                if Instant::now() >= deadline {
                    child.kill().unwrap();
                    let _ = child.wait();
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
                while unsafe { crate::bindings::hl_c_backend_checkpoint_test_phase() } != 2 {
                    assert!(
                        Instant::now() < deadline,
                        "ISA {isa} request did not acquire checkpoint transaction"
                    );
                    std::thread::yield_now();
                }
                let running = scope.spawn(|| engine.run(&[argument.as_ptr()]));
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
                assert_eq!(unsafe { crate::bindings::hl_c_backend_checkpoint_test_phase() }, 6);
                let deadline = Instant::now() + Duration::from_secs(5);
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
