/// Projects an execution coordinate into the architectural PC visible to a
/// guest. Retained non-PIE images execute in a high mapping while ADR/ADRP and
/// link-register values remain in their original low ELF coordinates.
pub trait Port {
    fn architectural_pc(&self, execution_pc: u64) -> u64;
}

pub(crate) struct Identity;

impl Port for Identity {
    fn architectural_pc(&self, execution_pc: u64) -> u64 {
        execution_pc
    }
}

pub(crate) fn stage_branch(
    staged: &mut crate::Aarch64CpuState,
    instruction: u64,
    target: u64,
) -> crate::Aarch64ExecutionExit {
    if target & 3 != 0 {
        return crate::Aarch64ExecutionExit::AlignmentFault {
            instruction,
            target,
            access: crate::AccessKind::Execute,
        };
    }
    staged.pc = target;
    crate::Aarch64ExecutionExit::Branch { target }
}
