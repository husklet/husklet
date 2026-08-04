use hl_linux::{AFFINITY_BYTES, AffinityMask, Errno, GuestMarshaller, GuestMemory, LinuxResult};

use crate::RuntimeProcessSyscalls;

pub trait RuntimeYieldPort: Send + Sync {
    fn yield_task(&self, process: hl_task::ProcessId, thread: hl_task::ThreadId) -> Result<(), ()>;
}

impl<M: GuestMemory> RuntimeProcessSyscalls<M> {
    pub(crate) fn sched_setscheduler(&self, pid: i32, policy: u32, parameter: u64) -> LinuxResult {
        if parameter == 0 || pid < 0 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let mut bytes = [0_u8; 4];
        let progress = GuestMarshaller::new(&self.memory, self.architecture).copy_from(parameter, &mut bytes);
        if progress.copied != bytes.len() || progress.fault.is_some() {
            return LinuxResult::Error(Errno::EFAULT);
        }
        let priority = i32::from_le_bytes(bytes);
        let target = match self.tasks.affinity_target(self.thread, pid) {
            Ok(target) => target,
            Err(_) => return LinuxResult::Error(Errno::ESRCH),
        };
        let base = policy & !0x4000_0000;
        let range = match base {
            1 | 2 => 1..=99,
            0 | 3 | 5 | 6 => 0..=0,
            _ => return LinuxResult::Error(Errno::EINVAL),
        };
        if !range.contains(&priority) {
            return LinuxResult::Error(Errno::EINVAL);
        }
        if matches!(base, 1 | 2) {
            return LinuxResult::Error(Errno::EPERM);
        }
        let current = match self.tasks.schedule(target) {
            Ok(profile) => profile,
            Err(_) => return LinuxResult::Error(Errno::ESRCH),
        };
        let Some(profile) = hl_task::SchedulingProfile::non_realtime(base, policy & 0x4000_0000 != 0) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        let profile = profile.with_nice(i32::from(current.nice()));
        match self.tasks.set_schedule(target, profile) {
            Ok(()) => LinuxResult::Value(0),
            Err(_) => LinuxResult::Error(Errno::ESRCH),
        }
    }

    pub(crate) fn sched_setparam(&self, pid: i32, parameter: u64) -> LinuxResult {
        if parameter == 0 || pid < 0 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let mut bytes = [0_u8; 4];
        let progress = GuestMarshaller::new(&self.memory, self.architecture).copy_from(parameter, &mut bytes);
        if progress.copied != 4 || progress.fault.is_some() {
            return LinuxResult::Error(Errno::EFAULT);
        }
        let target = match self.tasks.affinity_target(self.thread, pid) {
            Ok(target) => target,
            Err(_) => return LinuxResult::Error(Errno::ESRCH),
        };
        let profile = match self.tasks.schedule(target) {
            Ok(profile) => profile,
            Err(_) => return LinuxResult::Error(Errno::ESRCH),
        };
        let Some(profile) = profile.with_priority(i32::from_le_bytes(bytes)) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        match self.tasks.set_schedule(target, profile) {
            Ok(()) => LinuxResult::Value(0),
            Err(_) => LinuxResult::Error(Errno::ESRCH),
        }
    }

    pub(crate) fn sched_getscheduler(&self, pid: i32) -> LinuxResult {
        if pid < 0 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let target = match self.tasks.affinity_target(self.thread, pid) {
            Ok(target) => target,
            Err(_) => return LinuxResult::Error(Errno::ESRCH),
        };
        match self.tasks.schedule(target) {
            Ok(profile) => LinuxResult::Value(u64::from(profile.policy())),
            Err(_) => LinuxResult::Error(Errno::ESRCH),
        }
    }

    pub(crate) fn sched_getparam(&self, pid: i32, parameter: u64) -> LinuxResult {
        if parameter == 0 || pid < 0 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let target = match self.tasks.affinity_target(self.thread, pid) {
            Ok(target) => target,
            Err(_) => return LinuxResult::Error(Errno::ESRCH),
        };
        let priority = match self.tasks.schedule(target) {
            Ok(profile) => profile.priority(),
            Err(_) => return LinuxResult::Error(Errno::ESRCH),
        };
        let progress =
            GuestMarshaller::new(&self.memory, self.architecture).copy_to(parameter, &priority.to_le_bytes());
        if progress.copied != 4 || progress.fault.is_some() {
            LinuxResult::Error(Errno::EFAULT)
        } else {
            LinuxResult::Value(0)
        }
    }

    pub(crate) fn sched_priority(&self, policy: u32, maximum: bool) -> LinuxResult {
        let value = match policy {
            1 | 2 => {
                if maximum {
                    99
                } else {
                    1
                }
            }
            0 | 3 | 5 | 6 => 0,
            _ => return LinuxResult::Error(Errno::EINVAL),
        };
        LinuxResult::Value(value)
    }

    pub(crate) fn sched_rr_interval(&self, pid: i32, output: u64) -> LinuxResult {
        if pid < 0 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        if self.tasks.affinity_target(self.thread, pid).is_err() {
            return LinuxResult::Error(Errno::ESRCH);
        }
        let mut interval = [0_u8; 16];
        interval[8..].copy_from_slice(&100_000_000_u64.to_le_bytes());
        let progress = GuestMarshaller::new(&self.memory, self.architecture).copy_to(output, &interval);
        if progress.copied != interval.len() || progress.fault.is_some() {
            LinuxResult::Error(Errno::EFAULT)
        } else {
            LinuxResult::Value(0)
        }
    }

    pub(crate) fn sched_setattr(&self, pid: i32, address: u64, flags: u64) -> LinuxResult {
        if address == 0 || flags != 0 || pid < 0 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let mut bytes = [0_u8; 48];
        let progress = GuestMarshaller::new(&self.memory, self.architecture).copy_from(address, &mut bytes);
        if progress.copied != bytes.len() || progress.fault.is_some() {
            return LinuxResult::Error(Errno::EFAULT);
        }
        let size = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        if size != 0 && size < bytes.len() as u32 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let target = match self.tasks.affinity_target(self.thread, pid) {
            Ok(target) => target,
            Err(_) => return LinuxResult::Error(Errno::ESRCH),
        };
        let policy = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        let attr_flags = u64::from_le_bytes([
            bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
        ]);
        let priority = i32::from_le_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
        let nice = i32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
        let base = policy & !0x4000_0000;
        let range = match base {
            1 | 2 => 1..=99,
            0 | 3 | 5 | 6 => 0..=0,
            _ => return LinuxResult::Error(Errno::EINVAL),
        };
        if !range.contains(&priority) {
            return LinuxResult::Error(Errno::EINVAL);
        }
        if matches!(base, 1 | 2) {
            return LinuxResult::Error(Errno::EPERM);
        }
        let reset = policy & 0x4000_0000 != 0 || attr_flags & 1 != 0;
        let Some(mut profile) = hl_task::SchedulingProfile::non_realtime(base, reset) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        if attr_flags & 0x10 == 0 {
            profile = profile.with_nice(nice);
        } else if let Ok(current) = self.tasks.schedule(target) {
            profile = profile.with_nice(i32::from(current.nice()));
        }
        match self.tasks.set_schedule(target, profile) {
            Ok(()) => LinuxResult::Value(0),
            Err(_) => LinuxResult::Error(Errno::ESRCH),
        }
    }

    pub(crate) fn sched_getattr(&self, pid: i32, address: u64, size: usize, flags: u64) -> LinuxResult {
        if pid < 0 || flags != 0 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let target = match self.tasks.affinity_target(self.thread, pid) {
            Ok(target) => target,
            Err(_) => return LinuxResult::Error(Errno::ESRCH),
        };
        if address == 0 {
            return LinuxResult::Error(Errno::EFAULT);
        }
        let copy = if size == 0 || size > 48 { 48 } else { size };
        let profile = match self.tasks.schedule(target) {
            Ok(profile) => profile,
            Err(_) => return LinuxResult::Error(Errno::ESRCH),
        };
        let mut bytes = [0_u8; 48];
        if copy >= 4 {
            bytes[0..4].copy_from_slice(&(copy as u32).to_le_bytes());
        }
        if copy >= 8 {
            bytes[4..8].copy_from_slice(&profile.policy().to_le_bytes());
        }
        if copy >= 24 {
            bytes[16..20].copy_from_slice(&i32::from(profile.nice()).to_le_bytes());
            bytes[20..24].copy_from_slice(&profile.priority().to_le_bytes());
        }
        let progress = GuestMarshaller::new(&self.memory, self.architecture).copy_to(address, &bytes[..copy]);
        if progress.copied != copy || progress.fault.is_some() {
            LinuxResult::Error(Errno::EFAULT)
        } else {
            LinuxResult::Value(0)
        }
    }

    pub(crate) fn setpriority(&self, which: u32, who: i32, nice: i32) -> LinuxResult {
        if which > 2 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        if which != 0 {
            return LinuxResult::Error(Errno::ENOSYS);
        }
        let target = match self.tasks.affinity_target(self.thread, who) {
            Ok(target) => target,
            Err(_) => return LinuxResult::Error(Errno::ESRCH),
        };
        let profile = match self.tasks.schedule(target) {
            Ok(profile) => profile.with_nice(nice),
            Err(_) => return LinuxResult::Error(Errno::ESRCH),
        };
        match self.tasks.set_schedule(target, profile) {
            Ok(()) => LinuxResult::Value(0),
            Err(_) => LinuxResult::Error(Errno::ESRCH),
        }
    }

    pub(crate) fn getpriority(&self, which: u32, who: i32) -> LinuxResult {
        if which > 2 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        if which != 0 {
            return LinuxResult::Error(Errno::ENOSYS);
        }
        let target = match self.tasks.affinity_target(self.thread, who) {
            Ok(target) => target,
            Err(_) => return LinuxResult::Error(Errno::ESRCH),
        };
        match self.tasks.schedule(target) {
            Ok(profile) => LinuxResult::Value((20_i32 - i32::from(profile.nice())) as u64),
            Err(_) => LinuxResult::Error(Errno::ESRCH),
        }
    }

    pub(crate) fn sched_setaffinity(&self, pid: i32, size: usize, address: u64) -> LinuxResult {
        let copy = size.min(AFFINITY_BYTES);
        let mut bytes = [0_u8; AFFINITY_BYTES];
        if copy != 0 {
            let progress = GuestMarshaller::new(&self.memory, self.architecture).copy_from(address, &mut bytes[..copy]);
            if progress.copied != copy || progress.fault.is_some() {
                return LinuxResult::Error(Errno::EFAULT);
            }
        }
        let target = match self.tasks.affinity_target(self.thread, pid) {
            Ok(target) => target,
            Err(_) => return LinuxResult::Error(Errno::ESRCH),
        };
        let affinity = match AffinityMask::decode(&bytes[..copy], self.tasks.topology().online()) {
            Ok(affinity) => affinity,
            Err(error) => return LinuxResult::Error(error),
        };
        match self.tasks.set_affinity(target, affinity) {
            Ok(()) => LinuxResult::Value(0),
            Err(_) => LinuxResult::Error(Errno::ESRCH),
        }
    }

    pub(crate) fn sched_getaffinity(&self, pid: i32, size: usize, address: u64) -> LinuxResult {
        let width = self.tasks.topology().online().div_ceil(64) * 8;
        if size % 8 != 0 || size < width {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let target = match self.tasks.affinity_target(self.thread, pid) {
            Ok(target) => target,
            Err(_) => return LinuxResult::Error(Errno::ESRCH),
        };
        let affinity = match self.tasks.affinity(target) {
            Ok(affinity) => affinity,
            Err(_) => return LinuxResult::Error(Errno::ESRCH),
        };
        let bytes = AffinityMask::encode(affinity);
        let copy = size.min(AFFINITY_BYTES);
        let progress = GuestMarshaller::new(&self.memory, self.architecture).copy_to(address, &bytes[..copy]);
        if progress.copied != copy || progress.fault.is_some() {
            return LinuxResult::Error(Errno::EFAULT);
        }
        LinuxResult::Value(width as u64)
    }

    pub(crate) fn sched_yield(&self) -> LinuxResult {
        let Some(scheduler) = &self.scheduler else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        match scheduler.yield_task(self.process, self.thread) {
            Ok(()) => LinuxResult::Value(0),
            Err(()) => LinuxResult::Error(Errno::EIO),
        }
    }

    pub(crate) fn getcpu(&self, processor: u64, node: u64) -> LinuxResult {
        let affinity = match self.tasks.affinity(self.thread) {
            Ok(affinity) => affinity,
            Err(_) => return LinuxResult::Error(Errno::ESRCH),
        };
        let current = u32::try_from(affinity.first()).unwrap_or(0);
        let marshaller = GuestMarshaller::new(&self.memory, self.architecture);
        if processor != 0 {
            let progress = marshaller.copy_to(processor, &current.to_le_bytes());
            if progress.copied != 4 || progress.fault.is_some() {
                return LinuxResult::Error(Errno::EFAULT);
            }
        }
        if node != 0 {
            let progress = marshaller.copy_to(node, &0_u32.to_le_bytes());
            if progress.copied != 4 || progress.fault.is_some() {
                return LinuxResult::Error(Errno::EFAULT);
            }
        }
        LinuxResult::Value(0)
    }
}
