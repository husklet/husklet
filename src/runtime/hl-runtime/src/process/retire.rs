use hl_linux::{Errno, GuestMemory, LinuxResult, ProcessAbi, SeccompKillScope};
use hl_task::{ExitStatus, ThreadId};

use crate::RuntimeProcessSyscalls;

impl<M: GuestMemory> RuntimeProcessSyscalls<M> {
    pub(crate) fn exit(&self, status: u64, group: bool) -> LinuxResult {
        if self.tasks.snapshot().init == Some(self.process) {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let status = ExitStatus::Code(ProcessAbi::new(&self.memory, self.architecture).exit(status));
        let retired = self.seccomp_threads(group);
        if let Some(result) = self.coordinated_exit(group, status, &retired) {
            return result;
        }
        if let Err(error) = self.cleanup_robust_lists(group) {
            return LinuxResult::Error(error);
        }
        self.cleanup_pi_owners(group);
        self.cleanup_clear_tid(group);
        let result = if group {
            self.tasks.exit_process(self.process, status)
        } else {
            self.tasks.exit_thread(self.thread, status)
        };
        match result {
            Ok(()) => {
                self.retire_seccomp(&retired);
                self.retire_alarm();
                LinuxResult::Value(0)
            }
            Err(_) => LinuxResult::Error(Errno::EINVAL),
        }
    }

    pub fn terminate_signal(&self, signal: u8, dumped_core: bool) -> Result<(), ()> {
        self.terminate_scope(SeccompKillScope::Process, signal, dumped_core)
    }

    pub fn terminate_seccomp(&self, scope: SeccompKillScope, signal: u8) -> Result<(), ()> {
        self.terminate_scope(scope, signal, signal == 31)
    }

    fn terminate_scope(&self, scope: SeccompKillScope, signal: u8, dumped_core: bool) -> Result<(), ()> {
        if self.tasks.snapshot().init == Some(self.process) {
            return Err(());
        }
        let group = scope == SeccompKillScope::Process;
        let status = ExitStatus::Signal { signal, dumped_core };
        let threads = self.seccomp_threads(group);
        if group && let Some(runtime) = self.exit_runtime.as_ref() {
            return match self.exit_group(runtime, status) {
                LinuxResult::Value(_) => {
                    self.retire_seccomp(&threads);
                    Ok(())
                }
                LinuxResult::Error(_) | LinuxResult::Restart(_) => Err(()),
            };
        }
        self.cleanup_robust_lists(group).map_err(|_| ())?;
        self.cleanup_pi_owners(group);
        self.cleanup_clear_tid(group);
        if group {
            self.tasks.exit_process(self.process, status).map_err(|_| ())?;
        } else {
            self.tasks.exit_thread(self.thread, status).map_err(|_| ())?;
        }
        self.retire_seccomp(&threads);
        self.retire_alarm();
        Ok(())
    }

    pub(crate) fn seccomp_threads(&self, group: bool) -> Vec<ThreadId> {
        self.tasks
            .snapshot()
            .threads
            .into_iter()
            .filter_map(|thread| {
                (thread.process == self.process && (group || thread.id == self.thread)).then_some(thread.id)
            })
            .collect()
    }

    pub(crate) fn retire_seccomp(&self, threads: &[ThreadId]) {
        if let Some(seccomp) = self.seccomp.as_ref() {
            seccomp.retire(threads);
        }
    }

    pub(crate) fn coordinated_exit(
        &self,
        group: bool,
        status: ExitStatus,
        threads: &[ThreadId],
    ) -> Option<LinuxResult> {
        let runtime = group.then_some(self.exit_runtime.as_ref()).flatten()?;
        let result = self.exit_group(runtime, status);
        if matches!(result, LinuxResult::Value(_)) {
            self.retire_seccomp(threads);
        }
        Some(result)
    }
}
