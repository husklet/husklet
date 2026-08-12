#![allow(unsafe_code)]

#[allow(dead_code)]
pub(crate) mod control;
pub(crate) mod process;
pub(crate) mod worker;

use crate::activation::GuestIsa;
use crate::composition::RuntimeServices;
use crate::engine::{EngineError, EngineExit, ExitKind, StopRequest};
use crate::launch_plan::RuntimeLaunchPlan;
use std::ffi::{CString, c_char, c_int, c_uint, c_ulonglong, c_void};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileExt;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;

mod wire;

const STATUS_OK: c_int = 0;
const REQUEST_INTERRUPT: c_uint = 1;
const REQUEST_FORCE_STOP: c_uint = 2;
const REQUEST_SIGNAL: c_uint = 3;
const SYSCALL_TRAP_ABI: u32 = 1;
const SYSCALL_TRAP_DECLINED: u32 = 0;
const SYSCALL_TRAP_CONTINUE: u32 = 1;
const SYSCALL_TRAP_EXIT: u32 = 2;
const SYSCALL_TRAP_FAULT: u32 = 3;
const SYSCALL_TRAP_REPLACE_IMAGE: u32 = 4;
const TASK_EVENT_CLONE_THREAD: u64 = u64::MAX;
const TASK_EVENT_FORK_PROCESS: u64 = u64::MAX - 1;
const TASK_EVENT_EXIT_THREAD: u64 = u64::MAX - 2;
const TASK_EVENT_PREPARE_FORK: u64 = u64::MAX - 3;
const TASK_EVENT_CANCEL_FORK: u64 = u64::MAX - 4;
const TASK_EVENT_EXEC_THREAD: u64 = u64::MAX - 5;
const TASK_EVENT_REAP_PROCESS: u64 = u64::MAX - 6;
const TASK_EVENT_CREDENTIALS_CHANGED: u64 = u64::MAX - 7;

#[cfg(test)]
static EVENT_CAPTURE_LOCK: Mutex<()> = Mutex::new(());

#[repr(C)]
struct CSyscallCpuAarch64 {
    abi: u32,
    size: u32,
    x: [u64; 31],
    sp: u64,
    pc: u64,
    tls: u64,
    nzcv: u64,
    task: u64,
}

#[repr(C)]
struct CSyscallTrapResult {
    abi: u32,
    size: u32,
    outcome: u32,
    exit_status: i32,
    image_generation: u64,
}

impl CSyscallTrapResult {
    fn fault(&mut self) -> c_int {
        hl_log::hl_error!(hl_log::tag::EXEC, "retained runtime syscall transition failed closed");
        self.outcome = SYSCALL_TRAP_FAULT;
        0
    }
}

struct CSyscallTrapContext {
    trap: Option<Arc<dyn hl_runtime::RuntimeSyscallTrap>>,
    retained_exit: Option<Arc<hl_runtime::RetainedExitTrap>>,
    retained_tasks: Option<OnceLock<Arc<hl_runtime::RetainedTaskContext>>>,
}

fn retained_tasks(context: &CSyscallTrapContext, initial_task: u64) -> Option<&hl_runtime::RetainedTaskContext> {
    let slot = context.retained_tasks.as_ref()?;
    if slot.get().is_none() {
        let initial_task = u32::try_from(initial_task).ok()?;
        let tasks = Arc::new(hl_runtime::RetainedTaskContext::new_init(initial_task).ok()?);
        let _ = slot.set(tasks);
    }
    slot.get().map(Arc::as_ref)
}

unsafe extern "C" fn c_syscall_trap(
    context: *mut c_void,
    architecture: c_uint,
    cpu: *mut CSyscallCpuAarch64,
    result: *mut CSyscallTrapResult,
) -> c_int {
    if context.is_null() || cpu.is_null() || result.is_null() {
        return -1;
    }
    // SAFETY: the null checks above establish non-null pointers; the C callback contract
    // supplies uniquely borrowed CPU and result records for the duration of this call.
    let cpu = unsafe { &mut *cpu };
    // SAFETY: as above, the backend owns this result record and grants this callback its
    // only mutable borrow until the callback returns.
    let result = unsafe { &mut *result };
    if cpu.abi != SYSCALL_TRAP_ABI || cpu.size < std::mem::size_of::<CSyscallCpuAarch64>() as u32 {
        return -1;
    }
    result.abi = SYSCALL_TRAP_ABI;
    result.size = std::mem::size_of::<CSyscallTrapResult>() as u32;
    result.outcome = SYSCALL_TRAP_DECLINED;
    // SAFETY: create_with_streams passes a pointer to a boxed CSyscallTrapContext whose
    // allocation remains pinned and alive until after the backend is destroyed.
    let context = unsafe { &*(context.cast::<CSyscallTrapContext>()) };
    if matches!(
        cpu.x[8],
        TASK_EVENT_CLONE_THREAD
            | TASK_EVENT_FORK_PROCESS
            | TASK_EVENT_EXIT_THREAD
            | TASK_EVENT_PREPARE_FORK
            | TASK_EVENT_CANCEL_FORK
            | TASK_EVENT_EXEC_THREAD
            | TASK_EVENT_REAP_PROCESS
            | TASK_EVENT_CREDENTIALS_CHANGED
    ) {
        if context.retained_tasks.is_none() {
            return 0;
        }
        let Some(tasks) = retained_tasks(context, cpu.task) else {
            return result.fault();
        };
        if cpu.x[8] == TASK_EVENT_CREDENTIALS_CHANGED {
            let Ok(task) = u32::try_from(cpu.task) else {
                return result.fault();
            };
            let mut credentials = [0_u32; 8];
            for (destination, source) in credentials.iter_mut().zip(cpu.x) {
                let Ok(value) = u32::try_from(source) else {
                    return result.fault();
                };
                *destination = value;
            }
            if tasks.publish_credentials(task, credentials) != hl_runtime::RuntimeTrapOutcome::Continue {
                return result.fault();
            }
            result.outcome = SYSCALL_TRAP_CONTINUE;
            return 0;
        }
        let Ok(source) = u32::try_from(cpu.x[1]) else {
            return result.fault();
        };
        let Ok(value) = u32::try_from(cpu.x[0]) else {
            return result.fault();
        };
        let outcome = match cpu.x[8] {
            TASK_EVENT_CLONE_THREAD => tasks.clone_thread(source, value),
            TASK_EVENT_FORK_PROCESS => tasks.complete_fork_process(source, value, cpu.x[2] != 0),
            TASK_EVENT_EXIT_THREAD => tasks.exit_thread(source),
            TASK_EVENT_PREPARE_FORK => tasks.prepare_fork_process(),
            TASK_EVENT_CANCEL_FORK => tasks.cancel_fork_process(),
            TASK_EVENT_EXEC_THREAD => tasks.exec_thread(source),
            TASK_EVENT_REAP_PROCESS => tasks.reap_process(source, value, cpu.x[2] as u32),
            _ => unreachable!(),
        };
        if outcome != hl_runtime::RuntimeTrapOutcome::Continue {
            return result.fault();
        }
        result.outcome = SYSCALL_TRAP_CONTINUE;
        return 0;
    }
    if matches!(cpu.x[8], 172..=178) {
        if context.retained_tasks.is_some() {
            let Some(tasks) = retained_tasks(context, cpu.task) else {
                return result.fault();
            };
            let task = u32::try_from(cpu.task).unwrap_or(u32::MAX);
            let (outcome, value) = tasks.dispatch_aarch64(cpu.x[8], task);
            if outcome != hl_runtime::RuntimeTrapOutcome::Continue {
                return result.fault();
            }
            cpu.x[0] = value;
            result.outcome = SYSCALL_TRAP_CONTINUE;
            return 0;
        }
        return 0;
    }
    if let Some(trap) = context.retained_exit.as_deref() {
        match trap.dispatch_aarch64(cpu.x[8], cpu.x[0]) {
            hl_runtime::RuntimeTrapOutcome::Exit(status) => {
                result.outcome = SYSCALL_TRAP_EXIT;
                result.exit_status = status;
                return 0;
            }
            _ => return result.fault(),
        }
    }
    let Some(trap) = context.trap.as_deref() else {
        return 0;
    };
    if architecture != GuestIsa::Aarch64 as c_uint {
        return -1;
    }
    let mut state = hl_runtime::Aarch64CpuState {
        registers: cpu.x,
        sp: cpu.sp,
        pc: cpu.pc,
        tls: cpu.tls,
        nzcv: hl_runtime::Nzcv::from_bits(cpu.nzcv as u32),
        ..Default::default()
    };
    let mut snapshot = hl_runtime::ExecutionCpuSnapshot::Aarch64(state.clone());
    let outcome = trap.dispatch(hl_isa::GuestArchitecture::Aarch64, &mut snapshot);
    let hl_runtime::ExecutionCpuSnapshot::Aarch64(updated) = snapshot else {
        return -1;
    };
    state = updated;
    cpu.x = state.registers;
    cpu.sp = state.sp;
    cpu.pc = state.pc;
    cpu.tls = state.tls;
    cpu.nzcv = u64::from(state.nzcv.bits());
    match outcome {
        hl_runtime::RuntimeTrapOutcome::Continue => result.outcome = SYSCALL_TRAP_CONTINUE,
        hl_runtime::RuntimeTrapOutcome::ReplaceImage { generation } => {
            result.outcome = SYSCALL_TRAP_REPLACE_IMAGE;
            result.image_generation = generation;
        }
        hl_runtime::RuntimeTrapOutcome::Exit(status) => {
            result.outcome = SYSCALL_TRAP_EXIT;
            result.exit_status = status;
        }
        hl_runtime::RuntimeTrapOutcome::Fault => result.outcome = SYSCALL_TRAP_FAULT,
    }
    0
}

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
            | "HL_C_NO_RUNTIME_EXIT"
            | "HL_C_NO_RUNTIME_IDENTITY"
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

#[derive(Debug, Eq, PartialEq)]
#[repr(C)]
struct CMainImagePlan {
    abi: u32,
    size: u32,
    architecture: u32,
    kind: u32,
    link_start: u64,
    link_end: u64,
    has_interpreter: u32,
    reserved: u32,
    interpreter_identity: u64,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(C)]
struct CAddressProjection {
    abi: u32,
    size: u32,
    flags: u32,
    reserved: u32,
    guest_start: u64,
    guest_end: u64,
    storage_bias: u64,
}

struct CImageFile(std::fs::File);

impl hl_loader::ImageReadAt for CImageFile {
    fn length(&self) -> Result<u64, ()> {
        self.0.metadata().map(|metadata| metadata.len()).map_err(|_| ())
    }

    fn read_exact_at(&self, offset: u64, output: &mut [u8]) -> Result<(), ()> {
        self.0.read_exact_at(output, offset).map_err(|_| ())
    }
}

fn c_main_image_plan(
    isa: GuestIsa,
    path: Option<&CString>,
    authority: Option<&crate::executable::ExecutableAuthority>,
) -> Result<CMainImagePlan, EngineError> {
    let source = if authority.is_some() { "authority" } else { "path" };
    let reject = |stage| {
        hl_log::hl_verdict!(
            hl_log::tag::EXEC,
            "execution.c.image_plan.rejected",
            isa = ?isa,
            source = %source,
            stage = %stage;
            "retained C image plan rejected isa={isa:?} source={source} stage={stage}"
        );
        EngineError::LaunchFailed
    };
    let file = if let Some(authority) = authority {
        // SAFETY: dup creates independent ownership; File closes only that duplicate.
        let descriptor = unsafe { libc::dup(authority.descriptor().as_raw_fd()) };
        if descriptor < 0 {
            return Err(reject("duplicate"));
        }
        // SAFETY: descriptor is the newly owned duplicate above.
        unsafe { std::fs::File::from_raw_fd(descriptor) }
    } else {
        let path = path.ok_or_else(|| reject("select"))?;
        std::fs::File::open(std::ffi::OsStr::from_bytes(path.as_bytes())).map_err(|_| reject("open"))?
    };
    let architecture = match isa {
        GuestIsa::Aarch64 => hl_isa::GuestArchitecture::Aarch64,
        GuestIsa::X86_64 => hl_isa::GuestArchitecture::X86_64,
    };
    let plan = hl_loader::MainImageInspector::new(architecture, hl_loader::ImageLimits::default())
        .inspect(&CImageFile(file))
        .map_err(|_| reject("inspect"))?;
    let kind = match plan.kind {
        hl_loader::ImageKind::Executable => 1,
        hl_loader::ImageKind::PositionIndependent => 2,
    };
    let interpreter_identity = plan.interpreter.as_deref().map_or(0, |path| {
        path.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
        })
    });
    Ok(CMainImagePlan {
        abi: 1,
        size: u32::try_from(std::mem::size_of::<CMainImagePlan>()).unwrap(),
        architecture: isa as u32,
        kind,
        link_start: plan.link_start,
        link_end: plan.link_end,
        has_interpreter: u32::from(plan.interpreter.is_some()),
        reserved: 0,
        interpreter_identity,
    })
}

unsafe extern "C" {
    #[cfg(test)]
    fn hl_native_address_projection_init(
        projection: *mut CAddressProjection,
        guest_start: u64,
        guest_end: u64,
        storage_start: u64,
    ) -> c_int;
    #[cfg(test)]
    fn hl_native_address_projection_init_elf(
        projection: *mut CAddressProjection,
        kind: u32,
        link_start: u64,
        link_end: u64,
        storage_start: u64,
    ) -> c_int;
    #[cfg(test)]
    fn hl_native_address_projection_storage(
        projection: *const CAddressProjection,
        guest: u64,
        storage: *mut u64,
    ) -> c_int;
    #[cfg(test)]
    fn hl_native_address_projection_guest(
        projection: *const CAddressProjection,
        storage: u64,
        guest: *mut u64,
    ) -> c_int;
    fn hl_c_backend_create(
        isa: c_uint,
        rootfs: *const c_char,
        executable_host: *const c_char,
        executable_fd: c_int,
        image_plan: *const CMainImagePlan,
        option_count: c_uint,
        option_names: *const *const c_char,
        option_values: *const *const c_char,
        standard_fds: *const c_int,
        syscall_context: *mut c_void,
        syscall_dispatch: Option<
            unsafe extern "C" fn(*mut c_void, c_uint, *mut CSyscallCpuAarch64, *mut CSyscallTrapResult) -> c_int,
        >,
        output: *mut *mut c_void,
    ) -> c_int;
    fn hl_c_backend_run(backend: *mut c_void, argc: c_int, argv: *const *const c_char) -> c_int;
    fn hl_c_backend_request(backend: *mut c_void, request: c_uint, signal: c_int) -> c_int;
    fn hl_c_backend_exit_kind(backend: *const c_void) -> c_uint;
    fn hl_c_backend_exit_status(backend: *const c_void) -> c_int;
    fn hl_c_backend_exit_detail(backend: *const c_void) -> c_ulonglong;
    fn hl_c_backend_destroy(backend: *mut c_void);
}

pub(crate) fn retained_c_link_anchor() -> usize {
    hl_c_backend_run as *const () as usize
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
    _syscall_trap: Box<CSyscallTrapContext>,
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
        // SAFETY: terminal_handle is published only after backend creation succeeds and is
        // cleared before destruction; the backend permits concurrent request calls.
        let status = unsafe { hl_c_backend_request(handle as *mut c_void, REQUEST_SIGNAL, libc::SIGWINCH) };
        (status == STATUS_OK)
            .then_some(())
            .ok_or(crate::composition::CompositionError::RuntimeConstruction)
    }
}

impl StreamBridge {
    fn relay_output(source: OwnedFd, destination: Arc<Mutex<Box<dyn Write + Send>>>) {
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
    }

    fn inherited() -> Self {
        Self {
            output_workers: Vec::new(),
            guest_fds: None,
            terminal: None,
        }
    }

    fn pipe() -> Result<(OwnedFd, OwnedFd), EngineError> {
        let mut descriptors = [-1; 2];
        #[cfg(target_os = "linux")]
        // SAFETY: descriptors names two writable integers; successful pipe2 returns
        // two distinct close-on-exec descriptors.
        let status = unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) };
        #[cfg(not(target_os = "linux"))]
        // SAFETY: descriptors names two writable integers; successful pipe returns
        // two distinct descriptors which are immediately wrapped for cleanup.
        let status = unsafe { libc::pipe(descriptors.as_mut_ptr()) };
        if status != 0 {
            return Err(EngineError::LaunchFailed);
        }
        // SAFETY: pipe succeeded and transferred these distinct descriptors.
        let pair = unsafe {
            (
                OwnedFd::from_raw_fd(descriptors[0]),
                OwnedFd::from_raw_fd(descriptors[1]),
            )
        };
        #[cfg(not(target_os = "linux"))]
        for descriptor in [&pair.0, &pair.1] {
            // F_GETFD/F_SETFD operate on the live descriptor and retain no pointer.
            // SAFETY: descriptor owns a live pipe endpoint and F_GETFD takes no pointer.
            let flags = unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_GETFD) };
            // SAFETY: descriptor remains live, flags came from F_GETFD, and F_SETFD retains
            // neither the descriptor nor any borrowed memory.
            if flags < 0 || unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_SETFD, flags | libc::FD_CLOEXEC) } != 0
            {
                return Err(EngineError::LaunchFailed);
            }
        }
        Ok(pair)
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
                    .spawn(move || Self::relay_output(source, destination))
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
        let encoded_environment = Self::encode_environment(&plan.environment);
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
        let image_plan = c_main_image_plan(isa, executable_host.as_ref(), executable_authority)?;
        let mut handle = std::ptr::null_mut();
        let retained_exit = Arc::new(hl_runtime::RetainedExitTrap);
        // The retained trap callback currently exposes the AArch64 register ABI.
        // x86-64 remains on the retained engine's internal Linux syscall path
        // until its typed trap record is available.
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
        // SAFETY: all C strings, pointer arrays, image_plan, descriptors, callback context,
        // and output slot remain valid for the call. The backend copies configuration and
        // returns an exclusively owned handle on success; syscall_trap then outlives it.
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
            // SAFETY: creation returned this uniquely owned handle, and terminal attachment
            // failed before the handle could escape into the executor.
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
        // SAFETY: self owns a live backend handle until Drop and the accessor is read-only.
        let kind = unsafe { hl_c_backend_exit_kind(self.handle.as_ptr()) };
        EngineExit {
            kind: match kind {
                1 => ExitKind::Code,
                2 => ExitKind::Signal,
                3 => ExitKind::Fault,
                _ => ExitKind::EngineError,
            },
            // SAFETY: self owns a live backend handle and this accessor is read-only.
            guest_status: unsafe { hl_c_backend_exit_status(self.handle.as_ptr()) },
            // SAFETY: self owns a live backend handle and this accessor is read-only.
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
        // SAFETY: self owns the live handle; count matches pointers, and every pointed-to
        // CString remains alive and NUL-terminated for the duration of the call.
        Ok(unsafe { hl_c_backend_run(self.handle.as_ptr(), count, pointers.as_ptr()) })
    }

    #[cfg(test)]
    pub(crate) fn start_plan(&self, plan: &RuntimeLaunchPlan) -> Result<(), EngineError> {
        let status = self.run_plan_status(plan)?;
        (status == STATUS_OK).then_some(()).ok_or(EngineError::LaunchFailed)
    }

    pub(crate) fn stop_request(&self, request: StopRequest) -> Result<(), EngineError> {
        let kind = match request {
            StopRequest::Interrupt => REQUEST_INTERRUPT,
            StopRequest::Force => REQUEST_FORCE_STOP,
            StopRequest::Signal(_) => REQUEST_SIGNAL,
        };
        // SAFETY: self owns a live handle and the backend contract permits request while run
        // is active; the call receives only scalar request data.
        let status = unsafe { hl_c_backend_request(self.handle.as_ptr(), kind, request.signal()) };
        if status == STATUS_OK {
            Ok(())
        } else {
            Err(EngineError::StopFailed)
        }
    }
}

impl Drop for CGuestExecutor {
    fn drop(&mut self) {
        if let Ok(mut handle) = self.terminal_handle.lock() {
            *handle = None;
        }
        // SAFETY: this executor uniquely owns the live handle and Drop runs exactly once,
        // after terminal users have been prevented from issuing further requests.
        unsafe { hl_c_backend_destroy(self.handle.as_ptr()) };
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CAddressProjection, CGuestExecutor, CSyscallCpuAarch64, CSyscallTrapContext, CSyscallTrapResult,
        EVENT_CAPTURE_LOCK, SYSCALL_TRAP_ABI, SYSCALL_TRAP_CONTINUE, SYSCALL_TRAP_DECLINED, SYSCALL_TRAP_FAULT,
        StreamBridge, TASK_EVENT_CLONE_THREAD, TASK_EVENT_CREDENTIALS_CHANGED, TASK_EVENT_FORK_PROCESS,
        TASK_EVENT_PREPARE_FORK, c_file_volumes, c_main_image_plan, c_option, c_syscall_trap,
        hl_native_address_projection_guest, hl_native_address_projection_init, hl_native_address_projection_init_elf,
        hl_native_address_projection_storage,
    };
    use crate::activation::GuestIsa;
    use crate::composition::{
        ActivationChannel, CompositionError, RuntimeServices, StandardStreams, Terminal, TerminalPort,
    };
    use std::ffi::CString;
    use std::io::{Cursor, Read, Seek, Write};
    use std::os::fd::FromRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::sync::OnceLock;
    use std::sync::{Arc, Mutex};

    #[test]
    fn generic_address_projection_forces_displaced_et_exec_storage() {
        assert_eq!(std::mem::size_of::<CAddressProjection>(), 40);
        let mut projection = CAddressProjection::default();
        assert_eq!(
            unsafe { hl_native_address_projection_init(&raw mut projection, 0x40_0000, 0x41_0000, 0xa0_0000) },
            0
        );
        assert_eq!(projection.abi, 1);
        assert_eq!(projection.size as usize, std::mem::size_of::<CAddressProjection>());
        assert_eq!(projection.flags, 1);
        assert_eq!(projection.storage_bias, 0x60_0000);

        for (guest, storage) in [
            (0x3f_ffff, 0x3f_ffff),
            (0x40_0000, 0xa0_0000),
            (0x40_1234, 0xa0_1234),
            (0x40_ffff, 0xa0_ffff),
            (0x41_0000, 0x41_0000),
        ] {
            let mut actual = 0;
            assert_eq!(
                unsafe { hl_native_address_projection_storage(&raw const projection, guest, &raw mut actual) },
                0
            );
            assert_eq!(actual, storage);
            actual = 0;
            assert_eq!(
                unsafe { hl_native_address_projection_guest(&raw const projection, storage, &raw mut actual) },
                0
            );
            assert_eq!(actual, guest);
        }
    }

    #[test]
    fn generic_address_projection_rejects_mutated_or_overflowing_contracts() {
        let mut projection = CAddressProjection::default();
        assert_ne!(
            unsafe { hl_native_address_projection_init(&raw mut projection, 0x4000, 0x5000, 0x3000) },
            0
        );
        assert_ne!(
            unsafe {
                hl_native_address_projection_init(
                    &raw mut projection,
                    u64::MAX - 0x2000,
                    u64::MAX - 0x1000,
                    u64::MAX - 0x100,
                )
            },
            0
        );
        assert_eq!(
            unsafe { hl_native_address_projection_init(&raw mut projection, 0x4000, 0x5000, 0x8000) },
            0
        );
        projection.flags = 0;
        let mut output = 0;
        assert_ne!(
            unsafe { hl_native_address_projection_storage(&raw const projection, 0x4000, &raw mut output) },
            0
        );
    }

    #[test]
    fn elf_kind_alone_selects_et_exec_or_position_independent_coordinates() {
        let mut executable = CAddressProjection::default();
        assert_eq!(
            unsafe { hl_native_address_projection_init_elf(&raw mut executable, 1, 0x40_0000, 0x41_0000, 0xa0_0000,) },
            0
        );
        assert_eq!((executable.guest_start, executable.guest_end), (0x40_0000, 0x41_0000));
        assert_eq!(executable.storage_bias, 0x60_0000);

        // PT_INTERP presence distinguishes dynamic PIE from static PIE, but is
        // deliberately absent from this ABI: both are ET_DYN identity mappings.
        let mut dynamic_pie = CAddressProjection::default();
        let mut static_pie = CAddressProjection::default();
        for projection in [&raw mut dynamic_pie, &raw mut static_pie] {
            assert_eq!(
                unsafe { hl_native_address_projection_init_elf(projection, 2, 0, 0x10_0000, 0x70_0000_0000) },
                0
            );
        }
        assert_eq!(dynamic_pie, static_pie);
        assert_eq!(dynamic_pie.flags, 0);
        assert_eq!(
            (dynamic_pie.guest_start, dynamic_pie.guest_end, dynamic_pie.storage_bias),
            (0x70_0000_0000, 0x70_0010_0000, 0)
        );

        assert_ne!(
            unsafe { hl_native_address_projection_init_elf(&raw mut static_pie, 3, 0, 0x1000, 0x8000) },
            0
        );
    }

    #[test]
    fn syscall_callback_abi_layout_is_stable() {
        assert_eq!(std::mem::size_of::<CSyscallCpuAarch64>(), 296);
        assert_eq!(std::mem::size_of::<CSyscallTrapResult>(), 24);
        assert_eq!(SYSCALL_TRAP_ABI, 1);
    }

    #[test]
    fn declined_syscall_callback_preserves_snapshot_for_c_fallback() {
        let mut context = CSyscallTrapContext {
            trap: None,
            retained_exit: None,
            retained_tasks: None,
        };
        let mut cpu = CSyscallCpuAarch64 {
            abi: SYSCALL_TRAP_ABI,
            size: std::mem::size_of::<CSyscallCpuAarch64>() as u32,
            x: std::array::from_fn(|index| index as u64 * 17),
            sp: 0x1234,
            pc: 0x5678,
            tls: 0x9abc,
            nzcv: 0xf000_0000,
            task: 1,
        };
        let before = (cpu.x, cpu.sp, cpu.pc, cpu.tls, cpu.nzcv, cpu.task);
        let mut result = CSyscallTrapResult {
            abi: 0,
            size: 0,
            outcome: u32::MAX,
            exit_status: -1,
            image_generation: u64::MAX,
        };
        // SAFETY: context, cpu, and result are live uniquely borrowed test values whose
        // layouts match the callback ABI for the complete call.
        let status = unsafe {
            c_syscall_trap(
                (&mut context as *mut CSyscallTrapContext).cast(),
                GuestIsa::Aarch64 as u32,
                &mut cpu,
                &mut result,
            )
        };
        assert_eq!(status, 0);
        assert_eq!(result.outcome, SYSCALL_TRAP_DECLINED);
        assert_eq!((cpu.x, cpu.sp, cpu.pc, cpu.tls, cpu.nzcv, cpu.task), before);
    }

    #[test]
    fn retained_task_events_publish_fork_and_thread_identity() {
        let mut context = CSyscallTrapContext {
            trap: None,
            retained_exit: None,
            retained_tasks: Some(OnceLock::new()),
        };
        let mut cpu = CSyscallCpuAarch64 {
            abi: SYSCALL_TRAP_ABI,
            size: std::mem::size_of::<CSyscallCpuAarch64>() as u32,
            x: [0; 31],
            sp: 0,
            pc: 0,
            tls: 0,
            nzcv: 0,
            task: 41,
        };
        let mut result = CSyscallTrapResult {
            abi: 0,
            size: 0,
            outcome: u32::MAX,
            exit_status: -1,
            image_generation: 0,
        };
        // SAFETY: the closure passes live uniquely borrowed ABI records and a context that
        // remains allocated for every synchronous callback invocation.
        let mut dispatch = |cpu: &mut CSyscallCpuAarch64, result: &mut CSyscallTrapResult| unsafe {
            c_syscall_trap(
                (&mut context as *mut CSyscallTrapContext).cast(),
                GuestIsa::Aarch64 as u32,
                cpu,
                result,
            )
        };

        cpu.x[8] = 172;
        assert_eq!(dispatch(&mut cpu, &mut result), 0);
        assert_eq!((result.outcome, cpu.x[0]), (SYSCALL_TRAP_CONTINUE, 41));

        cpu.x = [0; 31];
        cpu.x[..8].copy_from_slice(&[10, 11, 12, 13, 20, 21, 22, 23]);
        cpu.x[8] = TASK_EVENT_CREDENTIALS_CHANGED;
        assert_eq!(dispatch(&mut cpu, &mut result), 0);
        for (number, expected) in [(174, 10), (175, 11), (176, 20), (177, 21)] {
            cpu.x = [0; 31];
            cpu.x[8] = number;
            assert_eq!(dispatch(&mut cpu, &mut result), 0);
            assert_eq!((result.outcome, cpu.x[0]), (SYSCALL_TRAP_CONTINUE, expected));
        }
        cpu.x = [0; 31];
        cpu.x[0] = u64::MAX;
        cpu.x[8] = TASK_EVENT_CREDENTIALS_CHANGED;
        assert_eq!(dispatch(&mut cpu, &mut result), 0);
        assert_eq!(result.outcome, SYSCALL_TRAP_FAULT);

        cpu.x = [0; 31];
        cpu.x[8] = TASK_EVENT_CLONE_THREAD;
        cpu.x[0] = 1001;
        cpu.x[1] = 41;
        assert_eq!(dispatch(&mut cpu, &mut result), 0);
        cpu.x = [0; 31];
        cpu.x[8] = 178;
        cpu.task = 1001;
        assert_eq!(dispatch(&mut cpu, &mut result), 0);
        assert_eq!(cpu.x[0], 1001);

        cpu.x = [0; 31];
        cpu.x[8] = TASK_EVENT_PREPARE_FORK;
        cpu.x[1] = 41;
        cpu.task = 41;
        assert_eq!(dispatch(&mut cpu, &mut result), 0);
        cpu.x = [0; 31];
        cpu.x[8] = TASK_EVENT_FORK_PROCESS;
        cpu.x[0] = 73;
        cpu.x[1] = 41;
        cpu.x[2] = 1;
        cpu.task = 41;
        assert_eq!(dispatch(&mut cpu, &mut result), 0);
        cpu.x = [0; 31];
        cpu.x[8] = 173;
        cpu.task = 73;
        assert_eq!(dispatch(&mut cpu, &mut result), 0);
        assert_eq!(cpu.x[0], 41);
    }

    #[derive(Clone)]
    struct Capture(Arc<Mutex<Vec<u8>>>);
    struct EventCapture(Arc<Mutex<String>>);
    struct Channel;
    struct Port;

    impl hl_log::Sink for EventCapture {
        fn write_line(&self, line: &str) {
            self.0.lock().unwrap().push_str(line);
        }
    }

    fn capture_events(run: impl FnOnce()) -> String {
        let _guard = EVENT_CAPTURE_LOCK.lock().unwrap();
        let output = Arc::new(Mutex::new(String::new()));
        hl_log::Events::global().set(Box::new(EventCapture(Arc::clone(&output))));
        run();
        hl_log::Events::global().reset();
        Arc::try_unwrap(output).unwrap().into_inner().unwrap()
    }

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

    fn exiting_x86_64(status: u16) -> Vec<u8> {
        const LINK_BASE: u64 = 0x0040_0000;
        const ENTRY_OFFSET: usize = 0x100;
        let mut bytes = vec![0_u8; 4096];
        bytes[..4].copy_from_slice(b"\x7fELF");
        bytes[4..7].copy_from_slice(&[2, 1, 1]);
        put_u16(&mut bytes, 16, 2);
        put_u16(&mut bytes, 18, hl_isa::GuestArchitecture::X86_64.elf_machine());
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
        let code = [
            0xb8,
            60,
            0,
            0,
            0, // mov eax, SYS_exit
            0xbf,
            status as u8,
            0,
            0,
            0, // mov edi, status
            0x0f,
            0x05, // syscall
        ];
        bytes[ENTRY_OFFSET..ENTRY_OFFSET + code.len()].copy_from_slice(&code);
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

    fn matching_process_and_thread_identity_arm() -> Vec<u8> {
        const ENTRY_OFFSET: usize = 0x100;
        let mut bytes = exiting_arm(0);
        for (index, instruction) in [
            0xd280_0008_u32 | (172 << 5),
            0xd400_0001,
            0xaa00_03e9,
            0xd280_0008_u32 | (178 << 5),
            0xd400_0001,
            0xeb09_001f,
            0x9a9f_07e0,
            0xd280_0ba8,
            0xd400_0001,
        ]
        .into_iter()
        .enumerate()
        {
            let offset = ENTRY_OFFSET + index * 4;
            bytes[offset..offset + 4].copy_from_slice(&instruction.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn retained_process_identity_is_returned_by_the_rust_owned_route() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("guest");
        std::fs::write(&path, matching_process_and_thread_identity_arm()).unwrap();
        let plan = executable_plan(&path);
        let executor = CGuestExecutor::create_with_streams(GuestIsa::Aarch64, &plan, None, [0, 1, 2], None).unwrap();
        executor.start_plan(&plan).unwrap();
        assert_eq!(executor.exit().guest_status, 0);
    }

    #[test]
    fn retained_x86_64_backend_runs_a_minimal_guest() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("guest-x86_64");
        std::fs::write(&path, exiting_x86_64(37)).unwrap();
        let plan = executable_plan(&path);
        let executor = CGuestExecutor::create_with_streams(GuestIsa::X86_64, &plan, None, [0, 1, 2], None).unwrap();
        executor.start_plan(&plan).unwrap();
        assert_eq!(executor.exit().guest_status, 37);
    }

    #[test]
    fn retained_c_main_image_plan_is_rust_inspected() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("guest");
        let executable = exiting_arm(0);
        std::fs::write(&path, &executable).unwrap();
        let path = CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        let plan = c_main_image_plan(GuestIsa::Aarch64, Some(&path), None).unwrap();
        assert_eq!(plan.kind, 1);
        assert_eq!((plan.link_start, plan.link_end), (0x0040_0000, 0x0041_0000));
        assert_eq!(plan.has_interpreter, 0);

        let mut pie = executable;
        put_u16(&mut pie, 16, 3);
        put_u64(&mut pie, 24, 0x0080_0100);
        put_u64(&mut pie, 80, 0x0080_0000);
        put_u64(&mut pie, 88, 0x0080_0000);
        std::fs::write(std::ffi::OsStr::from_bytes(path.as_bytes()), pie).unwrap();
        let plan = c_main_image_plan(GuestIsa::Aarch64, Some(&path), None).unwrap();
        assert_eq!(plan.kind, 2);
        assert_eq!((plan.link_start, plan.link_end), (0x0080_0000, 0x0081_0000));

        let mut interpreted = exiting_arm(0);
        put_u16(&mut interpreted, 56, 2);
        put_u32(&mut interpreted, 120, 3);
        put_u64(&mut interpreted, 128, 192);
        put_u64(&mut interpreted, 152, 7);
        put_u64(&mut interpreted, 160, 7);
        interpreted[192..199].copy_from_slice(b"/ld.so\0");
        std::fs::write(std::ffi::OsStr::from_bytes(path.as_bytes()), interpreted).unwrap();
        let plan = c_main_image_plan(GuestIsa::Aarch64, Some(&path), None).unwrap();
        assert_eq!(plan.has_interpreter, 1);
        assert_ne!(plan.interpreter_identity, 0);

        std::fs::write(std::ffi::OsStr::from_bytes(path.as_bytes()), b"not an elf").unwrap();
        let events = capture_events(|| {
            assert!(c_main_image_plan(GuestIsa::Aarch64, Some(&path), None).is_err());
        });
        assert!(
            events.contains(r#""event":"execution.c.image_plan.rejected""#),
            "{events}"
        );
        assert!(events.contains(r#""source":"path""#), "{events}");
        assert!(events.contains(r#""stage":"inspect""#), "{events}");
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
    fn rust_placement_drives_displaced_exec_and_pie_launches() {
        let directory = tempfile::tempdir().unwrap();
        for (name, kind, link_base, status) in [("exec", 2_u16, 0x0080_0000_u64, 31_u16), ("pie", 3, 0, 32)] {
            let path = directory.path().join(name);
            let mut executable = exiting_arm(status);
            put_u16(&mut executable, 16, kind);
            put_u64(&mut executable, 24, link_base + 0x100);
            put_u64(&mut executable, 80, link_base);
            put_u64(&mut executable, 88, link_base);
            std::fs::write(&path, executable).unwrap();
            let launch = executable_plan(&path);
            let executor =
                CGuestExecutor::create_with_streams(GuestIsa::Aarch64, &launch, None, [0, 1, 2], None).unwrap();
            executor.start_plan(&launch).unwrap();
            assert_eq!(executor.exit().guest_status, i32::from(status));
        }
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
