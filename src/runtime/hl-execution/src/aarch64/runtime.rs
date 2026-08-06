use super::{atomic, system};
use crate::{
    Aarch64CpuState, Aarch64DecodeError, Aarch64Decoder, Aarch64ExecutionExit, Aarch64FpExecutor, Aarch64Interpreter,
    Aarch64Ir, Aarch64SoftFloat, ExclusiveMemory, GuestOperandMemory, GuestSystemPort, PcCoordinatePort,
    aarch64_memory::Aarch64MemoryInterpreter,
};

impl Aarch64Interpreter {
    pub fn execute_runtime<M: GuestOperandMemory + ExclusiveMemory, S: GuestSystemPort>(
        cpu: &mut Aarch64CpuState,
        memory: &mut M,
        system: &mut S,
        coordinates: &dyn PcCoordinatePort,
        word: u32,
    ) -> Aarch64ExecutionExit {
        match Aarch64Decoder::decode(word) {
            Ok(ir) => Self::execute_runtime_ir(cpu, memory, system, coordinates, ir),
            Err(Aarch64DecodeError::Reserved) => Aarch64ExecutionExit::UndefinedInstruction {
                instruction: cpu.pc,
                word,
            },
            Err(Aarch64DecodeError::Unsupported) => Aarch64ExecutionExit::UnsupportedInstruction {
                instruction: cpu.pc,
                word,
            },
        }
    }

    pub(crate) fn execute_runtime_ir<M: GuestOperandMemory + ExclusiveMemory, S: GuestSystemPort>(
        cpu: &mut Aarch64CpuState,
        memory: &mut M,
        system: &mut S,
        coordinates: &dyn PcCoordinatePort,
        ir: Aarch64Ir,
    ) -> Aarch64ExecutionExit {
        if Aarch64MemoryInterpreter::is_memory(ir.instruction) {
            Aarch64MemoryInterpreter::execute(cpu, memory, coordinates, ir)
        } else if atomic::Executor::handles(ir.instruction) {
            atomic::Executor::execute(cpu, memory, ir)
        } else if system::Executor::handles(ir.instruction) {
            system::Executor::execute(cpu, system, ir)
        } else if Aarch64FpExecutor::is_fp(ir.instruction) {
            Aarch64FpExecutor::execute(cpu, &mut Aarch64SoftFloat, ir)
        } else {
            Self::execute(cpu, coordinates, ir)
        }
    }
}
