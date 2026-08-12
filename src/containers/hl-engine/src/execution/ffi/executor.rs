#![allow(unsafe_code)]

use std::ffi::{CString, c_int, c_uint};
use std::os::fd::AsRawFd;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex, OnceLock};

use super::super::{
    CGuestExecutor, CSyscallTrapContext, CTerminalWindowNotification, EngineError, EngineExit, ExitKind, GuestIsa,
    REQUEST_FORCE_STOP, REQUEST_INTERRUPT, REQUEST_SIGNAL, RuntimeLaunchPlan, STATUS_OK, StopRequest, StreamBridge,
    c_file_volumes, c_main_image_plan, c_option, c_syscall_trap, hl_c_backend_create, hl_c_backend_destroy,
    hl_c_backend_exit_detail, hl_c_backend_exit_kind, hl_c_backend_exit_status, hl_c_backend_request, hl_c_backend_run,
};

// The C lifecycle contract explicitly permits request() from a second thread
// while run() is active. Ownership remains with this value until Drop.
unsafe impl Send for CGuestExecutor {}
unsafe impl Sync for CGuestExecutor {}

impl CGuestExecutor {
    fn encode_environment_byte(encoded: &mut Vec<u8>, byte: u8) {
        match byte {
            b'\\' => encoded.extend_from_slice(b"\\\\"),
            b'\n' => encoded.extend_from_slice(b"\\n"),
            byte => encoded.push(byte),
        }
    }

    fn encode_environment(environment: &[Vec<u8>]) -> Vec<u8> {
        let mut encoded = Vec::new();
        for (index, record) in environment.iter().enumerate() {
            if index != 0 {
                encoded.push(b'\n');
            }
            for byte in record {
                Self::encode_environment_byte(&mut encoded, *byte);
            }
        }
        encoded
    }

    pub(in crate::execution) fn create_with_streams(
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
            .map(std::borrow::ToOwned::to_owned)
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
        option_records.push((
            CString::new("HL_GUEST_ENV").unwrap(),
            CString::new(Self::encode_environment(&plan.environment)).map_err(|_| EngineError::LaunchFailed)?,
        ));
        option_records.push((CString::new("HL_GUEST_ENV_ESC").unwrap(), CString::new("1").unwrap()));
        option_records.push((CString::new("HL_GUEST_ENV_EXACT").unwrap(), CString::new("1").unwrap()));
        let option_names = option_records.iter().map(|(name, _)| name.as_ptr()).collect::<Vec<_>>();
        let option_values = option_records
            .iter()
            .map(|(_, value)| value.as_ptr())
            .collect::<Vec<_>>();
        let option_count = c_uint::try_from(option_records.len()).map_err(|_| EngineError::LaunchFailed)?;
        let image_plan = c_main_image_plan(isa, executable_host.as_ref(), executable_authority)?;
        let mut handle = std::ptr::null_mut();
        let retained_exit = Arc::new(hl_runtime::RetainedExitTrap);
        let runtime_exit = isa == GuestIsa::Aarch64 && plan.options.get("HL_C_NO_RUNTIME_EXIT").is_none();
        let runtime_identity = runtime_exit && plan.options.get("HL_C_NO_RUNTIME_IDENTITY").is_none();
        let mut syscall_trap = Box::new(CSyscallTrapContext {
            trap: runtime_exit.then(|| Arc::clone(&retained_exit) as Arc<dyn hl_runtime::RuntimeSyscallTrap>),
            retained_exit: runtime_exit.then_some(retained_exit),
            retained_tasks: runtime_identity.then(OnceLock::new),
        });
        let syscall_context = if runtime_exit {
            (&raw mut *syscall_trap).cast()
        } else {
            std::ptr::null_mut()
        };
        let provider_descriptor = std::env::var("HL_C_PROVIDER_FD")
            .ok()
            .filter(|value| !value.is_empty() && value.len() <= 10 && value.bytes().all(|byte| byte.is_ascii_digit()))
            .and_then(|value| value.parse::<c_int>().ok())
            .filter(|value| *value >= 3)
            .unwrap_or(-1);
        // SAFETY: all borrowed records remain valid for the call; the backend copies configuration.
        let status = unsafe {
            hl_c_backend_create(
                isa as c_uint,
                rootfs.as_ref().map_or(std::ptr::null(), |value| value.as_ptr()),
                executable_host
                    .as_ref()
                    .map_or(std::ptr::null(), |value| value.as_ptr()),
                executable_authority.map_or(-1, |authority| authority.descriptor().as_raw_fd()),
                &raw const image_plan,
                option_count,
                option_names.as_ptr(),
                option_values.as_ptr(),
                standard_fds.as_ptr(),
                provider_descriptor,
                syscall_context,
                runtime_exit.then_some(c_syscall_trap),
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
            // SAFETY: creation returned this uniquely owned handle and it has not escaped.
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
            _syscall_trap: syscall_trap,
        })
    }

    pub(crate) fn exit(&self) -> EngineExit {
        // SAFETY: self owns a live backend handle until Drop.
        let kind = unsafe { hl_c_backend_exit_kind(self.handle.as_ptr()) };
        EngineExit {
            kind: match kind {
                1 => ExitKind::Code,
                2 => ExitKind::Signal,
                3 => ExitKind::Fault,
                _ => ExitKind::EngineError,
            },
            // SAFETY: self owns a live backend handle.
            guest_status: unsafe { hl_c_backend_exit_status(self.handle.as_ptr()) },
            // SAFETY: self owns a live backend handle.
            detail: unsafe { hl_c_backend_exit_detail(self.handle.as_ptr()) },
            fault: None,
        }
    }

    pub(in crate::execution) fn run_plan_status(&self, plan: &RuntimeLaunchPlan) -> Result<c_int, EngineError> {
        let arguments = plan
            .arguments
            .iter()
            .map(|value| CString::new(value.as_slice()).map_err(|_| EngineError::LaunchFailed))
            .collect::<Result<Vec<_>, _>>()?;
        let pointers = arguments.iter().map(|value| value.as_ptr()).collect::<Vec<_>>();
        let count = c_int::try_from(pointers.len()).map_err(|_| EngineError::LaunchFailed)?;
        // SAFETY: count matches pointers and all strings remain alive for the call.
        Ok(unsafe { hl_c_backend_run(self.handle.as_ptr(), count, pointers.as_ptr()) })
    }

    #[cfg(test)]
    pub(crate) fn start_plan(&self, plan: &RuntimeLaunchPlan) -> Result<(), EngineError> {
        (self.run_plan_status(plan)? == STATUS_OK)
            .then_some(())
            .ok_or(EngineError::LaunchFailed)
    }

    pub(crate) fn stop_request(&self, request: StopRequest) -> Result<(), EngineError> {
        let kind = match request {
            StopRequest::Interrupt => REQUEST_INTERRUPT,
            StopRequest::Force => REQUEST_FORCE_STOP,
            StopRequest::Signal(_) => REQUEST_SIGNAL,
        };
        // SAFETY: backend permits request while run is active.
        let status = unsafe { hl_c_backend_request(self.handle.as_ptr(), kind, request.signal()) };
        (status == STATUS_OK).then_some(()).ok_or(EngineError::StopFailed)
    }
}

impl Drop for CGuestExecutor {
    fn drop(&mut self) {
        if let Ok(mut handle) = self.terminal_handle.lock() {
            *handle = None;
        }
        // SAFETY: this executor uniquely owns the live handle and Drop runs once.
        unsafe { hl_c_backend_destroy(self.handle.as_ptr()) };
    }
}
