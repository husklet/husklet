use hl_linux::{
    Errno, GuestMarshaller, GuestMemory, GuestNetworkAddress, LinuxResult, MessageCopyoutResult, MessageImport,
    NetworkAbi, NetworkMarshalError,
};
use hl_network::{ControlCodec, ControlMessage, ControlWord};

use crate::{
    HostControl, HostSend, RuntimeNetworkHost, RuntimeNetworkSyscalls, RuntimeSocket, RuntimeSocketKind,
    network::errno::SocketErrno, network::wait::SocketCancellation,
};

const MSG_PEEK: u32 = 0x2;
const MSG_CTRUNC: u32 = 0x8;
const MSG_TRUNC: u32 = 0x20;
const MSG_DONTWAIT: u32 = 0x40;
const MSG_CMSG_CLOEXEC: u32 = 0x4000_0000;
const CONTROL_MAXIMUM: usize = 65_536;

enum ReceiveSlot {
    Rights(usize),
    Control(hl_network::ControlMessage),
}

struct ReceivePlan<A> {
    attachments: Vec<A>,
    slots: Vec<ReceiveSlot>,
}

impl<A> ReceivePlan<A> {
    fn new(controls: Vec<HostControl<A>>) -> Self {
        let mut plan = Self {
            attachments: Vec::new(),
            slots: Vec::with_capacity(controls.len()),
        };
        for control in controls {
            plan.push(control);
        }
        plan
    }

    fn push(&mut self, control: HostControl<A>) {
        match control {
            HostControl::Rights(rights) => {
                self.slots.push(ReceiveSlot::Rights(rights.len()));
                self.attachments.extend(rights);
            }
            HostControl::Credentials(credentials) => {
                self.slots
                    .push(ReceiveSlot::Control(hl_network::ControlMessage::Credentials {
                        process: credentials.process,
                        user: credentials.user,
                        group: credentials.group,
                    }));
            }
            HostControl::Unknown { level, kind, data } => {
                self.slots
                    .push(ReceiveSlot::Control(hl_network::ControlMessage::Unknown {
                        level,
                        kind,
                        data,
                    }));
            }
        }
    }

    fn fit(mut self, capacity: usize) -> Result<(Self, bool), hl_network::ControlError> {
        let controls = self
            .slots
            .iter()
            .map(|slot| match slot {
                ReceiveSlot::Rights(count) => ControlMessage::Rights(vec![0; *count]),
                ReceiveSlot::Control(control) => control.clone(),
            })
            .collect::<Vec<_>>();
        let encoded = ControlCodec::encode(&controls, ControlWord::Eight, capacity)?;
        let visible = ControlCodec::decode(&encoded.bytes, ControlWord::Eight)?;
        let delivered = visible
            .iter()
            .map(|control| match control {
                ControlMessage::Rights(numbers) => numbers.len(),
                _ => 0,
            })
            .sum();
        self.attachments.truncate(delivered);
        self.slots = visible
            .into_iter()
            .map(|control| match control {
                ControlMessage::Rights(numbers) => ReceiveSlot::Rights(numbers.len()),
                control => ReceiveSlot::Control(control),
            })
            .collect();
        Ok((self, encoded.truncated))
    }

    fn controls(slots: &[ReceiveSlot], numbers: &[i32]) -> Vec<hl_network::ControlMessage> {
        let mut next = 0;
        slots
            .iter()
            .map(|slot| match slot {
                ReceiveSlot::Rights(count) => {
                    let end = next + count;
                    let control = hl_network::ControlMessage::Rights(numbers[next..end].to_vec());
                    next = end;
                    control
                }
                ReceiveSlot::Control(control) => control.clone(),
            })
            .collect()
    }
}

impl<H: RuntimeNetworkHost, M: GuestMemory> RuntimeNetworkSyscalls<H, M> {
    pub(crate) fn send_host(
        &self,
        socket: &RuntimeSocket<H>,
        payload: Vec<u8>,
        address: Option<GuestNetworkAddress>,
        controls: Vec<hl_network::ControlMessage>,
        nonblocking: bool,
    ) -> LinuxResult {
        let RuntimeSocketKind::Host { description, token } = &socket.kind else {
            return LinuxResult::Error(Errno::ENOTSOCK);
        };
        if controls.is_empty() {
            let result = if let Some(address) = address {
                self.send_address(socket, &payload, address, nonblocking)
            } else {
                self.write_socket(socket, &payload, nonblocking)
            };
            return match result {
                Ok(count) => LinuxResult::Value(count as u64),
                Err(error) => LinuxResult::Error(error),
            };
        }
        let Some(host) = &self.host else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let address = match address.map(Self::host_address).transpose() {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(SocketErrno::marshal(error)),
        };
        if let Some(address) = &address
            && let Err(error) = self.route(address)
        {
            return LinuxResult::Error(error);
        }
        let route = address.map(|address| self.connect_route(address));
        let record = socket
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .socket_type
            != hl_network::SocketType::Stream;
        loop {
            let (host_controls, has_rights) = match self.export_controls(&controls, self.transfer.as_deref()) {
                Ok(controls) => controls,
                Err(error) => return LinuxResult::Error(error),
            };
            let request = HostSend {
                payload: payload.clone(),
                route: route.clone(),
                controls: host_controls,
                nonblocking: true,
                record,
            };
            match host.send_message(*token, request) {
                Ok(sent)
                    if sent.count <= payload.len()
                        && sent.rights_consumed == (has_rights && (record || sent.count > 0)) =>
                {
                    return LinuxResult::Value(sent.count as u64);
                }
                Ok(_) => return LinuxResult::Error(Errno::EIO),
                Err(crate::RuntimeNetworkError::WouldBlock) if !nonblocking => {}
                Err(error) => return LinuxResult::Error(SocketErrno::runtime(error)),
            }
            let wait = self.wait.as_ref().ok_or(Errno::EAGAIN);
            let Ok(wait) = wait else {
                return LinuxResult::Error(Errno::EAGAIN);
            };
            let cancellation = SocketCancellation::new(wait.interruption());
            if let Err(error) = description.wait_writable(&cancellation) {
                return LinuxResult::Error(crate::filesystem::FileErrno::object(error));
            }
        }
    }

    fn export_controls(
        &self,
        controls: &[hl_network::ControlMessage],
        transfer: Option<&dyn crate::DescriptorTransfer<H::Attachment>>,
    ) -> Result<(Vec<HostControl<H::Attachment>>, bool), Errno> {
        let mut output = Vec::with_capacity(controls.len());
        let mut has_rights = false;
        for control in controls {
            let translated = self.export_control(control, transfer)?;
            has_rights |= matches!(&translated, HostControl::Rights(rights) if !rights.is_empty());
            output.push(translated);
        }
        Ok((output, has_rights))
    }

    fn export_control(
        &self,
        control: &hl_network::ControlMessage,
        transfer: Option<&dyn crate::DescriptorTransfer<H::Attachment>>,
    ) -> Result<HostControl<H::Attachment>, Errno> {
        match control {
            hl_network::ControlMessage::Rights(numbers) => self.export_rights(numbers, transfer),
            hl_network::ControlMessage::Credentials { process, user, group } => {
                Ok(HostControl::Credentials(hl_network::SenderCredentials {
                    process: *process,
                    user: *user,
                    group: *group,
                }))
            }
            hl_network::ControlMessage::Unknown { level, kind, data } => Ok(HostControl::Unknown {
                level: *level,
                kind: *kind,
                data: data.clone(),
            }),
        }
    }

    fn export_rights(
        &self,
        numbers: &[i32],
        transfer: Option<&dyn crate::DescriptorTransfer<H::Attachment>>,
    ) -> Result<HostControl<H::Attachment>, Errno> {
        let Some(transfer) = transfer else {
            return Err(Errno::EOPNOTSUPP);
        };
        let mut attachments = Vec::with_capacity(numbers.len());
        for number in numbers {
            attachments.push(self.export_reference(*number, transfer)?);
        }
        Ok(HostControl::Rights(attachments))
    }

    fn export_reference(
        &self,
        number: i32,
        transfer: &dyn crate::DescriptorTransfer<H::Attachment>,
    ) -> Result<H::Attachment, Errno> {
        let Ok(reference) = self.descriptors.export_description(number) else {
            return Err(Errno::EBADF);
        };
        match transfer.export(&reference) {
            Ok(attachment) => Ok(attachment),
            Err(error) => Err(SocketErrno::runtime(error)),
        }
    }

    fn send_address(
        &self,
        socket: &RuntimeSocket<H>,
        payload: &[u8],
        address: GuestNetworkAddress,
        nonblocking: bool,
    ) -> Result<usize, Errno> {
        let address = Self::host_address(address).map_err(SocketErrno::marshal)?;
        self.route(&address)?;
        let RuntimeSocketKind::Host { description, token } = &socket.kind else {
            return Err(Errno::ENOTSOCK);
        };
        let host = self.host.as_ref().ok_or(Errno::ENOSYS)?;
        loop {
            match host.send_to_route(*token, payload, self.connect_route(address.clone()), true) {
                Ok(count) => return Ok(count),
                Err(crate::RuntimeNetworkError::WouldBlock) if !nonblocking => {}
                Err(error) => return Err(SocketErrno::runtime(error)),
            }
            let wait = self.wait.as_ref().ok_or(Errno::EAGAIN)?;
            let cancellation = SocketCancellation::new(wait.interruption());
            description
                .wait_writable(&cancellation)
                .map_err(crate::filesystem::FileErrno::object)?;
        }
    }

    pub(crate) fn recv_host(
        &self,
        socket: &RuntimeSocket<H>,
        imported: &MessageImport,
        flags: u32,
        abi: &NetworkAbi<'_, M>,
    ) -> LinuxResult {
        let Ok(length) = usize::try_from(imported.vectors.total_length) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        let nonblocking = flags & MSG_DONTWAIT != 0
            || socket
                .snapshot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .nonblocking;
        let record = socket
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .socket_type
            != hl_network::SocketType::Stream;
        let RuntimeSocketKind::Host { description, token } = &socket.kind else {
            return LinuxResult::Error(Errno::ENOTSOCK);
        };
        let Some(host) = &self.host else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let received = loop {
            match host.receive_message(*token, length, CONTROL_MAXIMUM, true, flags & MSG_PEEK != 0) {
                Ok(received) => break received,
                Err(crate::RuntimeNetworkError::WouldBlock) if !nonblocking => {}
                Err(error) => return LinuxResult::Error(SocketErrno::runtime(error)),
            }
            let Some(wait) = &self.wait else {
                return LinuxResult::Error(Errno::EAGAIN);
            };
            let cancellation = SocketCancellation::new(wait.interruption());
            if let Err(error) = description.wait_readable(&cancellation) {
                return LinuxResult::Error(crate::filesystem::FileErrno::object(error));
            }
        };
        let (plan, locally_truncated) =
            match ReceivePlan::new(received.controls).fit(imported.header.control_length.min(CONTROL_MAXIMUM)) {
                Ok(value) => value,
                Err(error) => {
                    return LinuxResult::Error(SocketErrno::marshal(NetworkMarshalError::Control(error)));
                }
            };
        let attachments = plan.attachments;
        let slots = plan.slots;
        let message_flags = (if received.payload_truncated { MSG_TRUNC } else { 0 })
            | (if received.control_truncated || locally_truncated {
                MSG_CTRUNC
            } else {
                0
            });
        let copyout = |numbers: &[i32]| {
            let result = MessageCopyoutResult {
                address: received.source.as_ref().map(Self::guest_address),
                data: received.payload.clone(),
                controls: ReceivePlan::<H::Attachment>::controls(&slots, numbers),
                flags: message_flags,
            };
            let staged = abi.prepare_receive(imported, &result).map_err(SocketErrno::marshal)?;
            staged
                .commit(&GuestMarshaller::new(&self.memory, self.architecture))
                .map_err(SocketErrno::marshal)
        };
        if let Err(error) = self.publish_receive(attachments, flags, copyout) {
            return LinuxResult::Error(error);
        }
        LinuxResult::Value(if record && flags & MSG_TRUNC != 0 {
            received.full_length as u64
        } else {
            received.payload.len() as u64
        })
    }

    fn publish_receive(
        &self,
        attachments: Vec<H::Attachment>,
        flags: u32,
        copyout: impl FnOnce(&[i32]) -> Result<(), Errno>,
    ) -> Result<(), Errno> {
        if attachments.is_empty() {
            return copyout(&[]);
        }
        let transfer = self.transfer.as_ref().ok_or(Errno::EOPNOTSUPP)?;
        let imported = transfer.import(attachments).map_err(SocketErrno::runtime)?;
        let prepared = imported
            .prepare(&self.descriptors, flags & MSG_CMSG_CLOEXEC != 0)
            .map_err(SocketErrno::runtime)?;
        prepared
            .publish_after(copyout)
            .map(|_| ())
            .map_err(|error| match error {
                crate::TransferCommitError::Runtime(error) => SocketErrno::runtime(error),
                crate::TransferCommitError::Copyout(error) => error,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{HostControl, ReceivePlan, ReceiveSlot};

    #[test]
    fn capacity_limits_rights() {
        let plan = ReceivePlan::new(vec![HostControl::Rights(vec![(), (), ()])]);
        let (plan, truncated) = plan.fit(24).unwrap();
        assert!(truncated);
        assert_eq!(plan.attachments.len(), 2);
        assert!(matches!(plan.slots.as_slice(), [ReceiveSlot::Rights(2)]));
    }

    #[test]
    fn zero_capacity_drops() {
        let plan = ReceivePlan::new(vec![HostControl::Rights(vec![()])]);
        let (plan, truncated) = plan.fit(0).unwrap();
        assert!(truncated);
        assert!(plan.attachments.is_empty());
        assert!(plan.slots.is_empty());
    }
}
