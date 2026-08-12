#![allow(unsafe_code)]

use super::control::{FRAME_SIZE, FailureStage, Message};
use super::{CGuestExecutor, wire};
use crate::engine::EngineError;
use std::io::{Read, Write};
use std::os::fd::{FromRawFd, RawFd};
use std::sync::Arc;

const MAXIMUM_PLAN: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WorkerError {
    Descriptor,
    Plan,
    Control,
    Create,
    Start,
}

impl WorkerError {
    /// Deliberately consumes an error that cannot be reported without corrupting
    /// the guest-owned stderr stream.
    fn abandon(self) {}
}

pub(crate) fn run(
    plan_descriptor: RawFd,
    control_descriptor: RawFd,
    provider_descriptor: Option<RawFd>,
) -> Result<i32, WorkerError> {
    // Lifecycle events belong to the supervising parent: this process's stderr is
    // the guest's stderr stream, so host diagnostics here would corrupt guest output.
    if plan_descriptor < 3
        || control_descriptor < 3
        || plan_descriptor == control_descriptor
        || provider_descriptor
            .is_some_and(|provider| provider < 3 || provider == plan_descriptor || provider == control_descriptor)
    {
        return Err(WorkerError::Descriptor);
    }
    // SAFETY: this is the one-shot worker entry and takes unique ownership of inherited descriptors.
    let plan_file = unsafe { std::fs::File::from_raw_fd(plan_descriptor) };
    // SAFETY: same one-shot ownership contract, for the distinct control descriptor.
    let mut control = unsafe { std::os::unix::net::UnixStream::from_raw_fd(control_descriptor) };
    let executable_authority = crate::executable::ExecutableAuthority::receive_optional(&control).map_err(|_| {
        send_error(&mut control, FailureStage::Control, 3);
        WorkerError::Control
    })?;
    let mut bytes = Vec::new();
    plan_file
        .take(MAXIMUM_PLAN + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| WorkerError::Plan)?;
    if bytes.len() as u64 > MAXIMUM_PLAN {
        send_error(&mut control, FailureStage::Decode, 1);
        return Err(WorkerError::Plan);
    }
    let (isa, plan) = wire::decode(&bytes).map_err(|_| {
        send_error(&mut control, FailureStage::Decode, 2);
        WorkerError::Plan
    })?;
    let executor = Arc::new(
        CGuestExecutor::create_with_provider(
            isa,
            &plan,
            executable_authority.as_ref(),
            [0, 1, 2],
            None,
            provider_descriptor,
        )
        .map_err(|_| {
            send_error(&mut control, FailureStage::Create, 1);
            WorkerError::Create
        })?,
    );
    write_message(&mut control, Message::Ready)?;
    if read_message(&mut control)? != Message::Start {
        send_error(&mut control, FailureStage::Control, 1);
        return Err(WorkerError::Control);
    }
    let request_control = control.try_clone().map_err(|_| WorkerError::Control)?;
    let request_executor = Arc::clone(&executor);
    std::thread::Builder::new()
        .name("hl-c-worker-control".into())
        .spawn(move || serve_requests(request_control, request_executor))
        .map_err(|_| WorkerError::Control)?;
    write_message(&mut control, Message::Started)?;
    if let Err(code) = start_result(executor.run_plan_status(&plan)) {
        send_error(&mut control, FailureStage::Start, code);
        return Err(WorkerError::Start);
    }
    let exit = executor.exit();
    write_message(&mut control, Message::Exit(exit))?;
    Ok(exit.process_status())
}

fn serve_requests(mut control: std::os::unix::net::UnixStream, executor: Arc<CGuestExecutor>) {
    loop {
        let Ok(message) = read_message(&mut control) else {
            return;
        };
        let result = match message {
            Message::Stop(request) => executor.stop_request(request),
            Message::Resize { rows, columns } => resize(rows, columns)
                .and_then(|()| executor.stop_request(crate::engine::StopRequest::Signal(libc::SIGWINCH))),
            _ => Err(EngineError::StopFailed),
        };
        if result.is_err() {
            send_error(&mut control, FailureStage::Control, 2);
            return;
        }
    }
}

fn resize(rows: u16, columns: u16) -> Result<(), EngineError> {
    let window = libc::winsize {
        ws_row: rows,
        ws_col: columns,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: fd 0 is the inherited PTY slave for terminal launches; ioctl retains no pointer.
    (unsafe { libc::ioctl(0, libc::TIOCSWINSZ, &raw const window) } == 0)
        .then_some(())
        .ok_or(EngineError::StopFailed)
}

fn read_message(stream: &mut std::os::unix::net::UnixStream) -> Result<Message, WorkerError> {
    let mut frame = [0_u8; FRAME_SIZE];
    stream.read_exact(&mut frame).map_err(|_| WorkerError::Control)?;
    Message::decode(&frame).map_err(|_| WorkerError::Control)
}

fn write_message(stream: &mut std::os::unix::net::UnixStream, message: Message) -> Result<(), WorkerError> {
    let frame = message.encode().map_err(|_| WorkerError::Control)?;
    stream.write_all(&frame).map_err(|_| WorkerError::Control)
}

fn send_error(stream: &mut std::os::unix::net::UnixStream, stage: FailureStage, code: i32) {
    if let Err(error) = write_message(stream, Message::Error { stage, code }) {
        error.abandon();
    }
}

fn start_result(result: Result<i32, EngineError>) -> Result<(), i32> {
    match result {
        Ok(super::STATUS_OK) => Ok(()),
        Ok(status) => Err(status),
        Err(_) => Err(-1),
    }
}

#[cfg(test)]
mod tests {
    use super::start_result;
    use crate::engine::EngineError;

    #[test]
    fn retained_start_preserves_the_c_status_for_the_owner() {
        assert_eq!(start_result(Ok(0)), Ok(()));
        assert_eq!(start_result(Ok(6)), Err(6));
        assert_eq!(start_result(Ok(13)), Err(13));
        assert_eq!(start_result(Err(EngineError::LaunchFailed)), Err(-1));
    }
}
