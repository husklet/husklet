use super::TaskRegistry;
use crate::{CpuAffinity, TaskError, ThreadId};

impl TaskRegistry {
    /// Returns a thread's explicit mask, or the instance online set when it has
    /// never been constrained.
    pub fn affinity(&self, thread: ThreadId) -> Result<CpuAffinity, TaskError> {
        let state = self.lock();
        Ok(Self::thread(&state, thread)?
            .affinity
            .unwrap_or_else(|| self.topology.affinity()))
    }

    /// Replaces one live thread's mask after Linux ABI intersection.
    pub fn set_affinity(&self, thread: ThreadId, affinity: CpuAffinity) -> Result<(), TaskError> {
        if !affinity.is_subset(self.topology.affinity()) {
            return Err(TaskError::InvalidPlan);
        }
        let mut state = self.lock();
        Self::thread_mut(&mut state, thread)?.affinity = Some(affinity);
        Ok(())
    }

    /// Resolves zero as the caller, a TID as that thread, and a PID as its
    /// thread-group leader without consulting unrelated host processes.
    pub fn affinity_target(&self, caller: ThreadId, number: i32) -> Result<ThreadId, TaskError> {
        let state = self.lock();
        if number == 0 {
            Self::thread(&state, caller)?;
            return Ok(caller);
        }
        let number = u32::try_from(number).map_err(|_| TaskError::InvalidThread)?;
        if let Some((slot, entry)) = state
            .threads
            .iter()
            .enumerate()
            .find(|(slot, entry)| entry.value.is_some() && *slot as u32 + 1 == number)
        {
            return Ok(ThreadId::new(slot as u32, entry.generation));
        }
        state
            .processes
            .iter()
            .enumerate()
            .find(|(slot, entry)| entry.value.is_some() && *slot as u32 + 1 == number)
            .and_then(|(_, entry)| entry.value.as_ref().map(|process| process.leader))
            .ok_or(TaskError::InvalidThread)
    }
}
