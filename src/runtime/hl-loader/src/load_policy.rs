use crate::{ImageLimits, StackLimits};

/// How an `ET_EXEC` storage span is reserved.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutablePlacement {
    /// Require storage at the ELF link address.
    FixedLink,
    /// Try the ELF link address without replacement, then use a hint only when
    /// an existing mapping conflicts with that exact span.
    PreferLink { fallback_hint: Option<u64> },
    /// Permit rebased storage while retaining link-time guest-visible values.
    Rebased { deterministic_hint: Option<u64> },
}

/// Bounds and placement policy for one load transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LoadLimits {
    pub image: ImageLimits,
    pub stack: StackLimits,
    /// Inaccessible interval immediately below the usable main stack.
    pub stack_guard_size: u64,
    /// Writable bytes above `stack_guard_size`.
    pub stack_size: u64,
    /// Writable x86-64 cushion above the logical main-stack top for bounded
    /// vector loads which cross that boundary.
    pub x86_stack_overread_size: u64,
    pub executable_placement: ExecutablePlacement,
    pub pie_hint: Option<u64>,
    pub interpreter_hint: Option<u64>,
    pub stack_hint: Option<u64>,
    pub host_page_size: u64,
}

impl Default for LoadLimits {
    fn default() -> Self {
        Self {
            image: ImageLimits::default(),
            stack: StackLimits::default(),
            stack_guard_size: 1024 * 1024,
            stack_size: 8 * 1024 * 1024,
            x86_stack_overread_size: 64 * 1024,
            executable_placement: ExecutablePlacement::PreferLink { fallback_hint: None },
            pie_hint: None,
            interpreter_hint: None,
            stack_hint: None,
            host_page_size: 4096,
        }
    }
}
