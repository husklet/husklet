use std::sync::atomic::Ordering;

use hl_isa::AddressRange;

use super::host::Coordinator;
use super::port::Host;

impl<H: Host> Coordinator<H> {
    #[must_use]
    pub fn observer_epoch(&self) -> u64 {
        self.host.epoch.load(Ordering::Acquire)
    }

    /// Changes when bytes in a logically executable mapping are committed.
    #[must_use]
    pub fn instruction_epoch(&self) -> u64 {
        self.host.executable.generation()
    }

    /// Returns stable evidence for the executable pages covered by `range`.
    #[must_use]
    pub fn executable_token(&self, range: AddressRange, incarnation: u64) -> crate::ExecutableToken {
        self.host.executable.token(range, incarnation)
    }

    /// Publishes completed guest instruction-cache maintenance to every executor.
    pub fn publish_instruction(&self) {
        self.host.executable.publish_all();
    }
}
