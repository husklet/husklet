use hl_descriptor::{DescriptorFlags, ObjectError, ObjectKind, StatusFlags};
use hl_linux::{Errno, GuestMarshaller, GuestMemory, LinuxResult};

use crate::{filesystem::errno::FileErrno, filesystem::syscalls::RuntimeFilesystemSyscalls};

const FIONREAD: u32 = 0x541b;
const SIOCOUTQ: u32 = 0x5411;
const SIOCATMARK: u32 = 0x8905;
const FIONBIO: u32 = 0x5421;
const FIOCLEX: u32 = 0x5451;
const FIONCLEX: u32 = 0x5450;
const TCGETS: u32 = 0x5401;
const TCSETS: u32 = 0x5402;
const TCSETSW: u32 = 0x5403;
const TCSETSF: u32 = 0x5404;
const TIOCSCTTY: u32 = 0x540e;
const TIOCNOTTY: u32 = 0x5422;
const TIOCGSID: u32 = 0x5429;
const TIOCGPGRP: u32 = 0x540f;
const TIOCSPGRP: u32 = 0x5410;
const TIOCPKT: u32 = 0x5420;
const TIOCGPTPEER: u32 = 0x5441;
const TIOCGPTN: u32 = 0x8004_5430;
const TIOCSPTLCK: u32 = 0x4004_5431;
const TCSBRK: u32 = 0x5409;
const TCXONC: u32 = 0x540a;
const TCFLSH: u32 = 0x540b;
const TIOCGWINSZ: u32 = 0x5413;
const TIOCSWINSZ: u32 = 0x5414;
const TCSBRKP: u32 = 0x5425;
const TCGETS2: u32 = 0x802c_542a;
const TCSETS2: u32 = 0x402c_542b;
const TCSETSW2: u32 = 0x402c_542c;
const TCSETSF2: u32 = 0x402c_542d;
const SIOCGIFNAME: u32 = 0x8910;
const SIOCGIFCONF: u32 = 0x8912;
const SIOCGIFFLAGS: u32 = 0x8913;
const SIOCGIFADDR: u32 = 0x8915;
const SIOCGIFBRDADDR: u32 = 0x8919;
const SIOCGIFNETMASK: u32 = 0x891b;
const SIOCGIFMTU: u32 = 0x8921;
const SIOCGIFHWADDR: u32 = 0x8927;
const SIOCGIFINDEX: u32 = 0x8933;

impl<M: GuestMemory> RuntimeFilesystemSyscalls<M> {
    fn terminal_process(&self) -> Option<(hl_task::SessionId, bool, bool)> {
        let (tasks, process) = self.terminal_tasks.as_ref()?;
        let session = tasks.session_id(*process).ok()?;
        let attached = tasks.terminal_session(*process).ok()?.is_some();
        let leader = tasks
            .snapshot()
            .sessions
            .iter()
            .any(|entry| entry.id == session && entry.leader == *process);
        Some((session, leader, attached))
    }

    pub(super) fn ioctl(&self, descriptor: i32, request: u32, argument: u64) -> LinuxResult {
        let lease = match self.descriptors.pin(descriptor) {
            Ok(lease) => lease,
            Err(error) => return LinuxResult::Error(FileErrno::descriptor(error)),
        };
        if let Some(result) = self.terminal_ioctl(&lease, request, argument) {
            return result;
        }
        if matches!(
            request,
            SIOCGIFNAME
                | SIOCGIFCONF
                | SIOCGIFFLAGS
                | SIOCGIFADDR
                | SIOCGIFBRDADDR
                | SIOCGIFNETMASK
                | SIOCGIFMTU
                | SIOCGIFHWADDR
                | SIOCGIFINDEX
        ) {
            return self.interface_ioctl(lease.description_identity(), request, argument);
        }
        match request {
            FIONBIO => {
                let Some(enabled) = self.ioctl_int(argument) else {
                    return LinuxResult::Error(Errno::EFAULT);
                };
                let mut bits = lease.status().bits();
                if enabled == 0 {
                    bits &= !StatusFlags::NONBLOCKING;
                } else {
                    bits |= StatusFlags::NONBLOCKING;
                }
                match lease.set_status(StatusFlags::from_bits(bits)) {
                    Ok(()) => LinuxResult::Value(0),
                    Err(error) => LinuxResult::Error(FileErrno::object(error)),
                }
            }
            FIONREAD => {
                if lease.object().kind() == ObjectKind::Socket {
                    let Some(ioctl) = &self.socket_ioctl else {
                        return LinuxResult::Error(Errno::ENOTTY);
                    };
                    return match ioctl.input_queue(lease.description_identity()) {
                        Ok(Some(pending)) => self.ioctl_write_int(argument, pending.min(i32::MAX as u64) as i32),
                        Ok(None) => LinuxResult::Error(Errno::ENOTTY),
                        Err(()) => LinuxResult::Error(Errno::EIO),
                    };
                }
                if lease.object().kind() == ObjectKind::Pipe {
                    return match lease.probe_read(i32::MAX as usize) {
                        Ok(Some(pending)) => self.ioctl_write_int(argument, pending as i32),
                        Ok(None) | Err(ObjectError::NotSupported) => LinuxResult::Error(Errno::ENOTTY),
                        Err(error) => LinuxResult::Error(FileErrno::object(error)),
                    };
                }
                if lease.object().kind() != ObjectKind::File {
                    return LinuxResult::Error(Errno::ENOTTY);
                }
                let metadata = match lease.metadata() {
                    Ok(metadata) => metadata,
                    Err(ObjectError::NotSupported) => {
                        return LinuxResult::Error(Errno::ENOTTY);
                    }
                    Err(error) => return LinuxResult::Error(FileErrno::object(error)),
                };
                let offset = match lease.seek(hl_descriptor::SeekPosition::Current(0)) {
                    Ok(offset) => offset,
                    Err(ObjectError::NotSupported) => lease.offset(),
                    Err(error) => return LinuxResult::Error(FileErrno::object(error)),
                };
                let pending = metadata.size.saturating_sub(offset);
                let encoded = pending.min(i32::MAX as u64) as i32;
                self.ioctl_write_int(argument, encoded)
            }
            SIOCOUTQ => {
                let Some(ioctl) = &self.socket_ioctl else {
                    return LinuxResult::Error(Errno::ENOTTY);
                };
                match ioctl.output_queue(lease.description_identity()) {
                    Ok(Some(pending)) => self.ioctl_write_int(argument, pending.min(i32::MAX as u64) as i32),
                    Ok(None) => LinuxResult::Error(Errno::ENOTTY),
                    Err(()) => LinuxResult::Error(Errno::EIO),
                }
            }
            SIOCATMARK => {
                let Some(ioctl) = &self.socket_ioctl else {
                    return LinuxResult::Error(Errno::ENOTTY);
                };
                match ioctl.at_urgent_mark(lease.description_identity()) {
                    Ok(Some(marked)) => self.ioctl_write_int(argument, i32::from(marked)),
                    Ok(None) => LinuxResult::Error(Errno::ENOTTY),
                    Err(()) => LinuxResult::Error(Errno::EINVAL),
                }
            }
            FIOCLEX | FIONCLEX => {
                let flags = if request == FIOCLEX {
                    DescriptorFlags::from_bits(DescriptorFlags::CLOSE_ON_EXEC)
                } else {
                    DescriptorFlags::default()
                };
                match self.descriptors.set_flags(descriptor, flags) {
                    Ok(()) => LinuxResult::Value(0),
                    Err(error) => LinuxResult::Error(FileErrno::descriptor(error)),
                }
            }
            _ => LinuxResult::Error(Errno::ENOTTY),
        }
    }

    fn interface_ioctl(
        &self,
        identity: hl_descriptor::DescriptionIdentity,
        request: u32,
        argument: u64,
    ) -> LinuxResult {
        let Some(port) = &self.socket_ioctl else {
            return LinuxResult::Error(Errno::ENOTTY);
        };
        let interfaces = match port.interfaces(identity) {
            Ok(Some(value)) => value,
            Ok(None) => return LinuxResult::Error(Errno::ENOTTY),
            Err(()) => return LinuxResult::Error(Errno::EIO),
        };
        if request == SIOCGIFCONF {
            return self.interface_list(argument, &interfaces);
        }
        let Some(mut bytes) = self.ioctl_read(argument, 40) else {
            return LinuxResult::Error(Errno::EFAULT);
        };
        let interface = if request == SIOCGIFNAME {
            let index = u32::from_le_bytes(bytes[16..20].try_into().unwrap());
            interfaces.iter().find(|interface| interface.index == index)
        } else {
            let end = bytes[..16].iter().position(|byte| *byte == 0).unwrap_or(16);
            interfaces.iter().find(|interface| interface.name == bytes[..end])
        };
        let Some(interface) = interface else {
            return LinuxResult::Error(Errno::ENODEV);
        };
        match request {
            SIOCGIFNAME => Self::write_name(&mut bytes, &interface.name),
            SIOCGIFFLAGS => bytes[16..18].copy_from_slice(&interface.flags.to_le_bytes()),
            SIOCGIFADDR => Self::write_ipv4(&mut bytes, interface.ipv4),
            SIOCGIFBRDADDR => Self::write_ipv4(&mut bytes, Self::broadcast(interface.ipv4, interface.prefix)),
            SIOCGIFNETMASK => Self::write_ipv4(&mut bytes, Self::mask(interface.prefix)),
            SIOCGIFMTU => bytes[16..20].copy_from_slice(&interface.mtu.to_le_bytes()),
            SIOCGIFINDEX => bytes[16..20].copy_from_slice(&interface.index.to_le_bytes()),
            SIOCGIFHWADDR => {
                bytes[16..18].copy_from_slice(&interface.hardware_type.to_le_bytes());
                bytes[18..24].copy_from_slice(&interface.mac);
            }
            _ => unreachable!(),
        }
        self.ioctl_write(argument, &bytes)
    }

    fn interface_list(&self, argument: u64, interfaces: &[hl_network::NamespaceInterface]) -> LinuxResult {
        let Some(mut header) = self.ioctl_read(argument, 16) else {
            return LinuxResult::Error(Errno::EFAULT);
        };
        let capacity = i32::from_le_bytes(header[..4].try_into().unwrap()).max(0) as usize;
        let pointer = u64::from_le_bytes(header[8..16].try_into().unwrap());
        let available = interfaces.len() * 40;
        let copied = if pointer == 0 {
            available
        } else {
            capacity.min(available) / 40 * 40
        };
        if pointer != 0 {
            let mut records = vec![0; copied];
            for (slot, interface) in interfaces.iter().take(copied / 40).enumerate() {
                let record = &mut records[slot * 40..slot * 40 + 40];
                Self::write_name(record, &interface.name);
                Self::write_ipv4(record, interface.ipv4);
            }
            if !matches!(self.ioctl_write(pointer, &records), LinuxResult::Value(0)) {
                return LinuxResult::Error(Errno::EFAULT);
            }
        }
        header[..4].copy_from_slice(&(copied as i32).to_le_bytes());
        self.ioctl_write(argument, &header)
    }

    fn write_name(output: &mut [u8], name: &[u8]) {
        output[..16].fill(0);
        let count = name.len().min(15);
        output[..count].copy_from_slice(&name[..count]);
    }

    fn write_ipv4(output: &mut [u8], address: [u8; 4]) {
        output[16..32].fill(0);
        output[16..18].copy_from_slice(&2_u16.to_le_bytes());
        output[20..24].copy_from_slice(&address);
    }

    fn mask(prefix: u8) -> [u8; 4] {
        u32::MAX.checked_shl(u32::from(32 - prefix)).unwrap_or(0).to_be_bytes()
    }

    fn broadcast(address: [u8; 4], prefix: u8) -> [u8; 4] {
        let address = u32::from_be_bytes(address);
        let mask = u32::from_be_bytes(Self::mask(prefix));
        (address | !mask).to_be_bytes()
    }

    fn terminal_ioctl(
        &self,
        lease: &hl_descriptor::OperationLease,
        request: u32,
        argument: u64,
    ) -> Option<LinuxResult> {
        let bindings = self.terminals.as_ref()?;
        let terminal = bindings.get(lease.description_identity())?;
        if matches!(
            request,
            TCSETS | TCSETSW | TCSETSF | TCSETS2 | TCSETSW2 | TCSETSF2 | TIOCSPGRP | TCFLSH | TCXONC | TCSBRK | TCSBRKP
        ) && let Some(result) = self.terminal_access(lease, super::job::TerminalAccess::Control)
        {
            return Some(result);
        }
        Some(match request {
            TCGETS | TCGETS2 => self.ioctl_write(argument, &terminal.pair.settings().encode(request == TCGETS2)),
            TCSETS | TCSETSW | TCSETSF | TCSETS2 | TCSETSW2 | TCSETSF2 => {
                self.configure_terminal(&terminal, request, argument)
            }
            TIOCGPTN if terminal.endpoint == hl_terminal::Endpoint::Master => {
                self.ioctl_write_int(argument, i32::from(terminal.pair.id().index))
            }
            TIOCSPTLCK if terminal.endpoint == hl_terminal::Endpoint::Master => LinuxResult::Value(0),
            TIOCGPTPEER if terminal.endpoint == hl_terminal::Endpoint::Master => {
                self.terminal_peer(&terminal, argument)
            }
            TIOCPKT if terminal.endpoint == hl_terminal::Endpoint::Master => {
                let Some(enabled) = self.ioctl_int(argument) else {
                    return Some(LinuxResult::Error(Errno::EFAULT));
                };
                match terminal.pair.set_packet_mode(enabled != 0) {
                    Ok(()) => LinuxResult::Value(0),
                    Err(_) => LinuxResult::Error(Errno::EIO),
                }
            }
            TIOCSPGRP => self.terminal_foreground(&terminal, argument),
            TIOCGPGRP => match self.terminal_process() {
                Some((session, _, true)) if terminal.controlling_session() == Some(session.number()) => {
                    match terminal.pair.foreground() {
                        Some(group) => self.ioctl_write_int(argument, group.number as i32),
                        None => LinuxResult::Error(Errno::ENOTTY),
                    }
                }
                _ => LinuxResult::Error(Errno::ENOTTY),
            },
            TIOCSCTTY => match self.terminal_process() {
                Some((session, true, _)) => match terminal.acquire_controlling_changed(session.number()) {
                    Ok(created) => match &self.terminal_tasks {
                        Some((tasks, process)) if tasks.attach_terminal(*process, session).is_ok() => {
                            LinuxResult::Value(0)
                        }
                        _ => {
                            if created {
                                let _ = terminal.detach_controlling(session.number());
                            }
                            LinuxResult::Error(Errno::EPERM)
                        }
                    },
                    Err(_) => LinuxResult::Error(Errno::EPERM),
                },
                _ => LinuxResult::Error(Errno::EPERM),
            },
            TIOCNOTTY => {
                if terminal.endpoint != hl_terminal::Endpoint::Slave {
                    return Some(LinuxResult::Error(Errno::ENOTTY));
                }
                let Some((tasks, process)) = &self.terminal_tasks else {
                    return Some(LinuxResult::Error(Errno::ENOTTY));
                };
                let Ok(prepared) = tasks.prepare_terminal_transition(*process, hl_task::TerminalTransition::Detach)
                else {
                    return Some(LinuxResult::Error(Errno::ENOTTY));
                };
                let effects = prepared.effects();
                if terminal.controlling_session() != Some(effects.session.number()) {
                    return Some(LinuxResult::Error(Errno::ENOTTY));
                }
                let foreground = terminal.pair.foreground().and_then(|group| {
                    group
                        .number
                        .checked_sub(1)
                        .and_then(|slot| hl_task::ProcessGroupId::from_wire(slot, group.generation))
                });
                let prepared = prepared.target_foreground(foreground);
                let detached = if effects.session_wide {
                    terminal.detach_controlling(effects.session.number())
                } else {
                    Ok(())
                };
                finish_terminal_detach(Some(prepared), detached)
            }
            TIOCGSID => match terminal.controlling_session() {
                Some(session) => self.ioctl_write_int(argument, session as i32),
                None => LinuxResult::Error(Errno::ENOTTY),
            },
            FIONREAD => self.ioctl_write_int(
                argument,
                terminal.pair.pending(terminal.endpoint).min(i32::MAX as usize) as i32,
            ),
            TIOCGWINSZ => {
                let window = terminal.pair.window();
                let mut bytes = Vec::with_capacity(8);
                for value in [window.rows, window.columns, window.pixel_width, window.pixel_height] {
                    bytes.extend_from_slice(&value.to_le_bytes());
                }
                self.ioctl_write(argument, &bytes)
            }
            TIOCSWINSZ => {
                let Some(bytes) = self.ioctl_read(argument, 8) else {
                    return Some(LinuxResult::Error(Errno::EFAULT));
                };
                let half = |offset: usize| u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
                match terminal.pair.set_window(hl_terminal::Window {
                    rows: half(0),
                    columns: half(2),
                    pixel_width: half(4),
                    pixel_height: half(6),
                }) {
                    Ok(changed) => {
                        if changed
                            && let Some((tasks, _)) = &self.terminal_tasks
                            && let (Some(session), Some(group)) =
                                (terminal.controlling_session(), terminal.pair.foreground())
                            && let Some(slot) = group.number.checked_sub(1)
                            && let Some(group) = hl_task::ProcessGroupId::from_wire(slot, group.generation)
                        {
                            let _ = tasks.terminal_window_changed(session, group);
                        }
                        LinuxResult::Value(0)
                    }
                    Err(_) => LinuxResult::Error(Errno::EIO),
                }
            }
            TCFLSH if argument <= 2 => match terminal.pair.flush(argument != 1, argument != 0) {
                Ok(()) => LinuxResult::Value(0),
                Err(_) => LinuxResult::Error(Errno::EIO),
            },
            TCFLSH => LinuxResult::Error(Errno::EINVAL),
            TCXONC if argument <= 3 => LinuxResult::Value(0),
            TCXONC => LinuxResult::Error(Errno::EINVAL),
            TCSBRK | TCSBRKP => LinuxResult::Value(0),
            _ => return None,
        })
    }

    fn configure_terminal(&self, terminal: &hl_terminal::Handle, request: u32, argument: u64) -> LinuxResult {
        let length = if matches!(request, TCSETS2 | TCSETSW2 | TCSETSF2) {
            44
        } else {
            36
        };
        let Some(bytes) = self.ioctl_read(argument, length) else {
            return LinuxResult::Error(Errno::EFAULT);
        };
        let Ok(settings) = hl_terminal::Settings::decode(&bytes) else {
            return LinuxResult::Error(Errno::EIO);
        };
        match terminal.pair.configure(settings, matches!(request, TCSETSF | TCSETSF2)) {
            Ok(()) => LinuxResult::Value(0),
            Err(_) => LinuxResult::Error(Errno::EIO),
        }
    }

    fn terminal_foreground(&self, terminal: &hl_terminal::Handle, argument: u64) -> LinuxResult {
        let Some(group) = self.ioctl_int(argument) else {
            return LinuxResult::Error(Errno::EFAULT);
        };
        if group <= 0 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let Some((session, _, true)) = self.terminal_process() else {
            return LinuxResult::Error(Errno::ENOTTY);
        };
        if terminal.controlling_session() != Some(session.number()) {
            return LinuxResult::Error(Errno::ENOTTY);
        }
        let Some((tasks, process)) = &self.terminal_tasks else {
            return LinuxResult::Error(Errno::ENOTTY);
        };
        let snapshot = tasks.snapshot();
        let Some(group_id) = snapshot
            .process_groups
            .iter()
            .find(|candidate| candidate.id.number() == group as u32)
            .map(|candidate| candidate.id)
        else {
            return LinuxResult::Error(Errno::ESRCH);
        };
        if tasks.set_foreground_group(*process, group_id).is_err() {
            return LinuxResult::Error(Errno::EPERM);
        }
        let (_, generation) = group_id.wire_parts();
        match terminal.pair.set_foreground(hl_terminal::ForegroundGroup {
            number: group as u32,
            generation,
        }) {
            Ok(()) => LinuxResult::Value(0),
            Err(_) => LinuxResult::Error(Errno::EIO),
        }
    }

    fn terminal_peer(&self, terminal: &hl_terminal::Handle, flags: u64) -> LinuxResult {
        const ACCESS_MASK: u64 = 3;
        const NO_CTTY: u64 = 0o0000_0400;
        const CLOSE_EXEC: u64 = 0o0020_0000;
        if flags & !(ACCESS_MASK | NO_CTTY | CLOSE_EXEC) != 0 || flags & ACCESS_MASK == 3 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let Ok(object) = terminal.slave() else {
            return LinuxResult::Error(Errno::EIO);
        };
        let status = StatusFlags::from_bits(flags as u32 & ACCESS_MASK as u32);
        let descriptor_flags = if flags & CLOSE_EXEC != 0 {
            DescriptorFlags::from_bits(DescriptorFlags::CLOSE_ON_EXEC)
        } else {
            DescriptorFlags::default()
        };
        let install = match self
            .descriptors
            .prepare_open(0, object.clone(), status, descriptor_flags)
        {
            Ok(install) => install,
            Err(error) => return LinuxResult::Error(FileErrno::descriptor(error)),
        };
        let Some(bindings) = &self.terminals else {
            return LinuxResult::Error(Errno::EIO);
        };
        object.bind(install.description_identity(), bindings);
        LinuxResult::Value(install.publish() as u64)
    }

    fn ioctl_read(&self, address: u64, length: usize) -> Option<Vec<u8>> {
        let mut bytes = vec![0_u8; length];
        let copied = GuestMarshaller::new(&self.memory, self.architecture).copy_from(address, &mut bytes);
        (copied.copied == length && copied.fault.is_none()).then_some(bytes)
    }

    fn ioctl_write(&self, address: u64, bytes: &[u8]) -> LinuxResult {
        let copied = GuestMarshaller::new(&self.memory, self.architecture).copy_to(address, bytes);
        if copied.copied == bytes.len() && copied.fault.is_none() {
            LinuxResult::Value(0)
        } else {
            LinuxResult::Error(Errno::EFAULT)
        }
    }

    fn ioctl_int(&self, address: u64) -> Option<i32> {
        let mut bytes = [0_u8; 4];
        let copied = GuestMarshaller::new(&self.memory, self.architecture).copy_from(address, &mut bytes);
        (copied.copied == bytes.len() && copied.fault.is_none()).then(|| i32::from_le_bytes(bytes))
    }

    fn ioctl_write_int(&self, address: u64, value: i32) -> LinuxResult {
        let bytes = value.to_le_bytes();
        let copied = GuestMarshaller::new(&self.memory, self.architecture).copy_to(address, &bytes);
        if copied.copied == bytes.len() && copied.fault.is_none() {
            LinuxResult::Value(0)
        } else {
            LinuxResult::Error(Errno::EFAULT)
        }
    }
}

pub(super) fn finish_terminal_detach(
    prepared: Option<hl_task::PreparedTerminalTransition<'_>>,
    detached: Result<(), hl_terminal::CatalogError>,
) -> LinuxResult {
    match detached {
        Ok(()) => {
            if let Some(prepared) = prepared {
                let effects = prepared.effects();
                let committed = prepared.commit();
                debug_assert_eq!(committed, effects);
            }
            LinuxResult::Value(0)
        }
        Err(_) => LinuxResult::Error(Errno::ENOTTY),
    }
}
