use std::sync::{Arc, Mutex};

use hl_isa::GuestArchitecture;
use hl_linux::{
    Aarch64SignalMachine, GuestAccess, GuestFault, GuestMemory, LinuxResult, SignalFrameImage, SignalMachine,
    SyscallFamily, SyscallOperation, TaskSignalTimeSyscalls, X86SignalMachine,
};
use hl_task::{
    AlternateStack, PendingTarget, ProcessCredentials, ProcessLimits, RegistryConfig, SignalAction, SignalDisposition,
    SignalInfo, SignalMask, SignalNumber, TaskRegistry,
};

use crate::{FramePort, PreparedFramePublication, RuntimeProcessSyscalls};

#[derive(Clone)]
struct Memory;

impl GuestMemory for Memory {
    fn probe(&self, _: u64, length: usize, _: GuestAccess) -> Result<usize, GuestFault> {
        Ok(length)
    }
    fn read(&self, _: u64, _: &mut [u8]) -> Result<usize, GuestFault> {
        Ok(0)
    }
    fn write(&self, _: u64, input: &[u8]) -> Result<usize, GuestFault> {
        Ok(input.len())
    }
}

#[derive(Clone)]
struct PortState {
    machine: SignalMachine,
    frame: Option<(u64, Vec<u8>)>,
    fail_install: bool,
    fail_publish: bool,
}

struct Port(Arc<Mutex<PortState>>);

struct Publication {
    state: Arc<Mutex<PortState>>,
    machine: SignalMachine,
    frame: Option<(u64, Vec<u8>)>,
    previous: Option<PortState>,
    committed: bool,
}

impl PreparedFramePublication for Publication {
    fn publish(&mut self) -> Result<(), ()> {
        let mut state = self.state.lock().unwrap();
        if state.fail_publish {
            return Err(());
        }
        self.previous = Some(state.clone());
        state.machine = self.machine.clone();
        if let Some(frame) = self.frame.clone() {
            state.frame = Some(frame);
        }
        Ok(())
    }

    fn commit(mut self: Box<Self>) {
        self.committed = true;
    }
}

impl Drop for Publication {
    fn drop(&mut self) {
        if !self.committed
            && let Some(previous) = self.previous.take()
        {
            *self.state.lock().unwrap() = previous;
        }
    }
}

impl FramePort for Port {
    fn snapshot(&self, _: hl_task::ThreadId) -> Result<SignalMachine, ()> {
        Ok(self.0.lock().unwrap().machine.clone())
    }

    fn default_sigreturn_pc(&self, _: GuestArchitecture) -> Option<u64> {
        Some(0x60_000)
    }

    fn prepare_install(
        &self,
        _: hl_task::ThreadId,
        image: &SignalFrameImage,
    ) -> Result<Box<dyn PreparedFramePublication>, ()> {
        if self.0.lock().unwrap().fail_install {
            return Err(());
        }
        Ok(Box::new(Publication {
            state: self.0.clone(),
            machine: image.handler_machine.clone(),
            frame: Some((image.write_address, image.bytes.clone())),
            previous: None,
            committed: false,
        }))
    }

    fn read_frame(&self, _: hl_task::ThreadId, address: u64, length: usize) -> Result<Vec<u8>, ()> {
        let state = self.0.lock().unwrap();
        let (base, bytes) = state.frame.as_ref().ok_or(())?;
        let offset = usize::try_from(address.checked_sub(*base).ok_or(())?).map_err(|_| ())?;
        bytes
            .get(offset..offset.checked_add(length).ok_or(())?)
            .map(<[u8]>::to_vec)
            .ok_or(())
    }

    fn prepare_restore(
        &self,
        _: hl_task::ThreadId,
        machine: &SignalMachine,
    ) -> Result<Box<dyn PreparedFramePublication>, ()> {
        Ok(Box::new(Publication {
            state: self.0.clone(),
            machine: machine.clone(),
            frame: None,
            previous: None,
            committed: false,
        }))
    }
}

struct Fixture {
    tasks: Arc<TaskRegistry>,
    process: hl_task::ProcessId,
    thread: hl_task::ThreadId,
}

impl Fixture {
    fn new() -> Self {
        let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
        let (process, thread) = tasks
            .create_init(ProcessCredentials::new(0, 0, &[], 8).unwrap(), ProcessLimits::default())
            .unwrap();
        Self { tasks, process, thread }
    }

    fn force(&self, signal: SignalNumber) {
        self.tasks
            .enqueue_signal(PendingTarget::Thread(self.thread), SignalInfo::bare(signal))
            .unwrap();
        let prepared = self.tasks.prepare_deliverable_signal(self.thread).unwrap().unwrap();
        self.tasks.force_signal_delivery(prepared).unwrap();
    }

    fn runtime(&self, architecture: GuestArchitecture, port: Arc<Port>) -> RuntimeProcessSyscalls<Memory> {
        RuntimeProcessSyscalls::new(self.tasks.clone(), self.process, self.thread, Memory, architecture)
            .with_signal_frame(port)
    }
}

fn operation(name: &'static str) -> SyscallOperation {
    SyscallOperation {
        canonical_number: 139,
        name,
        family: SyscallFamily::TaskSignalTime,
    }
}

fn install_action(fixture: &Fixture, signal: SignalNumber) {
    fixture
        .tasks
        .set_action(
            fixture.process,
            signal,
            SignalAction {
                disposition: SignalDisposition::Handler(0x50_000),
                flags: 0,
                restorer: 0,
                mask: SignalMask::from_bits(1 << 6),
            },
        )
        .unwrap();
}

#[test]
fn delivery_trip_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let signal = SignalNumber::new(35).unwrap();
        install_action(&fixture, signal);
        fixture.force(signal);
        let original = match architecture {
            GuestArchitecture::Aarch64 => SignalMachine::Aarch64(Aarch64SignalMachine {
                registers: [7; 31],
                vectors: [9; 32],
                stack_pointer: 0x20_000,
                program_counter: 0x40_000,
                pstate: 0,
                fpcr: 0,
                fpsr: 0,
            }),
            GuestArchitecture::X86_64 => {
                let mut registers = [7; 16];
                registers[4] = 0x20_000;
                SignalMachine::X86_64(X86SignalMachine {
                    registers,
                    vectors: [9; 16],
                    vector_upper: [10; 16],
                    stack_pointer: 0x20_000,
                    instruction_pointer: 0x40_000,
                    rflags: 0x202,
                })
            }
        };
        let state = Arc::new(Mutex::new(PortState {
            machine: original.clone(),
            frame: None,
            fail_install: false,
            fail_publish: false,
        }));
        let port = Arc::new(Port(state.clone()));
        let mut runtime = fixture.runtime(architecture, port);
        assert_eq!(runtime.deliver_forced_frame(), LinuxResult::Value(0));
        if let SignalMachine::X86_64(machine) = &mut state.lock().unwrap().machine {
            machine.stack_pointer += 8;
            machine.registers[4] += 8;
        }
        assert_eq!(runtime.handle(operation("rt_sigreturn"), [0; 6]), LinuxResult::Value(7),);
        assert_eq!(state.lock().unwrap().machine, original);
        assert_eq!(
            fixture
                .tasks
                .deliver_thread_state(fixture.thread)
                .unwrap()
                .alternate_stack,
            AlternateStack::Disabled,
        );
    }
}

#[test]
fn frame_exact_retry() {
    let fixture = Fixture::new();
    let signal = SignalNumber::new(35).unwrap();
    install_action(&fixture, signal);
    fixture.force(signal);
    let machine = SignalMachine::Aarch64(Aarch64SignalMachine {
        registers: [0; 31],
        vectors: [0; 32],
        stack_pointer: 0x20_000,
        program_counter: 0x40_000,
        pstate: 0,
        fpcr: 0,
        fpsr: 0,
    });
    let port = Arc::new(Port(Arc::new(Mutex::new(PortState {
        machine,
        frame: None,
        fail_install: true,
        fail_publish: false,
    }))));
    let runtime = fixture.runtime(GuestArchitecture::Aarch64, port);
    assert_eq!(
        runtime.deliver_forced_frame(),
        LinuxResult::Error(hl_linux::Errno::EFAULT),
    );
    let retained = fixture.tasks.prepare_forced_delivery(fixture.thread).unwrap();
    assert_eq!(retained.info().signal, signal);
    assert!(matches!(
        fixture.tasks.action(fixture.process, signal).unwrap().disposition,
        SignalDisposition::Handler(_),
    ));
}

#[test]
fn frame_publish_retry() {
    let fixture = Fixture::new();
    let signal = SignalNumber::new(35).unwrap();
    install_action(&fixture, signal);
    fixture.force(signal);
    let machine = SignalMachine::Aarch64(Aarch64SignalMachine {
        registers: [0; 31],
        vectors: [0; 32],
        stack_pointer: 0x20_000,
        program_counter: 0x40_000,
        pstate: 0,
        fpcr: 0,
        fpsr: 0,
    });
    let state = Arc::new(Mutex::new(PortState {
        machine: machine.clone(),
        frame: None,
        fail_install: false,
        fail_publish: true,
    }));
    let port = Arc::new(Port(state.clone()));
    let runtime = fixture.runtime(GuestArchitecture::Aarch64, port);
    assert_eq!(runtime.deliver_forced_frame(), LinuxResult::Error(hl_linux::Errno::EIO));
    assert_eq!(state.lock().unwrap().machine, machine);
    assert_eq!(
        fixture
            .tasks
            .prepare_forced_delivery(fixture.thread)
            .unwrap()
            .info()
            .signal,
        signal,
    );
}

#[test]
fn frame_depth_overflow_continues_delivery() {
    let fixture = Fixture::new();
    let signal = SignalNumber::new(35).unwrap();
    install_action(&fixture, signal);
    for value in 0..33 {
        let mut info = SignalInfo::bare(signal);
        info.value = value;
        fixture
            .tasks
            .enqueue_signal(PendingTarget::Thread(fixture.thread), info)
            .unwrap();
    }
    for depth in 0..32 {
        let prepared = fixture
            .tasks
            .prepare_deliverable_signal(fixture.thread)
            .unwrap()
            .unwrap();
        fixture.tasks.force_signal_delivery(prepared).unwrap();
        let forced = fixture.tasks.prepare_forced_delivery(fixture.thread).unwrap();
        fixture
            .tasks
            .commit_frame_delivery(
                forced,
                SignalMask::from_bits(0),
                AlternateStack::Disabled,
                0x40_000 - depth * 0x1000,
                false,
            )
            .unwrap();
    }
    let machine = SignalMachine::Aarch64(Aarch64SignalMachine {
        registers: [0; 31],
        vectors: [0; 32],
        stack_pointer: 0x20_000,
        program_counter: 0x40_000,
        pstate: 0,
        fpcr: 0,
        fpsr: 0,
    });
    let state = Arc::new(Mutex::new(PortState {
        machine: machine.clone(),
        frame: None,
        fail_install: false,
        fail_publish: false,
    }));
    let port = Arc::new(Port(state.clone()));
    let runtime = fixture.runtime(GuestArchitecture::Aarch64, port);
    assert_eq!(
        runtime.deliver_signal_boundary(),
        Ok(crate::SignalBoundaryOutcome::Handled)
    );
    assert_ne!(state.lock().unwrap().machine, machine);
    assert!(
        !fixture
            .tasks
            .pending_signal_mask(fixture.thread)
            .unwrap()
            .contains(signal)
    );
}

#[test]
fn reset_publication_prepare() {
    let fixture = Fixture::new();
    let signal = SignalNumber::new(35).unwrap();
    fixture
        .tasks
        .set_action(
            fixture.process,
            signal,
            SignalAction {
                disposition: SignalDisposition::Handler(0x50_000),
                flags: 0x8000_0000,
                restorer: 0,
                mask: SignalMask::from_bits(0),
            },
        )
        .unwrap();
    fixture.force(signal);
    let machine = SignalMachine::Aarch64(Aarch64SignalMachine {
        registers: [0; 31],
        vectors: [0; 32],
        stack_pointer: 0x20_000,
        program_counter: 0x40_000,
        pstate: 0,
        fpcr: 0,
        fpsr: 0,
    });
    let port = Arc::new(Port(Arc::new(Mutex::new(PortState {
        machine,
        frame: None,
        fail_install: false,
        fail_publish: false,
    }))));
    let runtime = fixture.runtime(GuestArchitecture::Aarch64, port);
    assert_eq!(runtime.deliver_forced_frame(), LinuxResult::Value(0));
    assert_eq!(
        fixture.tasks.action(fixture.process, signal).unwrap(),
        SignalAction::DEFAULT,
    );
}

#[test]
fn nonlocal_unwind_reconciles_frame_scope() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let deferred = SignalNumber::new(35).unwrap();
        let first = SignalNumber::new(36).unwrap();
        let later = SignalNumber::new(34).unwrap();
        for signal in [deferred, first, later] {
            install_action(&fixture, signal);
        }
        fixture
            .tasks
            .enqueue_signal(PendingTarget::Thread(fixture.thread), SignalInfo::bare(deferred))
            .unwrap();
        fixture
            .tasks
            .enqueue_signal(PendingTarget::Thread(fixture.thread), SignalInfo::bare(first))
            .unwrap();
        let machine = match architecture {
            GuestArchitecture::Aarch64 => SignalMachine::Aarch64(Aarch64SignalMachine {
                registers: [0; 31],
                vectors: [0; 32],
                stack_pointer: 0x20_000,
                program_counter: 0x40_000,
                pstate: 0,
                fpcr: 0,
                fpsr: 0,
            }),
            GuestArchitecture::X86_64 => {
                let mut registers = [0; 16];
                registers[4] = 0x20_000;
                SignalMachine::X86_64(X86SignalMachine {
                    registers,
                    vectors: [0; 16],
                    vector_upper: [0; 16],
                    stack_pointer: 0x20_000,
                    instruction_pointer: 0x40_000,
                    rflags: 0x202,
                })
            }
        };
        let state = Arc::new(Mutex::new(PortState {
            machine,
            frame: None,
            fail_install: false,
            fail_publish: false,
        }));
        let port = Arc::new(Port(state.clone()));
        let runtime = fixture.runtime(architecture, port);
        assert_eq!(
            runtime.deliver_signal_boundary(),
            Ok(crate::SignalBoundaryOutcome::Handled)
        );

        match &mut state.lock().unwrap().machine {
            SignalMachine::Aarch64(machine) => machine.stack_pointer = 0x20_000,
            SignalMachine::X86_64(machine) => {
                machine.stack_pointer = 0x20_000;
                machine.registers[4] = 0x20_000;
            }
        }
        fixture
            .tasks
            .enqueue_signal(PendingTarget::Thread(fixture.thread), SignalInfo::bare(later))
            .unwrap();
        assert_eq!(
            runtime.deliver_signal_boundary(),
            Ok(crate::SignalBoundaryOutcome::Handled)
        );
        let pending = fixture.tasks.pending_signal_mask(fixture.thread).unwrap();
        assert!(pending.contains(later));
        assert!(!pending.contains(deferred));
    }
}
