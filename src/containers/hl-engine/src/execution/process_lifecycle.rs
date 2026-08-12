#![allow(unsafe_code)]

use super::control::{FRAME_SIZE, Message};
use crate::activation::GuestIsa;
use crate::engine::{EngineError, EngineExit};
use std::ffi::{CString, OsString};
use std::io::{Read, Seek, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::process::Child;

pub(super) fn worker_executable(
    isa: GuestIsa,
    test_binary_directory: Option<OsString>,
    current_executable: Option<std::path::PathBuf>,
) -> Result<std::path::PathBuf, EngineError> {
    if let Some(directory) = test_binary_directory {
        return Ok(std::path::PathBuf::from(directory).join(isa.engine_stem()));
    }
    let executable = current_executable.ok_or(EngineError::LaunchFailed)?;
    let parent = executable.parent().ok_or(EngineError::LaunchFailed)?;
    let directory = if parent.file_name().is_some_and(|name| name == "deps") {
        parent.parent().unwrap_or(parent)
    } else {
        parent
    };
    Ok(directory.join(isa.engine_stem()))
}

pub(super) struct ChildGuard(pub(super) Option<Child>);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let Some(child) = self.0.as_mut() else { return };
        let running = child.try_wait().ok().flatten().is_none();
        if running {
            hl_log::hl_verdict!(
                hl_log::tag::EXEC,
                "retained_c.worker.create_rollback",
                stage = %"create",
                reason = %"post_spawn_rollback",
                pid = child.id();
                "retained C worker failure stage=create reason=post_spawn_rollback pid={}", child.id()
            );
        }
        signal_process_group_best_effort(child.id(), libc::SIGKILL);
        if running {
            let _ = child.wait();
        }
    }
}

pub(super) fn signal_process_group(process: u32, signal: i32) -> Result<(), EngineError> {
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

pub(super) fn signal_process_group_best_effort(process: u32, signal: i32) {
    if signal_process_group(process, signal).is_err() {
        hl_log::hl_debug!(
            hl_log::tag::EXEC,
            "retained C worker process group already unavailable pid={process} signal={signal}"
        );
    }
}

pub(super) fn process_status_matches(status: &std::process::ExitStatus, exit: EngineExit) -> bool {
    status.code() == Some(exit.process_status())
}

pub(super) fn sealed_plan(bytes: &[u8]) -> Result<std::fs::File, EngineError> {
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

pub(super) fn duplicate_high(descriptor: i32) -> Result<OwnedFd, EngineError> {
    // SAFETY: descriptor is live; F_DUPFD_CLOEXEC returns unique ownership at fd >= 10.
    let duplicate = unsafe { libc::fcntl(descriptor, libc::F_DUPFD_CLOEXEC, 10) };
    if duplicate < 0 {
        Err(EngineError::LaunchFailed)
    } else {
        // SAFETY: successful fcntl returned a new descriptor.
        Ok(unsafe { OwnedFd::from_raw_fd(duplicate) })
    }
}

pub(super) fn read_message(stream: &mut std::os::unix::net::UnixStream) -> Result<Message, EngineError> {
    let mut frame = [0_u8; FRAME_SIZE];
    stream.read_exact(&mut frame).map_err(|_| EngineError::WaitFailed)?;
    Message::decode(&frame).map_err(|_| EngineError::WaitFailed)
}

pub(super) fn write_message(stream: &mut std::os::unix::net::UnixStream, message: Message) -> Result<(), EngineError> {
    let frame = message.encode().map_err(|_| EngineError::StopFailed)?;
    stream.write_all(&frame).map_err(|_| EngineError::StopFailed)
}
