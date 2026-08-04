mod control;
mod syscalls;

pub use control::{Control, ControlError, InstallTransaction, ListenerRequest, PolicySnapshot, RestoreTransaction};
pub use syscalls::RuntimeSyscalls;

pub trait PrctlPort: Send + Sync {
    fn mode(&self) -> hl_linux::LinuxResult;
    fn strict(&self) -> hl_linux::LinuxResult;
    fn filter(&self, address: u64) -> hl_linux::LinuxResult;
    fn retire(&self, threads: &[hl_task::ThreadId]);
}

#[cfg(test)]
mod test;
