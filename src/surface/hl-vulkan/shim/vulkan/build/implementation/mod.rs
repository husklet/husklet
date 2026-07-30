//! The generator's classification of every hand-written `vk*` body.
//!
//! Three disjoint classes, kept apart on purpose. `lowered` + `runtime` names PERFORM the command;
//! `refused` names only report a truthful failure with the correct ABI. Merging them is how a completeness
//! census comes to count a refusal as an implementation, which is how seven mandatory core-1.4 commands
//! stayed silent `void` no-ops while the count looked complete.

mod lowered;
mod refused;
mod runtime;

pub struct Implementations;

impl Implementations {
    /// Whether `name` has any hand-written body (so the generator must not emit a default stub for it).
    pub fn contains(&self, name: &str) -> bool {
        self.is_lowered(name) || refused::NAMES.contains(&name)
    }

    /// Whether `name` genuinely performs its Vulkan command.
    pub fn is_lowered(&self, name: &str) -> bool {
        runtime::NAMES.contains(&name) || lowered::NAMES.contains(&name)
    }

    pub fn lowered(&self) -> impl Iterator<Item = &&'static str> {
        runtime::NAMES.iter().chain(lowered::NAMES.iter())
    }

    pub fn refused(&self) -> impl Iterator<Item = &&'static str> {
        refused::NAMES.iter()
    }
}
