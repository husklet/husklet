use hl_linux::{Errno, GuestAccess, GuestMarshaller, GuestMemory, LinuxResult};
use hl_task::{CapabilitySets, ProcessCredentials};

use crate::RuntimeProcessSyscalls;

const VERSION_1: u32 = 0x1998_0330;
const VERSION_2: u32 = 0x2007_1026;
const VERSION_3: u32 = 0x2008_0522;

impl<M: GuestMemory> RuntimeProcessSyscalls<M> {
    pub(crate) fn capget(&self, header: u64, data: u64) -> LinuxResult {
        let (version, pid) = match self.cap_header(header) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        let words = match Self::cap_words(version) {
            Some(value) => value,
            None => return self.cap_version(header),
        };
        let credentials = match self.cap_target(pid) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        let bytes = Self::cap_encode(credentials.capabilities, words);
        let marshaller = GuestMarshaller::new(&self.memory, self.architecture);
        if marshaller.probe(data, bytes.len(), GuestAccess::Write).is_err() {
            return LinuxResult::Error(Errno::EFAULT);
        }
        let progress = marshaller.copy_to(data, &bytes);
        if progress.fault.is_some() {
            LinuxResult::Error(Errno::EFAULT)
        } else {
            LinuxResult::Value(0)
        }
    }

    pub(crate) fn capset(&self, header: u64, data: u64) -> LinuxResult {
        let (version, pid) = match self.cap_header(header) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        let words = match Self::cap_words(version) {
            Some(value) => value,
            None => return self.cap_version(header),
        };
        if pid != 0 && pid as u32 != self.thread.number() {
            return LinuxResult::Error(Errno::EPERM);
        }
        let requested = match self.cap_decode(data, words) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        let mut credentials = match self.snapshot() {
            Ok(value) => value.credentials,
            Err(error) => return LinuxResult::Error(error),
        };
        let current = credentials.capabilities;
        let mut requested = requested;
        requested.ambient = current.ambient & requested.permitted & requested.inheritable;
        let valid = requested.effective & !requested.permitted == 0
            && requested.permitted & !current.permitted == 0
            && requested.inheritable & !(current.inheritable | current.permitted) == 0
            && (requested.effective | requested.permitted | requested.inheritable) & !CapabilitySets::SUPPORTED == 0;
        if !valid {
            return LinuxResult::Error(Errno::EPERM);
        }
        credentials.capabilities = requested;
        credentials.raise_setid();
        self.replace_credentials(credentials)
    }

    fn cap_header(&self, address: u64) -> Result<(u32, i32), Errno> {
        let mut bytes = [0; 8];
        let progress = GuestMarshaller::new(&self.memory, self.architecture).copy_from(address, &mut bytes);
        if progress.fault.is_some() {
            return Err(Errno::EFAULT);
        }
        Ok((
            u32::from_le_bytes(bytes[..4].try_into().unwrap()),
            i32::from_le_bytes(bytes[4..].try_into().unwrap()),
        ))
    }

    fn cap_target(&self, pid: i32) -> Result<ProcessCredentials, Errno> {
        if pid < 0 {
            return Err(Errno::EINVAL);
        }
        let snapshot = self.tasks.snapshot();
        let process = Self::cap_process(&snapshot.processes, pid, self.process)?;
        snapshot
            .processes
            .into_iter()
            .find(|candidate| candidate.id == process)
            .map(|candidate| candidate.credentials)
            .ok_or(Errno::ESRCH)
    }

    fn cap_process(
        processes: &[hl_task::ProcessSnapshot],
        pid: i32,
        current: hl_task::ProcessId,
    ) -> Result<hl_task::ProcessId, Errno> {
        if pid == 0 {
            return Ok(current);
        }
        for process in processes {
            if Self::cap_thread(process, pid as u32) {
                return Ok(process.id);
            }
        }
        Err(Errno::ESRCH)
    }

    fn cap_thread(process: &hl_task::ProcessSnapshot, pid: u32) -> bool {
        process.threads.iter().any(|thread| thread.number() == pid)
    }

    fn cap_version(&self, header: u64) -> LinuxResult {
        let marshaller = GuestMarshaller::new(&self.memory, self.architecture);
        if marshaller.probe(header, 4, GuestAccess::Write).is_err()
            || marshaller.copy_to(header, &VERSION_3.to_le_bytes()).fault.is_some()
        {
            LinuxResult::Error(Errno::EFAULT)
        } else {
            LinuxResult::Error(Errno::EINVAL)
        }
    }

    fn cap_words(version: u32) -> Option<usize> {
        match version {
            VERSION_1 => Some(1),
            VERSION_2 | VERSION_3 => Some(2),
            _ => None,
        }
    }

    fn cap_encode(sets: CapabilitySets, words: usize) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(words * 12);
        for word in 0..words {
            let shift = word * 32;
            for value in [sets.effective, sets.permitted, sets.inheritable] {
                bytes.extend_from_slice(&((value >> shift) as u32).to_le_bytes());
            }
        }
        bytes
    }

    fn cap_decode(&self, address: u64, words: usize) -> Result<CapabilitySets, Errno> {
        let mut bytes = vec![0; words * 12];
        if GuestMarshaller::new(&self.memory, self.architecture)
            .copy_from(address, &mut bytes)
            .fault
            .is_some()
        {
            return Err(Errno::EFAULT);
        }
        let mut values = [0_u64; 3];
        for word in 0..words {
            for field in 0..3 {
                let offset = (word * 3 + field) * 4;
                values[field] |=
                    u64::from(u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())) << (word * 32);
            }
        }
        Ok(CapabilitySets {
            effective: values[0],
            permitted: values[1],
            inheritable: values[2],
            ambient: 0,
        })
    }
}
