#![allow(unsafe_code)]

use super::control::{FRAME_SIZE, FailureStage, Message};
use super::{CGuestExecutor, wire};
use crate::engine::EngineError;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
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
    let checkpoint_descriptor = inherited_descriptor("HL_C_CHECKPOINT_FD");
    let checkpoint_trigger_descriptor = inherited_descriptor("HL_C_CHECKPOINT_TRIGGER_FD");
    // Lifecycle events belong to the supervising parent: this process's stderr is
    // the guest's stderr stream, so host diagnostics here would corrupt guest output.
    if plan_descriptor < 3
        || control_descriptor < 3
        || plan_descriptor == control_descriptor
        || provider_descriptor
            .is_some_and(|provider| provider < 3 || provider == plan_descriptor || provider == control_descriptor)
        || checkpoint_descriptor.is_some() != checkpoint_trigger_descriptor.is_some()
    {
        return Err(WorkerError::Descriptor);
    }
    if let (Some(checkpoint), Some(trigger)) = (checkpoint_descriptor, checkpoint_trigger_descriptor) {
        if [plan_descriptor, control_descriptor].contains(&checkpoint)
            || [plan_descriptor, control_descriptor, checkpoint].contains(&trigger)
        {
            return Err(WorkerError::Descriptor);
        }
        // SAFETY: both descriptors are uniquely inherited by this one-shot worker.
        if unsafe { super::hl_c_backend_checkpoint_adopt(checkpoint, trigger) } != super::STATUS_OK {
            return Err(WorkerError::Create);
        }
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
    let (isa, mut plan) = wire::decode(&bytes).map_err(|_| {
        send_error(&mut control, FailureStage::Decode, 2);
        WorkerError::Plan
    })?;
    let launch_domain = launch_domain().map_err(|_| {
        send_error(&mut control, FailureStage::Create, 2);
        WorkerError::Create
    })?;
    plan.options
        .set("HL_LAUNCH_DOMAIN", &launch_domain, true)
        .map_err(|_| {
            send_error(&mut control, FailureStage::Create, 3);
            WorkerError::Create
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
    // The retained engine shares this launcher process with Rust. Its checkpoint
    // descriptor scan must not publish the Rust lifecycle channel as a guest
    // socket. Registration occurs after backend creation, which initializes the
    // engine-private descriptor registry.
    // SAFETY: backend creation initialized the process-private descriptor
    // registry, `control` owns a live descriptor for this call, and the C
    // function copies only its integer value without retaining Rust memory.
    if unsafe { super::hl_c_backend_private_descriptor_add(control.as_raw_fd()) } != super::STATUS_OK {
        return Err(WorkerError::Descriptor);
    }
    write_message(&mut control, Message::Ready)?;
    if read_message(&mut control)? != Message::Start {
        send_error(&mut control, FailureStage::Control, 1);
        return Err(WorkerError::Control);
    }
    let request_control = control.try_clone().map_err(|_| WorkerError::Control)?;
    // SAFETY: `request_control` owns a live duplicate for this call, the
    // process-private registry is initialized, and the C function copies only
    // the descriptor value without retaining Rust memory.
    if unsafe { super::hl_c_backend_private_descriptor_add(request_control.as_raw_fd()) } != super::STATUS_OK {
        return Err(WorkerError::Descriptor);
    }
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

fn launch_domain() -> Result<String, std::io::Error> {
    loop {
        let mut identity = [0_u8; 16];
        std::fs::File::open("/dev/urandom")?.read_exact(&mut identity)?;
        if identity.iter().any(|byte| *byte != 0) {
            return Ok(hex_identity(&identity));
        }
    }
}

fn hex_identity(identity: &[u8; 16]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(identity.len() * 2);
    for byte in identity {
        encoded.push(char::from(DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn inherited_descriptor(name: &str) -> Option<RawFd> {
    let value = std::env::var(name).ok()?;
    (!value.is_empty() && value.len() <= 10 && value.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| value.parse().ok())
        .flatten()
        .filter(|descriptor| *descriptor >= 3)
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
    use super::{hex_identity, start_result};
    use crate::engine::EngineError;

    #[test]
    fn retained_start_preserves_the_c_status_for_the_owner() {
        assert_eq!(start_result(Ok(0)), Ok(()));
        assert_eq!(start_result(Ok(6)), Err(6));
        assert_eq!(start_result(Ok(13)), Err(13));
        assert_eq!(start_result(Err(EngineError::LaunchFailed)), Err(-1));
    }

    #[test]
    fn launch_domain_identity_is_fixed_width_lowercase_hex() {
        let identity = [
            0x00, 0x01, 0x09, 0x0a, 0x0f, 0x10, 0x7f, 0x80, 0xab, 0xcd, 0xef, 0xf0, 2, 3, 4, 5,
        ];
        assert_eq!(hex_identity(&identity), "0001090a0f107f80abcdeff002030405");
    }
}
