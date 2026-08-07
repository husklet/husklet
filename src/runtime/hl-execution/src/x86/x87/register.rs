use crate::{CpuState, ExecutionExit, ExtendedClass, ExtendedReal, X87StackOperation};

use super::memory::ExtendedMemory;

impl ExtendedMemory {
    pub(crate) fn stack(
        cpu: &mut CpuState,
        source: u8,
        operation: X87StackOperation,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let top = usize::from((cpu.x87_status >> 11) & 7);
        let source_index = (top + usize::from(source)) & 7;
        let mut staged = cpu.clone();
        staged.x87_status &= !(1 << 9);
        let blocked = match operation {
            X87StackOperation::Load => Self::load_register(&mut staged, source_index, top),
            X87StackOperation::Exchange => Self::exchange_register(&mut staged, source_index, top),
            X87StackOperation::Store | X87StackOperation::StorePop => {
                Self::store_register(&mut staged, source_index, top, operation == X87StackOperation::StorePop)
            }
        };
        if blocked {
            *cpu = staged;
            return ExecutionExit::UndefinedInstruction { instruction };
        }
        staged.rip = next;
        *cpu = staged;
        ExecutionExit::Continue
    }

    fn load_register(cpu: &mut CpuState, source: usize, top: usize) -> bool {
        let destination = top.wrapping_sub(1) & 7;
        let overflow = cpu.x87_classes[destination] != ExtendedClass::Empty;
        let underflow = cpu.x87_classes[source] == ExtendedClass::Empty;
        if overflow || underflow {
            if Self::raise_stack(cpu, overflow) {
                return true;
            }
            cpu.x87_values[destination] = ExtendedReal::INDEFINITE;
            cpu.x87_classes[destination] = ExtendedClass::QuietNan;
        } else {
            cpu.x87_values[destination] = cpu.x87_values[source];
            cpu.x87_classes[destination] = cpu.x87_classes[source];
        }
        cpu.x87_status = (cpu.x87_status & !0x3800) | (destination as u16) << 11;
        false
    }

    fn exchange_register(cpu: &mut CpuState, source: usize, top: usize) -> bool {
        let underflow = cpu.x87_classes[top] == ExtendedClass::Empty || cpu.x87_classes[source] == ExtendedClass::Empty;
        if underflow {
            if Self::raise_stack(cpu, false) {
                return true;
            }
            Self::fill_empty(cpu, top);
            Self::fill_empty(cpu, source);
        }
        cpu.x87_values.swap(top, source);
        cpu.x87_classes.swap(top, source);
        false
    }

    fn fill_empty(cpu: &mut CpuState, index: usize) {
        if cpu.x87_classes[index] != ExtendedClass::Empty {
            return;
        }
        cpu.x87_values[index] = ExtendedReal::INDEFINITE;
        cpu.x87_classes[index] = ExtendedClass::QuietNan;
    }

    fn store_register(cpu: &mut CpuState, destination: usize, top: usize, pop: bool) -> bool {
        if cpu.x87_classes[top] == ExtendedClass::Empty {
            if Self::raise_stack(cpu, false) {
                return true;
            }
            cpu.x87_values[destination] = ExtendedReal::INDEFINITE;
            cpu.x87_classes[destination] = ExtendedClass::QuietNan;
        } else {
            cpu.x87_values[destination] = cpu.x87_values[top];
            cpu.x87_classes[destination] = cpu.x87_classes[top];
        }
        if pop {
            cpu.x87_classes[top] = ExtendedClass::Empty;
            cpu.x87_status = (cpu.x87_status & !0x3800) | (((top + 1) & 7) as u16) << 11;
        }
        false
    }
}
