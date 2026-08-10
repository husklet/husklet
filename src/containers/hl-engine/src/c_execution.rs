#![allow(unsafe_code)]

use crate::activation::GuestIsa;
use crate::composition::RuntimeServices;
use crate::engine::{EngineError, EngineExit, ExitKind, StopRequest};
use crate::launch_plan::RuntimeLaunchPlan;
use crate::runtime_machine::GuestExecutionPort;
use hl_runtime::RuntimeAssembly;
use std::ffi::{CString, c_char, c_int, c_uint, c_ulonglong, c_void};
use std::ptr::NonNull;

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
}

// The C lifecycle contract explicitly permits request() from a second thread
// while run() is active. Ownership remains with this value until Drop.
unsafe impl Send for CGuestExecutor {}
unsafe impl Sync for CGuestExecutor {}

impl CGuestExecutor {
    pub(crate) fn create(isa: GuestIsa, plan: &RuntimeLaunchPlan) -> Result<Self, EngineError> {
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
        let mut handle = std::ptr::null_mut();
        let status = unsafe {
            hl_c_backend_create(
                isa as c_uint,
                rootfs.as_ref().map_or(std::ptr::null(), |value| value.as_ptr()),
                option_count,
                option_names.as_ptr(),
                option_values.as_ptr(),
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
        Ok(Self { handle })
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
