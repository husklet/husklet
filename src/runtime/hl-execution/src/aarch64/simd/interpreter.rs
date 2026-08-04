use crate::{Aarch64CpuState, Aarch64ExecutionExit, Aarch64Instruction, Aarch64Ir, SimdLogic};
pub(crate) struct Aarch64SimdInterpreter;
impl Aarch64SimdInterpreter {
    pub(crate) fn is_simd(instruction: Aarch64Instruction) -> bool {
        matches!(
            instruction,
            Aarch64Instruction::SimdAes { .. }
                | Aarch64Instruction::SimdSha1 { .. }
                | Aarch64Instruction::SimdSha256 { .. }
                | Aarch64Instruction::SimdImmediate { .. }
                | Aarch64Instruction::SimdLogic { .. }
                | Aarch64Instruction::SimdAddSubtract { .. }
                | Aarch64Instruction::SimdCopy { .. }
                | Aarch64Instruction::SimdExtract { .. }
                | Aarch64Instruction::SimdTable { .. }
                | Aarch64Instruction::SimdPermute { .. }
                | Aarch64Instruction::SimdUnary { .. }
                | Aarch64Instruction::SimdSaturatingUnary { .. }
                | Aarch64Instruction::SimdSaturatingAccumulate { .. }
                | Aarch64Instruction::SimdShift { .. }
                | Aarch64Instruction::SimdVariable { .. }
                | Aarch64Instruction::SimdLane { .. }
                | Aarch64Instruction::SimdDot { .. }
                | Aarch64Instruction::SimdMatrix { .. }
                | Aarch64Instruction::SimdElementProduct { .. }
                | Aarch64Instruction::SimdHighProduct { .. }
                | Aarch64Instruction::SimdLongElement { .. }
                | Aarch64Instruction::SimdSaturatingLong { .. }
                | Aarch64Instruction::SimdWide { .. }
                | Aarch64Instruction::SimdReduce { .. }
                | Aarch64Instruction::SimdScalarShift { .. }
                | Aarch64Instruction::SimdScalarMove { .. }
        )
    }
    pub(crate) fn execute(cpu: &mut Aarch64CpuState, ir: Aarch64Ir) -> Aarch64ExecutionExit {
        let mut staged = cpu.clone();
        staged.pc = cpu.pc.wrapping_add(4);
        match ir.instruction {
            Aarch64Instruction::SimdAes {
                operation,
                source,
                destination,
            } => crate::aarch64_simd_aes::Aes::execute(cpu, &mut staged, operation, source, destination),
            Aarch64Instruction::SimdSha1 {
                operation,
                first,
                second,
                destination,
            } => crate::aarch64_simd_sha1::Sha1Unit::execute(cpu, &mut staged, operation, first, second, destination),
            Aarch64Instruction::SimdSha256 {
                operation,
                first,
                second,
                destination,
            } => {
                crate::aarch64_simd_sha256::Sha256Unit::execute(cpu, &mut staged, operation, first, second, destination)
            }
            Aarch64Instruction::SimdImmediate {
                destination,
                pattern,
                invert,
                modify,
                wide,
            } => {
                let value = crate::aarch64_simd_immediate::execute(cpu, destination, pattern, invert, modify);
                staged.write_vector_width(destination, value, wide);
            }
            Aarch64Instruction::SimdLogic {
                operation,
                left,
                right,
                destination,
                wide,
            } => {
                let left = cpu.vector(left);
                let right = cpu.vector(right);
                let destination_value = cpu.vector(destination);
                let value = match operation {
                    SimdLogic::And => left & right,
                    SimdLogic::Orr => left | right,
                    SimdLogic::ExclusiveOr => left ^ right,
                    SimdLogic::BitClear => left & !right,
                    SimdLogic::BitSelect => (left & destination_value) | (right & !destination_value),
                    SimdLogic::BitInsertTrue => destination_value ^ ((destination_value ^ left) & right),
                    SimdLogic::BitInsertFalse => destination_value ^ ((destination_value ^ left) & !right),
                };
                staged.write_vector_width(destination, value, wide);
            }
            Aarch64Instruction::SimdAddSubtract {
                subtract,
                saturating,
                unsigned,
                lane_bits,
                left,
                right,
                destination,
                wide,
            } => {
                let lanes = if wide { 128 } else { 64 } / lane_bits;
                let mask = Self::lane_mask(lane_bits);
                let mut value = 0_u128;
                for lane in 0..lanes {
                    let left = u128::from(cpu.vector_lane(left, lane_bits, lane));
                    let right = u128::from(cpu.vector_lane(right, lane_bits, lane));
                    let (result, saturated) =
                        Self::add_lane(saturating, unsigned, subtract, left, right, lane_bits, mask);
                    staged.fpsr |= u64::from(saturated) << 27;
                    value |= result << (u32::from(lane) * u32::from(lane_bits));
                }
                staged.write_vector_width(destination, value, wide);
            }
            Aarch64Instruction::SimdCopy {
                operation,
                lane_bits,
                lane,
                source,
                destination,
                wide,
            } => Self::execute_copy(cpu, &mut staged, operation, lane_bits, lane, source, destination, wide),
            Aarch64Instruction::SimdExtract {
                first,
                second,
                destination,
                position,
                wide,
            } => {
                let bytes = if wide { 16 } else { 8 };
                let first = cpu.vector(first).to_le_bytes();
                let second = cpu.vector(second).to_le_bytes();
                let mut result = [0_u8; 16];
                let mut combined = [0_u8; 32];
                combined[..bytes].copy_from_slice(&first[..bytes]);
                combined[bytes..bytes * 2].copy_from_slice(&second[..bytes]);
                let start = usize::from(position);
                result[..bytes].copy_from_slice(&combined[start..start + bytes]);
                staged.set_vector(destination, u128::from_le_bytes(result));
            }
            Aarch64Instruction::SimdTable {
                first_table,
                table_count,
                indexes,
                destination,
                extend,
                wide,
            } => {
                let result = Self::table(cpu, first_table, table_count, indexes, destination, extend, wide);
                staged.write_vector_width(destination, result, wide);
            }
            Aarch64Instruction::SimdPermute {
                operation,
                lane_bits,
                left,
                right,
                destination,
                wide,
            } => Self::permute(cpu, &mut staged, operation, lane_bits, left, right, destination, wide),
            Aarch64Instruction::SimdUnary {
                operation,
                lane_bits,
                source,
                destination,
                wide,
            } => Self::unary(cpu, &mut staged, operation, lane_bits, source, destination, wide),
            Aarch64Instruction::SimdSaturatingUnary {
                negate,
                lane_bits,
                source,
                destination,
                lanes,
            } => crate::aarch64_simd_saturating_unary::Saturation::apply(
                cpu,
                &mut staged,
                negate,
                lane_bits,
                source,
                destination,
                lanes,
            ),
            Aarch64Instruction::SimdSaturatingAccumulate {
                unsigned_destination,
                lane_bits,
                source,
                destination,
                lanes,
            } => crate::aarch64_simd_saturating_unary::Saturation::accumulate(
                cpu,
                &mut staged,
                unsigned_destination,
                lane_bits,
                source,
                destination,
                lanes,
            ),
            Aarch64Instruction::SimdShift {
                operation,
                amount,
                lane_bits,
                source,
                destination,
                wide,
            } => Self::shift(
                cpu,
                &mut staged,
                operation,
                amount,
                lane_bits,
                source,
                destination,
                wide,
            ),
            Aarch64Instruction::SimdVariable {
                signed,
                saturating,
                rounding,
                lane_bits,
                source,
                counts,
                destination,
                wide,
            } => {
                let (value, saturated) = crate::aarch64_simd_variable::VariableShift::execute(
                    cpu, signed, saturating, rounding, source, counts, lane_bits, wide,
                );
                staged.write_vector_width(destination, value, wide);
                if saturated {
                    staged.fpsr |= 1 << 27;
                }
            }
            Aarch64Instruction::SimdLane {
                operation,
                lane_bits,
                left,
                right,
                destination,
                wide,
            } => {
                staged.write_vector_width(
                    destination,
                    crate::aarch64_simd_lane_interpreter::execute(
                        cpu,
                        operation,
                        lane_bits,
                        left,
                        right,
                        destination,
                        wide,
                    ),
                    wide,
                );
            }
            Aarch64Instruction::SimdElementProduct {
                operation,
                lane_bits,
                left,
                right,
                index,
                destination,
                wide,
            } => staged.write_vector_width(
                destination,
                crate::aarch64_simd_lane_interpreter::element(
                    cpu,
                    operation,
                    lane_bits,
                    left,
                    right,
                    index,
                    destination,
                    wide,
                ),
                wide,
            ),
            Aarch64Instruction::SimdHighProduct {
                rounding,
                lane_bits,
                left,
                right,
                index,
                destination,
                wide,
                scalar,
            } => {
                let (value, saturated) = crate::aarch64_simd_high_product::HighProduct::execute(
                    cpu, rounding, lane_bits, left, right, index, wide, scalar,
                );
                staged.write_vector_width(destination, value, wide);
                staged.fpsr |= u64::from(saturated) << 27;
            }
            Aarch64Instruction::SimdLongElement {
                operation,
                signed,
                narrow_bits,
                left,
                right,
                index,
                destination,
                high,
            } => staged.set_vector(
                destination,
                crate::aarch64_simd_long_product::LongProduct::execute(
                    cpu,
                    operation,
                    signed,
                    narrow_bits,
                    left,
                    right,
                    index,
                    destination,
                    high,
                ),
            ),
            Aarch64Instruction::SimdDot {
                signed,
                left,
                right,
                index,
                destination,
                wide,
            } => staged.set_vector(
                destination,
                crate::aarch64_simd_dot::DotProduct::execute(cpu, signed, left, right, index, destination, wide),
            ),
            Aarch64Instruction::SimdMatrix {
                signedness,
                left,
                right,
                destination,
            } => staged.set_vector(
                destination,
                crate::aarch64_simd_matrix::MatrixProduct::execute(cpu, signedness, left, right, destination),
            ),
            Aarch64Instruction::SimdSaturatingLong {
                operation,
                narrow_bits,
                left,
                right,
                index,
                destination,
                high,
                scalar,
            } => {
                let (value, saturated) = crate::aarch64_simd_saturating_product::SaturatingProduct::execute(
                    cpu,
                    operation,
                    narrow_bits,
                    left,
                    right,
                    index,
                    destination,
                    high,
                    scalar,
                );
                staged.set_vector(destination, value);
                staged.fpsr |= u64::from(saturated) << 27;
            }
            Aarch64Instruction::SimdWide {
                operation,
                signed,
                lane_bits,
                left,
                right,
                destination,
                high,
            } => {
                let (value, wide, saturated) = crate::aarch64_simd_wide_interpreter::execute(
                    cpu,
                    operation,
                    signed,
                    lane_bits,
                    left,
                    right,
                    destination,
                    high,
                );
                staged.write_vector_width(destination, value, wide);
                if saturated {
                    staged.fpsr |= 1 << 27;
                }
            }
            Aarch64Instruction::SimdReduce {
                operation,
                lane_bits,
                source,
                destination,
                wide,
            } => {
                let value = crate::aarch64_simd_reduce_interpreter::execute(cpu, operation, lane_bits, source, wide);
                staged.set_vector(destination, value);
            }
            Aarch64Instruction::SimdScalarShift {
                amount,
                source,
                destination,
                left,
            } => crate::aarch64_simd_scalar::Scalar::execute(cpu, &mut staged, amount, source, destination, left),
            Aarch64Instruction::SimdScalarMove {
                lane_bits,
                lane,
                source,
                destination,
            } => crate::aarch64_simd_scalar::Scalar::move_lane(cpu, &mut staged, lane_bits, lane, source, destination),
            _ => unreachable!("SIMD interpreter called for non-SIMD instruction"),
        }
        *cpu = staged;
        Aarch64ExecutionExit::Continue
    }
}
