#![allow(unsafe_code)]

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
use std::thread::JoinHandle;

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

unsafe extern "C" {
    fn hl_c_backend_create(
        isa: c_uint,
        rootfs: *const c_char,
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

pub(crate) struct CGuestExecutor {
    handle: NonNull<c_void>,
    _streams: StreamBridge,
}

struct StreamBridge {
    output_workers: Vec<JoinHandle<()>>,
    guest_fds: Option<[OwnedFd; 3]>,
}

impl StreamBridge {
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
        if services.streams.terminal().is_some() {
            return Err(EngineError::Unsupported);
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
        })
    }

    fn descriptors(&self) -> [c_int; 3] {
        self.guest_fds
            .as_ref()
            .expect("stream descriptors remain live during create")
            .each_ref()
            .map(AsRawFd::as_raw_fd)
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
    pub(crate) fn create(
        isa: GuestIsa,
        plan: &RuntimeLaunchPlan,
        services: &RuntimeServices,
    ) -> Result<Self, EngineError> {
        if plan.result_path.is_some() {
            hl_log::hl_error!(
                hl_log::tag::EXEC,
                "c execution backend does not yet support result_path"
            );
            return Err(EngineError::Unsupported);
        }
        if plan.options.get("HL_OVERLAY_UPPER").is_some() {
            hl_log::hl_error!(
                hl_log::tag::EXEC,
                "c execution backend does not yet support HL_OVERLAY_UPPER"
            );
            return Err(EngineError::Unsupported);
        }
        let rootfs = plan
            .rootfs
            .as_ref()
            .map(|value| CString::new(value.as_slice()).map_err(|_| EngineError::LaunchFailed))
            .transpose()?;
        let mut option_records = plan
            .options
            .iter()
            .filter(|(name, _)| c_option(name))
            .map(|(name, value)| {
                Ok((
                    CString::new(name).map_err(|_| EngineError::LaunchFailed)?,
                    CString::new(value).map_err(|_| EngineError::LaunchFailed)?,
                ))
            })
            .collect::<Result<Vec<_>, EngineError>>()?;
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
        let streams = StreamBridge::new(services)?;
        let standard_fds = streams.descriptors();
        let mut handle = std::ptr::null_mut();
        let status = unsafe {
            hl_c_backend_create(
                isa as c_uint,
                rootfs.as_ref().map_or(std::ptr::null(), |value| value.as_ptr()),
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
        hl_log::hl_event!(
            hl_log::tag::EXEC,
            hl_log::Level::Info,
            "execution.backend.selected",
            backend = "c",
            isa = ?isa
        );
        Ok(Self {
            handle,
            _streams: streams,
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

    pub(crate) fn start_plan(&self, plan: &RuntimeLaunchPlan) -> Result<(), EngineError> {
        let arguments = plan
            .arguments
            .iter()
            .map(|value| CString::new(value.as_slice()).map_err(|_| EngineError::LaunchFailed))
            .collect::<Result<Vec<_>, _>>()?;
        let pointers = arguments.iter().map(|value| value.as_ptr()).collect::<Vec<_>>();
        let count = c_int::try_from(pointers.len()).map_err(|_| EngineError::LaunchFailed)?;
        let status = unsafe { hl_c_backend_run(self.handle.as_ptr(), count, pointers.as_ptr()) };
        if status == STATUS_OK {
            Ok(())
        } else {
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
        unsafe { hl_c_backend_destroy(self.handle.as_ptr()) };
    }
}

#[cfg(test)]
mod tests {
    use super::StreamBridge;
    use crate::composition::{ActivationChannel, CompositionError, RuntimeServices, StandardStreams};
    use std::io::{Cursor, Read, Write};
    use std::os::fd::FromRawFd;
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct Capture(Arc<Mutex<Vec<u8>>>);
    struct Channel;

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
    fn stream_bridge_preserves_three_application_owned_channels() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let error = Arc::new(Mutex::new(Vec::new()));
        let services = RuntimeServices {
            activation: Arc::new(Channel),
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
}
