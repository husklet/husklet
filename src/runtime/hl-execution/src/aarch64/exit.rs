#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionExit {
    Continue,
    Branch {
        target: u64,
    },
    Syscall {
        instruction: u64,
        immediate: u16,
    },
    Breakpoint {
        instruction: u64,
        immediate: u16,
    },
    UndefinedInstruction {
        instruction: u64,
        word: u32,
    },
    UnsupportedInstruction {
        instruction: u64,
        word: u32,
    },
    AlignmentFault {
        instruction: u64,
        target: u64,
        access: crate::AccessKind,
    },
    MemoryFault(crate::MemoryFault),
    OperandFault(crate::FaultAccess),
}

pub type Aarch64ExecutionExit = ExecutionExit;
