//! sendmmsg/recvmmsg batch message handling for the network syscall surface.

use hl_linux::{Errno, GuestMarshaller, GuestMemory, LinuxResult};

use crate::{RuntimeNetworkHost, RuntimeNetworkSyscalls, RuntimeSocketKind};

use super::{
    BatchReceiveEntry, MESSAGE_VECTOR_MAXIMUM, MSG_DONTWAIT, MSG_WAITFORONE, MULTI_MESSAGE_LENGTH_OFFSET,
    MULTI_MESSAGE_SIZE,
};

impl<H: RuntimeNetworkHost, M: GuestMemory> RuntimeNetworkSyscalls<H, M> {
    pub(crate) fn credential_controls(credentials: hl_network::SenderCredentials) -> Vec<hl_network::ControlMessage> {
        vec![hl_network::ControlMessage::Credentials {
            process: credentials.process,
            user: credentials.user,
            group: credentials.group,
        }]
    }

    pub(crate) fn sendmmsg(&self, descriptor: i32, messages: u64, count: u32, flags: u32) -> LinuxResult {
        if count > MESSAGE_VECTOR_MAXIMUM {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let bytes = count as usize * MULTI_MESSAGE_SIZE;
        let marshaller = GuestMarshaller::new(&self.memory, self.architecture);
        for access in [hl_linux::GuestAccess::Read, hl_linux::GuestAccess::Write] {
            match marshaller.probe(messages, bytes, access) {
                Ok(available) if available == bytes => {}
                _ => return LinuxResult::Error(Errno::EFAULT),
            }
        }
        let mut completed = 0_u32;
        while completed < count {
            let header = messages + completed as u64 * MULTI_MESSAGE_SIZE as u64;
            let length = match self.sendmsg(descriptor, header, flags) {
                LinuxResult::Value(value) => value,
                LinuxResult::Error(error) if completed == 0 => {
                    return LinuxResult::Error(error);
                }
                LinuxResult::Error(_) => break,
                LinuxResult::Restart(kind) if completed == 0 => {
                    return LinuxResult::Restart(kind);
                }
                LinuxResult::Restart(_) => break,
            };
            match self.write_message_length(&marshaller, header, length) {
                Ok(()) => {}
                Err(error) if completed == 0 => return LinuxResult::Error(error),
                Err(_) => break,
            }
            completed += 1;
        }
        LinuxResult::Value(completed as u64)
    }

    fn write_message_length(&self, marshaller: &GuestMarshaller<'_, M>, header: u64, length: u64) -> Result<(), Errno> {
        let encoded = (length as u32).to_le_bytes();
        if marshaller
            .copy_to(header + MULTI_MESSAGE_LENGTH_OFFSET, &encoded)
            .fault
            .is_some()
        {
            return Err(Errno::EFAULT);
        }
        Ok(())
    }

    pub(crate) fn recvmmsg(&self, descriptor: i32, messages: u64, count: u32, flags: u32, timeout: u64) -> LinuxResult {
        if count > MESSAGE_VECTOR_MAXIMUM {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let bytes = count as usize * MULTI_MESSAGE_SIZE;
        let marshaller = GuestMarshaller::new(&self.memory, self.architecture);
        for access in [hl_linux::GuestAccess::Read, hl_linux::GuestAccess::Write] {
            match marshaller.probe(messages, bytes, access) {
                Ok(available) if available == bytes => {}
                _ => return LinuxResult::Error(Errno::EFAULT),
            }
        }
        let (deadline, zero_timeout) = match self.receive_deadline(timeout, &marshaller) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        let socket = match self.lookup(descriptor) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        let endpoint = match &socket.kind {
            RuntimeSocketKind::Unix { pair, endpoint } => Some(&pair.endpoints[*endpoint]),
            RuntimeSocketKind::Host { .. } | RuntimeSocketKind::UnixStandalone { .. } => None,
        };
        let mut completed = 0_u32;
        loop {
            if completed >= count {
                break;
            }
            let header = messages + completed as u64 * MULTI_MESSAGE_SIZE as u64;
            let later_nonblocking = completed > 0;
            let nested_flags = (flags & !MSG_WAITFORONE)
                | if later_nonblocking { MSG_DONTWAIT } else { 0 }
                | if zero_timeout { MSG_DONTWAIT } else { 0 };
            match self.receive_batch_entry(
                descriptor,
                header,
                nested_flags,
                completed,
                zero_timeout,
                endpoint,
                deadline,
                &marshaller,
            ) {
                BatchReceiveEntry::Committed => completed += 1,
                BatchReceiveEntry::Retry => continue,
                BatchReceiveEntry::Failed(result) => {
                    self.copy_remaining_timeout(timeout, deadline, &marshaller);
                    return result;
                }
            }
        }
        self.copy_remaining_timeout(timeout, deadline, &marshaller);
        LinuxResult::Value(completed as u64)
    }

    fn receive_deadline(
        &self,
        pointer: u64,
        marshaller: &GuestMarshaller<'_, M>,
    ) -> Result<(Option<hl_time::Deadline>, bool), Errno> {
        if pointer == 0 {
            return Ok((None, false));
        }
        let mut bytes = [0_u8; 16];
        if marshaller.copy_from(pointer, &mut bytes).fault.is_some() {
            return Err(Errno::EFAULT);
        }
        let seconds = i64::from_le_bytes(bytes[..8].try_into().unwrap());
        let nanoseconds = i64::from_le_bytes(bytes[8..].try_into().unwrap());
        if seconds < 0 || !(0..1_000_000_000).contains(&nanoseconds) {
            return Err(Errno::EINVAL);
        }
        let duration = (seconds as u64)
            .checked_mul(1_000_000_000)
            .and_then(|value| value.checked_add(nanoseconds as u64))
            .ok_or(Errno::EINVAL)?;
        if duration == 0 {
            return Ok((Some(hl_time::Deadline::from_nanoseconds(0)), true));
        }
        let wait = self.wait.as_ref().ok_or(Errno::ENOSYS)?;
        let now = wait.monotonic_now().map_err(|_| Errno::EIO)?;
        Ok((
            Some(now.deadline_after(hl_time::Duration::from_nanoseconds(duration))),
            false,
        ))
    }

    fn batch_write_failure(completed: u32, error: Errno) -> LinuxResult {
        match completed {
            0 => LinuxResult::Error(error),
            value => LinuxResult::Value(value as u64),
        }
    }

    fn receive_batch_entry(
        &self,
        descriptor: i32,
        header: u64,
        flags: u32,
        completed: u32,
        zero_timeout: bool,
        endpoint: Option<&hl_network::UnixSocketEndpoint>,
        deadline: Option<hl_time::Deadline>,
        marshaller: &GuestMarshaller<'_, M>,
    ) -> BatchReceiveEntry {
        match self.receive_message(descriptor, header, flags, false) {
            LinuxResult::Value(length) => match self.write_message_length(marshaller, header, length) {
                Ok(()) => BatchReceiveEntry::Committed,
                Err(error) => BatchReceiveEntry::Failed(Self::batch_write_failure(completed, error)),
            },
            LinuxResult::Error(Errno::EAGAIN) if zero_timeout => {
                BatchReceiveEntry::Failed(LinuxResult::Value(completed as u64))
            }
            LinuxResult::Error(Errno::EAGAIN) if flags & MSG_DONTWAIT != 0 => {
                let result = match completed {
                    0 => LinuxResult::Error(Errno::EAGAIN),
                    value => LinuxResult::Value(value as u64),
                };
                BatchReceiveEntry::Failed(result)
            }
            LinuxResult::Error(Errno::EAGAIN) => {
                let Some(endpoint) = endpoint else {
                    return BatchReceiveEntry::Failed(Self::batch_write_failure(completed, Errno::EAGAIN));
                };
                match self.wait_for_message(endpoint, deadline) {
                    Ok(true) => BatchReceiveEntry::Retry,
                    Ok(false) => BatchReceiveEntry::Failed(LinuxResult::Value(completed as u64)),
                    Err(error) => BatchReceiveEntry::Failed(Self::batch_write_failure(completed, error)),
                }
            }
            result => BatchReceiveEntry::Failed(result),
        }
    }

    pub(super) fn wait_for_message(
        &self,
        endpoint: &hl_network::UnixSocketEndpoint,
        deadline: Option<hl_time::Deadline>,
    ) -> Result<bool, Errno> {
        let wait = self.wait.as_ref().ok_or(Errno::ENOSYS)?;
        loop {
            let observed = endpoint.message_wait_queue().observation();
            if endpoint.message_ready() || endpoint.readable_bytes() != 0 {
                return Ok(true);
            }
            match wait.wait(endpoint.message_wait_queue(), observed, deadline) {
                Ok(hl_sync::WaitOutcome::Notified) => {}
                Ok(hl_sync::WaitOutcome::Interrupted) => return Err(Errno::EINTR),
                Ok(hl_sync::WaitOutcome::TimedOut) => return Ok(false),
                Err(_) => return Err(Errno::EIO),
            }
        }
    }

    fn copy_remaining_timeout(
        &self,
        pointer: u64,
        deadline: Option<hl_time::Deadline>,
        marshaller: &GuestMarshaller<'_, M>,
    ) {
        let (Some(deadline), Some(wait)) = (deadline, self.wait.as_ref()) else {
            return;
        };
        if pointer == 0 {
            return;
        }
        let Ok(now) = wait.monotonic_now() else {
            return;
        };
        let remaining = deadline.remaining_at(now).timespec();
        let bytes = [
            remaining.seconds().to_le_bytes(),
            (remaining.subsecond_nanoseconds() as u64).to_le_bytes(),
        ]
        .concat();
        let _result = marshaller.copy_to(pointer, &bytes);
    }
}
