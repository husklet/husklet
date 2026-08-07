use super::ScalarInterpreter;
use crate::x86::{FmaForm, FmaOperation, VexImmediateShift, VexOperation};
use crate::{
    AccessKind, CpuState, ExecutionExit, Flag, GuestOperandMemory, ScalarInstruction, ScalarIr, ScalarOperand,
    ScalarWidth, Staged, VectorLane, VectorMemory, VectorSource,
};
impl ScalarInterpreter {
    pub(super) fn stage<M: GuestOperandMemory>(
        cpu: &CpuState,
        staged: &mut CpuState,
        memory: &M,
        ir: ScalarIr,
        instruction: u64,
        next: u64,
    ) -> Result<Staged<M::Reservation, M::BatchReservation>, ExecutionExit> {
        match ir.instruction {
            ScalarInstruction::VectorLaneInsert { .. }
            | ScalarInstruction::VectorLaneExtract { .. }
            | ScalarInstruction::VectorInsertSingle { .. }
            | ScalarInstruction::VexInsertSingle { .. } => unreachable!("eager vector lane transfer"),
            ScalarInstruction::MmxConvertToFloat { .. } | ScalarInstruction::MmxConvertFromFloat { .. } => {
                unreachable!("eager MMX floating-point conversion")
            }
            ScalarInstruction::VectorMove {
                vector,
                scalar,
                to_vector,
            } => Self::vector_move(staged, memory, vector, scalar, to_vector, ir.width, next, instruction),
            operation @ (ScalarInstruction::VectorUnpack { .. }
            | ScalarInstruction::CarrylessMultiply { .. }
            | ScalarInstruction::VectorBitwise { .. }
            | ScalarInstruction::VectorByteShift { .. }
            | ScalarInstruction::VectorLaneShift { .. }
            | ScalarInstruction::VectorVariableShift { .. }
            | ScalarInstruction::PackedString { .. }
            | ScalarInstruction::Ssse3 { .. }
            | ScalarInstruction::VectorAlign { .. }
            | ScalarInstruction::VectorInteger { .. }
            | ScalarInstruction::VectorShuffle { .. }
            | ScalarInstruction::VectorByteShuffle { .. }
            | ScalarInstruction::VectorTest { .. }
            | ScalarInstruction::VectorExtend { .. }
            | ScalarInstruction::VectorBlend { .. }
            | ScalarInstruction::VectorHorizontalMinimum { .. }
            | ScalarInstruction::VectorSad { .. }
            | ScalarInstruction::VectorDot { .. }
            | ScalarInstruction::VectorCompare { .. }
            | ScalarInstruction::VectorMask { .. }
            | ScalarInstruction::VectorInsertWord { .. }
            | ScalarInstruction::Aes { .. }
            | ScalarInstruction::Sha { .. }) => {
                crate::x86::vector::Executor::stage(staged, memory, operation, next, instruction)
            }
            ScalarInstruction::VectorStore { source, destination } => {
                let value = cpu.vectors[usize::from(source)] as u64;
                match destination {
                    VectorSource::Register(index) => {
                        staged.vectors[usize::from(index)] = u128::from(value);
                        Ok(Staged::Cpu)
                    }
                    VectorSource::Memory(address) => Self::write(
                        staged,
                        memory,
                        ScalarOperand::Memory(address),
                        ScalarWidth::Qword,
                        value,
                        next,
                        instruction,
                    ),
                }
            }
            ScalarInstruction::VectorLoad { destination, source } => {
                let value = match source {
                    VectorSource::Register(index) => cpu.vectors[usize::from(index)] as u64,
                    VectorSource::Memory(address) => Self::read(
                        cpu,
                        memory,
                        ScalarOperand::Memory(address),
                        ScalarWidth::Qword,
                        next,
                        instruction,
                    )?,
                };
                staged.vectors[usize::from(destination)] = u128::from(value);
                Ok(Staged::Cpu)
            }
            ScalarInstruction::VectorTransport {
                vector,
                operand,
                store,
                aligned,
            } => VectorMemory::staged_transfer(staged, memory, vector, operand, store, aligned, next, instruction),
            ScalarInstruction::VexVectorTransport {
                vector,
                operand,
                store,
                wide,
            } => {
                if wide {
                    if !store {
                        let value = Self::vex_read(cpu, memory, operand, true, next, instruction)?;
                        staged.vectors[usize::from(vector)] = value[0];
                        staged.vector_upper[usize::from(vector)] = value[1];
                        Ok(Staged::Cpu)
                    } else if let VectorSource::Register(destination) = operand {
                        staged.vectors[usize::from(destination)] = cpu.vectors[usize::from(vector)];
                        staged.vector_upper[usize::from(destination)] = cpu.vector_upper[usize::from(vector)];
                        Ok(Staged::Cpu)
                    } else {
                        let VectorSource::Memory(address) = operand else {
                            unreachable!()
                        };
                        let address = address.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
                        let writes = [(address, 8), (address + 8, 8), (address + 16, 8), (address + 24, 8)];
                        let reservation = memory.reserve_write_batch(&writes).map_err(|fault| {
                            ExecutionExit::OperandFault(crate::FaultAccess::operand(
                                instruction,
                                fault,
                                AccessKind::Write,
                                32,
                            ))
                        })?;
                        let low = cpu.vectors[usize::from(vector)];
                        let high = cpu.vector_upper[usize::from(vector)];
                        Ok(Staged::Batch(
                            reservation,
                            [low as u64, (low >> 64) as u64, high as u64, (high >> 64) as u64],
                            address,
                            32,
                        ))
                    }
                } else {
                    // Only the architectural destination is zero-extended; a store leaves
                    // the source register, `vector`, alone.
                    if store {
                        if let VectorSource::Register(destination) = operand {
                            staged.vector_upper[usize::from(destination)] = 0;
                        }
                    } else {
                        staged.vector_upper[usize::from(vector)] = 0;
                    }
                    VectorMemory::staged_transfer(staged, memory, vector, operand, store, false, next, instruction)
                }
            }
            ScalarInstruction::VexVectorTest {
                left,
                right,
                lane,
                wide,
            } => {
                let right = Self::vex_read(cpu, memory, right, wide, next, instruction)?;
                let left = [cpu.vectors[usize::from(left)], cpu.vector_upper[usize::from(left)]];
                let mask = match lane {
                    4 => (0..4).fold(0_u128, |value, lane| value | (1_u128 << (lane * 32 + 31))),
                    8 => (1_u128 << 63) | (1_u128 << 127),
                    _ => u128::MAX,
                };
                let halves = if wide { 2 } else { 1 };
                let zero = (0..halves).all(|half| left[half] & right[half] & mask == 0);
                let carry = (0..halves).all(|half| !left[half] & right[half] & mask == 0);
                staged.flags = crate::FlagState::default()
                    .with(Flag::Zero, zero)
                    .with(Flag::Carry, carry);
                Ok(Staged::Cpu)
            }
            ScalarInstruction::VexMaskedMemory {
                vector,
                mask,
                address,
                lane,
                store,
                wide,
            } => {
                let base = address.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
                let masks = [cpu.vectors[usize::from(mask)], cpu.vector_upper[usize::from(mask)]];
                let data = [cpu.vectors[usize::from(vector)], cpu.vector_upper[usize::from(vector)]];
                let lanes = if wide { 32 / lane } else { 16 / lane };
                if store {
                    let mut reservations = std::array::from_fn(|_| None);
                    let mut values = [0_u64; 8];
                    let mut count = 0_usize;
                    for index in 0..lanes {
                        let half = usize::from(index / (16 / lane));
                        let shift = u32::from(index % (16 / lane)) * u32::from(lane) * 8;
                        if masks[half] >> (shift + u32::from(lane) * 8 - 1) & 1 != 0 {
                            let lane_address = base + u64::from(index) * u64::from(lane);
                            reservations[count] = Some(memory.reserve_write(lane_address, lane).map_err(|()| {
                                ExecutionExit::OperandFault(crate::FaultAccess::operand(
                                    instruction,
                                    lane_address,
                                    AccessKind::Write,
                                    u64::from(lane),
                                ))
                            })?);
                            values[count] = (data[half] >> shift) as u64;
                            count += 1;
                        }
                    }
                    Ok(Staged::Sparse(
                        Box::new(crate::x86::SparseWrites { reservations, values }),
                        count as u8,
                        base,
                        if wide { 32 } else { 16 },
                    ))
                } else {
                    let mut output = [0_u128; 2];
                    for index in 0..lanes {
                        let half = usize::from(index / (16 / lane));
                        let shift = u32::from(index % (16 / lane)) * u32::from(lane) * 8;
                        if masks[half] >> (shift + u32::from(lane) * 8 - 1) & 1 != 0 {
                            let lane_address = base + u64::from(index) * u64::from(lane);
                            let value = memory.read(lane_address, lane).map_err(|()| {
                                ExecutionExit::OperandFault(crate::FaultAccess::operand(
                                    instruction,
                                    lane_address,
                                    AccessKind::Read,
                                    u64::from(lane),
                                ))
                            })?;
                            output[half] |= u128::from(value) << shift;
                        }
                    }
                    staged.vectors[usize::from(vector)] = output[0];
                    staged.vector_upper[usize::from(vector)] = if wide { output[1] } else { 0 };
                    Ok(Staged::Cpu)
                }
            }
            ScalarInstruction::VectorMaskedStore {
                source,
                mask,
                mmx,
                address,
            } => {
                let base = address.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
                let data = if mmx {
                    u128::from(cpu.read_mmx(source))
                } else {
                    cpu.vectors[usize::from(source)]
                };
                let selection = if mmx {
                    u128::from(cpu.read_mmx(mask))
                } else {
                    cpu.vectors[usize::from(mask)]
                };
                let bytes = if mmx { 8_u8 } else { 16 };
                let mut reservations = std::array::from_fn(|_| None);
                let mut values = [0_u64; 8];
                let mut count = 0_usize;
                let mut index = 0_u8;
                while index < bytes {
                    if selection >> (u32::from(index) * 8 + 7) & 1 == 0 {
                        index += 1;
                        continue;
                    }
                    let start = index;
                    while index < bytes && index - start < 8 && selection >> (u32::from(index) * 8 + 7) & 1 != 0 {
                        index += 1;
                    }
                    let length = index - start;
                    let lane_address = base + u64::from(start);
                    reservations[count] = Some(memory.reserve_write(lane_address, length).map_err(|()| {
                        ExecutionExit::OperandFault(crate::FaultAccess::operand(
                            instruction,
                            lane_address,
                            AccessKind::Write,
                            u64::from(length),
                        ))
                    })?);
                    values[count] = (data >> (u32::from(start) * 8)) as u64;
                    count += 1;
                }
                Ok(Staged::Sparse(
                    Box::new(crate::x86::SparseWrites { reservations, values }),
                    count as u8,
                    base,
                    bytes,
                ))
            }
            ScalarInstruction::VexAes {
                operation,
                destination,
                first,
                second,
            } => {
                let second = Self::vex_read_bytes(cpu, memory, second, 16, next, instruction)?[0];
                let first = cpu.vectors[usize::from(first)];
                staged.vectors[usize::from(destination)] = crate::x86::vector::Aes::execute(first, second, operation);
                staged.vector_upper[usize::from(destination)] = 0;
                Ok(Staged::Cpu)
            }
            ScalarInstruction::VexScalarMerge {
                destination,
                first,
                second,
                double,
            } => {
                let VectorSource::Register(second) = second else {
                    return Err(ExecutionExit::UndefinedInstruction { instruction });
                };
                let element_bits = if double { 64 } else { 32 };
                let mask = (1_u128 << element_bits) - 1;
                staged.vectors[usize::from(destination)] =
                    (cpu.vectors[usize::from(first)] & !mask) | (cpu.vectors[usize::from(second)] & mask);
                staged.vector_upper[usize::from(destination)] = 0;
                Ok(Staged::Cpu)
            }
            ScalarInstruction::VexScalarLoad {
                destination,
                source,
                double,
            } => {
                let bytes = if double { 8 } else { 4 };
                staged.vectors[usize::from(destination)] =
                    Self::vex_read_bytes(cpu, memory, source, bytes, next, instruction)?[0]
                        & if double {
                            u128::from(u64::MAX)
                        } else {
                            u128::from(u32::MAX)
                        };
                staged.vector_upper[usize::from(destination)] = 0;
                Ok(Staged::Cpu)
            }
            ScalarInstruction::VexScalarMultiply {
                destination,
                first,
                second,
            } => {
                let right = Self::vex_read_bytes(cpu, memory, second, 4, next, instruction)?[0] as u32;
                let left = cpu.vectors[usize::from(first)] as u32;
                let (result, exceptions) = crate::x86::vector::Half::multiply_single(left, right, cpu.mxcsr);
                staged.vectors[usize::from(destination)] =
                    (cpu.vectors[usize::from(first)] & !u128::from(u32::MAX)) | u128::from(result);
                staged.vector_upper[usize::from(destination)] = 0;
                staged.mxcsr |= exceptions;
                Ok(Staged::Cpu)
            }
            ScalarInstruction::VexFma {
                operation,
                form,
                destination,
                first,
                second,
                format,
                scalar,
                wide,
            } => {
                let lane_bytes = crate::x86::scalar::arithmetic::Arithmetic::bytes(format);
                let source_bytes = if scalar {
                    lane_bytes
                } else if wide {
                    32
                } else {
                    16
                };
                let right = Self::vex_read_bytes(cpu, memory, second, source_bytes, next, instruction)?;
                let old = [
                    cpu.vectors[usize::from(destination)],
                    cpu.vector_upper[usize::from(destination)],
                ];
                let first = [cpu.vectors[usize::from(first)], cpu.vector_upper[usize::from(first)]];
                let mut output = old;
                let lane_bits = u32::from(lane_bytes) * 8;
                let lanes = if scalar {
                    1
                } else if wide {
                    256 / lane_bits
                } else {
                    128 / lane_bits
                };
                let mut exceptions = 0;
                for lane in 0..lanes {
                    let half = (lane * lane_bits / 128) as usize;
                    let shift = lane * lane_bits % 128;
                    let dst = Self::fma_lane(old[half], shift, format);
                    let vex = Self::fma_lane(first[half], shift, format);
                    let rm = Self::fma_lane(right[half], shift, format);
                    let (a, b, c) = match form {
                        FmaForm::Form132 => (dst, rm, vex),
                        FmaForm::Form213 => (vex, dst, rm),
                        FmaForm::Form231 => (vex, rm, dst),
                    };
                    let subtract = match operation {
                        FmaOperation::Subtract | FmaOperation::NegativeSubtract => true,
                        FmaOperation::AddSubtract => lane & 1 == 0,
                        FmaOperation::SubtractAdd => lane & 1 != 0,
                        _ => false,
                    };
                    let negative = matches!(operation, FmaOperation::NegativeAdd | FmaOperation::NegativeSubtract);
                    let (value, raised) = Self::fma(a, b, c, format, negative, subtract, cpu.mxcsr);
                    exceptions |= raised;
                    output[half] = Self::fma_merge(output[half], value, shift, format);
                }
                staged.vectors[usize::from(destination)] = output[0];
                staged.vector_upper[usize::from(destination)] = if wide && !scalar { output[1] } else { 0 };
                staged.mxcsr |= exceptions;
                Ok(Staged::Cpu)
            }
            ScalarInstruction::VexHalfWiden {
                destination,
                source,
                wide,
            } => {
                let input_bytes = if wide { 16 } else { 8 };
                let input = Self::vex_read_bytes(cpu, memory, source, input_bytes, next, instruction)?;
                let lanes = if wide { 8 } else { 4 };
                let mut output = [0_u128; 2];
                for lane in 0..lanes {
                    let half = (input[lane / 8] >> ((lane % 8) * 16)) as u16;
                    output[lane / 4] |= u128::from(crate::x86::vector::Half::widen(half)) << ((lane % 4) * 32);
                }
                staged.vectors[usize::from(destination)] = output[0];
                staged.vector_upper[usize::from(destination)] = if wide { output[1] } else { 0 };
                Ok(Staged::Cpu)
            }
            ScalarInstruction::VexPackedDoubleConvert {
                destination,
                source,
                from_integer,
                truncate,
                wide,
            } => {
                use crate::x86::scalar::arithmetic::Arithmetic as FloatArithmetic;
                use hl_softfloat::{ExceptionFlags, RoundingMode, Value};
                let lanes = if wide { 4_usize } else { 2 };
                let input_bytes = (if from_integer { lanes * 4 } else { lanes * 8 }) as u8;
                let input = Self::vex_read_bytes(cpu, memory, source, input_bytes, next, instruction)?;
                let mut environment = FloatArithmetic::environment(cpu.mxcsr);
                if truncate {
                    environment.rounding = RoundingMode::TowardZero;
                }
                let mut output = [0_u128; 2];
                let mut exceptions = 0;
                for lane in 0..lanes {
                    if from_integer {
                        let value = (input[lane / 4] >> ((lane % 4) * 32)) as u32 as i32;
                        let result = environment.from_signed(hl_softfloat::Format::Binary64, i64::from(value));
                        output[lane / 2] |= u128::from(result.value.bits()) << ((lane % 2) * 64);
                        exceptions |= FloatArithmetic::exceptions(result.flags);
                    } else {
                        let bits = (input[lane / 2] >> ((lane % 2) * 64)) as u64;
                        let result = environment.to_signed(Value::from_bits(hl_softfloat::Format::Binary64, bits), 32);
                        let value = if result.flags.contains(ExceptionFlags::INVALID) {
                            0x8000_0000
                        } else {
                            result.value as u32
                        };
                        output[0] |= u128::from(value) << (lane * 32);
                        exceptions |= FloatArithmetic::exceptions(result.flags) & !(1 << 1);
                    }
                }
                staged.vectors[usize::from(destination)] = output[0];
                staged.vector_upper[usize::from(destination)] = if from_integer && wide { output[1] } else { 0 };
                staged.mxcsr |= exceptions;
                Ok(Staged::Cpu)
            }
            ScalarInstruction::VexHalfNarrow {
                source,
                destination,
                wide,
                control,
            } => {
                let lanes = if wide { 8 } else { 4 };
                let input = [cpu.vectors[usize::from(source)], cpu.vector_upper[usize::from(source)]];
                let mut output = 0_u128;
                let mut exceptions = 0;
                for lane in 0..lanes {
                    let single = (input[lane / 4] >> ((lane % 4) * 32)) as u32;
                    let (half, raised) = crate::x86::vector::Half::narrow(single, control, cpu.mxcsr);
                    output |= u128::from(half) << (lane * 16);
                    exceptions |= raised;
                }
                staged.mxcsr |= exceptions;
                if let VectorSource::Register(destination) = destination {
                    staged.vectors[usize::from(destination)] = output;
                    staged.vector_upper[usize::from(destination)] = 0;
                    Ok(Staged::Cpu)
                } else {
                    let VectorSource::Memory(address) = destination else {
                        unreachable!()
                    };
                    let address = address.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
                    let length = if wide { 16 } else { 8 };
                    let writes = if wide {
                        [(address, 8), (address + 8, 8)]
                    } else {
                        [(address, 8), (address, 0)]
                    };
                    let reservation =
                        memory
                            .reserve_write_batch(&writes[..usize::from(length / 8)])
                            .map_err(|fault| {
                                ExecutionExit::OperandFault(crate::FaultAccess::operand(
                                    instruction,
                                    fault,
                                    AccessKind::Write,
                                    u64::from(length),
                                ))
                            })?;
                    Ok(Staged::Batch(
                        reservation,
                        [output as u64, (output >> 64) as u64, 0, 0],
                        address,
                        length,
                    ))
                }
            }
            ScalarInstruction::VexGeneralToVector {
                destination,
                source,
                wide,
            } => {
                let width = if wide { ScalarWidth::Qword } else { ScalarWidth::Dword };
                let value = Self::read(cpu, memory, source, width, next, instruction)?;
                staged.vectors[usize::from(destination)] = u128::from(value);
                staged.vector_upper[usize::from(destination)] = 0;
                Ok(Staged::Cpu)
            }
            ScalarInstruction::VexQword { vector, operand, store } => {
                if store {
                    let value = cpu.vectors[usize::from(vector)] as u64;
                    match operand {
                        VectorSource::Register(destination) => {
                            staged.vectors[usize::from(destination)] = u128::from(value);
                            staged.vector_upper[usize::from(destination)] = 0;
                            Ok(Staged::Cpu)
                        }
                        VectorSource::Memory(address) => {
                            let address = address.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
                            let reservation = memory.reserve_write(address, 8).map_err(|()| {
                                ExecutionExit::OperandFault(crate::FaultAccess::operand(
                                    instruction,
                                    address,
                                    AccessKind::Write,
                                    8,
                                ))
                            })?;
                            Ok(Staged::Write(reservation, value, address, 8))
                        }
                    }
                } else {
                    let value = Self::vex_read_bytes(cpu, memory, operand, 8, next, instruction)?[0] as u64;
                    staged.vectors[usize::from(vector)] = u128::from(value);
                    staged.vector_upper[usize::from(vector)] = 0;
                    Ok(Staged::Cpu)
                }
            }
            ScalarInstruction::VexHalfMove {
                destination,
                first,
                second,
                high,
            } => {
                let selected = match second {
                    VectorSource::Register(source) => {
                        let value = cpu.vectors[usize::from(source)];
                        if high { value as u64 } else { (value >> 64) as u64 }
                    }
                    VectorSource::Memory(_) => {
                        Self::vex_read_bytes(cpu, memory, second, 8, next, instruction)?[0] as u64
                    }
                };
                let first = cpu.vectors[usize::from(first)];
                staged.vectors[usize::from(destination)] = if high {
                    (first & u128::from(u64::MAX)) | (u128::from(selected) << 64)
                } else {
                    (first & (u128::from(u64::MAX) << 64)) | u128::from(selected)
                };
                staged.vector_upper[usize::from(destination)] = 0;
                Ok(Staged::Cpu)
            }
            ScalarInstruction::VexHalfStore { source, address, high } => {
                let address = address.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
                let vector = cpu.vectors[usize::from(source)];
                let value = if high { (vector >> 64) as u64 } else { vector as u64 };
                let reservation = memory.reserve_write(address, 8).map_err(|()| {
                    ExecutionExit::OperandFault(crate::FaultAccess::operand(instruction, address, AccessKind::Write, 8))
                })?;
                Ok(Staged::Write(reservation, value, address, 8))
            }
            ScalarInstruction::VexMask {
                destination,
                source,
                lane,
                wide,
            } => {
                let mut mask = u64::from(VectorLane::sign_mask(cpu.vectors[usize::from(source)], lane));
                if wide {
                    mask |=
                        u64::from(VectorLane::sign_mask(cpu.vector_upper[usize::from(source)], lane)) << (16 / lane);
                }
                staged.write_register(destination, ScalarWidth::Dword, mask);
                Ok(Staged::Cpu)
            }
            ScalarInstruction::VexDwordToSingle {
                destination,
                source,
                wide,
                to_integer,
                truncate,
            } => {
                let input = Self::vex_read(cpu, memory, source, wide, next, instruction)?;
                let mut environment = crate::x86::scalar::arithmetic::Arithmetic::environment(cpu.mxcsr);
                if truncate {
                    environment.rounding = hl_softfloat::RoundingMode::TowardZero;
                }
                let mut output = [0_u128; 2];
                let mut exceptions = 0;
                for lane in 0..if wide { 8 } else { 4 } {
                    let bits = (input[lane / 4] >> ((lane % 4) * 32)) as u32;
                    let value = if to_integer {
                        let result = environment.to_signed(
                            hl_softfloat::Value::from_bits(hl_softfloat::Format::Binary32, u64::from(bits)),
                            32,
                        );
                        exceptions |= crate::x86::scalar::arithmetic::Arithmetic::exceptions(result.flags) & !(1 << 1);
                        if result.flags.contains(hl_softfloat::ExceptionFlags::INVALID) {
                            0x8000_0000
                        } else {
                            result.value as u32
                        }
                    } else {
                        let result = environment.from_signed(hl_softfloat::Format::Binary32, i64::from(bits as i32));
                        exceptions |= crate::x86::scalar::arithmetic::Arithmetic::exceptions(result.flags);
                        result.value.bits() as u32
                    };
                    output[lane / 4] |= u128::from(value) << ((lane % 4) * 32);
                }
                staged.vectors[usize::from(destination)] = output[0];
                staged.vector_upper[usize::from(destination)] = if wide { output[1] } else { 0 };
                staged.mxcsr |= exceptions;
                Ok(Staged::Cpu)
            }
            ScalarInstruction::VexFloatWidth {
                destination,
                first,
                source,
                destination_format,
                packed,
                wide,
            } => {
                use crate::x86::scalar::arithmetic::Arithmetic as FloatArithmetic;
                let source_format = match destination_format {
                    crate::FloatWidth::Single => crate::FloatWidth::Double,
                    crate::FloatWidth::Double => crate::FloatWidth::Single,
                };
                let source_bits = u32::from(FloatArithmetic::bytes(source_format)) * 8;
                let destination_bits = u32::from(FloatArithmetic::bytes(destination_format)) * 8;
                let lanes = if packed {
                    if wide {
                        256 / destination_bits
                    } else {
                        128 / destination_bits
                    }
                } else {
                    1
                };
                let input_bytes = if packed {
                    (lanes * source_bits / 8) as u8
                } else {
                    FloatArithmetic::bytes(source_format)
                };
                let input = Self::vex_read_bytes(cpu, memory, source, input_bytes, next, instruction)?;
                let mut output = if packed {
                    [0_u128; 2]
                } else {
                    [cpu.vectors[usize::from(first)], 0]
                };
                let environment = FloatArithmetic::environment(cpu.mxcsr);
                let mut exceptions = 0;
                for lane in 0..lanes {
                    let source_half = (lane * source_bits / 128) as usize;
                    let source_shift = lane * source_bits % 128;
                    let mask = if source_bits == 64 {
                        u128::from(u64::MAX)
                    } else {
                        u128::from(u32::MAX)
                    };
                    let bits = ((input[source_half] >> source_shift) & mask) as u64;
                    let result = environment.convert(
                        hl_softfloat::Value::from_bits(FloatArithmetic::soft_format(source_format), bits),
                        FloatArithmetic::soft_format(destination_format),
                    );
                    let value = crate::x86::scalar::conversion::Conversion::converted_nan(
                        result.value.bits(),
                        bits,
                        source_format,
                        destination_format,
                    );
                    let destination_half = (lane * destination_bits / 128) as usize;
                    let destination_shift = lane * destination_bits % 128;
                    let destination_mask = if destination_bits == 64 {
                        u128::from(u64::MAX)
                    } else {
                        u128::from(u32::MAX)
                    } << destination_shift;
                    output[destination_half] =
                        (output[destination_half] & !destination_mask) | (u128::from(value) << destination_shift);
                    exceptions |= FloatArithmetic::exceptions(result.flags);
                    if cpu.mxcsr & (1 << 6) != 0 {
                        exceptions &= !(1 << 1);
                    } else if FloatArithmetic::denormal(bits, source_format) {
                        exceptions |= 1 << 1;
                    }
                }
                staged.vectors[usize::from(destination)] = output[0];
                staged.vector_upper[usize::from(destination)] =
                    if packed && wide && destination_format == crate::FloatWidth::Double {
                        output[1]
                    } else {
                        0
                    };
                staged.mxcsr |= exceptions;
                Ok(Staged::Cpu)
            }
            ScalarInstruction::VexCompare {
                destination,
                first,
                second,
                format,
                scalar,
                wide,
                predicate,
            } => {
                let right = Self::vex_read(cpu, memory, second, wide, next, instruction)?;
                let left = [cpu.vectors[usize::from(first)], cpu.vector_upper[usize::from(first)]];
                let result = Self::vex_compare(left, right, format, scalar, wide, predicate);
                staged.vectors[usize::from(destination)] = result[0];
                staged.vector_upper[usize::from(destination)] = if wide && !scalar { result[1] } else { 0 };
                Ok(Staged::Cpu)
            }
            ScalarInstruction::VexRound {
                destination,
                first,
                source,
                format,
                scalar,
                wide,
                control,
            } => {
                let bytes = crate::x86::scalar::arithmetic::Arithmetic::bytes(format);
                let input = Self::vex_read_bytes(
                    cpu,
                    memory,
                    source,
                    if scalar {
                        bytes
                    } else if wide {
                        32
                    } else {
                        16
                    },
                    next,
                    instruction,
                )?;
                let mut output = if scalar {
                    [cpu.vectors[usize::from(first)], 0]
                } else {
                    [0, 0]
                };
                let mut environment = crate::x86::scalar::arithmetic::Arithmetic::environment(cpu.mxcsr);
                environment.rounding = match if control & 4 != 0 {
                    cpu.mxcsr >> 13 & 3
                } else {
                    u32::from(control & 3)
                } {
                    0 => hl_softfloat::RoundingMode::NearestEven,
                    1 => hl_softfloat::RoundingMode::TowardNegative,
                    2 => hl_softfloat::RoundingMode::TowardPositive,
                    _ => hl_softfloat::RoundingMode::TowardZero,
                };
                let lane_bits = u32::from(bytes) * 8;
                let lanes = if scalar {
                    1
                } else if wide {
                    256 / lane_bits
                } else {
                    128 / lane_bits
                };
                for lane in 0..lanes {
                    let half = (lane / (128 / lane_bits)) as usize;
                    let shift = lane % (128 / lane_bits) * lane_bits;
                    let bits = if bytes == 4 {
                        (input[half] >> shift) as u32 as u64
                    } else {
                        (input[half] >> shift) as u64
                    };
                    let result = environment.round_to_integral(
                        hl_softfloat::Value::from_bits(
                            crate::x86::scalar::arithmetic::Arithmetic::soft_format(format),
                            bits,
                        ),
                        control & 8 == 0,
                    );
                    let value = if crate::x86::scalar::arithmetic::Arithmetic::infinity(bits, format) {
                        bits
                    } else {
                        result.value.bits()
                    };
                    let mask = if bytes == 4 {
                        u128::from(u32::MAX)
                    } else {
                        u128::from(u64::MAX)
                    };
                    output[half] = (output[half] & !(mask << shift)) | ((u128::from(value) & mask) << shift);
                    let exceptions = crate::x86::scalar::arithmetic::Arithmetic::exceptions(result.flags) & !(1 << 1);
                    staged.mxcsr |= exceptions;
                }
                staged.vectors[usize::from(destination)] = output[0];
                staged.vector_upper[usize::from(destination)] = if wide && !scalar { output[1] } else { 0 };
                Ok(Staged::Cpu)
            }
            ScalarInstruction::VexBlend {
                destination,
                first,
                second,
                mask,
                lane,
                wide,
            } => {
                let right = Self::vex_read(cpu, memory, second, wide, next, instruction)?;
                let left = [cpu.vectors[usize::from(first)], cpu.vector_upper[usize::from(first)]];
                let masks = [cpu.vectors[usize::from(mask)], cpu.vector_upper[usize::from(mask)]];
                let result = Self::vex_blend(left, right, masks, lane, wide);
                staged.vectors[usize::from(destination)] = result[0];
                staged.vector_upper[usize::from(destination)] = if wide { result[1] } else { 0 };
                Ok(Staged::Cpu)
            }
            ScalarInstruction::VexBinary {
                operation,
                destination,
                first,
                second,
                wide,
                immediate,
            } => {
                let source_bytes = match operation {
                    VexOperation::BroadcastByte => 1,
                    VexOperation::BroadcastWord => 2,
                    VexOperation::BroadcastDword => 4,
                    VexOperation::BroadcastQword => 8,
                    VexOperation::Broadcast128 => 16,
                    VexOperation::DuplicateDouble if !wide => 8,
                    VexOperation::Insert128 => 16,
                    VexOperation::Widen { from, to, .. } => (if wide { 32 } else { 16 }) * from / to,
                    _ => {
                        if wide {
                            32
                        } else {
                            16
                        }
                    }
                };
                let right = Self::vex_read_bytes(cpu, memory, second, source_bytes, next, instruction)?;
                let left = [cpu.vectors[usize::from(first)], cpu.vector_upper[usize::from(first)]];
                if matches!(operation, VexOperation::DotSingle | VexOperation::DotDouble) {
                    let format = if operation == VexOperation::DotSingle {
                        crate::FloatWidth::Single
                    } else {
                        crate::FloatWidth::Double
                    };
                    let mut output = [0_u128; 2];
                    let mut exceptions = 0;
                    for half in 0..if wide { 2 } else { 1 } {
                        let (result, raised) = VectorLane::dot(left[half], right[half], immediate, format, cpu.mxcsr);
                        output[half] = result;
                        exceptions |= raised;
                    }
                    staged.vectors[usize::from(destination)] = output[0];
                    staged.vector_upper[usize::from(destination)] = if wide { output[1] } else { 0 };
                    staged.mxcsr |= exceptions;
                    return Ok(Staged::Cpu);
                }
                let result = Self::vex_binary(operation, left, right, wide, immediate);
                staged.vectors[usize::from(destination)] = result[0];
                staged.vector_upper[usize::from(destination)] = if wide { result[1] } else { 0 };
                Ok(Staged::Cpu)
            }
            ScalarInstruction::VexImmediateShift {
                operation,
                destination,
                source,
                lane,
                wide,
                count,
            } => {
                let input = [cpu.vectors[usize::from(source)], cpu.vector_upper[usize::from(source)]];
                let result = Self::vex_immediate_shift(input, operation, lane, wide, count);
                staged.vectors[usize::from(destination)] = result[0];
                staged.vector_upper[usize::from(destination)] = if wide { result[1] } else { 0 };
                Ok(Staged::Cpu)
            }
            ScalarInstruction::VexScalarCountShift {
                operation,
                destination,
                source,
                count,
                lane,
                wide,
            } => {
                let count = Self::vex_read_bytes(cpu, memory, count, 16, next, instruction)?[0] as u64;
                let input = [cpu.vectors[usize::from(source)], cpu.vector_upper[usize::from(source)]];
                let result = Self::vex_scalar_count_shift(input, operation, lane, wide, count);
                staged.vectors[usize::from(destination)] = result[0];
                staged.vector_upper[usize::from(destination)] = if wide { result[1] } else { 0 };
                Ok(Staged::Cpu)
            }
            ScalarInstruction::VexExtract128 {
                source,
                destination,
                high,
            } => {
                let value = if high {
                    cpu.vector_upper[usize::from(source)]
                } else {
                    cpu.vectors[usize::from(source)]
                };
                match destination {
                    VectorSource::Register(destination) => {
                        staged.vectors[usize::from(destination)] = value;
                        staged.vector_upper[usize::from(destination)] = 0;
                        Ok(Staged::Cpu)
                    }
                    VectorSource::Memory(address) => {
                        let address = address.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
                        let writes = [(address, 8), (address + 8, 8)];
                        let reservation = memory.reserve_write_batch(&writes).map_err(|fault| {
                            ExecutionExit::OperandFault(crate::FaultAccess::operand(
                                instruction,
                                fault,
                                AccessKind::Write,
                                16,
                            ))
                        })?;
                        Ok(Staged::Batch(
                            reservation,
                            [value as u64, (value >> 64) as u64, 0, 0],
                            address,
                            16,
                        ))
                    }
                }
            }
            ScalarInstruction::VexVectorToGeneral {
                source,
                destination,
                wide,
            } => {
                staged.write_register(
                    destination,
                    if wide { ScalarWidth::Qword } else { ScalarWidth::Dword },
                    cpu.vectors[usize::from(source)] as u64,
                );
                Ok(Staged::Cpu)
            }
            ScalarInstruction::VexZeroUpper => {
                staged.vector_upper.fill(0);
                Ok(Staged::Cpu)
            }
            ScalarInstruction::VexZeroAll => {
                staged.vectors.fill(0);
                staged.vector_upper.fill(0);
                Ok(Staged::Cpu)
            }
            ScalarInstruction::VectorHalf {
                vector,
                source,
                store,
                high,
            } => VectorMemory::half(staged, cpu, memory, vector, source, store, high, next, instruction),
            ScalarInstruction::VectorDuplicate {
                destination,
                source,
                lane,
                high,
            } => VectorMemory::duplicate(staged, cpu, memory, destination, source, lane, high, next, instruction),
            operation @ (ScalarInstruction::MmxScalar { .. }
            | ScalarInstruction::MmxTransport { .. }
            | ScalarInstruction::MmxVector { .. }
            | ScalarInstruction::MmxExtractWord { .. }
            | ScalarInstruction::MmxMask { .. }
            | ScalarInstruction::MmxInsertWord { .. }
            | ScalarInstruction::MmxPacked { .. }
            | ScalarInstruction::MmxShift { .. }
            | ScalarInstruction::MmxEmpty) => {
                crate::x86::mmx::Mmx::stage(staged, cpu, memory, operation, ir.width, next, instruction)
            }
            ScalarInstruction::BitOperation { .. }
            | ScalarInstruction::VectorPack { .. }
            | ScalarInstruction::Increment { .. }
            | ScalarInstruction::DoubleShift { .. }
            | ScalarInstruction::X87Control { .. }
            | ScalarInstruction::X87Extended { .. }
            | ScalarInstruction::X87Float { .. }
            | ScalarInstruction::X87Compare { .. }
            | ScalarInstruction::X87ConditionalMove { .. }
            | ScalarInstruction::X87Stack { .. }
            | ScalarInstruction::X87Initialize
            | ScalarInstruction::X87Status
            | ScalarInstruction::X87StatusStore { .. }
            | ScalarInstruction::X87Constant { .. }
            | ScalarInstruction::X87Environment { .. }
            | ScalarInstruction::X87Arithmetic { .. }
            | ScalarInstruction::X87StatusCompare { .. }
            | ScalarInstruction::X87Integer { .. }
            | ScalarInstruction::X87Unary { .. }
            | ScalarInstruction::X87Save { .. }
            | ScalarInstruction::VectorScalarMove { .. }
            | ScalarInstruction::MxcsrControl { .. }
            | ScalarInstruction::Fxsave { .. }
            | ScalarInstruction::Fxrstor { .. }
            | ScalarInstruction::ConvertFloatInteger { .. }
            | ScalarInstruction::ConvertIntegerFloat { .. }
            | ScalarInstruction::ConvertFloatWidth { .. }
            | ScalarInstruction::ConvertPackedSingle { .. }
            | ScalarInstruction::ConvertPackedDouble { .. }
            | ScalarInstruction::VectorFloatArithmetic { .. }
            | ScalarInstruction::VectorRound { .. }
            | ScalarInstruction::VectorPairArithmetic { .. }
            | ScalarInstruction::VexPairArithmetic { .. }
            | ScalarInstruction::VexFloatArithmetic { .. }
            | ScalarInstruction::VexGather { .. }
            | ScalarInstruction::VectorFloatCompare { .. }
            | ScalarInstruction::VectorScalarCompare { .. } => unreachable!(),
            _ => unreachable!("scalar-only instruction reached full-state staging"),
        }
    }

    fn vex_read<M: GuestOperandMemory>(
        cpu: &CpuState,
        memory: &M,
        source: VectorSource,
        wide: bool,
        next: u64,
        instruction: u64,
    ) -> Result<[u128; 2], ExecutionExit> {
        Self::vex_read_bytes(cpu, memory, source, if wide { 32 } else { 16 }, next, instruction)
    }

    pub(crate) fn vex_gather<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &M,
        destination: u8,
        mask: u8,
        index: u8,
        address: crate::EffectiveAddress,
        element: u8,
        index_bytes: u8,
        wide: bool,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let width = if wide { 32 } else { 16 };
        let lanes = if index_bytes == 4 { width / element } else { width / 8 };
        let result_bytes = lanes * element;
        let indices = [cpu.vectors[usize::from(index)], cpu.vector_upper[usize::from(index)]];
        let mut destination_value = [
            cpu.vectors[usize::from(destination)],
            cpu.vector_upper[usize::from(destination)],
        ];
        let mut mask_value = [cpu.vectors[usize::from(mask)], cpu.vector_upper[usize::from(mask)]];
        let element_mask = if element == 8 {
            u128::from(u64::MAX)
        } else {
            u128::from(u32::MAX)
        };
        let mut base = address.base.map_or(0, |register| cpu.registers[usize::from(register)]);
        base = base.wrapping_add(address.displacement as u64);
        let segment = match address.segment {
            Some(crate::Segment::Fs) => cpu.fs_base,
            Some(crate::Segment::Gs) => cpu.gs_base,
            None => 0,
        };
        for lane in 0..lanes {
            let mask_shift = lane * element * 8;
            let mask_half = usize::from(mask_shift / 128);
            let mask_offset = mask_shift % 128;
            if ((mask_value[mask_half] >> (mask_offset + element * 8 - 1)) & 1) != 0 {
                let index_shift = lane * index_bytes * 8;
                let index_half = usize::from(index_shift / 128);
                let index_offset = index_shift % 128;
                let offset = if index_bytes == 4 {
                    ((indices[index_half] >> index_offset) as u32 as i32) as i64
                } else {
                    (indices[index_half] >> index_offset) as u64 as i64
                };
                let mut guest = base.wrapping_add((offset as u64).wrapping_shl(u32::from(address.scale)));
                if address.address_32 {
                    guest = u64::from(guest as u32);
                }
                guest = guest.wrapping_add(segment);
                let Ok(value) = memory.read(guest, element) else {
                    cpu.vectors[usize::from(destination)] = destination_value[0];
                    cpu.vector_upper[usize::from(destination)] = destination_value[1];
                    cpu.vectors[usize::from(mask)] = mask_value[0];
                    cpu.vector_upper[usize::from(mask)] = mask_value[1];
                    return ExecutionExit::OperandFault(crate::FaultAccess::operand(
                        instruction,
                        guest,
                        AccessKind::Read,
                        u64::from(element),
                    ));
                };
                destination_value[mask_half] = destination_value[mask_half] & !(element_mask << mask_offset)
                    | (u128::from(value) & element_mask) << mask_offset;
            }
            mask_value[mask_half] &= !(element_mask << mask_offset);
        }
        let result_mask = if result_bytes >= 32 {
            [u128::MAX, u128::MAX]
        } else if result_bytes > 16 {
            [u128::MAX, (1_u128 << ((result_bytes - 16) * 8)) - 1]
        } else if result_bytes == 16 {
            [u128::MAX, 0]
        } else {
            [(1_u128 << (result_bytes * 8)) - 1, 0]
        };
        cpu.vectors[usize::from(destination)] = destination_value[0] & result_mask[0];
        cpu.vector_upper[usize::from(destination)] = destination_value[1] & result_mask[1];
        cpu.vectors[usize::from(mask)] = 0;
        cpu.vector_upper[usize::from(mask)] = 0;
        cpu.rip = next;
        ExecutionExit::Continue
    }

    fn vex_read_bytes<M: GuestOperandMemory>(
        cpu: &CpuState,
        memory: &M,
        source: VectorSource,
        bytes: u8,
        next: u64,
        instruction: u64,
    ) -> Result<[u128; 2], ExecutionExit> {
        if let VectorSource::Register(register) = source {
            return Ok([
                cpu.vectors[usize::from(register)],
                cpu.vector_upper[usize::from(register)],
            ]);
        }
        let VectorSource::Memory(address) = source else {
            unreachable!()
        };
        let address = address.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
        let chunks = usize::from(bytes.div_ceil(8));
        let mut words = [0_u64; 4];
        for (index, word) in words[..chunks].iter_mut().enumerate() {
            let cursor = address.wrapping_add((index * 8) as u64);
            let chunk = bytes.saturating_sub((index * 8) as u8).min(8);
            *word = memory.read(cursor, chunk).map_err(|()| {
                ExecutionExit::OperandFault(crate::FaultAccess::operand(
                    instruction,
                    cursor,
                    AccessKind::Read,
                    u64::from(bytes),
                ))
            })?;
        }
        Ok([
            u128::from(words[0]) | (u128::from(words[1]) << 64),
            u128::from(words[2]) | (u128::from(words[3]) << 64),
        ])
    }

    fn fma(
        a: u64,
        b: u64,
        c: u64,
        format: crate::FloatWidth,
        negative: bool,
        subtract: bool,
        mxcsr: u32,
    ) -> (u64, u32) {
        let nan = |bits| Self::fma_nan(bits, format);
        let quiet = match format {
            crate::FloatWidth::Single => 1 << 22,
            crate::FloatWidth::Double => 1_u64 << 51,
        };
        if nan(a) || nan(b) || nan(c) {
            let invalid = [a, b, c].into_iter().any(|bits| nan(bits) && bits & quiet == 0);
            return (
                (if nan(a) {
                    a
                } else if nan(b) {
                    b
                } else {
                    c
                }) | quiet,
                u32::from(invalid),
            );
        }
        let sign = match format {
            crate::FloatWidth::Single => 1 << 31,
            crate::FloatWidth::Double => 1_u64 << 63,
        };
        let left = if negative { a ^ sign } else { a };
        let addend = if subtract { c ^ sign } else { c };
        let environment = crate::x86::scalar::arithmetic::Arithmetic::environment(mxcsr);
        let soft = crate::x86::scalar::arithmetic::Arithmetic::soft_format(format);
        let result = environment.fused_multiply_add(
            hl_softfloat::Value::from_bits(soft, left),
            hl_softfloat::Value::from_bits(soft, b),
            hl_softfloat::Value::from_bits(soft, addend),
        );
        let mut bits = result.value.bits();
        if Self::fma_nan(bits, format) {
            bits = match format {
                crate::FloatWidth::Single => 0xffc0_0000,
                crate::FloatWidth::Double => 0xfff8_0000_0000_0000,
            };
        }
        let mut raised = crate::x86::scalar::arithmetic::Arithmetic::exceptions(result.flags);
        if mxcsr & (1 << 6) != 0 {
            raised &= !(1 << 1);
        } else if [a, b, c]
            .into_iter()
            .any(|bits| crate::x86::scalar::arithmetic::Arithmetic::denormal(bits, format))
        {
            raised |= 1 << 1;
        }
        (bits, raised)
    }

    const fn fma_nan(bits: u64, format: crate::FloatWidth) -> bool {
        match format {
            crate::FloatWidth::Single => bits & 0x7f80_0000 == 0x7f80_0000 && bits & 0x007f_ffff != 0,
            crate::FloatWidth::Double => {
                bits & 0x7ff0_0000_0000_0000 == 0x7ff0_0000_0000_0000 && bits & 0x000f_ffff_ffff_ffff != 0
            }
        }
    }

    const fn fma_lane(vector: u128, shift: u32, format: crate::FloatWidth) -> u64 {
        match format {
            crate::FloatWidth::Single => (vector >> shift) as u32 as u64,
            crate::FloatWidth::Double => (vector >> shift) as u64,
        }
    }

    const fn fma_merge(vector: u128, value: u64, shift: u32, format: crate::FloatWidth) -> u128 {
        let mask = match format {
            crate::FloatWidth::Single => u32::MAX as u128,
            crate::FloatWidth::Double => u64::MAX as u128,
        } << shift;
        vector & !mask | ((value as u128) << shift) & mask
    }

    fn vex_binary(operation: VexOperation, left: [u128; 2], right: [u128; 2], wide: bool, immediate: u8) -> [u128; 2] {
        if matches!(operation, VexOperation::ShuffleDword | VexOperation::ShuffleSingle) {
            let mut output = [0_u128; 2];
            for half in 0..if wide { 2 } else { 1 } {
                for lane in 0..4 {
                    let source = usize::from((immediate >> (lane * 2)) & 3);
                    let input = if operation == VexOperation::ShuffleSingle && lane < 2 {
                        left
                    } else {
                        right
                    };
                    output[half] |= ((input[half] >> (source * 32)) & u128::from(u32::MAX)) << (lane * 32);
                }
            }
            return output;
        }
        if let VexOperation::ShuffleWord { high } = operation {
            let mut output = [0_u128; 2];
            let base = if high { 4 } else { 0 };
            for half in 0..if wide { 2 } else { 1 } {
                output[half] = right[half]
                    & if high {
                        u128::from(u64::MAX)
                    } else {
                        u128::from(u64::MAX) << 64
                    };
                for lane in 0..4 {
                    let selected = usize::from((immediate >> (lane * 2)) & 3) + base;
                    let word = (right[half] >> (selected * 16)) & u128::from(u16::MAX);
                    output[half] |= word << ((base + lane) * 16);
                }
            }
            return output;
        }
        if operation == VexOperation::ShuffleDouble {
            let mut output = [0_u128; 2];
            for half in 0..if wide { 2 } else { 1 } {
                let low = left[half] >> (u32::from((immediate >> (half * 2)) & 1) * 64) & u128::from(u64::MAX);
                let high = right[half] >> (u32::from((immediate >> (half * 2 + 1)) & 1) * 64) & u128::from(u64::MAX);
                output[half] = low | high << 64;
            }
            return output;
        }
        if operation == VexOperation::Permute128 {
            let lanes = [left[0], left[1], right[0], right[1]];
            return [
                if immediate & 8 != 0 {
                    0
                } else {
                    lanes[usize::from(immediate & 3)]
                },
                if immediate & 0x80 != 0 {
                    0
                } else {
                    lanes[usize::from((immediate >> 4) & 3)]
                },
            ];
        }
        if operation == VexOperation::BroadcastByte {
            let byte = right[0] as u8;
            let repeated = u128::from_le_bytes([byte; 16]);
            return [repeated, if wide { repeated } else { 0 }];
        }
        if operation == VexOperation::BroadcastWord {
            let word = right[0] as u16;
            let repeated = (0..8).fold(0_u128, |value, lane| value | (u128::from(word) << (lane * 16)));
            return [repeated, if wide { repeated } else { 0 }];
        }
        if operation == VexOperation::BroadcastDword {
            let dword = right[0] as u32;
            let repeated = (0..4).fold(0_u128, |value, lane| value | (u128::from(dword) << (lane * 32)));
            return [repeated, if wide { repeated } else { 0 }];
        }
        if operation == VexOperation::BroadcastQword {
            let qword = right[0] as u64;
            let repeated = u128::from(qword) | (u128::from(qword) << 64);
            return [repeated, if wide { repeated } else { 0 }];
        }
        if matches!(
            operation,
            VexOperation::DuplicateDouble | VexOperation::DuplicateLowSingle | VexOperation::DuplicateHighSingle
        ) {
            let mut result = [0_u128; 2];
            for half in 0..if wide { 2 } else { 1 } {
                result[half] = match operation {
                    VexOperation::DuplicateDouble => {
                        let low = right[half] & u128::from(u64::MAX);
                        low | (low << 64)
                    }
                    VexOperation::DuplicateLowSingle | VexOperation::DuplicateHighSingle => {
                        let high = operation == VexOperation::DuplicateHighSingle;
                        let first = if high { right[half] >> 32 } else { right[half] } & u128::from(u32::MAX);
                        let second = if high { right[half] >> 96 } else { right[half] >> 64 } & u128::from(u32::MAX);
                        first | (first << 32) | (second << 64) | (second << 96)
                    }
                    _ => unreachable!(),
                };
            }
            return result;
        }
        if operation == VexOperation::Broadcast128 {
            return [right[0], right[0]];
        }
        if matches!(
            operation,
            VexOperation::MultiplyLowWord
                | VexOperation::MultiplyHighWordSigned
                | VexOperation::MultiplyHighWordUnsigned
                | VexOperation::MultiplyDwordSigned
                | VexOperation::MultiplyDwordUnsigned
        ) {
            let mut result = [0_u128; 2];
            for half in 0..if wide { 2 } else { 1 } {
                if matches!(
                    operation,
                    VexOperation::MultiplyDwordSigned | VexOperation::MultiplyDwordUnsigned
                ) {
                    for lane in 0..2 {
                        let shift = lane * 64;
                        let a = (left[half] >> shift) as u32;
                        let b = (right[half] >> shift) as u32;
                        let product = if operation == VexOperation::MultiplyDwordSigned {
                            u64::from_ne_bytes(((a as i32 as i64) * (b as i32 as i64)).to_ne_bytes())
                        } else {
                            u64::from(a) * u64::from(b)
                        };
                        result[half] |= u128::from(product) << shift;
                    }
                } else {
                    for lane in 0..8 {
                        let shift = lane * 16;
                        let a = (left[half] >> shift) as u16;
                        let b = (right[half] >> shift) as u16;
                        let product = match operation {
                            VexOperation::MultiplyLowWord => a.wrapping_mul(b),
                            VexOperation::MultiplyHighWordUnsigned => ((u32::from(a) * u32::from(b)) >> 16) as u16,
                            VexOperation::MultiplyHighWordSigned => {
                                (((a as i16 as i32) * (b as i16 as i32)) >> 16) as u16
                            }
                            _ => unreachable!(),
                        };
                        result[half] |= u128::from(product) << shift;
                    }
                }
            }
            return result;
        }
        if let VexOperation::Saturating {
            subtract,
            unsigned,
            word,
        } = operation
        {
            let bits = if word { 16 } else { 8 };
            let mask = (1_u128 << bits) - 1;
            let lanes = 128 / bits;
            let mut result = [0_u128; 2];
            for half in 0..if wide { 2 } else { 1 } {
                for lane in 0..lanes {
                    let shift = lane * bits;
                    let a = (left[half] >> shift) & mask;
                    let b = (right[half] >> shift) & mask;
                    let value = if unsigned {
                        let value = if subtract { a as i64 - b as i64 } else { (a + b) as i64 };
                        value.clamp(0, mask as i64) as u128
                    } else {
                        let sign = 1_u128 << (bits - 1);
                        let signed = |value: u128| ((value ^ sign) as i64) - sign as i64;
                        let value = if subtract {
                            signed(a) - signed(b)
                        } else {
                            signed(a) + signed(b)
                        };
                        let low = -(1_i64 << (bits - 1));
                        (value.clamp(low, (1_i64 << (bits - 1)) - 1) as u128) & mask
                    };
                    result[half] |= value << shift;
                }
            }
            return result;
        }
        if let VexOperation::Horizontal {
            subtract,
            saturating,
            dword,
        } = operation
        {
            let bits = if dword { 32 } else { 16 };
            let mask = (1_u128 << bits) - 1;
            let pairs = 64 / bits;
            let mut result = [0_u128; 2];
            for half in 0..if wide { 2 } else { 1 } {
                for (source_half, source) in [left[half], right[half]].into_iter().enumerate() {
                    for pair in 0..pairs {
                        let first = source >> (pair * 2 * bits) & mask;
                        let second = source >> ((pair * 2 + 1) * bits) & mask;
                        let value = if saturating {
                            let sign = 1_u128 << (bits - 1);
                            let signed = |value: u128| ((value ^ sign) as i64) - sign as i64;
                            let value = if subtract {
                                signed(first) - signed(second)
                            } else {
                                signed(first) + signed(second)
                            };
                            (value.clamp(-(1_i64 << (bits - 1)), (1_i64 << (bits - 1)) - 1) as u128) & mask
                        } else if subtract {
                            first.wrapping_sub(second) & mask
                        } else {
                            first.wrapping_add(second) & mask
                        };
                        result[half] |= value << ((source_half * pairs + pair) * bits);
                    }
                }
            }
            return result;
        }
        if let VexOperation::Sign { bytes } = operation {
            let bits = u32::from(bytes) * 8;
            let mask = (1_u128 << bits) - 1;
            let sign = 1_u128 << (bits - 1);
            let lanes = 16 / usize::from(bytes);
            let mut result = [0_u128; 2];
            for half in 0..if wide { 2 } else { 1 } {
                for lane in 0..lanes {
                    let shift = lane as u32 * bits;
                    let value = left[half] >> shift & mask;
                    let control = right[half] >> shift & mask;
                    let output = if control == 0 {
                        0
                    } else if control & sign != 0 {
                        value.wrapping_neg() & mask
                    } else {
                        value
                    };
                    result[half] |= output << shift;
                }
            }
            return result;
        }
        if let VexOperation::Absolute { bytes } = operation {
            let bits = u32::from(bytes) * 8;
            let mask = (1_u128 << bits) - 1;
            let sign = 1_u128 << (bits - 1);
            let lanes = 16 / usize::from(bytes);
            let mut result = [0_u128; 2];
            for half in 0..if wide { 2 } else { 1 } {
                for lane in 0..lanes {
                    let shift = lane as u32 * bits;
                    let value = right[half] >> shift & mask;
                    let output = if value & sign != 0 {
                        value.wrapping_neg() & mask
                    } else {
                        value
                    };
                    result[half] |= output << shift;
                }
            }
            return result;
        }
        if operation == VexOperation::MultiplyHighRoundWord {
            let mut result = [0_u128; 2];
            for half in 0..if wide { 2 } else { 1 } {
                for lane in 0..8 {
                    let shift = lane * 16;
                    let first = (left[half] >> shift) as u16 as i16 as i32;
                    let second = (right[half] >> shift) as u16 as i16 as i32;
                    let value = ((((first * second) >> 14) + 1) >> 1) as i16 as u16;
                    result[half] |= u128::from(value) << shift;
                }
            }
            return result;
        }
        if operation == VexOperation::HorizontalMinimumWord {
            let mut minimum = u16::MAX;
            let mut index = 0_u16;
            for lane in 0..8 {
                let value = (right[0] >> (lane * 16)) as u16;
                if value < minimum {
                    minimum = value;
                    index = lane;
                }
            }
            return [u128::from(minimum) | (u128::from(index) << 16), 0];
        }
        if let VexOperation::Extrema {
            maximum,
            unsigned,
            bytes,
        } = operation
        {
            let bits = u32::from(bytes) * 8;
            let mask = (1_u128 << bits) - 1;
            let sign = 1_u128 << (bits - 1);
            let lanes = 16 / usize::from(bytes);
            let mut result = [0_u128; 2];
            for half in 0..if wide { 2 } else { 1 } {
                for lane in 0..lanes {
                    let shift = lane as u32 * bits;
                    let a = left[half] >> shift & mask;
                    let b = right[half] >> shift & mask;
                    let order = if unsigned {
                        a.cmp(&b)
                    } else {
                        (a ^ sign).cmp(&(b ^ sign))
                    };
                    let select_left = if maximum { order.is_gt() } else { order.is_lt() };
                    result[half] |= (if select_left { a } else { b }) << shift;
                }
            }
            return result;
        }
        if let VexOperation::Average { word } = operation {
            let bits = if word { 16 } else { 8 };
            let mask = (1_u128 << bits) - 1;
            let lanes = 128 / bits;
            let mut result = [0_u128; 2];
            for half in 0..if wide { 2 } else { 1 } {
                for lane in 0..lanes {
                    let shift = lane * bits;
                    let a = left[half] >> shift & mask;
                    let b = right[half] >> shift & mask;
                    result[half] |= ((a + b + 1) >> 1) << shift;
                }
            }
            return result;
        }
        if matches!(
            operation,
            VexOperation::MultiplyAddWords | VexOperation::MultiplyAddBytes
        ) {
            let mut result = [0_u128; 2];
            for half in 0..if wide { 2 } else { 1 } {
                if operation == VexOperation::MultiplyAddWords {
                    for pair in 0..4 {
                        let first = pair * 2;
                        let product = |lane| {
                            let shift = lane * 16;
                            (left[half] >> shift) as u16 as i16 as i32 * (right[half] >> shift) as u16 as i16 as i32
                        };
                        let sum = product(first).wrapping_add(product(first + 1));
                        result[half] |= u128::from(sum as u32) << (pair * 32);
                    }
                } else {
                    for pair in 0..8 {
                        let first = pair * 2;
                        let product = |lane| {
                            let shift = lane * 8;
                            i32::from((left[half] >> shift) as u8) * i32::from((right[half] >> shift) as u8 as i8)
                        };
                        let sum = product(first) + product(first + 1);
                        result[half] |= u128::from(sum.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16 as u16)
                            << (pair * 16);
                    }
                }
            }
            return result;
        }
        if operation == VexOperation::SumAbsoluteDifferences {
            let mut result = [0_u128; 2];
            for half in 0..if wide { 2 } else { 1 } {
                result[half] = VectorLane::integer(
                    left[half],
                    right[half],
                    1,
                    crate::VectorArithmetic::SumAbsoluteDifferences,
                );
            }
            return result;
        }
        if matches!(
            operation,
            VexOperation::AddByte
                | VexOperation::AddWord
                | VexOperation::AddDword
                | VexOperation::AddQword
                | VexOperation::SubtractByte
                | VexOperation::SubtractWord
                | VexOperation::SubtractDword
                | VexOperation::SubtractQword
        ) {
            let bytes = match operation {
                VexOperation::AddByte | VexOperation::SubtractByte => 1,
                VexOperation::AddWord | VexOperation::SubtractWord => 2,
                VexOperation::AddDword | VexOperation::SubtractDword => 4,
                _ => 8,
            };
            let bits = bytes * 8;
            let mask = (1_u128 << bits) - 1;
            let subtract = matches!(
                operation,
                VexOperation::SubtractByte
                    | VexOperation::SubtractWord
                    | VexOperation::SubtractDword
                    | VexOperation::SubtractQword
            );
            let mut result = [0_u128; 2];
            for half in 0..if wide { 2 } else { 1 } {
                for lane in 0..16 / bytes {
                    let shift = lane * bits;
                    let a = (left[half] >> shift) & mask;
                    let b = (right[half] >> shift) & mask;
                    let value = if subtract { a.wrapping_sub(b) } else { a.wrapping_add(b) } & mask;
                    result[half] |= value << shift;
                }
            }
            return result;
        }
        if operation == VexOperation::Insert128 {
            let mut result = left;
            result[usize::from(immediate & 1)] = right[0];
            return result;
        }
        if operation == VexOperation::ShuffleByte {
            let mut result = [0_u128; 2];
            for half in 0..if wide { 2 } else { 1 } {
                result[half] = VectorLane::shuffle_bytes(left[half], right[half]);
            }
            return result;
        }
        if operation == VexOperation::PermuteQword {
            let mut result = [0_u128; 2];
            for lane in 0..4 {
                let source = usize::from(immediate >> (lane * 2) & 3);
                let value = (right[source / 2] >> ((source % 2) * 64)) as u64;
                result[lane / 2] |= u128::from(value) << ((lane % 2) * 64);
            }
            return result;
        }
        if let VexOperation::PermuteLaneDword { variable } | VexOperation::PermuteLaneQword { variable } = operation {
            let qword = matches!(operation, VexOperation::PermuteLaneQword { .. });
            let bits = if qword { 64 } else { 32 };
            let per_half = 128 / bits;
            let mut result = [0_u128; 2];
            for half in 0..if wide { 2 } else { 1 } {
                for lane in 0..per_half {
                    let selected = if variable {
                        let control = right[half] >> (lane * bits);
                        if qword {
                            usize::from(((control >> 1) & 1) as u8)
                        } else {
                            usize::from((control & 3) as u8)
                        }
                    } else if qword {
                        usize::from((immediate >> (half * 2 + lane)) & 1)
                    } else {
                        usize::from((immediate >> (lane * 2)) & 3)
                    };
                    let values = if variable { left[half] } else { right[half] };
                    let mask = if qword {
                        u128::from(u64::MAX)
                    } else {
                        u128::from(u32::MAX)
                    };
                    result[half] |= ((values >> (selected * bits)) & mask) << (lane * bits);
                }
            }
            return result;
        }
        if let VexOperation::Widen { from, to, signed } = operation {
            let (from, to) = (u32::from(from) * 8, u32::from(to) * 8);
            let mut result = [0_u128; 2];
            for lane in 0..(if wide { 256 } else { 128 } / to) as usize {
                let bit = lane as u32 * from;
                let raw = (if bit >= 128 {
                    right[1] >> (bit - 128)
                } else {
                    right[0] >> bit
                }) & ((1_u128 << from) - 1);
                let value = if signed && raw >> (from - 1) != 0 {
                    raw | !((1_u128 << from) - 1)
                } else {
                    raw
                } & if to == 128 { u128::MAX } else { (1_u128 << to) - 1 };
                let position = lane as u32 * to;
                result[(position / 128) as usize] |= value << (position % 128);
            }
            return result;
        }
        if operation == VexOperation::Align {
            let count = u32::from(immediate);
            let mut result = [0_u128; 2];
            for half in 0..if wide { 2 } else { 1 } {
                result[half] = if count == 0 {
                    right[half]
                } else if count < 16 {
                    right[half] >> (count * 8) | left[half] << (128 - count * 8)
                } else if count < 32 {
                    left[half] >> ((count - 16) * 8)
                } else {
                    0
                };
            }
            return result;
        }
        if operation == VexOperation::CarrylessMultiply {
            let first = (left[0] >> (u32::from(immediate & 1) * 64)) as u64;
            let second = (right[0] >> (u32::from((immediate >> 4) & 1) * 64)) as u64;
            return [VectorLane::carryless_multiply(first, second), 0];
        }
        if operation == VexOperation::MultipleSad {
            return [
                VectorLane::sad(left[0], right[0], immediate & 7),
                if wide {
                    VectorLane::sad(left[1], right[1], (immediate >> 3) & 7)
                } else {
                    0
                },
            ];
        }
        if matches!(
            operation,
            VexOperation::BlendWord | VexOperation::BlendDword | VexOperation::BlendQword
        ) {
            let bytes = match operation {
                VexOperation::BlendWord => 2,
                VexOperation::BlendDword => 4,
                VexOperation::BlendQword => 8,
                _ => unreachable!(),
            };
            let bits = bytes * 8;
            let lanes = if wide { 32 / bytes } else { 16 / bytes };
            let mask = (1_u128 << bits) - 1;
            let mut result = left;
            for lane in 0..lanes {
                let selected = if operation == VexOperation::BlendWord {
                    immediate & (1 << (lane % 8)) != 0
                } else {
                    immediate & (1 << lane) != 0
                };
                if selected {
                    let half = lane / (16 / bytes);
                    let shift = (lane % (16 / bytes)) * bits;
                    result[half] = result[half] & !(mask << shift) | (right[half] & (mask << shift));
                }
            }
            if !wide {
                result[1] = 0;
            }
            return result;
        }
        if matches!(
            operation,
            VexOperation::PackSignedWordByte
                | VexOperation::PackSignedDwordWord
                | VexOperation::PackUnsignedWordByte
                | VexOperation::PackUnsignedDwordWord
        ) {
            let dword = matches!(
                operation,
                VexOperation::PackSignedDwordWord | VexOperation::PackUnsignedDwordWord
            );
            let unsigned = matches!(
                operation,
                VexOperation::PackUnsignedWordByte | VexOperation::PackUnsignedDwordWord
            );
            let input_bits = if dword { 32 } else { 16 };
            let output_bits = input_bits / 2;
            let lanes = 128 / input_bits;
            let mut result = [0_u128; 2];
            for half in 0..if wide { 2 } else { 1 } {
                for source in 0..2 {
                    let input = if source == 0 { left[half] } else { right[half] };
                    for lane in 0..lanes {
                        let raw = (input >> (lane * input_bits)) as u32;
                        let signed = if dword {
                            raw as i32 as i64
                        } else {
                            raw as u16 as i16 as i64
                        };
                        let saturated = if unsigned {
                            signed.clamp(0, (1_i64 << output_bits) - 1) as u64
                        } else {
                            let low = -(1_i64 << (output_bits - 1));
                            signed.clamp(low, (1_i64 << (output_bits - 1)) - 1) as u64
                        };
                        let output_lane = source * lanes + lane;
                        let mask = (1_u128 << output_bits) - 1;
                        result[half] |= (u128::from(saturated) & mask) << (output_lane * output_bits);
                    }
                }
            }
            return result;
        }
        if matches!(
            operation,
            VexOperation::UnpackLowByte
                | VexOperation::UnpackLowWord
                | VexOperation::UnpackLowDword
                | VexOperation::UnpackLowQword
                | VexOperation::UnpackHighByte
                | VexOperation::UnpackHighWord
                | VexOperation::UnpackHighDword
                | VexOperation::UnpackHighQword
        ) {
            let (bytes, high) = match operation {
                VexOperation::UnpackLowByte => (1, false),
                VexOperation::UnpackLowWord => (2, false),
                VexOperation::UnpackLowDword => (4, false),
                VexOperation::UnpackLowQword => (8, false),
                VexOperation::UnpackHighByte => (1, true),
                VexOperation::UnpackHighWord => (2, true),
                VexOperation::UnpackHighDword => (4, true),
                VexOperation::UnpackHighQword => (8, true),
                _ => unreachable!(),
            };
            let bits = bytes * 8;
            let mask = if bits == 64 {
                u128::from(u64::MAX)
            } else {
                (1_u128 << bits) - 1
            };
            let count = 8 / bytes;
            let start = if high { count } else { 0 };
            let mut result = [0_u128; 2];
            for half in 0..if wide { 2 } else { 1 } {
                for lane in 0..count {
                    let shift = (start + lane) * bits;
                    result[half] |= ((left[half] >> shift) & mask) << (lane * 2 * bits);
                    result[half] |= ((right[half] >> shift) & mask) << ((lane * 2 + 1) * bits);
                }
            }
            return result;
        }
        if operation == VexOperation::ShiftRightBytes {
            let count = u32::from(immediate.min(16)) * 8;
            return [if count == 128 { 0 } else { right[0] >> count }, 0];
        }
        if matches!(
            operation,
            VexOperation::PermuteDword
                | VexOperation::ShiftLeftVariableDword
                | VexOperation::ShiftLeftVariableQword
                | VexOperation::ShiftRightVariableDword
                | VexOperation::ShiftRightVariableQword
                | VexOperation::ShiftArithmeticVariableDword
        ) {
            let (values, controls) = if operation == VexOperation::PermuteDword {
                (right, left)
            } else {
                (left, right)
            };
            let mut result = [0_u128; 2];
            let qword = matches!(
                operation,
                VexOperation::ShiftLeftVariableQword | VexOperation::ShiftRightVariableQword
            );
            let bits = if qword { 64 } else { 32 };
            let lanes = if wide { 256 / bits } else { 128 / bits };
            for lane in 0..lanes {
                let half = lane / (128 / bits);
                let shift = (lane % (128 / bits)) * bits;
                let control = if qword {
                    (controls[half] >> shift) as u64
                } else {
                    (controls[half] >> shift) as u32 as u64
                };
                let value = if operation == VexOperation::PermuteDword {
                    let source_lane = usize::from((control as u8) & if wide { 7 } else { 3 });
                    (values[source_lane / 4] >> ((source_lane % 4) * 32)) as u32 as u64
                } else {
                    let source = (values[half] >> shift) as u64;
                    match operation {
                        VexOperation::ShiftLeftVariableDword => {
                            if control >= 32 {
                                0
                            } else {
                                ((source as u32) << control) as u64
                            }
                        }
                        VexOperation::ShiftLeftVariableQword => {
                            if control >= 64 {
                                0
                            } else {
                                source << control
                            }
                        }
                        VexOperation::ShiftRightVariableDword => {
                            if control >= 32 {
                                0
                            } else {
                                ((source as u32) >> control) as u64
                            }
                        }
                        VexOperation::ShiftRightVariableQword => {
                            if control >= 64 {
                                0
                            } else {
                                source >> control
                            }
                        }
                        VexOperation::ShiftArithmeticVariableDword => {
                            if control >= 32 {
                                ((source as u32 as i32) >> 31) as u32 as u64
                            } else {
                                ((source as u32 as i32) >> control) as u32 as u64
                            }
                        }
                        _ => unreachable!(),
                    }
                };
                result[half] |= u128::from(value) << shift;
            }
            return result;
        }
        let mut output = [0_u128; 2];
        for half in 0..if wide { 2 } else { 1 } {
            output[half] = match operation {
                VexOperation::And => left[half] & right[half],
                VexOperation::AndNot => !left[half] & right[half],
                VexOperation::Or => left[half] | right[half],
                VexOperation::Xor => left[half] ^ right[half],
                VexOperation::AddSingle | VexOperation::MultiplySingle => {
                    let mut value = 0_u128;
                    for lane in 0..4 {
                        let shift = lane * 32;
                        let a = f32::from_bits((left[half] >> shift) as u32);
                        let b = f32::from_bits((right[half] >> shift) as u32);
                        let result = if operation == VexOperation::AddSingle {
                            a + b
                        } else {
                            a * b
                        };
                        value |= u128::from(result.to_bits()) << shift;
                    }
                    value
                }
                VexOperation::AddDouble | VexOperation::MultiplyDouble => {
                    let mut value = 0_u128;
                    for lane in 0..2 {
                        let shift = lane * 64;
                        let a = f64::from_bits((left[half] >> shift) as u64);
                        let b = f64::from_bits((right[half] >> shift) as u64);
                        let result = if operation == VexOperation::AddDouble {
                            a + b
                        } else {
                            a * b
                        };
                        value |= u128::from(result.to_bits()) << shift;
                    }
                    value
                }
                VexOperation::MultiplyLowDword => {
                    let mut value = 0_u128;
                    for lane in 0..4 {
                        let shift = lane * 32;
                        let a = (left[half] >> shift) as u32;
                        let b = (right[half] >> shift) as u32;
                        value |= u128::from(a.wrapping_mul(b)) << shift;
                    }
                    value
                }
                VexOperation::Compare { comparison, lane } => {
                    crate::VectorLane::compare(left[half], right[half], lane, comparison)
                }
                VexOperation::AddByte
                | VexOperation::AddWord
                | VexOperation::AddDword
                | VexOperation::AddQword
                | VexOperation::SubtractByte
                | VexOperation::SubtractWord
                | VexOperation::SubtractDword
                | VexOperation::SubtractQword => unreachable!(),
                VexOperation::Saturating { .. } => unreachable!(),
                VexOperation::Extrema { .. } => unreachable!(),
                VexOperation::Average { .. } => unreachable!(),
                VexOperation::MultiplyAddWords | VexOperation::MultiplyAddBytes => unreachable!(),
                VexOperation::SumAbsoluteDifferences => unreachable!(),
                VexOperation::Permute128
                | VexOperation::PermuteDword
                | VexOperation::PermuteQword
                | VexOperation::PermuteLaneDword { .. }
                | VexOperation::PermuteLaneQword { .. }
                | VexOperation::ShuffleByte
                | VexOperation::ShiftLeftVariableDword
                | VexOperation::ShiftLeftVariableQword
                | VexOperation::ShiftRightVariableDword
                | VexOperation::ShiftRightVariableQword
                | VexOperation::ShiftArithmeticVariableDword
                | VexOperation::UnpackLowByte
                | VexOperation::UnpackLowWord
                | VexOperation::UnpackLowDword
                | VexOperation::UnpackLowQword
                | VexOperation::UnpackHighByte
                | VexOperation::UnpackHighWord
                | VexOperation::UnpackHighDword
                | VexOperation::UnpackHighQword
                | VexOperation::MultiplyLowWord
                | VexOperation::MultiplyHighWordSigned
                | VexOperation::MultiplyHighWordUnsigned
                | VexOperation::MultiplyDwordSigned
                | VexOperation::MultiplyDwordUnsigned
                | VexOperation::BlendWord
                | VexOperation::BlendDword
                | VexOperation::BlendQword
                | VexOperation::PackSignedWordByte
                | VexOperation::PackSignedDwordWord
                | VexOperation::PackUnsignedWordByte
                | VexOperation::PackUnsignedDwordWord
                | VexOperation::BroadcastByte
                | VexOperation::BroadcastWord
                | VexOperation::BroadcastDword
                | VexOperation::BroadcastQword
                | VexOperation::Broadcast128
                | VexOperation::DuplicateDouble
                | VexOperation::DuplicateLowSingle
                | VexOperation::DuplicateHighSingle
                | VexOperation::DotSingle
                | VexOperation::DotDouble
                | VexOperation::CarrylessMultiply
                | VexOperation::MultipleSad
                | VexOperation::Horizontal { .. }
                | VexOperation::Sign { .. }
                | VexOperation::Absolute { .. }
                | VexOperation::MultiplyHighRoundWord
                | VexOperation::HorizontalMinimumWord
                | VexOperation::Insert128 => unreachable!(),
                VexOperation::Widen { .. } | VexOperation::Align | VexOperation::ShiftRightBytes => unreachable!(),
                VexOperation::ShuffleDword | VexOperation::ShuffleSingle => unreachable!(),
                VexOperation::ShuffleWord { .. } => unreachable!(),
                VexOperation::ShuffleDouble => unreachable!(),
            };
        }
        output
    }

    fn vex_immediate_shift(
        input: [u128; 2],
        operation: VexImmediateShift,
        lane: u8,
        wide: bool,
        count: u8,
    ) -> [u128; 2] {
        let mut output = [0_u128; 2];
        for half in 0..if wide { 2 } else { 1 } {
            if matches!(operation, VexImmediateShift::ByteRight | VexImmediateShift::ByteLeft) {
                let bits = u32::from(count.min(16)) * 8;
                output[half] = if bits == 128 {
                    0
                } else if operation == VexImmediateShift::ByteRight {
                    input[half] >> bits
                } else {
                    input[half] << bits
                };
                continue;
            }
            let bits = u32::from(lane) * 8;
            let mask = if bits == 64 { u64::MAX } else { (1_u64 << bits) - 1 };
            for offset in (0..128).step_by(bits as usize) {
                let value = ((input[half] >> offset) as u64) & mask;
                let shifted = match operation {
                    VexImmediateShift::LogicalRight => {
                        if u32::from(count) >= bits {
                            0
                        } else {
                            value >> count
                        }
                    }
                    VexImmediateShift::LogicalLeft => {
                        if u32::from(count) >= bits {
                            0
                        } else {
                            (value << count) & mask
                        }
                    }
                    VexImmediateShift::ArithmeticRight => {
                        let signed = ((value << (64 - bits)) as i64) >> (64 - bits);
                        (signed >> u32::from(count).min(bits - 1)) as u64 & mask
                    }
                    VexImmediateShift::ByteRight | VexImmediateShift::ByteLeft => unreachable!(),
                };
                output[half] |= u128::from(shifted) << offset;
            }
        }
        output
    }

    fn vex_scalar_count_shift(
        input: [u128; 2],
        operation: VexImmediateShift,
        lane: u8,
        wide: bool,
        count: u64,
    ) -> [u128; 2] {
        debug_assert!(!matches!(
            operation,
            VexImmediateShift::ByteRight | VexImmediateShift::ByteLeft
        ));
        let mut output = [0_u128; 2];
        let bits = u32::from(lane) * 8;
        let mask = if bits == 64 { u64::MAX } else { (1_u64 << bits) - 1 };
        for half in 0..if wide { 2 } else { 1 } {
            for offset in (0..128).step_by(bits as usize) {
                let value = ((input[half] >> offset) as u64) & mask;
                let shifted = match operation {
                    VexImmediateShift::LogicalRight => {
                        if count >= u64::from(bits) {
                            0
                        } else {
                            value >> count
                        }
                    }
                    VexImmediateShift::LogicalLeft => {
                        if count >= u64::from(bits) {
                            0
                        } else {
                            (value << count) & mask
                        }
                    }
                    VexImmediateShift::ArithmeticRight => {
                        let signed = ((value << (64 - bits)) as i64) >> (64 - bits);
                        (signed >> count.min(u64::from(bits - 1))) as u64 & mask
                    }
                    VexImmediateShift::ByteRight | VexImmediateShift::ByteLeft => unreachable!(),
                };
                output[half] |= u128::from(shifted) << offset;
            }
        }
        output
    }

    // The VEX compare predicates are defined on exact IEEE-754 equality, so an epsilon would be wrong.
    #[allow(clippy::float_cmp)]
    fn vex_compare(
        left: [u128; 2],
        right: [u128; 2],
        format: crate::FloatWidth,
        scalar: bool,
        wide: bool,
        predicate: u8,
    ) -> [u128; 2] {
        let bits = if format == crate::FloatWidth::Double { 64 } else { 32 };
        let lanes = if scalar {
            1
        } else if wide {
            256 / bits
        } else {
            128 / bits
        };
        let mut output = if scalar { [left[0], 0] } else { [0, 0] };
        for lane in 0..lanes {
            let half = lane / (128 / bits);
            let shift = (lane % (128 / bits)) * bits;
            let mask = if bits == 64 {
                u128::from(u64::MAX)
            } else {
                u128::from(u32::MAX)
            };
            let a_bits = (left[half] >> shift) & mask;
            let b_bits = (right[half] >> shift) & mask;
            let (a, b) = if bits == 64 {
                (f64::from_bits(a_bits as u64), f64::from_bits(b_bits as u64))
            } else {
                (
                    f64::from(f32::from_bits(a_bits as u32)),
                    f64::from(f32::from_bits(b_bits as u32)),
                )
            };
            let unordered = a.is_nan() || b.is_nan();
            let selected = match predicate & 15 {
                0 => !unordered && a == b,
                1 => !unordered && a < b,
                2 => !unordered && a <= b,
                3 => unordered,
                4 => unordered || a != b,
                5 => unordered || a >= b,
                6 => unordered || a > b,
                7 => !unordered,
                8 => unordered || a == b,
                9 => unordered || a < b,
                10 => unordered || a <= b,
                11 => false,
                12 => !unordered && a != b,
                13 => !unordered && a >= b,
                14 => !unordered && a > b,
                _ => true,
            };
            output[half] = output[half] & !(mask << shift) | if selected { mask << shift } else { 0 };
        }
        output
    }

    fn vex_blend(left: [u128; 2], right: [u128; 2], mask: [u128; 2], lane: u8, wide: bool) -> [u128; 2] {
        let lane_bits = u32::from(lane) * 8;
        let lane_mask = (1_u128 << lane_bits) - 1;
        let mut output = [0_u128; 2];
        for half in 0..if wide { 2 } else { 1 } {
            for index in 0..128 / lane_bits {
                let shift = index * lane_bits;
                let source = if mask[half] >> (shift + lane_bits - 1) & 1 != 0 {
                    right
                } else {
                    left
                };
                output[half] |= (source[half] >> shift & lane_mask) << shift;
            }
        }
        output
    }
}
