//! SIOCGIF* network interface ioctls for the filesystem syscall surface.

use hl_linux::{Errno, GuestMemory, LinuxResult};

use crate::filesystem::syscalls::RuntimeFilesystemSyscalls;

use super::{
    SIOCGIFADDR, SIOCGIFBRDADDR, SIOCGIFCONF, SIOCGIFFLAGS, SIOCGIFHWADDR, SIOCGIFINDEX, SIOCGIFMTU, SIOCGIFNAME,
    SIOCGIFNETMASK,
};

impl<M: GuestMemory> RuntimeFilesystemSyscalls<M> {
    pub(super) fn interface_ioctl(
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
}
