#![allow(unsafe_code)]

use super::checkpoint_control::CheckpointControl;
use super::control::{FailureStage, Message};
use super::environment::worker_environment;
use super::process_lifecycle::{
    ChildGuard, duplicate_high, process_status_matches, read_message, sealed_plan, signal_process_group,
    signal_process_group_best_effort, worker_executable, write_message,
};
use super::{StreamBridge, wire};
use crate::activation::GuestIsa;
use crate::composition::{CompositionError, NativeTerminalWindowNotification, RuntimeServices};
use crate::engine::{EngineError, EngineExit, StopRequest};
use crate::ffi::checkpoint::Broker;
use crate::launch_plan::RuntimeLaunchPlan;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::os::unix::process::ExitStatusExt;
use std::process::{Child, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const PLAN_DESCRIPTOR: i32 = 3;
const CONTROL_DESCRIPTOR: i32 = 4;
const PROVIDER_DESCRIPTOR: i32 = 5;
const CHECKPOINT_DESCRIPTOR: i32 = 6;
const CHECKPOINT_TRIGGER_DESCRIPTOR: i32 = 7;
const FORCE_GRACE: Duration = Duration::from_millis(25);

pub(crate) struct CWorker {
    child: Mutex<Option<Child>>,
    reader: Mutex<std::os::unix::net::UnixStream>,
    writer: Arc<Mutex<std::os::unix::net::UnixStream>>,
    streams: Mutex<Option<StreamBridge>>,
    exit: Mutex<Option<EngineExit>>,
    startup: Mutex<Startup>,
    startup_changed: Condvar,
    provider_broker: Option<std::thread::JoinHandle<()>>,
    checkpoint: Option<CheckpointControl>,
    diagnostics: bool,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Startup {
    Starting,
    Started,
    Failed,
}

#[cold]
fn report_worker_failure(stage: FailureStage, code: i32) {
    let stage = stage.name();
    hl_log::hl_verdict!(
        hl_log::tag::EXEC,
        "execution.c.lifecycle.failed",
        stage = %stage,
        code = code;
        "retained C lifecycle failed stage={stage} code={code}"
    );
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
        hl_log::hl_event!(
            hl_log::tag::EXEC,
            hl_log::Level::Info,
            "retained_c.worker.creating",
            isa = ?isa
        );
        let plan_file = sealed_plan(&wire::encode(isa, plan)?)?;
        let (parent_control, child_control) =
            std::os::unix::net::UnixStream::pair().map_err(|_| EngineError::LaunchFailed)?;
        let provider = services
            .projected_root_authority
            .as_ref()
            .map(|_| std::os::unix::net::UnixStream::pair().map_err(|_| EngineError::LaunchFailed))
            .transpose()?;
        let checkpoint_requested =
            plan.options.get("HL_CHECKPOINT").is_some() || plan.options.get("HL_RESTORE").is_some();
        let diagnostics = super::attestation::requested(plan);
        let checkpoint = if checkpoint_requested {
            let sink = services.checkpoint_sink.clone().ok_or(EngineError::LaunchFailed)?;
            let source = services.checkpoint_source.clone().ok_or(EngineError::LaunchFailed)?;
            let (broker, child) = Broker::pair().map_err(|_| EngineError::LaunchFailed)?;
            let control = CheckpointControl::start(sink, source, broker)?;
            Some((control, child))
        } else {
            None
        };
        let plan_inherit = duplicate_high(plan_file.as_raw_fd())?;
        let control_inherit = duplicate_high(child_control.as_raw_fd())?;
        let provider_inherit = provider
            .as_ref()
            .map(|(_, child)| duplicate_high(child.as_raw_fd()))
            .transpose()?;
        let checkpoint_inherit = checkpoint
            .as_ref()
            .map(|(_, child)| duplicate_high(child.as_raw_fd()))
            .transpose()?;
        let checkpoint_trigger_inherit = checkpoint
            .as_ref()
            .map(|(control, _)| duplicate_high(control.trigger_descriptor()))
            .transpose()?;
        let mut streams = StreamBridge::new(services)?;
        let [input, output, error] = streams.take_guest_fds()?;
        let executable = worker_executable(
            isa,
            std::env::var_os("HL_TEST_ENGINE_APP_BIN_DIR"),
            std::env::current_exe().ok(),
        )?;
        let mut command = super::platform::worker(executable);
        command
            .arg("--c-worker")
            .env_clear()
            .env("HL_C_PLAN_FD", PLAN_DESCRIPTOR.to_string())
            .env("HL_C_CONTROL_FD", CONTROL_DESCRIPTOR.to_string())
            .stdin(Stdio::from(input))
            .stdout(Stdio::from(output))
            .stderr(Stdio::from(error));
        if provider_inherit.is_some() {
            command.env("HL_C_PROVIDER_FD", PROVIDER_DESCRIPTOR.to_string());
        }
        if checkpoint_inherit.is_some() {
            command
                .env("HL_C_CHECKPOINT_FD", CHECKPOINT_DESCRIPTOR.to_string())
                .env("HL_C_CHECKPOINT_TRIGGER_FD", CHECKPOINT_TRIGGER_DESCRIPTOR.to_string());
        }
        for (name, value) in worker_environment(|name| std::env::var_os(name)) {
            command.env(name, value);
        }
        let plan_raw = plan_inherit.as_raw_fd();
        let control_raw = control_inherit.as_raw_fd();
        let provider_raw = provider_inherit.as_ref().map(AsRawFd::as_raw_fd);
        let checkpoint_raw = checkpoint_inherit.as_ref().map(AsRawFd::as_raw_fd);
        let checkpoint_trigger_raw = checkpoint_trigger_inherit.as_ref().map(AsRawFd::as_raw_fd);
        // SAFETY: the child performs only async-signal-safe dup2 calls before immediate exec.
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::dup2(plan_raw, PLAN_DESCRIPTOR) < 0 || libc::dup2(control_raw, CONTROL_DESCRIPTOR) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if let Some(provider) = provider_raw
                    && libc::dup2(provider, PROVIDER_DESCRIPTOR) < 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                if let (Some(checkpoint), Some(trigger)) = (checkpoint_raw, checkpoint_trigger_raw)
                    && (libc::dup2(checkpoint, CHECKPOINT_DESCRIPTOR) < 0
                        || libc::dup2(trigger, CHECKPOINT_TRIGGER_DESCRIPTOR) < 0)
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().map_err(|_| EngineError::LaunchFailed)?;
        hl_log::hl_event!(
            hl_log::tag::EXEC,
            hl_log::Level::Info,
            "retained_c.worker.spawned",
            isa = ?isa,
            pid = child.id()
        );
        let mut child = ChildGuard(Some(child));
        drop((
            plan_inherit,
            control_inherit,
            provider_inherit,
            checkpoint_inherit,
            checkpoint_trigger_inherit,
            child_control,
            plan_file,
        ));
        let provider_broker = match (provider, services.projected_root_authority.as_ref()) {
            (Some((parent, child)), Some(authority)) => {
                drop(child);
                Some(super::provider_broker::spawn(parent, Arc::clone(authority))?)
            }
            (None, None) => None,
            _ => return Err(EngineError::LaunchFailed),
        };
        crate::executable::ExecutableAuthority::send_optional(services.executable_authority.as_ref(), &parent_control)
            .map_err(|_| EngineError::LaunchFailed)?;
        hl_log::hl_event!(
            hl_log::tag::EXEC,
            hl_log::Level::Info,
            "retained_c.worker.authority_transferred",
            isa = ?isa,
            present = services.executable_authority.is_some()
        );
        let reader = parent_control.try_clone().map_err(|_| EngineError::LaunchFailed)?;
        let writer = Arc::new(Mutex::new(parent_control));
        streams.attach_terminal(Arc::new(WorkerTerminalNotification {
            writer: Arc::clone(&writer),
        }))?;
        let checkpoint = checkpoint.map(|(control, child)| {
            drop(child);
            control
        });
        Ok(Self {
            child: Mutex::new(child.0.take()),
            reader: Mutex::new(reader),
            writer,
            streams: Mutex::new(Some(streams)),
            exit: Mutex::new(None),
            startup: Mutex::new(Startup::Starting),
            startup_changed: Condvar::new(),
            provider_broker,
            checkpoint,
            diagnostics,
        })
    }

    pub(crate) fn checkpoint_supported(&self) -> Result<(), EngineError> {
        self.checkpoint.as_ref().map(|_| ()).ok_or(EngineError::Unsupported)
    }

    pub(crate) fn capture_checkpoint(&self) -> Result<(), EngineError> {
        let checkpoint = self.checkpoint.as_ref().ok_or(EngineError::Unsupported)?;
        let process = self.child.lock().map_err(|_| EngineError::Synchronization)?;
        let pid = process.as_ref().ok_or(EngineError::NotStarted)?.id();
        drop(process);
        // SAFETY: this pure query returns the retained C translation unit's reserved host signal.
        let signal = unsafe { super::hl_c_backend_checkpoint_interrupt_signal() };
        checkpoint.capture(pid, signal)
    }

    pub(crate) fn start(&self) -> Result<(), EngineError> {
        let result = self.start_inner();
        let mut startup = self.startup.lock().map_err(|_| EngineError::Synchronization)?;
        *startup = if result.is_ok() {
            Startup::Started
        } else {
            Startup::Failed
        };
        if let Err(error) = &result {
            hl_log::hl_verdict!(
                hl_log::tag::EXEC,
                "retained_c.worker.start_failed",
                stage = %"start",
                reason = ?error;
                "retained C worker failure stage=start reason={error:?}"
            );
        }
        self.startup_changed.notify_all();
        result
    }

    fn start_inner(&self) -> Result<(), EngineError> {
        let mut reader = self.reader.lock().map_err(|_| EngineError::Synchronization)?;
        let ready = read_message(&mut reader)?;
        if let Message::Error { stage, code } = ready {
            report_worker_failure(stage, code);
            return Err(EngineError::LaunchFailed);
        }
        if ready != Message::Ready {
            hl_log::hl_error!(
                hl_log::tag::EXEC,
                "retained C worker failed before start: message={ready:?}"
            );
            hl_log::hl_event!(
                hl_log::tag::EXEC,
                hl_log::Level::Error,
                "retained_c.worker.protocol_failed",
                phase = "ready",
                message = ?ready
            );
            return Err(EngineError::LaunchFailed);
        }
        hl_log::hl_event!(hl_log::tag::EXEC, hl_log::Level::Info, "retained_c.worker.ready");
        let mut writer = self.writer.lock().map_err(|_| EngineError::Synchronization)?;
        write_message(&mut writer, Message::Start)?;
        hl_log::hl_event!(
            hl_log::tag::EXEC,
            hl_log::Level::Info,
            "retained_c.worker.start_requested"
        );
        drop(writer);
        let started = read_message(&mut reader)?;
        if let Message::Error { stage, code } = started {
            report_worker_failure(stage, code);
            return Err(EngineError::LaunchFailed);
        }
        if started != Message::Started {
            hl_log::hl_verdict!(
                hl_log::tag::EXEC,
                "retained_c.worker.protocol_failed",
                phase = %"started",
                message = ?started;
                "retained C worker protocol failure phase=started message={started:?}"
            );
            return Err(EngineError::LaunchFailed);
        }
        hl_log::hl_event!(hl_log::tag::EXEC, hl_log::Level::Info, "retained_c.worker.started");
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
        if let Message::Error { stage, code } = message {
            report_worker_failure(stage, code);
            return Err(EngineError::WaitFailed);
        }
        let Message::Exit(exit) = message else {
            hl_log::hl_error!(
                hl_log::tag::EXEC,
                "retained C worker failed while running: message={message:?}"
            );
            hl_log::hl_event!(
                hl_log::tag::EXEC,
                hl_log::Level::Error,
                "retained_c.worker.protocol_failed",
                phase = "exit",
                message = ?message
            );
            return Err(EngineError::WaitFailed);
        };
        let (process, status) = {
            let mut child = self.child.lock().map_err(|_| EngineError::Synchronization)?;
            let child = child.as_mut().ok_or(EngineError::WaitFailed)?;
            let process = child.id();
            let status = child.wait().map_err(|_| EngineError::WaitFailed)?;
            (process, status)
        };
        // Quiesce descendants that inherited streams after the tracked leader exited.
        signal_process_group(process, libc::SIGKILL)?;
        if !process_status_matches(&status, exit) {
            hl_log::hl_error!(
                hl_log::tag::EXEC,
                "retained C worker process status disagrees with exit: process={status:?} exit={exit:?}"
            );
            hl_log::hl_event!(
                hl_log::tag::EXEC,
                hl_log::Level::Error,
                "retained_c.worker.status_mismatch",
                process = ?status,
                exit = ?exit
            );
            return Err(EngineError::WaitFailed);
        }
        *self.exit.lock().map_err(|_| EngineError::Synchronization)? = Some(exit);
        drop(self.streams.lock().map_err(|_| EngineError::Synchronization)?.take());
        super::attestation::report_completed(self.diagnostics);
        hl_log::hl_event!(
            hl_log::tag::EXEC,
            hl_log::Level::Info,
            "retained_c.worker.reaped",
            exit_kind = ?exit.kind,
            guest_status = exit.guest_status,
            detail = exit.detail
        );
        Ok(exit)
    }

    pub(crate) fn stop(&self, request: StopRequest) -> Result<(), EngineError> {
        hl_log::hl_event!(
            hl_log::tag::EXEC,
            hl_log::Level::Info,
            "retained_c.worker.stop_requested",
            request = ?request
        );
        let startup = self.startup.lock().map_err(|_| EngineError::Synchronization)?;
        let startup = self
            .startup_changed
            .wait_while(startup, |state| *state == Startup::Starting)
            .map_err(|_| EngineError::Synchronization)?;
        if *startup == Startup::Failed {
            return Err(EngineError::Exited);
        }
        drop(startup);
        // Freeze the private worker group: the C control thread cannot stop itself and reply.
        if matches!(request, StopRequest::Signal(signal) if signal == libc::SIGSTOP || signal == libc::SIGCONT) {
            let process = self
                .child
                .lock()
                .map_err(|_| EngineError::Synchronization)?
                .as_ref()
                .map(Child::id)
                .ok_or(EngineError::StopFailed)?;
            let StopRequest::Signal(signal) = request else {
                unreachable!();
            };
            return signal_process_group(process, signal);
        }
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
        let process = self
            .child
            .lock()
            .map_err(|_| EngineError::Synchronization)?
            .as_ref()
            .map(Child::id)
            .ok_or(EngineError::StopFailed)?;
        let worker_exited = requested.is_ok() && self.wait_force_grace()?;
        if worker_exited {
            return signal_process_group(process, libc::SIGKILL);
        }
        hl_log::hl_verdict!(
            hl_log::tag::EXEC,
            "retained_c.worker.force_kill",
            stage = %"stop",
            reason = %"force_kill",
            pid = process;
            "retained C worker failure stage=stop reason=force_kill pid={process}"
        );
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
        let (process, status) = {
            let mut child = self.child.lock().map_err(|_| EngineError::Synchronization)?;
            let child = child.as_mut().ok_or(EngineError::WaitFailed)?;
            let process = child.id();
            let status = child.wait().map_err(|_| EngineError::WaitFailed)?;
            (process, status)
        };
        signal_process_group(process, libc::SIGKILL)?;
        let signal = status.signal().ok_or(EngineError::WaitFailed)?;
        let exit = EngineExit {
            kind: crate::engine::ExitKind::Signal,
            guest_status: signal,
            detail: 0,
            fault: None,
        };
        *self.exit.lock().map_err(|_| EngineError::Synchronization)? = Some(exit);
        drop(self.streams.lock().map_err(|_| EngineError::Synchronization)?.take());
        hl_log::hl_event!(
            hl_log::tag::EXEC,
            hl_log::Level::Info,
            "retained_c.worker.reaped_without_frame",
            exit = ?exit
        );
        Ok(exit)
    }
}

impl Drop for CWorker {
    fn drop(&mut self) {
        let child = self.child.get_mut().unwrap_or_else(|error| error.into_inner());
        if let Some(child) = child.as_mut() {
            let running = child.try_wait().ok().flatten().is_none();
            if running {
                hl_log::hl_verdict!(
                    hl_log::tag::EXEC,
                    "retained_c.worker.drop_rollback",
                    stage = %"drop",
                    reason = %"worker_still_running",
                    pid = child.id();
                    "retained C worker failure stage=drop reason=worker_still_running pid={}", child.id()
                );
            }
            signal_process_group_best_effort(child.id(), libc::SIGKILL);
            if running {
                let _ = child.wait();
            }
        }
        if let Some(broker) = self.provider_broker.take() {
            let _ = broker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CWorker, ChildGuard, Startup, process_status_matches, read_message, report_worker_failure, worker_executable,
        write_message,
    };
    use crate::activation::GuestIsa;
    use crate::engine::StopRequest;
    use crate::engine::{EngineExit, ExitKind};
    use crate::execution::StreamBridge;
    use crate::execution::control::{FailureStage, Message};
    use std::io::{BufRead, BufReader};
    use std::os::unix::process::CommandExt;
    use std::os::unix::process::ExitStatusExt;
    use std::process::{Child, Command, Stdio};
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::{Duration, Instant};

    struct Capture(Arc<Mutex<String>>);

    impl hl_log::Sink for Capture {
        fn write_line(&self, line: &str) {
            self.0.lock().unwrap().push_str(line);
        }
    }

    fn capture_events(run: impl FnOnce()) -> String {
        let _guard = crate::execution::EVENT_CAPTURE_LOCK.lock().unwrap();
        let output = Arc::new(Mutex::new(String::new()));
        hl_log::Events::global().set(Box::new(Capture(Arc::clone(&output))));
        run();
        hl_log::Events::global().reset();
        Arc::try_unwrap(output).unwrap().into_inner().unwrap()
    }

    fn exit(status: i32) -> EngineExit {
        EngineExit {
            kind: ExitKind::Code,
            guest_status: status,
            detail: 0,
            fault: None,
        }
    }

    #[test]
    fn worker_executable_prefers_the_integration_binary_directory() {
        assert_eq!(
            worker_executable(
                GuestIsa::Aarch64,
                Some("/fixture/bin".into()),
                Some("/ignored/deps/test-hash".into()),
            ),
            Ok("/fixture/bin/hl-aarch64".into())
        );
    }

    #[test]
    fn worker_executable_defaults_to_the_calling_binary_sibling() {
        assert_eq!(
            worker_executable(GuestIsa::X86_64, None, Some("/product/bin/husklet".into())),
            Ok("/product/bin/hl-x86_64".into())
        );
        assert_eq!(
            worker_executable(GuestIsa::Aarch64, None, None),
            Err(crate::engine::EngineError::LaunchFailed)
        );
        assert_eq!(
            worker_executable(GuestIsa::Aarch64, None, Some("/target/debug/deps/test-hash".into())),
            Ok("/target/debug/hl-aarch64".into())
        );
    }

    #[test]
    fn framed_worker_failure_is_a_release_visible_supervisor_verdict() {
        let events = capture_events(|| report_worker_failure(FailureStage::Create, 17));
        for field in [
            r#""event":"execution.c.lifecycle.failed""#,
            r#""stage":"create""#,
            r#""code":17"#,
        ] {
            assert!(events.contains(field), "missing {field} in {events}");
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
        let events = capture_events(|| drop(ChildGuard(Some(child))));
        assert!(events.contains("retained_c.worker.create_rollback"));
        assert!(events.contains("\"stage\":\"create\""));
        assert!(events.contains("\"reason\":\"post_spawn_rollback\""));
        assert_group_gone(process);
    }

    #[test]
    fn pause_and_resume_signal_the_private_worker_group_without_control_rpc() {
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
            provider_broker: None,
            checkpoint: None,
            diagnostics: false,
        };

        worker.stop(StopRequest::Signal(libc::SIGSTOP)).unwrap();
        let mut status = 0;
        // SAFETY: process is the live child owned by worker and status is uniquely writable.
        assert_eq!(
            unsafe { libc::waitpid(i32::try_from(process).unwrap(), &raw mut status, libc::WUNTRACED) },
            process as i32
        );
        assert!(libc::WIFSTOPPED(status));
        assert_eq!(libc::WSTOPSIG(status), libc::SIGSTOP);

        worker.stop(StopRequest::Signal(libc::SIGCONT)).unwrap();
        // SAFETY: signal zero is an existence probe for the live child.
        assert_eq!(unsafe { libc::kill(i32::try_from(process).unwrap(), 0) }, 0);
        drop(worker);
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
            provider_broker: None,
            checkpoint: None,
            diagnostics: false,
        };
        let events = capture_events(|| worker.stop(StopRequest::Force).unwrap());
        assert!(events.contains("retained_c.worker.force_kill"));
        assert!(events.contains("\"stage\":\"stop\""));
        assert!(events.contains("\"reason\":\"force_kill\""));
        let status = worker.child.lock().unwrap().as_mut().unwrap().wait().unwrap();
        assert_eq!(status.signal(), Some(libc::SIGKILL));
        assert_group_gone(process);
    }

    #[test]
    fn force_stop_cleans_descendants_after_the_worker_exits_during_grace() {
        let mut child = session_child("sleep 60 >/dev/null & echo $!; exit 0", Stdio::piped());
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
            provider_broker: None,
            checkpoint: None,
            diagnostics: false,
        };

        worker.stop(StopRequest::Force).unwrap();

        assert_group_gone(process);
        // Prove the group assertion covered a real descendant rather than an
        // already-empty worker session.
        // SAFETY: signal zero is an existence probe for the child-reported positive pid.
        let status = unsafe { libc::kill(descendant, 0) };
        assert_eq!(status, -1);
        assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
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
            provider_broker: None,
            checkpoint: None,
            diagnostics: false,
        };
        let events = capture_events(|| drop(worker));
        assert!(events.contains("retained_c.worker.drop_rollback"));
        assert!(events.contains("\"stage\":\"drop\""));
        assert!(events.contains("\"reason\":\"worker_still_running\""));
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

    #[test]
    fn malformed_started_response_reports_both_protocol_and_start_failure() {
        let child = session_child("exec sleep 60", Stdio::null());
        let process = child.id();
        let (control, mut peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let reader = control.try_clone().unwrap();
        let worker = CWorker {
            child: Mutex::new(Some(child)),
            reader: Mutex::new(reader),
            writer: Arc::new(Mutex::new(control)),
            streams: Mutex::new(Some(StreamBridge::inherited())),
            exit: Mutex::new(None),
            startup: Mutex::new(Startup::Starting),
            startup_changed: Condvar::new(),
            provider_broker: None,
            checkpoint: None,
            diagnostics: false,
        };
        let peer_thread = std::thread::spawn(move || {
            write_message(&mut peer, Message::Ready).unwrap();
            assert_eq!(read_message(&mut peer).unwrap(), Message::Start);
            write_message(&mut peer, Message::Ready).unwrap();
        });
        let events = capture_events(|| assert_eq!(worker.start(), Err(crate::engine::EngineError::LaunchFailed)));
        peer_thread.join().unwrap();
        assert!(events.contains("retained_c.worker.protocol_failed"));
        assert!(events.contains("\"phase\":\"started\""));
        assert!(events.contains("retained_c.worker.start_failed"));
        assert!(events.contains("\"stage\":\"start\""));
        assert!(events.contains("\"reason\":\"LaunchFailed\""));
        drop(worker);
        assert_group_gone(process);
    }
}
