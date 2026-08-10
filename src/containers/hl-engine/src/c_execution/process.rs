use super::control::{FRAME_SIZE, Message};
use super::{StreamBridge, wire};
use crate::activation::GuestIsa;
use crate::composition::{CompositionError, NativeTerminalWindowNotification, RuntimeServices};
use crate::engine::{EngineError, EngineExit, StopRequest};
use crate::launch_plan::RuntimeLaunchPlan;
use std::ffi::CString;
use std::io::{Read, Seek, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

const PLAN_DESCRIPTOR: i32 = 3;
const CONTROL_DESCRIPTOR: i32 = 4;

pub(crate) struct CWorker {
    child: Mutex<Option<Child>>,
    reader: Mutex<std::os::unix::net::UnixStream>,
    writer: Arc<Mutex<std::os::unix::net::UnixStream>>,
    streams: Mutex<Option<StreamBridge>>,
    exit: Mutex<Option<EngineExit>>,
}

struct WorkerTerminalNotification {
    writer: Arc<Mutex<std::os::unix::net::UnixStream>>,
}

impl NativeTerminalWindowNotification for WorkerTerminalNotification {
    fn resize(&self, _: &std::fs::File, rows: u16, columns: u16) -> Result<(), CompositionError> {
        let mut writer = self.writer.lock().map_err(|_| CompositionError::RuntimeConstruction)?;
        write_message(&mut writer, Message::Resize { rows, columns }).map_err(|_| CompositionError::RuntimeConstruction)
    }
}

impl CWorker {
    pub(crate) fn create(
        isa: GuestIsa,
        plan: &RuntimeLaunchPlan,
        services: &RuntimeServices,
    ) -> Result<Self, EngineError> {
        let plan_file = sealed_plan(&wire::encode(isa, plan)?)?;
        let (parent_control, child_control) =
            std::os::unix::net::UnixStream::pair().map_err(|_| EngineError::LaunchFailed)?;
        let plan_inherit = duplicate_high(plan_file.as_raw_fd())?;
        let control_inherit = duplicate_high(child_control.as_raw_fd())?;
        let mut streams = StreamBridge::new(services)?;
        let [input, output, error] = streams.take_guest_fds()?;
        let executable = std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(|parent| parent.join(isa.engine_stem())))
            .ok_or(EngineError::LaunchFailed)?;
        let mut command = Command::new(executable);
        command
            .arg("--c-worker")
            .env("HL_C_PLAN_FD", PLAN_DESCRIPTOR.to_string())
            .env("HL_C_CONTROL_FD", CONTROL_DESCRIPTOR.to_string())
            .stdin(Stdio::from(input))
            .stdout(Stdio::from(output))
            .stderr(Stdio::from(error));
        let plan_raw = plan_inherit.as_raw_fd();
        let control_raw = control_inherit.as_raw_fd();
        // SAFETY: the child performs only async-signal-safe dup2 calls before immediate exec.
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(plan_raw, PLAN_DESCRIPTOR) < 0 || libc::dup2(control_raw, CONTROL_DESCRIPTOR) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().map_err(|_| EngineError::LaunchFailed)?;
        drop((plan_inherit, control_inherit, child_control, plan_file));
        let reader = parent_control.try_clone().map_err(|_| EngineError::LaunchFailed)?;
        let writer = Arc::new(Mutex::new(parent_control));
        streams.attach_terminal(Arc::new(WorkerTerminalNotification {
            writer: Arc::clone(&writer),
        }))?;
        Ok(Self {
            child: Mutex::new(Some(child)),
            reader: Mutex::new(reader),
            writer,
            streams: Mutex::new(Some(streams)),
            exit: Mutex::new(None),
        })
    }

    pub(crate) fn start(&self) -> Result<(), EngineError> {
        let mut reader = self.reader.lock().map_err(|_| EngineError::Synchronization)?;
        let ready = read_message(&mut reader)?;
        if ready != Message::Ready {
            return Err(EngineError::LaunchFailed);
        }
        let mut writer = self.writer.lock().map_err(|_| EngineError::Synchronization)?;
        write_message(&mut writer, Message::Start)
    }

    pub(crate) fn wait(&self) -> Result<EngineExit, EngineError> {
        if let Some(exit) = *self.exit.lock().map_err(|_| EngineError::Synchronization)? {
            return Ok(exit);
        }
        let mut reader = self.reader.lock().map_err(|_| EngineError::Synchronization)?;
        let message = read_message(&mut reader)?;
        drop(reader);
        let Message::Exit(exit) = message else {
            return Err(EngineError::WaitFailed);
        };
        let status = self
            .child
            .lock()
            .map_err(|_| EngineError::Synchronization)?
            .as_mut()
            .ok_or(EngineError::WaitFailed)?
            .wait()
            .map_err(|_| EngineError::WaitFailed)?;
        if !status.success() && status.code() != Some(crate::program::Program::exit_status(exit)) {
            return Err(EngineError::WaitFailed);
        }
        *self.exit.lock().map_err(|_| EngineError::Synchronization)? = Some(exit);
        drop(self.streams.lock().map_err(|_| EngineError::Synchronization)?.take());
        Ok(exit)
    }

    pub(crate) fn stop(&self, request: StopRequest) -> Result<(), EngineError> {
        let mut writer = self.writer.lock().map_err(|_| EngineError::Synchronization)?;
        write_message(&mut writer, Message::Stop(request))
    }
}

impl Drop for CWorker {
    fn drop(&mut self) {
        let child = self.child.get_mut().unwrap_or_else(|error| error.into_inner());
        if let Some(child) = child.as_mut()
            && child.try_wait().ok().flatten().is_none()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn sealed_plan(bytes: &[u8]) -> Result<std::fs::File, EngineError> {
    let name = CString::new("hl-c-plan").expect("literal has no NUL");
    // SAFETY: name is terminated and flags request a private sealing-capable descriptor.
    let descriptor = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING) };
    if descriptor < 0 {
        return Err(EngineError::LaunchFailed);
    }
    // SAFETY: memfd_create returned unique ownership.
    let mut file = unsafe { std::fs::File::from_raw_fd(descriptor) };
    file.write_all(bytes).map_err(|_| EngineError::LaunchFailed)?;
    file.rewind().map_err(|_| EngineError::LaunchFailed)?;
    let seals = libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;
    // SAFETY: descriptor is a live sealing-capable memfd.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, seals) } < 0 {
        return Err(EngineError::LaunchFailed);
    }
    Ok(file)
}

fn duplicate_high(descriptor: i32) -> Result<OwnedFd, EngineError> {
    // SAFETY: descriptor is live; F_DUPFD_CLOEXEC returns unique ownership at fd >= 10.
    let duplicate = unsafe { libc::fcntl(descriptor, libc::F_DUPFD_CLOEXEC, 10) };
    if duplicate < 0 {
        Err(EngineError::LaunchFailed)
    } else {
        // SAFETY: successful fcntl returned a new descriptor.
        Ok(unsafe { OwnedFd::from_raw_fd(duplicate) })
    }
}

fn read_message(stream: &mut std::os::unix::net::UnixStream) -> Result<Message, EngineError> {
    let mut frame = [0_u8; FRAME_SIZE];
    stream.read_exact(&mut frame).map_err(|_| EngineError::WaitFailed)?;
    Message::decode(&frame).map_err(|_| EngineError::WaitFailed)
}

fn write_message(stream: &mut std::os::unix::net::UnixStream, message: Message) -> Result<(), EngineError> {
    let frame = message.encode().map_err(|_| EngineError::StopFailed)?;
    stream.write_all(&frame).map_err(|_| EngineError::StopFailed)
}
