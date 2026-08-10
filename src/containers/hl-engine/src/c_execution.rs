#![allow(unsafe_code)]

#[allow(dead_code)]
pub(crate) mod control;
pub(crate) mod process;
pub(crate) mod worker;

use crate::activation::GuestIsa;
use crate::composition::RuntimeServices;
use crate::engine::{EngineError, EngineExit, ExitKind, StopRequest};
use crate::launch_plan::RuntimeLaunchPlan;
use crate::runtime_machine::GuestExecutionPort;
use hl_runtime::RuntimeAssembly;
use std::ffi::{CString, c_char, c_int, c_uint, c_ulonglong, c_void};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::ptr::NonNull;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

mod wire;

const STATUS_OK: c_int = 0;
const REQUEST_INTERRUPT: c_uint = 1;
const REQUEST_FORCE_STOP: c_uint = 2;
const REQUEST_SIGNAL: c_uint = 3;

fn c_option(name: &str) -> bool {
    !matches!(
        name,
        "HL_EXECUTION_BACKEND"
            | "HL_A64_DIRTY_OVERFLOW_CONTINUE"
            | "HL_A64_DIRTY_OVERFLOW_EXIT"
            | "HL_A64_NO_WRITE_COMMIT"
            | "HL_A64_NO_WRITE_RESERVE"
            | "HL_A64_RUNTIME_WRITE_RESERVE"
            | "HL_NATIVE_ADMISSION_CACHE"
            | "HL_NATIVE_DIAGNOSTICS"
            | "HL_NATIVE_DIRECT_HOLD_RUNS"
            | "HL_NATIVE_DIRECT_STICKY"
            | "HL_NATIVE_DIRECT_STICKY_LIMIT"
            | "HL_NATIVE_DIRECT_STICKY_PERMANENT"
            | "HL_NATIVE_EXECUTION"
            | "HL_NATIVE_SPLIT_MODE_EXECUTORS"
            | "HL_SECCOMP_BASELINE"
    )
}

fn c_volume_path(value: &str) -> String {
    value.bytes().fold(String::new(), |mut output, byte| {
        if matches!(byte, b'%' | b':' | b',') {
            use std::fmt::Write as _;
            write!(output, "%{byte:02X}").expect("writing to a String cannot fail");
        } else {
            output.push(char::from(byte));
        }
        output
    })
}

fn c_file_volumes(value: &str) -> Result<Vec<String>, EngineError> {
    value
        .lines()
        .map(|record| {
            let (source, guest) = record.split_once('\t').ok_or(EngineError::LaunchFailed)?;
            let (access, source) = source.split_once(':').ok_or(EngineError::LaunchFailed)?;
            if !matches!(access, "ro" | "rw") || source.is_empty() || !guest.starts_with('/') {
                return Err(EngineError::LaunchFailed);
            }
            Ok(format!(
                "v2:{access}:{}:{}",
                c_volume_path(guest),
                c_volume_path(source)
            ))
        })
        .collect()
}

unsafe extern "C" {
    fn hl_c_backend_create(
        isa: c_uint,
        rootfs: *const c_char,
        executable_host: *const c_char,
        executable_fd: c_int,
        option_count: c_uint,
        option_names: *const *const c_char,
        option_values: *const *const c_char,
        standard_fds: *const c_int,
        output: *mut *mut c_void,
    ) -> c_int;
    fn hl_c_backend_run(backend: *mut c_void, argc: c_int, argv: *const *const c_char) -> c_int;
    fn hl_c_backend_request(backend: *mut c_void, request: c_uint, signal: c_int) -> c_int;
    fn hl_c_backend_exit_kind(backend: *const c_void) -> c_uint;
    fn hl_c_backend_exit_status(backend: *const c_void) -> c_int;
    fn hl_c_backend_exit_detail(backend: *const c_void) -> c_ulonglong;
    fn hl_c_backend_destroy(backend: *mut c_void);
}

#[link(name = "util")]
unsafe extern "C" {
    fn openpty(
        master: *mut c_int,
        slave: *mut c_int,
        name: *mut c_char,
        termios: *const libc::termios,
        window: *const libc::winsize,
    ) -> c_int;
}

pub(crate) struct CGuestExecutor {
    handle: NonNull<c_void>,
    terminal_handle: Arc<Mutex<Option<usize>>>,
    _streams: StreamBridge,
}

struct StreamBridge {
    output_workers: Vec<JoinHandle<()>>,
    guest_fds: Option<[OwnedFd; 3]>,
    terminal: Option<(Arc<crate::composition::Terminal>, OwnedFd)>,
}

struct CTerminalWindowNotification {
    handle: Arc<Mutex<Option<usize>>>,
}

impl crate::composition::NativeTerminalWindowNotification for CTerminalWindowNotification {
    fn resize(
        &self,
        master: &std::fs::File,
        rows: u16,
        columns: u16,
    ) -> Result<(), crate::composition::CompositionError> {
        let window = libc::winsize {
            ws_row: rows,
            ws_col: columns,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: master is a live PTY descriptor and window is initialized.
        if unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &window) } != 0 {
            return Err(crate::composition::CompositionError::RuntimeConstruction);
        }
        let handle = self
            .handle
            .lock()
            .map_err(|_| crate::composition::CompositionError::RuntimeConstruction)?
            .ok_or(crate::composition::CompositionError::RuntimeConstruction)?;
        let status = unsafe { hl_c_backend_request(handle as *mut c_void, REQUEST_SIGNAL, libc::SIGWINCH) };
        (status == STATUS_OK)
            .then_some(())
            .ok_or(crate::composition::CompositionError::RuntimeConstruction)
    }
}

impl StreamBridge {
    fn inherited() -> Self {
        Self {
            output_workers: Vec::new(),
            guest_fds: None,
            terminal: None,
        }
    }

    fn pipe() -> Result<(OwnedFd, OwnedFd), EngineError> {
        let mut descriptors = [-1; 2];
        // SAFETY: descriptors names two writable integers; successful pipe2
        // returns two uniquely owned CLOEXEC descriptors.
        if unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            return Err(EngineError::LaunchFailed);
        }
        // SAFETY: pipe2 succeeded and transferred these distinct descriptors.
        Ok(unsafe {
            (
                OwnedFd::from_raw_fd(descriptors[0]),
                OwnedFd::from_raw_fd(descriptors[1]),
            )
        })
    }

    fn new(services: &RuntimeServices) -> Result<Self, EngineError> {
        if let Some(terminal) = services.streams.terminal() {
            let initial = terminal.initial();
            let window = libc::winsize {
                ws_row: initial.rows,
                ws_col: initial.columns,
                ws_xpixel: initial.pixel_width,
                ws_ypixel: initial.pixel_height,
            };
            let mut master = -1;
            let mut slave = -1;
            // SAFETY: output pointers and the initialized window live for the call.
            if unsafe {
                openpty(
                    &raw mut master,
                    &raw mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    &raw const window,
                )
            } != 0
            {
                return Err(EngineError::LaunchFailed);
            }
            // SAFETY: successful openpty returns two uniquely owned descriptors.
            let master = unsafe { OwnedFd::from_raw_fd(master) };
            // SAFETY: same successful call, with distinct slave ownership.
            let slave = unsafe { OwnedFd::from_raw_fd(slave) };
            let duplicate = |descriptor: &OwnedFd| {
                // SAFETY: descriptor is live; successful dup creates new ownership.
                let duplicate = unsafe { libc::dup(descriptor.as_raw_fd()) };
                if duplicate < 0 {
                    Err(EngineError::LaunchFailed)
                } else {
                    // SAFETY: successful dup returned a new owned descriptor.
                    Ok(unsafe { OwnedFd::from_raw_fd(duplicate) })
                }
            };
            let output = duplicate(&slave)?;
            let error = duplicate(&slave)?;
            return Ok(Self {
                output_workers: Vec::new(),
                guest_fds: Some([slave, output, error]),
                terminal: Some((terminal, master)),
            });
        }
        let (guest_input, host_input) = Self::pipe()?;
        let (host_output, guest_output) = Self::pipe()?;
        let (host_error, guest_error) = Self::pipe()?;

        let input = services.streams.input();
        std::thread::Builder::new()
            .name("hl-c-stdin".into())
            .spawn(move || {
                let mut destination = std::fs::File::from(host_input);
                let mut bytes = [0; 16 * 1024];
                loop {
                    let count = match input.lock() {
                        Ok(mut source) => match source.read(&mut bytes) {
                            Ok(count) => count,
                            Err(error) => {
                                hl_log::hl_error!(hl_log::tag::EXEC, "c stdin bridge read failed: error={error}");
                                return;
                            }
                        },
                        Err(_) => return,
                    };
                    if count == 0 || destination.write_all(&bytes[..count]).is_err() {
                        return;
                    }
                }
            })
            .map_err(|_| EngineError::LaunchFailed)?;

        let mut output_workers = Vec::with_capacity(2);
        for (name, source, destination) in [
            ("hl-c-stdout", host_output, services.streams.output()),
            ("hl-c-stderr", host_error, services.streams.error()),
        ] {
            output_workers.push(
                std::thread::Builder::new()
                    .name(name.into())
                    .spawn(move || {
                        let mut source = std::fs::File::from(source);
                        let mut bytes = [0; 16 * 1024];
                        loop {
                            let count = match source.read(&mut bytes) {
                                Ok(0) => break,
                                Ok(count) => count,
                                Err(error) => {
                                    hl_log::hl_error!(hl_log::tag::EXEC, "c output bridge read failed: error={error}");
                                    break;
                                }
                            };
                            let result = destination
                                .lock()
                                .map_err(|_| ())
                                .and_then(|mut output| output.write_all(&bytes[..count]).map_err(|_| ()));
                            if result.is_err() {
                                break;
                            }
                        }
                    })
                    .map_err(|_| EngineError::LaunchFailed)?,
            );
        }
        Ok(Self {
            output_workers,
            guest_fds: Some([guest_input, guest_output, guest_error]),
            terminal: None,
        })
    }

    #[cfg(test)]
    fn descriptors(&self) -> [c_int; 3] {
        self.guest_fds
            .as_ref()
            .expect("stream descriptors remain live during create")
            .each_ref()
            .map(AsRawFd::as_raw_fd)
    }

    fn attach_terminal(
        &mut self,
        notification: Arc<dyn crate::composition::NativeTerminalWindowNotification>,
    ) -> Result<(), EngineError> {
        let Some((terminal, master)) = self.terminal.take() else {
            return Ok(());
        };
        terminal
            .attach_native(std::fs::File::from(master), notification)
            .map_err(|_| EngineError::LaunchFailed)
    }

    fn take_guest_fds(&mut self) -> Result<[OwnedFd; 3], EngineError> {
        self.guest_fds.take().ok_or(EngineError::LaunchFailed)
    }
}

impl Drop for StreamBridge {
    fn drop(&mut self) {
        drop(self.guest_fds.take());
        for worker in self.output_workers.drain(..) {
            let _ = worker.join();
        }
    }
}

// The C lifecycle contract explicitly permits request() from a second thread
// while run() is active. Ownership remains with this value until Drop.
unsafe impl Send for CGuestExecutor {}
unsafe impl Sync for CGuestExecutor {}

impl CGuestExecutor {
    fn create_with_streams(
        isa: GuestIsa,
        plan: &RuntimeLaunchPlan,
        executable_authority: Option<&crate::executable::ExecutableAuthority>,
        standard_fds: [c_int; 3],
        streams: Option<StreamBridge>,
    ) -> Result<Self, EngineError> {
        if plan.result_path.is_some() {
            hl_log::hl_error!(
                hl_log::tag::EXEC,
                "c execution backend does not yet support result_path"
            );
            return Err(EngineError::Unsupported);
        }
        let rootfs = plan
            .rootfs
            .as_ref()
            .map(|value| CString::new(value.as_slice()).map_err(|_| EngineError::LaunchFailed))
            .transpose()?;
        let executable_host = plan
            .executable_host
            .as_ref()
            .map(|value| CString::new(value.as_slice()).map_err(|_| EngineError::LaunchFailed))
            .transpose()?;
        let mut option_records = plan
            .options
            .iter()
            .filter(|(name, _)| c_option(name) && *name != "HL_NAME_BINDS" && *name != "HL_VOLUMES")
            .map(|(name, value)| {
                Ok((
                    CString::new(name).map_err(|_| EngineError::LaunchFailed)?,
                    CString::new(value).map_err(|_| EngineError::LaunchFailed)?,
                ))
            })
            .collect::<Result<Vec<_>, EngineError>>()?;
        let mut volumes = plan
            .options
            .get("HL_VOLUMES")
            .filter(|value| !value.is_empty())
            .map(|value| value.to_owned())
            .into_iter()
            .collect::<Vec<_>>();
        if let Some(files) = plan.options.get("HL_NAME_BINDS") {
            volumes.extend(c_file_volumes(files)?);
        }
        if !volumes.is_empty() {
            option_records.push((
                CString::new("HL_VOLUMES").unwrap(),
                CString::new(volumes.join(",")).map_err(|_| EngineError::LaunchFailed)?,
            ));
        }
        let mut encoded_environment = Vec::new();
        for (index, record) in plan.environment.iter().enumerate() {
            if index != 0 {
                encoded_environment.push(b'\n');
            }
            for byte in record {
                match byte {
                    b'\\' => encoded_environment.extend_from_slice(b"\\\\"),
                    b'\n' => encoded_environment.extend_from_slice(b"\\n"),
                    byte => encoded_environment.push(*byte),
                }
            }
        }
        option_records.push((
            CString::new("HL_GUEST_ENV").unwrap(),
            CString::new(encoded_environment).map_err(|_| EngineError::LaunchFailed)?,
        ));
        option_records.push((CString::new("HL_GUEST_ENV_ESC").unwrap(), CString::new("1").unwrap()));
        option_records.push((CString::new("HL_GUEST_ENV_EXACT").unwrap(), CString::new("1").unwrap()));
        let option_names = option_records.iter().map(|(name, _)| name.as_ptr()).collect::<Vec<_>>();
        let option_values = option_records
            .iter()
            .map(|(_, value)| value.as_ptr())
            .collect::<Vec<_>>();
        let option_count = c_uint::try_from(option_records.len()).map_err(|_| EngineError::LaunchFailed)?;
        let mut handle = std::ptr::null_mut();
        let status = unsafe {
            hl_c_backend_create(
                isa as c_uint,
                rootfs.as_ref().map_or(std::ptr::null(), |value| value.as_ptr()),
                executable_host
                    .as_ref()
                    .map_or(std::ptr::null(), |value| value.as_ptr()),
                executable_authority.map_or(-1, |authority| authority.descriptor().as_raw_fd()),
                option_count,
                option_names.as_ptr(),
                option_values.as_ptr(),
                standard_fds.as_ptr(),
                &raw mut handle,
            )
        };
        if status != STATUS_OK {
            hl_log::hl_error!(
                hl_log::tag::EXEC,
                "c execution backend create failed: isa={isa:?} status={status}"
            );
            return Err(EngineError::LaunchFailed);
        }
        let handle = NonNull::new(handle).ok_or(EngineError::LaunchFailed)?;
        let terminal_handle = Arc::new(Mutex::new(Some(handle.as_ptr() as usize)));
        let mut streams = streams;
        if let Some(streams) = streams.as_mut()
            && let Err(error) = streams.attach_terminal(Arc::new(CTerminalWindowNotification {
                handle: Arc::clone(&terminal_handle),
            }))
        {
            unsafe { hl_c_backend_destroy(handle.as_ptr()) };
            return Err(error);
        }
        hl_log::hl_event!(
            hl_log::tag::EXEC,
            hl_log::Level::Info,
            "execution.backend.selected",
            backend = "c",
            isa = ?isa
        );
        Ok(Self {
            handle,
            terminal_handle,
            _streams: streams.unwrap_or_else(StreamBridge::inherited),
        })
    }

    pub(crate) fn exit(&self) -> EngineExit {
        let kind = unsafe { hl_c_backend_exit_kind(self.handle.as_ptr()) };
        EngineExit {
            kind: match kind {
                1 => ExitKind::Code,
                2 => ExitKind::Signal,
                3 => ExitKind::Fault,
                _ => ExitKind::EngineError,
            },
            guest_status: unsafe { hl_c_backend_exit_status(self.handle.as_ptr()) },
            detail: unsafe { hl_c_backend_exit_detail(self.handle.as_ptr()) },
            fault: None,
        }
    }

    fn run_plan_status(&self, plan: &RuntimeLaunchPlan) -> Result<c_int, EngineError> {
        let arguments = plan
            .arguments
            .iter()
            .map(|value| CString::new(value.as_slice()).map_err(|_| EngineError::LaunchFailed))
            .collect::<Result<Vec<_>, _>>()?;
        let pointers = arguments.iter().map(|value| value.as_ptr()).collect::<Vec<_>>();
        let count = c_int::try_from(pointers.len()).map_err(|_| EngineError::LaunchFailed)?;
        Ok(unsafe { hl_c_backend_run(self.handle.as_ptr(), count, pointers.as_ptr()) })
    }

    pub(crate) fn start_plan(&self, plan: &RuntimeLaunchPlan) -> Result<(), EngineError> {
        let status = self.run_plan_status(plan)?;
        if status == STATUS_OK {
            Ok(())
        } else {
            hl_log::hl_error!(
                hl_log::tag::EXEC,
                "c execution backend start failed: status={status} argc={}",
                plan.arguments.len()
            );
            Err(EngineError::LaunchFailed)
        }
    }

    pub(crate) fn stop_request(&self, request: StopRequest) -> Result<(), EngineError> {
        let kind = match request {
            StopRequest::Interrupt => REQUEST_INTERRUPT,
            StopRequest::Force => REQUEST_FORCE_STOP,
            StopRequest::Signal(_) => REQUEST_SIGNAL,
        };
        let status = unsafe { hl_c_backend_request(self.handle.as_ptr(), kind, request.signal()) };
        if status == STATUS_OK {
            Ok(())
        } else {
            Err(EngineError::StopFailed)
        }
    }
}

impl GuestExecutionPort for CGuestExecutor {
    fn start(
        &self,
        _: GuestIsa,
        plan: &RuntimeLaunchPlan,
        _: &RuntimeAssembly,
        _: &RuntimeServices,
    ) -> Result<(), EngineError> {
        self.start_plan(plan)
    }

    fn wait(&self, _: &RuntimeAssembly) -> Result<EngineExit, EngineError> {
        Ok(self.exit())
    }

    fn stop(&self, _: &RuntimeAssembly, request: StopRequest) -> Result<(), EngineError> {
        self.stop_request(request)
    }
}

impl Drop for CGuestExecutor {
    fn drop(&mut self) {
        if let Ok(mut handle) = self.terminal_handle.lock() {
            *handle = None;
        }
        unsafe { hl_c_backend_destroy(self.handle.as_ptr()) };
    }
}

#[cfg(test)]
mod tests {
    use super::{CGuestExecutor, StreamBridge, c_file_volumes, c_option};
    use crate::activation::GuestIsa;
    use crate::composition::{
        ActivationChannel, CompositionError, RuntimeServices, StandardStreams, Terminal, TerminalPort,
    };
    use std::io::{Cursor, Read, Seek, Write};
    use std::os::fd::FromRawFd;
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct Capture(Arc<Mutex<Vec<u8>>>);
    struct Channel;
    struct Port;

    impl TerminalPort for Port {
        fn read(&self, _: &mut [u8]) -> std::io::Result<usize> {
            Ok(0)
        }

        fn write(&self, input: &[u8]) -> std::io::Result<usize> {
            Ok(input.len())
        }

        fn close(&self) {}
    }

    impl ActivationChannel for Channel {
        fn send(&self, _: &[u8]) -> Result<(), CompositionError> {
            Ok(())
        }

        fn receive(&self, _: usize) -> Result<Vec<u8>, CompositionError> {
            Ok(Vec::new())
        }
    }

    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn rust_validation_projects_one_lifetime_stable_c_record_set() {
        let mut options = crate::options::Options::default();
        assert!(options.iter().next().is_none());
        options.set("HL_CWD", "", true).unwrap();
        options.set("HL_UID", "7", true).unwrap();
        options.set("HL_UID", "8", false).unwrap();
        options.set("HL_UID", "9", true).unwrap();
        options.set("HL_EXECUTION_BACKEND", "c", true).unwrap();
        assert_eq!(
            options.set("HL_UID", "18446744073709551616", true),
            Err(crate::options::OptionError::InvalidValue)
        );

        let records = options.iter().filter(|(name, _)| c_option(name)).collect::<Vec<_>>();
        assert_eq!(records, [("HL_CWD", b"".as_slice()), ("HL_UID", b"9".as_slice())]);
        assert!(!records.iter().any(|(name, _)| *name == "HL_LOG"));
    }

    fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn exiting_arm(status: u16) -> Vec<u8> {
        const LINK_BASE: u64 = 0x0040_0000;
        const ENTRY_OFFSET: usize = 0x100;
        let mut bytes = vec![0_u8; 4096];
        bytes[..4].copy_from_slice(b"\x7fELF");
        bytes[4..7].copy_from_slice(&[2, 1, 1]);
        put_u16(&mut bytes, 16, 2);
        put_u16(&mut bytes, 18, hl_isa::GuestArchitecture::Aarch64.elf_machine());
        put_u32(&mut bytes, 20, 1);
        put_u64(&mut bytes, 24, LINK_BASE + ENTRY_OFFSET as u64);
        put_u64(&mut bytes, 32, 64);
        put_u16(&mut bytes, 52, 64);
        put_u16(&mut bytes, 54, 56);
        put_u16(&mut bytes, 56, 1);
        put_u32(&mut bytes, 64, 1);
        put_u32(&mut bytes, 68, 5);
        put_u64(&mut bytes, 80, LINK_BASE);
        put_u64(&mut bytes, 88, LINK_BASE);
        let image_length = bytes.len() as u64;
        put_u64(&mut bytes, 96, image_length);
        put_u64(&mut bytes, 104, image_length);
        put_u64(&mut bytes, 112, 4096);
        for (index, instruction) in [
            0xd280_0ba8_u32,
            0xd280_0000_u32 | (u32::from(status) << 5),
            0xd400_0001_u32,
        ]
        .into_iter()
        .enumerate()
        {
            let offset = ENTRY_OFFSET + index * 4;
            bytes[offset..offset + 4].copy_from_slice(&instruction.to_le_bytes());
        }
        bytes
    }

    fn executable_plan(path: &std::path::Path) -> crate::launch_plan::RuntimeLaunchPlan {
        crate::launch_plan::RuntimeLaunchPlan {
            rootfs: None,
            executable_host: Some(path.as_os_str().as_encoded_bytes().to_vec()),
            arguments: vec![b"/guest".to_vec()],
            environment: Vec::new(),
            result_path: None,
            options: crate::options::Options::default(),
        }
    }

    #[test]
    fn descriptor_authority_wins_over_a_replaced_executable_path() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("guest");
        std::fs::write(&path, exiting_arm(42)).unwrap();
        let selected = std::fs::File::open(&path).unwrap();
        let authority = crate::executable::ExecutableAuthority::new(selected.into());
        let replacement = directory.path().join("replacement");
        std::fs::write(&replacement, exiting_arm(7)).unwrap();
        std::fs::rename(replacement, &path).unwrap();
        let plan = executable_plan(&path);

        let selected =
            CGuestExecutor::create_with_streams(GuestIsa::Aarch64, &plan, Some(&authority), [0, 1, 2], None).unwrap();
        drop(authority);
        selected.start_plan(&plan).unwrap();
        assert_eq!(selected.exit().guest_status, 42);

        let replacement = CGuestExecutor::create_with_streams(GuestIsa::Aarch64, &plan, None, [0, 1, 2], None).unwrap();
        replacement.start_plan(&plan).unwrap();
        assert_eq!(replacement.exit().guest_status, 7);
    }

    #[test]
    fn rejected_descriptor_authority_remains_owned_by_the_caller() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("invalid");
        std::fs::write(&path, b"").unwrap();
        let authority = crate::executable::ExecutableAuthority::new(std::fs::File::open(&path).unwrap().into());
        let plan = executable_plan(&path);

        assert!(
            CGuestExecutor::create_with_streams(GuestIsa::Aarch64, &plan, Some(&authority), [0, 1, 2], None).is_err()
        );
        let mut retained = std::fs::File::from(authority.descriptor().try_clone_to_owned().unwrap());
        retained.rewind().unwrap();
        let mut bytes = Vec::new();
        retained.read_to_end(&mut bytes).unwrap();
        assert!(bytes.is_empty());
    }

    #[test]
    fn exact_file_bindings_translate_to_retained_volume_records() {
        assert_eq!(
            c_file_volumes("ro:/host/a:b\t/etc/a,b\nrw:/host/c\t/run/c").unwrap(),
            ["v2:ro:/etc/a%2Cb:/host/a%3Ab", "v2:rw:/run/c:/host/c",]
        );
    }

    #[test]
    fn stream_bridge_preserves_three_application_owned_channels() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let error = Arc::new(Mutex::new(Vec::new()));
        let services = RuntimeServices {
            activation: Arc::new(Channel),
            executable_authority: None,
            checkpoint_sink: None,
            checkpoint_source: None,
            streams: StandardStreams::new(
                Cursor::new(b"input".to_vec()),
                Capture(Arc::clone(&output)),
                Capture(Arc::clone(&error)),
            ),
        };
        let mut bridge = StreamBridge::new(&services).unwrap();
        let descriptors = bridge.descriptors();
        // SAFETY: dup creates independent owned descriptors from live bridge ends.
        let mut input = unsafe { std::fs::File::from_raw_fd(libc::dup(descriptors[0])) };
        let mut stdout = unsafe { std::fs::File::from_raw_fd(libc::dup(descriptors[1])) };
        let mut stderr = unsafe { std::fs::File::from_raw_fd(libc::dup(descriptors[2])) };
        let mut bytes = [0; 5];
        input.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"input");
        stdout.write_all(b"out").unwrap();
        stderr.write_all(b"err").unwrap();
        drop((input, stdout, stderr));
        drop(bridge.guest_fds.take());
        drop(bridge);
        assert_eq!(&*output.lock().unwrap(), b"out");
        assert_eq!(&*error.lock().unwrap(), b"err");
    }

    #[test]
    fn terminal_bridge_creates_shared_tty_descriptors_at_initial_size() {
        let terminal = Terminal::new(Arc::new(Port), 37, 91).unwrap();
        let services = RuntimeServices {
            activation: Arc::new(Channel),
            executable_authority: None,
            checkpoint_sink: None,
            checkpoint_source: None,
            streams: StandardStreams::new(Cursor::new(Vec::new()), Vec::new(), Vec::new()).with_terminal(terminal),
        };
        let bridge = StreamBridge::new(&services).unwrap();
        let descriptors = bridge.descriptors();
        for descriptor in descriptors {
            assert_eq!(unsafe { libc::isatty(descriptor) }, 1);
            let mut window = libc::winsize {
                ws_row: 0,
                ws_col: 0,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            assert_eq!(unsafe { libc::ioctl(descriptor, libc::TIOCGWINSZ, &raw mut window) }, 0);
            assert_eq!((window.ws_row, window.ws_col), (37, 91));
        }
    }
}
