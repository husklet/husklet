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

unsafe extern "C" {
    fn hl_c_backend_create(isa: c_uint, rootfs: *const c_char, output: *mut *mut c_void) -> c_int;
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
        let rootfs = plan
            .rootfs
            .as_ref()
            .map(|value| CString::new(value.as_slice()).map_err(|_| EngineError::LaunchFailed))
            .transpose()?;
        let mut handle = std::ptr::null_mut();
        let status = unsafe {
            hl_c_backend_create(
                isa as c_uint,
                rootfs.as_ref().map_or(std::ptr::null(), |value| value.as_ptr()),
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
