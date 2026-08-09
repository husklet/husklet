//! Trap dispatch entry point for the runtime syscall router.

use hl_execution::ExecutionCpuSnapshot;
use hl_isa::{CoreRegister, GuestArchitecture};
use hl_linux::{RegisterView, SyscallDispatcher, SyscallDisposition, SyscallFrameDecoder, SyscallPorts};

use crate::{RuntimeSyscallTrap, RuntimeTrapOutcome};

use super::{RouterDependencies, RuntimeSyscallRouter, RuntimeTerminal, SyscallRecord};

impl RuntimeSyscallTrap for RuntimeSyscallRouter {
    fn dispatch(&self, architecture: GuestArchitecture, cpu: &mut ExecutionCpuSnapshot) -> RuntimeTrapOutcome {
        let pc = match &*cpu {
            ExecutionCpuSnapshot::Aarch64(cpu) => cpu.pc,
            ExecutionCpuSnapshot::X86_64(cpu) => cpu.rip,
        };
        let Ok(frame) = SyscallFrameDecoder::decode(architecture, &CpuRegisters(cpu)) else {
            hl_log::hl_debug!(hl_log::tag::SYSCALL, "frame decode faulted pc={pc:#x}");
            return RuntimeTrapOutcome::Fault;
        };
        let route = SyscallDispatcher::route(architecture, frame.raw_number);
        // Fires only for a number the table refuses, so it never narrates a working guest.
        match route.disposition {
            SyscallDisposition::Operation(_) => {}
            SyscallDisposition::Unsupported { canonical_number, name } => hl_log::hl_debug!(
                hl_log::tag::SYSCALL,
                "unsupported name={name} number={} canonical={canonical_number} pc={pc:#x} arguments={:#x?}",
                frame.raw_number,
                frame.arguments,
            ),
            SyscallDisposition::Reserved { canonical_number } => hl_log::hl_debug!(
                hl_log::tag::SYSCALL,
                "reserved number={} canonical={canonical_number} pc={pc:#x}",
                frame.raw_number,
            ),
        }
        hl_log::hl_trace!(
            hl_log::tag::SYSCALL,
            "enter name={} number={} pc={pc:#x} arguments={:#x?}",
            Self::trace_identity(architecture, frame.raw_number, route.disposition).0,
            frame.raw_number,
            frame.arguments,
        );
        let direct_identity = self
            .seccomp_control
            .as_ref()
            .filter(|control| !control.requires_evaluation())
            .and_then(|_| self.task_identity_result(route.disposition));
        let mut dependencies = direct_identity
            .is_none()
            .then(|| self.ports.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
        let enforced = if let Some(dependencies) = dependencies.as_mut() {
            match self.seccomp_result(dependencies.seccomp.evaluate(&frame, pc)) {
                Ok(value) => value,
                Err(()) => return RuntimeTrapOutcome::Fault,
            }
        } else {
            None
        };
        let killed = enforced.and_then(|value| value.1);
        let result = if let Some(result) = direct_identity {
            result
        } else if let Some((result, _)) = enforced {
            result
        } else if let Some(result) = self.task_identity_result(route.disposition) {
            result
        } else if matches!(route.disposition,
            SyscallDisposition::Operation(operation) if matches!(operation.name, "clone" | "clone3"))
        {
            let clone3 = matches!(route.disposition,
                SyscallDisposition::Operation(operation) if operation.name == "clone3");
            Self::clone_result(
                dependencies.as_deref().expect("non-identity dispatch owns ports"),
                cpu,
                frame,
                clone3,
            )
        } else if matches!(route.disposition,
            SyscallDisposition::Operation(operation) if matches!(operation.name, "fork" | "vfork"))
        {
            let vfork = matches!(route.disposition,
                SyscallDisposition::Operation(operation) if operation.name == "vfork");
            Self::fork_result(
                dependencies.as_deref().expect("non-identity dispatch owns ports"),
                cpu,
                architecture,
                vfork,
            )
        } else if architecture == GuestArchitecture::X86_64 && frame.raw_number == 158 {
            crate::architecture_thread::arch_prctl(
                cpu,
                dependencies
                    .as_deref()
                    .expect("non-identity dispatch owns ports")
                    .architecture_memory
                    .as_ref(),
                frame.arguments[0],
                frame.arguments[1],
            )
        } else {
            let RouterDependencies {
                architecture_memory: _,
                process_fork: _,
                thread_clone: _,
                aio,
                filesystem,
                descriptor_io,
                event,
                memory,
                network,
                task_signal_time,
                ipc,
                seccomp,
            } = &mut **dependencies.as_mut().expect("non-identity dispatch owns ports");
            let mut ports = SyscallPorts {
                aio: aio.as_mut(),
                filesystem: filesystem.as_mut(),
                descriptor_io: descriptor_io.as_mut(),
                event: event.as_mut(),
                memory: memory.as_mut(),
                network: network.as_mut(),
                task_signal_time: task_signal_time.as_mut(),
                ipc: ipc.as_mut(),
                seccomp: seccomp.as_mut(),
            };
            SyscallDispatcher::dispatch(frame, &mut ports)
        };
        hl_log::hl_trace!(
            hl_log::tag::SYSCALL,
            "exit name={} number={} pc={pc:#x} result={:#x}",
            Self::trace_identity(architecture, frame.raw_number, route.disposition).0,
            frame.raw_number,
            result.encode(),
        );
        if let Some(trace) = &self.trace {
            let (name, number) = Self::trace_identity(architecture, frame.raw_number, route.disposition);
            trace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(SyscallRecord {
                    architecture,
                    number,
                    name,
                    arguments: frame.arguments,
                    result: result.encode(),
                    pc,
                });
        }
        let replacement = match (&self.exec, route.disposition, result) {
            (Some((thread, queue)), SyscallDisposition::Operation(operation), hl_linux::LinuxResult::Value(0))
                if matches!(operation.name, "execve" | "execveat") =>
            {
                queue.current(*thread)
            }
            _ => None,
        };
        if let Some(key) = replacement {
            return RuntimeTrapOutcome::ReplaceImage {
                generation: key.generation,
            };
        }
        if matches!(result, hl_linux::LinuxResult::Restart(_)) {
            match cpu {
                ExecutionCpuSnapshot::Aarch64(cpu) => {
                    cpu.pc = cpu.pc.wrapping_sub(4);
                    cpu.registers[0] = frame.arguments[0];
                }
                ExecutionCpuSnapshot::X86_64(cpu) => {
                    cpu.rip = cpu.rip.wrapping_sub(2);
                    cpu.registers[0] = frame.raw_number;
                }
            }
            return RuntimeTrapOutcome::Continue;
        }
        if SyscallFrameDecoder::write_result(&frame, &mut CpuRegisters(cpu), result).is_err() {
            hl_log::hl_debug!(
                hl_log::tag::SYSCALL,
                "result write-back faulted number={} pc={pc:#x}",
                frame.raw_number,
            );
            return RuntimeTrapOutcome::Fault;
        }
        if let Some(signal) = killed {
            return RuntimeTrapOutcome::Exit(128 + i32::from(signal));
        }
        match route.disposition {
            SyscallDisposition::Operation(operation)
                if matches!(operation.name, "exit" | "exit_group")
                    && matches!(result, hl_linux::LinuxResult::Value(_)) =>
            {
                let status = (frame.arguments[0] & 0xff) as i32;
                let terminal = if operation.name == "exit_group" {
                    RuntimeTerminal::Group(status)
                } else {
                    RuntimeTerminal::Thread(status)
                };
                hl_log::hl_info!(
                    hl_log::tag::SYSCALL,
                    "thread terminal name={} status={status}",
                    operation.name,
                );
                *self.terminal.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(terminal);
                RuntimeTrapOutcome::Exit(status)
            }
            _ => RuntimeTrapOutcome::Continue,
        }
    }
}

pub(super) struct CpuRegisters<'a>(pub(super) &'a mut ExecutionCpuSnapshot);

impl RegisterView for CpuRegisters<'_> {
    fn read(&self, register: CoreRegister) -> Option<u64> {
        let CoreRegister::GeneralPurpose(index) = register else {
            return None;
        };
        match &*self.0 {
            ExecutionCpuSnapshot::Aarch64(cpu) => cpu.registers.get(usize::from(index)).copied(),
            ExecutionCpuSnapshot::X86_64(cpu) => cpu.registers.get(usize::from(index)).copied(),
        }
    }

    fn write(&mut self, register: CoreRegister, value: u64) -> bool {
        let CoreRegister::GeneralPurpose(index) = register else {
            return false;
        };
        let register = match &mut *self.0 {
            ExecutionCpuSnapshot::Aarch64(cpu) => cpu.registers.get_mut(usize::from(index)),
            ExecutionCpuSnapshot::X86_64(cpu) => cpu.registers.get_mut(usize::from(index)),
        };
        let Some(register) = register else {
            return false;
        };
        *register = value;
        true
    }
}
