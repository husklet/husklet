use hl_linux::{Errno, GuestMemory, LinuxResult};
use hl_task::{PendingTarget, ProcessId, ProcessSnapshot, SignalInfo, SignalNumber};

use crate::RuntimeProcessSyscalls;

impl<M: GuestMemory> RuntimeProcessSyscalls<M> {
    pub(crate) fn kill(&self, raw_target: i32, raw_signal: u32) -> LinuxResult {
        let signal = match Self::signal(raw_signal) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        let snapshot = self.tasks.snapshot();
        let targets = Self::kill_targets(&snapshot, self.process, raw_target);
        self.deliver_processes(&snapshot.processes, targets, signal)
    }

    fn kill_targets(snapshot: &hl_task::RegistrySnapshot, sender: ProcessId, raw_target: i32) -> Vec<ProcessId> {
        if raw_target > 0 {
            return snapshot
                .processes
                .iter()
                .filter(|process| process.id.number() == raw_target as u32)
                .map(|process| process.id)
                .collect();
        }
        if raw_target == 0 {
            let group = snapshot
                .processes
                .iter()
                .find(|process| process.id == sender)
                .map(|process| process.process_group);
            return snapshot
                .processes
                .iter()
                .filter(|process| Some(process.process_group) == group)
                .map(|process| process.id)
                .collect();
        }
        if raw_target == -1 {
            return snapshot
                .processes
                .iter()
                .filter(|process| Some(process.id) != snapshot.init && process.id != sender)
                .map(|process| process.id)
                .collect();
        }
        let number = raw_target.unsigned_abs();
        snapshot
            .processes
            .iter()
            .filter(|process| process.process_group.number() == number)
            .map(|process| process.id)
            .collect()
    }

    pub(crate) fn tkill(&self, tid: i32, raw_signal: u32) -> LinuxResult {
        if tid <= 0 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        self.deliver_thread(None, tid, raw_signal)
    }

    pub(crate) fn tgkill(&self, pid: i32, tid: i32, raw_signal: u32) -> LinuxResult {
        if pid <= 0 || tid <= 0 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        self.deliver_thread(Some(pid), tid, raw_signal)
    }

    pub(crate) fn rt_sigqueueinfo(&self, pid: i32, raw_signal: u32, information: u64) -> LinuxResult {
        let queued =
            match hl_linux::SignalAbi::new(&self.memory, self.architecture).queued_info(pid, raw_signal, information) {
                Ok(value) => value,
                Err(error) => return LinuxResult::Error(error.errno()),
            };
        if queued.target <= 0 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        if queued.target as u32 != self.process.number() && (queued.code >= 0 || queued.code == -6) {
            return LinuxResult::Error(Errno::EPERM);
        }
        let snapshot = self.tasks.snapshot();
        let Some(sender) = snapshot.processes.iter().find(|process| process.id == self.process) else {
            return LinuxResult::Error(Errno::ESRCH);
        };
        let Some(target) = snapshot
            .processes
            .iter()
            .find(|process| process.id.number() == queued.target as u32)
        else {
            return LinuxResult::Error(Errno::ESRCH);
        };
        if !Self::permitted(sender, target) {
            return LinuxResult::Error(Errno::EPERM);
        }
        let Some(info) = queued.info else {
            return LinuxResult::Value(0);
        };
        match self.tasks.enqueue_signal(PendingTarget::Process(target.id), info) {
            Ok(_) => LinuxResult::Value(0),
            Err(_) => LinuxResult::Error(Errno::EAGAIN),
        }
    }

    pub(crate) fn rt_tgsigqueueinfo(&self, pid: i32, tid: i32, raw_signal: u32, information: u64) -> LinuxResult {
        let queued =
            match hl_linux::SignalAbi::new(&self.memory, self.architecture).queued_info(tid, raw_signal, information) {
                Ok(value) => value,
                Err(error) => return LinuxResult::Error(error.errno()),
            };
        if pid <= 0 || tid <= 0 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        if queued.code >= 0 || queued.code == -6 {
            return LinuxResult::Error(Errno::EPERM);
        }
        let snapshot = self.tasks.snapshot();
        let Some(thread) = snapshot.threads.iter().find(|thread| thread.id.number() == tid as u32) else {
            return LinuxResult::Error(Errno::ESRCH);
        };
        let Some(target) = snapshot.processes.iter().find(|process| process.id == thread.process) else {
            return LinuxResult::Error(Errno::ESRCH);
        };
        if target.id.number() != pid as u32 {
            return LinuxResult::Error(Errno::ESRCH);
        }
        let Some(sender) = snapshot.processes.iter().find(|process| process.id == self.process) else {
            return LinuxResult::Error(Errno::ESRCH);
        };
        if !Self::permitted(sender, target) {
            return LinuxResult::Error(Errno::EPERM);
        }
        let Some(info) = queued.info else {
            return LinuxResult::Value(0);
        };
        match self.tasks.enqueue_signal(PendingTarget::Thread(thread.id), info) {
            Ok(_) => LinuxResult::Value(0),
            Err(_) => LinuxResult::Error(Errno::EAGAIN),
        }
    }

    pub(crate) fn deliver_processes(
        &self,
        snapshots: &[ProcessSnapshot],
        targets: Vec<ProcessId>,
        signal: Option<SignalNumber>,
    ) -> LinuxResult {
        if targets.is_empty() {
            return LinuxResult::Error(Errno::ESRCH);
        }
        let Some(sender) = snapshots.iter().find(|process| process.id == self.process) else {
            return LinuxResult::Error(Errno::ESRCH)
        };
        let mut permitted = 0;
        for target in targets {
            match self.deliver_process_signal(snapshots, sender, target, signal) {
                Ok(true) => permitted += 1,
                Ok(false) => {}
                Err(error) => return LinuxResult::Error(error),
            }
        }
        if permitted == 0 {
            LinuxResult::Error(Errno::EPERM)
        } else {
            LinuxResult::Value(0)
        }
    }

    fn deliver_process_signal(
        &self,
        snapshots: &[ProcessSnapshot],
        sender: &ProcessSnapshot,
        target: ProcessId,
        signal: Option<SignalNumber>,
    ) -> Result<bool, Errno> {
        let Some(snapshot) = snapshots.iter().find(|process| process.id == target) else {
            return Ok(false);
        };
        if !Self::permitted(sender, snapshot) {
            return Ok(false);
        }
        if let Some(signal) = signal {
            self.tasks
                .enqueue_signal(PendingTarget::Process(target), Self::info(signal, sender, 0))
                .map_err(|_| Errno::EAGAIN)?;
        }
        Ok(true)
    }

    fn deliver_thread(&self, expected_process: Option<i32>, tid: i32, raw_signal: u32) -> LinuxResult {
        let signal = match Self::signal(raw_signal) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        let Ok(Some(target)) = self.tasks.signal_thread_target(self.process, tid as u32) else {
            return LinuxResult::Error(Errno::ESRCH)
        };
        if expected_process.is_some_and(|pid| target.process.number() != pid as u32) {
            return LinuxResult::Error(Errno::ESRCH);
        }
        if !Self::credentials_permitted(&target.sender_credentials, &target.target_credentials) {
            return LinuxResult::Error(Errno::EPERM);
        }
        let Some(signal) = signal else {
            return LinuxResult::Value(0);
        };
        match self.tasks.enqueue_signal(
            PendingTarget::Thread(target.thread),
            Self::info_from_credentials(signal, &target.sender_credentials, self.process, -6),
        ) {
            Ok(_) => LinuxResult::Value(0),
            Err(_) => LinuxResult::Error(Errno::EAGAIN),
        }
    }

    pub(crate) fn signal(raw: u32) -> Result<Option<SignalNumber>, Errno> {
        if raw == 0 {
            return Ok(None);
        }
        let number = u8::try_from(raw).map_err(|_| Errno::EINVAL)?;
        SignalNumber::new(number).map(Some).map_err(|_| Errno::EINVAL)
    }

    pub(crate) fn permitted(sender: &ProcessSnapshot, target: &ProcessSnapshot) -> bool {
        Self::credentials_permitted(&sender.credentials, &target.credentials)
    }

    fn credentials_permitted(source: &hl_task::ProcessCredentials, destination: &hl_task::ProcessCredentials) -> bool {
        source.has_capability(hl_task::CapabilitySets::KILL)
            || source.real_user == destination.real_user
            || source.real_user == destination.saved_user
            || source.effective_user == destination.real_user
            || source.effective_user == destination.saved_user
    }

    fn info_from_credentials(
        signal: SignalNumber,
        sender: &hl_task::ProcessCredentials,
        sender_id: ProcessId,
        code: i32,
    ) -> SignalInfo {
        SignalInfo {
            signal,
            code,
            sender_process: sender_id.number(),
            sender_user: sender.real_user,
            ..SignalInfo::bare(signal)
        }
    }

    pub(crate) fn info(signal: SignalNumber, sender: &ProcessSnapshot, code: i32) -> SignalInfo {
        SignalInfo {
            signal,
            code,
            sender_process: sender.id.number(),
            sender_user: sender.credentials.real_user,
            ..SignalInfo::bare(signal)
        }
    }
}
