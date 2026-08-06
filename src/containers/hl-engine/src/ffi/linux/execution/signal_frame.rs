use std::sync::Arc;

use hl_execution::{Aarch64CpuState, CpuState, ExecutionCpuSnapshot, ExecutionMachine, ExecutionSnapshot, FlagState};
use hl_isa::GuestAddress;
use hl_linux::{Aarch64SignalMachine, SignalFrameImage, SignalMachine, X86SignalMachine};
use hl_memory::Protection;
use hl_runtime::{FramePort, PreparedFramePublication};
use hl_task::ThreadId;

use super::threads::ThreadSet;

pub(super) struct Port {
    threads: Arc<ThreadSet>,
    sigreturn_pc: u64,
}

struct Publication {
    machine: Arc<ExecutionMachine>,
    replacement: Option<ExecutionSnapshot>,
    space: Arc<super::space::AddressSpace>,
    address: Option<u64>,
    previous: Vec<u8>,
    published: bool,
    committed: bool,
    previous_snapshot: Option<ExecutionSnapshot>,
}

impl Port {
    pub(super) fn new(threads: Arc<ThreadSet>, sigreturn_pc: u64) -> Arc<Self> {
        Arc::new(Self { threads, sigreturn_pc })
    }

    fn execution(machine: &SignalMachine, previous: &ExecutionSnapshot) -> ExecutionSnapshot {
        let cpu = match machine {
            SignalMachine::Aarch64(signal) => {
                let mut cpu = Aarch64CpuState::default();
                cpu.registers = signal.registers;
                cpu.vectors = signal.vectors;
                cpu.sp = signal.stack_pointer;
                cpu.pc = signal.program_counter;
                cpu.nzcv = hl_execution::Nzcv::from_bits(signal.pstate as u32);
                cpu.fpcr = u64::from(signal.fpcr);
                cpu.fpsr = u64::from(signal.fpsr);
                if let ExecutionCpuSnapshot::Aarch64(old) = &previous.cpu {
                    cpu.tls = old.tls;
                }
                ExecutionCpuSnapshot::Aarch64(cpu)
            }
            SignalMachine::X86_64(signal) => {
                let mut cpu = CpuState::default();
                cpu.registers = signal.registers;
                cpu.vectors = signal.vectors;
                cpu.vector_upper = signal.vector_upper;
                cpu.registers[4] = signal.stack_pointer;
                cpu.rip = signal.instruction_pointer;
                cpu.flags = FlagState::from_bits(signal.rflags as u16);
                if let ExecutionCpuSnapshot::X86_64(old) = &previous.cpu {
                    cpu.fs_base = old.fs_base;
                    cpu.gs_base = old.gs_base;
                    cpu.direction = old.direction;
                    cpu.x87_control = old.x87_control;
                    cpu.x87_status = old.x87_status;
                    cpu.x87_values = old.x87_values;
                    cpu.x87_classes = old.x87_classes;
                    cpu.mxcsr = old.mxcsr;
                }
                ExecutionCpuSnapshot::X86_64(cpu)
            }
        };
        ExecutionSnapshot {
            version: previous.version,
            cpu,
            cache_epoch: previous.cache_epoch,
            fault: None,
        }
    }

    fn signal(snapshot: &ExecutionSnapshot) -> SignalMachine {
        match &snapshot.cpu {
            ExecutionCpuSnapshot::Aarch64(cpu) => SignalMachine::Aarch64(Aarch64SignalMachine {
                registers: cpu.registers,
                vectors: cpu.vectors,
                stack_pointer: cpu.sp,
                program_counter: cpu.pc,
                pstate: u64::from(cpu.nzcv.bits()),
                fpcr: cpu.fpcr as u32,
                fpsr: cpu.fpsr as u32,
            }),
            ExecutionCpuSnapshot::X86_64(cpu) => SignalMachine::X86_64(X86SignalMachine {
                registers: cpu.registers,
                vectors: cpu.vectors,
                vector_upper: cpu.vector_upper,
                stack_pointer: cpu.registers[4],
                instruction_pointer: cpu.rip,
                rflags: u64::from(cpu.flags.bits()),
            }),
        }
    }

    fn frozen(&self, thread: ThreadId) -> Result<(super::threads::ThreadRun, ExecutionSnapshot), ()> {
        let run = self.threads.find(thread).ok_or(())?;
        run.machine.freeze().map_err(|_| ())?;
        if let Ok(snapshot) = run.machine.snapshot() { Ok((run, snapshot)) } else {
            let _ = run.machine.thaw();
            Err(())
        }
    }
}

impl FramePort for Port {
    fn snapshot(&self, thread: ThreadId) -> Result<SignalMachine, ()> {
        let (run, snapshot) = self.frozen(thread)?;
        let signal = Self::signal(&snapshot);
        run.machine.thaw().map_err(|_| ())?;
        Ok(signal)
    }

    fn default_sigreturn_pc(&self, _: hl_isa::GuestArchitecture) -> Option<u64> {
        Some(self.sigreturn_pc)
    }

    fn prepare_install(
        &self,
        thread: ThreadId,
        image: &SignalFrameImage,
    ) -> Result<Box<dyn PreparedFramePublication>, ()> {
        let (run, snapshot) = self.frozen(thread)?;
        let mut previous = vec![0; image.bytes.len()];
        if run
            .space
            .arena()
            .snapshot_read(image.write_address, &mut previous, Protection::WRITE)
            .is_err()
        {
            let _ = run.machine.thaw();
            return Err(());
        }
        let memory = run.space.guest_memory();
        if hl_linux::GuestMemory::write(&memory, image.write_address, &image.bytes) != Ok(image.bytes.len()) {
            let _ = run.machine.thaw();
            return Err(());
        }
        Ok(Box::new(Publication {
            machine: run.machine,
            replacement: Some(Self::execution(&image.handler_machine, &snapshot)),
            space: run.space,
            address: Some(image.write_address),
            previous,
            published: false,
            committed: false,
            previous_snapshot: None,
        }))
    }

    fn read_frame(&self, thread: ThreadId, address: u64, length: usize) -> Result<Vec<u8>, ()> {
        let run = self.threads.find(thread).ok_or(())?;
        let mut bytes = vec![0; length];
        run.space
            .mappings()
            .read(GuestAddress::new(address), &mut bytes, Protection::READ)
            .map_err(|_| ())?;
        Ok(bytes)
    }

    fn prepare_restore(
        &self,
        thread: ThreadId,
        machine: &SignalMachine,
    ) -> Result<Box<dyn PreparedFramePublication>, ()> {
        let (run, snapshot) = self.frozen(thread)?;
        Ok(Box::new(Publication {
            machine: run.machine,
            replacement: Some(Self::execution(machine, &snapshot)),
            space: run.space,
            address: None,
            previous: Vec::new(),
            published: false,
            committed: false,
            previous_snapshot: None,
        }))
    }
}

impl PreparedFramePublication for Publication {
    fn publish(&mut self) -> Result<(), ()> {
        let replacement = self.replacement.take().ok_or(())?;
        let previous = self.machine.replace(replacement).map_err(|_| ())?;
        self.previous_snapshot = Some(previous);
        self.published = true;
        Ok(())
    }

    fn commit(mut self: Box<Self>) {
        self.committed = true;
        let _ = self.machine.thaw();
    }
}

impl Drop for Publication {
    fn drop(&mut self) {
        if self.published && !self.committed
            && let Some(previous) = self.previous_snapshot.take() {
                let _ = self.machine.replace(previous);
            }
        if !self.committed {
            if let Some(address) = self.address {
                let memory = self.space.guest_memory();
                let _ = hl_linux::GuestMemory::write(&memory, address, &self.previous);
            }
            let _ = self.machine.thaw();
        }
    }
}
