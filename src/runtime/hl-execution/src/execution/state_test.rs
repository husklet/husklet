use crate::{
    Aarch64CpuState, AccessKind, CpuState, EXECUTION_SNAPSHOT_VERSION, ExclusiveReservation, ExecutionCpuSnapshot,
    ExecutionMachine, ExecutionSnapshot, MappingGeneration, MemoryFault, Nzcv,
};

#[test]
fn aarch64_snapshot_preserves() {
    let mut cpu = Aarch64CpuState::default();
    cpu.registers[3] = 0x33;
    cpu.vectors[7] = u128::MAX - 7;
    cpu.pc = 0x1000;
    cpu.nzcv = Nzcv::from_bits(Nzcv::NEGATIVE | Nzcv::CARRY);
    cpu.exclusive = Some(ExclusiveReservation::new(0x2000, 8, false, MappingGeneration::new(9)));
    let snapshot = ExecutionSnapshot {
        version: EXECUTION_SNAPSHOT_VERSION,
        cpu: ExecutionCpuSnapshot::Aarch64(cpu.clone()),
        cache_epoch: 4,
        fault: Some(MemoryFault {
            instruction: 0x1000,
            address: 0x3000,
            access: AccessKind::Write,
        }),
    };
    let machine = ExecutionMachine::new(snapshot.clone()).unwrap();
    assert_eq!(
        ExecutionSnapshot::decode(&snapshot.encode().unwrap()).unwrap(),
        snapshot,
    );
    machine.freeze().unwrap();
    assert_eq!(machine.snapshot().unwrap(), snapshot);
    let child = machine.fork_child().unwrap();
    child.freeze().unwrap();
    let child = child.snapshot().unwrap();
    let ExecutionCpuSnapshot::Aarch64(child_cpu) = child.cpu else {
        panic!("wrong architecture");
    };
    assert_eq!(child_cpu.registers, cpu.registers);
    assert_eq!(child_cpu.vectors, cpu.vectors);
    assert_eq!(child_cpu.exclusive, None);
    assert_eq!(child.cache_epoch, 5);
    assert_eq!(child.fault, None);
}

#[test]
fn x86_snapshot_preserves() {
    let mut cpu = CpuState::default();
    cpu.registers[0] = 0x55;
    cpu.vectors[2] = 0x1122_3344;
    cpu.vector_upper[2] = 0x5566_7788;
    cpu.rip = 0x4000;
    cpu.fs_base = 0x5000;
    cpu.direction = true;
    cpu.alignment_check = true;
    cpu.id_flag = true;
    cpu.x87_control = 0x0b7f;
    cpu.x87_status = 0x2841;
    cpu.x87_values[5] = crate::ExtendedReal::from_bits(0x4000_8000_0000_0000_0001);
    cpu.x87_classes[5] = crate::ExtendedClass::Normal;
    cpu.mxcsr = 0x5f80;
    let snapshot = ExecutionSnapshot {
        version: EXECUTION_SNAPSHOT_VERSION,
        cpu: ExecutionCpuSnapshot::X86_64(cpu),
        cache_epoch: 7,
        fault: Some(MemoryFault {
            instruction: 0x4000,
            address: 0x6000,
            access: AccessKind::Read,
        }),
    };
    let machine = ExecutionMachine::new(snapshot.clone()).unwrap();
    assert_eq!(
        ExecutionSnapshot::decode(&snapshot.encode().unwrap()).unwrap(),
        snapshot,
    );
    machine.freeze().unwrap();
    assert_eq!(machine.snapshot().unwrap(), snapshot);

    let mut invalid = snapshot.encode().unwrap();
    *invalid.last_mut().unwrap() = 1;
    assert!(ExecutionSnapshot::decode(&invalid).is_err());
}

#[test]
fn legacy_alignment_snapshot() {
    let snapshot = ExecutionSnapshot {
        version: EXECUTION_SNAPSHOT_VERSION,
        cpu: ExecutionCpuSnapshot::X86_64(CpuState::default()),
        cache_epoch: 1,
        fault: None,
    };
    let bytes = snapshot.encode().unwrap();
    let decoded = ExecutionSnapshot::decode(&bytes).unwrap();
    let ExecutionCpuSnapshot::X86_64(cpu) = decoded.cpu else {
        panic!("wrong architecture");
    };
    assert!(!cpu.alignment_check);
}

#[test]
fn restore_requires_freeze() {
    let x86 = ExecutionSnapshot {
        version: EXECUTION_SNAPSHOT_VERSION,
        cpu: ExecutionCpuSnapshot::X86_64(CpuState::default()),
        cache_epoch: 1,
        fault: None,
    };
    let arm = ExecutionSnapshot {
        version: EXECUTION_SNAPSHOT_VERSION,
        cpu: ExecutionCpuSnapshot::Aarch64(Aarch64CpuState::default()),
        cache_epoch: 1,
        fault: None,
    };
    let machine = ExecutionMachine::new(x86.clone()).unwrap();
    assert!(machine.replace(x86).is_err());
    machine.freeze().unwrap();
    assert!(machine.replace(arm).is_err());
    machine.thaw().unwrap();
}
