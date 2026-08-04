use crate::RuntimeProcessSyscalls;
use hl_linux::{Errno, GuestMarshaller, GuestMemory, LinuxResult, SystemInfo};

impl<M: GuestMemory> RuntimeProcessSyscalls<M> {
    pub(crate) fn sysinfo(&self, destination: u64) -> LinuxResult {
        let Some(clock) = &self.clock else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let uptime_seconds = match clock.monotonic_now() {
            Ok(now) => now.nanoseconds() / 1_000_000_000,
            Err(_) => return LinuxResult::Error(Errno::EIO),
        };
        let count = self.tasks.snapshot().processes.len().min(u16::MAX as usize) as u16;
        self.system.observe_uptime(uptime_seconds);
        let resources = self.system.snapshot();
        let (total_ram, free_ram) = resources.visible_memory();
        let encoded = SystemInfo {
            uptime_seconds,
            loads: resources.loads,
            total_ram,
            free_ram,
            processes: count,
            ..SystemInfo::default()
        }
        .encode();
        let progress = GuestMarshaller::new(&self.memory, self.architecture).copy_to(destination, &encoded);
        if progress.copied == encoded.len() && progress.fault.is_none() {
            LinuxResult::Value(0)
        } else {
            LinuxResult::Error(Errno::EFAULT)
        }
    }
}
