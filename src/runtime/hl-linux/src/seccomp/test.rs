use hl_isa::GuestArchitecture;

use crate::{
    Action, BpfInstruction, BpfProgram, Data, Decision, KillScope, Mode, Policy, PolicyError, SeccompBaseline,
    SeccompStatus, SECCOMP_MAXIMUM_INSTRUCTIONS, VmError,
};

#[test]
fn launch_baselines_have_explicit_visible_status() {
    assert_eq!(
        SeccompBaseline::Container.status(),
        SeccompStatus {
            mode: Mode::Filter,
            filters: 1,
        },
    );
    assert_eq!(
        SeccompBaseline::Disabled.status(),
        SeccompStatus {
            mode: Mode::Disabled,
            filters: 0,
        },
    );
}

struct Fixture;

impl Fixture {
    fn instruction(code: u16, value: u32) -> BpfInstruction {
        BpfInstruction {
            code,
            jump_true: 0,
            jump_false: 0,
            value,
        }
    }

    fn returning(raw: u32) -> BpfProgram {
        BpfProgram::new(vec![Self::instruction(0x06, raw)]).unwrap()
    }

    fn data(architecture: GuestArchitecture) -> Data {
        Data {
            number: 60,
            architecture: Data::audit_arch(architecture),
            instruction_pointer: 0x1234,
            arguments: [1, 2, 3, 4, 5, 6],
        }
    }
}

#[test]
fn audit_arch_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let input = Fixture::data(architecture);
        let program = BpfProgram::new(vec![
            Fixture::instruction(0x20, 4),
            BpfInstruction {
                code: 0x15,
                jump_true: 0,
                jump_false: 1,
                value: Data::audit_arch(architecture),
            },
            Fixture::instruction(0x06, 0x7fff_0000),
            Fixture::instruction(0x06, 0),
        ])
        .unwrap();
        assert_eq!(program.evaluate(input), Action::Allow { data: 0 },);
        let argument = BpfProgram::new(vec![
            Fixture::instruction(0x20, 16),
            BpfInstruction {
                code: 0x15,
                jump_true: 0,
                jump_false: 1,
                value: 1,
            },
            Fixture::instruction(0x06, 0x7fff_0000),
            Fixture::instruction(0x06, 0),
        ])
        .unwrap();
        assert_eq!(argument.evaluate(input), Action::Allow { data: 0 });
    }
}

#[test]
fn action_precedence() {
    let words = [
        0x8000_0001,
        0x0000_0002,
        0x0003_0003,
        0x0005_0004,
        0x7fc0_0005,
        0x7ff0_0006,
        0x7ffc_0007,
        0x7fff_0008,
    ];
    for (precedence, word) in words.into_iter().enumerate() {
        let action = Fixture::returning(word).evaluate(Fixture::data(GuestArchitecture::X86_64));
        assert_eq!(action.raw(), word);
        assert_eq!(usize::from(action.precedence()), precedence);
    }
    assert_eq!(Action::from_raw(0x1234_5678), Action::KillThread { data: 0x5678 },);
}

#[test]
fn malformed_program_execution() {
    assert_eq!(BpfProgram::new(Vec::new()), Err(VmError::Empty));
    assert_eq!(
        BpfProgram::new(vec![Fixture::instruction(0x06, 0); SECCOMP_MAXIMUM_INSTRUCTIONS + 1]),
        Err(VmError::TooLong),
    );
    for (program, error) in [
        (vec![Fixture::instruction(0xffff, 0)], VmError::InvalidOpcode),
        (
            vec![Fixture::instruction(0x60, 16), Fixture::instruction(0x06, 0)],
            VmError::InvalidScratch,
        ),
        (
            vec![Fixture::instruction(0x20, 62), Fixture::instruction(0x06, 0)],
            VmError::InvalidLoad,
        ),
        (
            vec![Fixture::instruction(0x34, 0), Fixture::instruction(0x06, 0)],
            VmError::DivisionByZero,
        ),
        (
            vec![Fixture::instruction(0x05, 1), Fixture::instruction(0x06, 0)],
            VmError::InvalidJump,
        ),
        (
            vec![Fixture::instruction(0x05, u32::MAX), Fixture::instruction(0x06, 0)],
            VmError::InvalidJump,
        ),
        (
            vec![Fixture::instruction(0x20, 0xffff_f000), Fixture::instruction(0x06, 0)],
            VmError::InvalidLoad,
        ),
        (
            vec![
                Fixture::instruction(0x05, 1),
                Fixture::instruction(0xffff, 0),
                Fixture::instruction(0x06, 0),
            ],
            VmError::InvalidOpcode,
        ),
        (vec![Fixture::instruction(0x00, 1)], VmError::Unterminated),
    ] {
        assert_eq!(BpfProgram::new(program), Err(error));
    }
}

#[test]
fn conditional_fallthrough_terminate() {
    let conditional = BpfProgram::new(vec![
        BpfInstruction {
            code: 0x15,
            jump_true: 0,
            jump_false: 0,
            value: 0,
        },
        Fixture::instruction(0x06, 0x7fff_0000),
    ])
    .unwrap();
    assert_eq!(
        conditional.evaluate(Fixture::data(GuestArchitecture::Aarch64)),
        Action::Allow { data: 0 },
    );
    let maximum = BpfProgram::new(vec![
        Fixture::instruction(0x06, 0x7fff_0000);
        SECCOMP_MAXIMUM_INSTRUCTIONS
    ])
    .unwrap();
    assert_eq!(maximum.instructions().len(), SECCOMP_MAXIMUM_INSTRUCTIONS);
}

#[test]
fn arithmetic_scratch_execute() {
    let program = BpfProgram::new(vec![
        Fixture::instruction(0x00, 6),
        Fixture::instruction(0x02, 0),
        Fixture::instruction(0x01, 3),
        Fixture::instruction(0x0c, 0),
        Fixture::instruction(0x24, 2),
        Fixture::instruction(0x14, 1),
        BpfInstruction {
            code: 0x15,
            jump_true: 0,
            jump_false: 1,
            value: 17,
        },
        Fixture::instruction(0x06, 0x7fff_0000),
        Fixture::instruction(0x06, 0x0000_0000),
    ])
    .unwrap();
    assert_eq!(
        program.evaluate(Fixture::data(GuestArchitecture::Aarch64)),
        Action::Allow { data: 0 },
    );
}

#[test]
fn indirect_out_closed() {
    let indirect = BpfProgram::new(vec![
        Fixture::instruction(0x01, 63),
        Fixture::instruction(0x40, 4),
        Fixture::instruction(0x16, 0),
    ])
    .unwrap();
    assert_eq!(
        indirect.evaluate(Fixture::data(GuestArchitecture::X86_64)),
        Action::KillThread { data: 0 },
    );
    let divide = BpfProgram::new(vec![
        Fixture::instruction(0x01, 0),
        Fixture::instruction(0x3c, 0),
        Fixture::instruction(0x16, 0),
    ])
    .unwrap();
    assert_eq!(
        divide.evaluate(Fixture::data(GuestArchitecture::X86_64)),
        Action::KillThread { data: 0 },
    );
}

#[test]
fn stacked_filters_ties() {
    let mut policy = Policy::default();
    policy.enable_nnp();
    for raw in [0x0005_0001, 0x7fff_0000, 0x0005_0002] {
        let plan = Policy::install_plan(Fixture::returning(raw), 0).unwrap();
        policy.install(plan, false, false).unwrap();
    }
    assert_eq!(policy.mode(), Mode::Filter);
    assert_eq!(
        policy.evaluate(Fixture::data(GuestArchitecture::X86_64)),
        Action::Errno { data: 2 },
    );
}

#[test]
fn installation_authority_explicit() {
    let program = Fixture::returning(0x7fff_0000);
    assert_eq!(
        Policy::install_plan(program.clone(), 0x20),
        Err(PolicyError::InvalidFlags),
    );
    let listener = Policy::install_plan(program.clone(), 0x08).unwrap();
    let mut policy = Policy::default();
    assert_eq!(
        policy.install(listener, true, false),
        Err(PolicyError::ListenerUnavailable),
    );
    let plan = Policy::install_plan(program, 0x13).unwrap();
    assert!(plan.flags.synchronize_threads);
    assert!(plan.flags.synchronize_threads_esrch);
    policy.enable_nnp();
    policy.install(plan, false, true).unwrap();
    assert_eq!(policy.fork_snapshot(), policy);
    assert_eq!(policy.exec_snapshot(), policy);
}

#[test]
fn decisions_errno_semantics() {
    let input = Fixture::data(GuestArchitecture::X86_64);
    for (raw, expected) in [
        (0x0005_ffff, Decision::ReturnErrno(4095)),
        (0x7ff0_0042, Decision::Trace { data: 0x42 }),
        (0x7fc0_0043, Decision::UserNotification { data: 0x43 }),
        (
            0x8000_0000,
            Decision::Kill {
                scope: KillScope::Process,
                signal: 31,
            },
        ),
        (
            0x0000_0000,
            Decision::Kill {
                scope: KillScope::Thread,
                signal: 31,
            },
        ),
    ] {
        let mut policy = Policy::default();
        policy.enable_nnp();
        let plan = Policy::install_plan(Fixture::returning(raw), 0).unwrap();
        policy.install(plan, false, false).unwrap();
        assert_eq!(policy.decide(input), expected);
    }

    let mut trap = Policy::default();
    trap.enable_nnp();
    let plan = Policy::install_plan(Fixture::returning(0x0003_0044), 0).unwrap();
    trap.install(plan, false, false).unwrap();
    assert!(matches!(
        trap.decide(input),
        Decision::Trap(plan)
            if plan.error == 0x44
                && plan.signal == 31
                && plan.code == 1
                && plan.syscall_number == input.number
                && plan.audit_architecture == input.architecture
    ));
}
