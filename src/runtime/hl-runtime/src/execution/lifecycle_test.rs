use std::sync::Arc;

use hl_checkpoint::{Section, SectionKind};
use hl_execution::{
    Aarch64CpuState, AccessKind, CpuState, EXECUTION_SNAPSHOT_VERSION, ExclusiveReservation, ExecutionCpuSnapshot,
    ExecutionMachine, ExecutionSnapshot, MappingGeneration, MemoryFault,
};
use hl_task::{ForkCloneFlags, ForkEntityId, ForkRequest};

use crate::{
    CheckpointParticipant, ExecutionCheckpointParticipant, ExecutionForkParticipant, ForkContext, ForkParticipant,
    RuntimeSyscallTrap, RuntimeTrapOutcome, dispatch_runtime_syscall,
};

struct ReplaceTrap;

impl RuntimeSyscallTrap for ReplaceTrap {
    fn dispatch(&self, _: hl_isa::GuestArchitecture, _: &mut ExecutionCpuSnapshot) -> RuntimeTrapOutcome {
        RuntimeTrapOutcome::ReplaceImage { generation: 7 }
    }
}

fn request() -> ForkRequest {
    ForkRequest {
        parent: ForkEntityId { slot: 1, generation: 1 },
        child: ForkEntityId { slot: 2, generation: 1 },
        flags: ForkCloneFlags::default(),
    }
}

fn aarch64() -> ExecutionSnapshot {
    let mut cpu = Aarch64CpuState::default();
    cpu.registers[0] = 11;
    cpu.vectors[0] = 22;
    cpu.exclusive = Some(ExclusiveReservation::new(0x1000, 8, false, MappingGeneration::new(3)));
    ExecutionSnapshot {
        version: EXECUTION_SNAPSHOT_VERSION,
        cpu: ExecutionCpuSnapshot::Aarch64(cpu),
        cache_epoch: 5,
        fault: Some(MemoryFault {
            instruction: 7,
            address: 8,
            access: AccessKind::Execute,
        }),
    }
}

trait TestView {
    fn observed(&self) -> ExecutionSnapshot;
}

impl TestView for ExecutionMachine {
    fn observed(&self) -> ExecutionSnapshot {
        self.freeze().unwrap();
        let snapshot = self.snapshot().unwrap();
        self.thaw().unwrap();
        snapshot
    }
}

#[test]
fn fork_fault_state() {
    let parent = Arc::new(ExecutionMachine::new(aarch64()).unwrap());
    let participant = ExecutionForkParticipant::new(Arc::clone(&parent));
    let context = ForkContext {
        transaction: 9,
        request: request(),
    };
    let reservation = participant.prepare(context).unwrap();
    participant.freeze(context, reservation).unwrap();
    participant.clone_parent(context, reservation).unwrap();
    participant.clone_child(context, reservation).unwrap();
    participant.repair_parent(context, reservation).unwrap();
    participant.repair_child(context, reservation).unwrap();
    participant.commit(context, reservation).unwrap();
    let child = participant.take_child(context.transaction).unwrap();
    let parent_snapshot = parent.observed();
    let child_snapshot = child.observed();
    let ExecutionCpuSnapshot::Aarch64(parent_cpu) = parent_snapshot.cpu else {
        panic!("wrong parent architecture");
    };
    let ExecutionCpuSnapshot::Aarch64(child_cpu) = child_snapshot.cpu else {
        panic!("wrong child architecture");
    };
    assert_eq!(parent_cpu.exclusive, None);
    assert_eq!(child_cpu.exclusive, None);
    assert_eq!(parent_snapshot.cache_epoch, 6);
    assert_eq!(child_snapshot.cache_epoch, 6);
    assert!(parent_snapshot.fault.is_some());
    assert_eq!(child_snapshot.fault, None);
}

#[test]
fn fork_parent_state() {
    for stop in 0..=5 {
        let original = aarch64();
        let parent = Arc::new(ExecutionMachine::new(original.clone()).unwrap());
        let participant = ExecutionForkParticipant::new(Arc::clone(&parent));
        let context = ForkContext {
            transaction: stop + 1,
            request: request(),
        };
        let reservation = participant.prepare(context).unwrap();
        if stop >= 1 {
            participant.freeze(context, reservation).unwrap();
        }
        if stop >= 2 {
            participant.clone_parent(context, reservation).unwrap();
        }
        if stop >= 3 {
            participant.clone_child(context, reservation).unwrap();
        }
        if stop >= 4 {
            participant.repair_parent(context, reservation).unwrap();
        }
        if stop >= 5 {
            participant.repair_child(context, reservation).unwrap();
        }
        participant.rollback(context, reservation);
        assert_eq!(parent.observed(), original);
    }
}

#[test]
fn checkpoint_resume_compensating() {
    let original = ExecutionSnapshot {
        version: EXECUTION_SNAPSHOT_VERSION,
        cpu: ExecutionCpuSnapshot::X86_64(CpuState::default()),
        cache_epoch: 2,
        fault: None,
    };
    let mut replacement = original.clone();
    replacement.cache_epoch = 8;
    let machine = Arc::new(ExecutionMachine::new(original.clone()).unwrap());
    let participant = ExecutionCheckpointParticipant::new(Arc::clone(&machine));
    let section = Section::new(
        SectionKind::new(8).unwrap(),
        EXECUTION_SNAPSHOT_VERSION,
        replacement.encode().unwrap(),
    );
    let reservation = participant.stage(&section).unwrap();
    participant.commit(reservation).unwrap();
    participant.rollback(reservation);
    assert_eq!(machine.observed(), original);
    let reservation = participant.stage(&section).unwrap();
    participant.commit(reservation).unwrap();
    participant.resume(reservation).unwrap();
    assert_eq!(machine.observed(), replacement);
}

#[test]
fn checkpoint_clears_exclusive() {
    let original = aarch64();
    let machine = Arc::new(ExecutionMachine::new(original.clone()).unwrap());
    let participant = ExecutionCheckpointParticipant::new(Arc::clone(&machine));
    participant.freeze().unwrap();
    let encoded = participant.snapshot().unwrap();
    participant.thaw().unwrap();
    let captured = ExecutionSnapshot::decode(&encoded).unwrap();
    let ExecutionCpuSnapshot::Aarch64(cpu) = captured.cpu else {
        panic!("wrong checkpoint architecture");
    };
    assert_eq!(cpu.exclusive, None);
    assert_eq!(machine.observed(), original);
}

#[test]
fn replace_preserves_cpu() {
    let original = aarch64();
    let machine = ExecutionMachine::new(original.clone()).unwrap();
    assert_eq!(
        dispatch_runtime_syscall(&machine, 5, &ReplaceTrap),
        hl_execution::StepOutcome::ReplaceImage { generation: 7 },
    );
    assert_eq!(machine.observed(), original);
}
