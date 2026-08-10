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
use std::os::unix::process::ExitStatusExt;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const PLAN_DESCRIPTOR: i32 = 3;
const CONTROL_DESCRIPTOR: i32 = 4;
const FORCE_GRACE: Duration = Duration::from_millis(25);

pub(crate) struct CWorker {
    child: Mutex<Option<Child>>,
    reader: Mutex<std::os::unix::net::UnixStream>,
    writer: Arc<Mutex<std::os::unix::net::UnixStream>>,
    streams: Mutex<Option<StreamBridge>>,
    exit: Mutex<Option<EngineExit>>,
    startup: Mutex<Startup>,
    startup_changed: Condvar,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Startup {
    Starting,
    Started,
    Failed,
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
            .env_clear()
            .env("HL_C_PLAN_FD", PLAN_DESCRIPTOR.to_string())
            .env("HL_C_CONTROL_FD", CONTROL_DESCRIPTOR.to_string())
            .stdin(Stdio::from(input))
            .stdout(Stdio::from(output))
            .stderr(Stdio::from(error));
        for name in [hl_log::LOG_TAGS, hl_log::LOG_LEVEL, hl_log::PROFILE_TAGS] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        let plan_raw = plan_inherit.as_raw_fd();
        let control_raw = control_inherit.as_raw_fd();
        // SAFETY: the child performs only async-signal-safe dup2 calls before immediate exec.
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::dup2(plan_raw, PLAN_DESCRIPTOR) < 0 || libc::dup2(control_raw, CONTROL_DESCRIPTOR) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().map_err(|_| EngineError::LaunchFailed)?;
        let mut child = ChildGuard(Some(child));
        drop((plan_inherit, control_inherit, child_control, plan_file));
        crate::executable::ExecutableAuthority::send_optional(services.executable_authority.as_ref(), &parent_control)
            .map_err(|_| EngineError::LaunchFailed)?;
        let reader = parent_control.try_clone().map_err(|_| EngineError::LaunchFailed)?;
        let writer = Arc::new(Mutex::new(parent_control));
        streams.attach_terminal(Arc::new(WorkerTerminalNotification {
            writer: Arc::clone(&writer),
        }))?;
        Ok(Self {
            child: Mutex::new(child.0.take()),
            reader: Mutex::new(reader),
            writer,
            streams: Mutex::new(Some(streams)),
            exit: Mutex::new(None),
            startup: Mutex::new(Startup::Starting),
            startup_changed: Condvar::new(),
        })
    }

    pub(crate) fn start(&self) -> Result<(), EngineError> {
        let result = self.start_inner();
        let mut startup = self.startup.lock().map_err(|_| EngineError::Synchronization)?;
        *startup = if result.is_ok() {
            Startup::Started
        } else {
            Startup::Failed
        };
        self.startup_changed.notify_all();
        result
    }

    fn start_inner(&self) -> Result<(), EngineError> {
        let mut reader = self.reader.lock().map_err(|_| EngineError::Synchronization)?;
        let ready = read_message(&mut reader)?;
        if ready != Message::Ready {
            hl_log::hl_error!(
                hl_log::tag::EXEC,
                "retained C worker failed before start: message={ready:?}"
            );
            return Err(EngineError::LaunchFailed);
        }
        let mut writer = self.writer.lock().map_err(|_| EngineError::Synchronization)?;
        write_message(&mut writer, Message::Start)?;
        drop(writer);
        if read_message(&mut reader)? != Message::Started {
            return Err(EngineError::LaunchFailed);
        }
        Ok(())
    }

    pub(crate) fn wait(&self) -> Result<EngineExit, EngineError> {
        if let Some(exit) = *self.exit.lock().map_err(|_| EngineError::Synchronization)? {
            return Ok(exit);
        }
        let mut reader = self.reader.lock().map_err(|_| EngineError::Synchronization)?;
        let Ok(message) = read_message(&mut reader) else {
            drop(reader);
            return self.reap_without_frame();
        };
        drop(reader);
        let Message::Exit(exit) = message else {
            hl_log::hl_error!(
                hl_log::tag::EXEC,
                "retained C worker failed while running: message={message:?}"
            );
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
        if !process_status_matches(&status, exit) {
            hl_log::hl_error!(
                hl_log::tag::EXEC,
                "retained C worker process status disagrees with exit: process={status:?} exit={exit:?}"
            );
            return Err(EngineError::WaitFailed);
        }
        *self.exit.lock().map_err(|_| EngineError::Synchronization)? = Some(exit);
        drop(self.streams.lock().map_err(|_| EngineError::Synchronization)?.take());
        Ok(exit)
    }

    pub(crate) fn stop(&self, request: StopRequest) -> Result<(), EngineError> {
        let startup = self.startup.lock().map_err(|_| EngineError::Synchronization)?;
        let startup = self
            .startup_changed
            .wait_while(startup, |state| *state == Startup::Starting)
            .map_err(|_| EngineError::Synchronization)?;
        if *startup == Startup::Failed {
            return Err(EngineError::Exited);
        }
        drop(startup);
        let mut writer = self.writer.lock().map_err(|_| EngineError::Synchronization)?;
        let requested = write_message(&mut writer, Message::Stop(request));
        drop(writer);
        if request != StopRequest::Force {
            if requested.is_err()
                && self
                    .child
                    .lock()
                    .map_err(|_| EngineError::Synchronization)?
                    .as_mut()
                    .ok_or(EngineError::StopFailed)?
                    .try_wait()
                    .map_err(|_| EngineError::StopFailed)?
                    .is_some()
            {
                return Ok(());
            }
            return requested;
        }
        if requested.is_ok() && self.wait_force_grace()? {
            return Ok(());
        }
        let process = self
            .child
            .lock()
            .map_err(|_| EngineError::Synchronization)?
            .as_ref()
            .map(Child::id)
            .ok_or(EngineError::StopFailed)?;
        signal_process_group(process, libc::SIGKILL)
    }

    fn wait_force_grace(&self) -> Result<bool, EngineError> {
        let deadline = Instant::now() + FORCE_GRACE;
        loop {
            if self
                .child
                .lock()
                .map_err(|_| EngineError::Synchronization)?
                .as_mut()
                .ok_or(EngineError::StopFailed)?
                .try_wait()
                .map_err(|_| EngineError::StopFailed)?
                .is_some()
            {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn reap_without_frame(&self) -> Result<EngineExit, EngineError> {
        let status = self
            .child
            .lock()
            .map_err(|_| EngineError::Synchronization)?
            .as_mut()
            .ok_or(EngineError::WaitFailed)?
            .wait()
            .map_err(|_| EngineError::WaitFailed)?;
        let signal = status.signal().ok_or(EngineError::WaitFailed)?;
        let exit = EngineExit {
            kind: crate::engine::ExitKind::Signal,
            guest_status: signal,
            detail: 0,
            fault: None,
        };
        *self.exit.lock().map_err(|_| EngineError::Synchronization)? = Some(exit);
        drop(self.streams.lock().map_err(|_| EngineError::Synchronization)?.take());
        Ok(exit)
    }
}

impl Drop for CWorker {
    fn drop(&mut self) {
        let child = self.child.get_mut().unwrap_or_else(|error| error.into_inner());
        if let Some(child) = child.as_mut()
            && child.try_wait().ok().flatten().is_none()
        {
            let _ = signal_process_group(child.id(), libc::SIGKILL);
            let _ = child.wait();
        }
    }
}

struct ChildGuard(Option<Child>);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let Some(child) = self.0.as_mut() else { return };
        if child.try_wait().ok().flatten().is_none() {
            let _ = signal_process_group(child.id(), libc::SIGKILL);
            let _ = child.wait();
        }
    }
}

fn signal_process_group(process: u32, signal: i32) -> Result<(), EngineError> {
    let process = i32::try_from(process).map_err(|_| EngineError::StopFailed)?;
    // SAFETY: the worker called setsid before exec, so its pid names its private process group.
    if unsafe { libc::kill(-process, signal) } == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(EngineError::StopFailed)
    }
}

fn process_status_matches(status: &std::process::ExitStatus, exit: EngineExit) -> bool {
    status.code() == Some(crate::program::Program::exit_status(exit))
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

#[cfg(test)]
mod tests {
    use super::{CWorker, ChildGuard, Startup, process_status_matches};
    use crate::c_execution::StreamBridge;
    use crate::engine::StopRequest;
    use crate::engine::{EngineExit, ExitKind};
    use std::io::{BufRead, BufReader};
    use std::os::unix::process::CommandExt;
    use std::os::unix::process::ExitStatusExt;
    use std::process::{Child, Command, Stdio};
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::{Duration, Instant};

    fn exit(status: i32) -> EngineExit {
        EngineExit {
            kind: ExitKind::Code,
            guest_status: status,
            detail: 0,
            fault: None,
        }
    }

    #[test]
    fn worker_status_must_match_even_when_the_process_reports_success() {
        let success = std::process::ExitStatus::from_raw(0);
        let seven = std::process::ExitStatus::from_raw(7 << 8);
        assert!(process_status_matches(&success, exit(0)));
        assert!(process_status_matches(&seven, exit(7)));
        assert!(!process_status_matches(&success, exit(7)));
        assert!(!process_status_matches(&seven, exit(0)));
    }

    fn session_child(script: &str, stdout: Stdio) -> Child {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(script).stdout(stdout);
        // SAFETY: setsid is async-signal-safe, retains no storage, and cannot unwind.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
        command.spawn().unwrap()
    }

    fn group_exists(process: u32) -> bool {
        let process = i32::try_from(process).unwrap();
        // SAFETY: signal zero performs only an existence/permission probe.
        if unsafe { libc::kill(-process, 0) } == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }

    fn assert_group_gone(process: u32) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while group_exists(process) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!group_exists(process), "worker process group {process} leaked");
    }

    #[test]
    fn post_spawn_failure_guard_reaps_the_worker() {
        let child = session_child("exec sleep 60", Stdio::null());
        let process = child.id();
        drop(ChildGuard(Some(child)));
        assert_group_gone(process);
    }

    #[test]
    fn force_stop_survives_a_broken_control_channel() {
        let child = session_child("exec sleep 60", Stdio::null());
        let process = child.id();
        let (control, peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let reader = control.try_clone().unwrap();
        drop(peer);
        let worker = CWorker {
            child: Mutex::new(Some(child)),
            reader: Mutex::new(reader),
            writer: Arc::new(Mutex::new(control)),
            streams: Mutex::new(Some(StreamBridge::inherited())),
            exit: Mutex::new(None),
            startup: Mutex::new(Startup::Started),
            startup_changed: Condvar::new(),
        };
        worker.stop(StopRequest::Force).unwrap();
        let status = worker.child.lock().unwrap().as_mut().unwrap().wait().unwrap();
        assert_eq!(status.signal(), Some(libc::SIGKILL));
        assert_group_gone(process);
    }

    #[test]
    fn dropping_worker_kills_its_descendant_process_group() {
        let mut child = session_child("sleep 60 & echo $!; wait", Stdio::piped());
        let process = child.id();
        let descendant = {
            let mut line = String::new();
            BufReader::new(child.stdout.take().unwrap())
                .read_line(&mut line)
                .unwrap();
            line.trim().parse::<i32>().unwrap()
        };
        let (control, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let reader = control.try_clone().unwrap();
        let worker = CWorker {
            child: Mutex::new(Some(child)),
            reader: Mutex::new(reader),
            writer: Arc::new(Mutex::new(control)),
            streams: Mutex::new(Some(StreamBridge::inherited())),
            exit: Mutex::new(None),
            startup: Mutex::new(Startup::Started),
            startup_changed: Condvar::new(),
        };
        drop(worker);
        assert_group_gone(process);
        // The descendant may briefly remain as an init-owned zombie, but it must no longer be
        // signalable as a live process after the private group receives SIGKILL.
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            // SAFETY: signal zero is an existence probe for the child-reported positive pid.
            let status = unsafe { libc::kill(descendant, 0) };
            let gone = status != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
            if gone || Instant::now() >= deadline {
                assert!(gone, "worker descendant {descendant} leaked");
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
