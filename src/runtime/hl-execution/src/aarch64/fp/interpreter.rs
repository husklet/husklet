use crate::{
    Aarch64CpuState, Aarch64DecodeError, Aarch64Decoder, Aarch64ExecutionExit, Aarch64Instruction, Aarch64Ir,
    FpArithmetic, FpArithmeticPort, FpFormat, FpUnaryOperation, Nzcv,
};

pub struct Executor;
pub type Aarch64FpExecutor = Executor;

impl Aarch64FpExecutor {
    pub fn execute_word<P: FpArithmeticPort>(
        cpu: &mut Aarch64CpuState,
        port: &mut P,
        word: u32,
    ) -> Aarch64ExecutionExit {
        match Aarch64Decoder::decode(word) {
            Ok(ir) if Self::is_fp(ir.instruction) => Self::execute(cpu, port, ir),
            Ok(_) | Err(Aarch64DecodeError::Unsupported) => Aarch64ExecutionExit::UnsupportedInstruction {
                instruction: cpu.pc,
                word,
            },
            Err(Aarch64DecodeError::Reserved) => Aarch64ExecutionExit::UndefinedInstruction {
                instruction: cpu.pc,
                word,
            },
        }
    }
    pub(crate) fn is_fp(instruction: Aarch64Instruction) -> bool {
        matches!(
            instruction,
            Aarch64Instruction::FpImmediate { .. }
                | Aarch64Instruction::FpUnary { .. }
                | Aarch64Instruction::FpBinary { .. }
                | Aarch64Instruction::FpSelect { .. }
                | Aarch64Instruction::FpFused { .. }
                | Aarch64Instruction::FpRoundIntegral { .. }
                | Aarch64Instruction::SimdFpUnary { .. }
                | Aarch64Instruction::SimdFpRoundIntegral { .. }
                | Aarch64Instruction::FpFormatConvert { .. }
                | Aarch64Instruction::FpCompare { .. }
                | Aarch64Instruction::SimdFpCompare { .. }
                | Aarch64Instruction::FpGeneralMove { .. }
                | Aarch64Instruction::FpConvert { .. }
                | Aarch64Instruction::Bf16Convert { .. }
                | Aarch64Instruction::SimdFpEstimate { .. }
                | Aarch64Instruction::SimdFpRecipExponent { .. }
                | Aarch64Instruction::SimdUnsignedEstimate { .. }
                | Aarch64Instruction::SimdFpStep { .. }
                | Aarch64Instruction::SimdFpNarrowOdd { .. }
                | Aarch64Instruction::SimdFpConvert { .. }
                | Aarch64Instruction::SimdFpInteger { .. }
                | Aarch64Instruction::SimdIntegerFp { .. }
                | Aarch64Instruction::SimdFpBinary { .. }
                | Aarch64Instruction::SimdFpFused { .. }
                | Aarch64Instruction::SimdFpProduct { .. }
                | Aarch64Instruction::SimdFpReduce { .. }
        )
    }

    pub(crate) fn execute<P: FpArithmeticPort>(
        cpu: &mut Aarch64CpuState,
        port: &mut P,
        ir: Aarch64Ir,
    ) -> Aarch64ExecutionExit {
        let mut staged = cpu.clone();
        staged.pc = cpu.pc.wrapping_add(4);
        match ir.instruction {
            Aarch64Instruction::FpImmediate {
                format,
                immediate,
                destination,
            } => {
                staged.set_vector(
                    destination,
                    u128::from(crate::aarch64_fp::ImmediateEncoding::expand(format, immediate)),
                );
            }
            Aarch64Instruction::FpUnary {
                operation,
                format,
                source,
                destination,
            } => {
                let bits = cpu.vector_lane(source, format.bits(), 0);
                let value = match operation {
                    FpUnaryOperation::Move => bits,
                    FpUnaryOperation::Absolute => bits & !Self::sign_mask(format),
                    FpUnaryOperation::Negate => bits ^ Self::sign_mask(format),
                    FpUnaryOperation::SquareRoot => {
                        let result = port.evaluate(Self::request(cpu, FpArithmetic::SquareRoot, format, bits, 0));
                        staged.fpsr |= u64::from(result.exceptions);
                        result.value
                    }
                };
                staged.set_vector(destination, u128::from(value));
            }
            Aarch64Instruction::FpBinary {
                operation,
                format,
                left,
                right,
                destination,
            } => {
                let result = port.evaluate(Self::request(
                    cpu,
                    FpArithmetic::Binary(operation),
                    format,
                    cpu.vector_lane(left, format.bits(), 0),
                    cpu.vector_lane(right, format.bits(), 0),
                ));
                staged.fpsr |= u64::from(result.exceptions);
                staged.set_vector(destination, u128::from(result.value));
            }
            Aarch64Instruction::FpSelect {
                format,
                source,
                alternate,
                destination,
                condition,
            } => {
                let selected = if Self::condition_holds(cpu.nzcv, condition.0) {
                    source
                } else {
                    alternate
                };
                staged.set_vector(destination, u128::from(cpu.vector_lane(selected, format.bits(), 0)));
            }
            Aarch64Instruction::FpFused {
                format,
                left,
                right,
                addend,
                destination,
                negate_product,
                negate_addend,
            } => {
                let sign = 1_u64 << (format.bits() - 1);
                let mut left = cpu.vector_lane(left, format.bits(), 0);
                let mut addend = cpu.vector_lane(addend, format.bits(), 0);
                if negate_product {
                    left ^= sign;
                }
                if negate_addend {
                    addend ^= sign;
                }
                let mut request = Self::request(
                    cpu,
                    FpArithmetic::FusedMultiplyAdd,
                    format,
                    left,
                    cpu.vector_lane(right, format.bits(), 0),
                );
                request.addend = addend;
                let result = port.evaluate(request);
                staged.fpsr |= u64::from(result.exceptions);
                staged.set_vector(destination, u128::from(result.value));
            }
            Aarch64Instruction::FpRoundIntegral {
                format,
                source,
                destination,
                rounding,
                exact,
            } => {
                let result = port.evaluate(Self::request(
                    cpu,
                    FpArithmetic::RoundToIntegral { rounding, exact },
                    format,
                    cpu.vector_lane(source, format.bits(), 0),
                    0,
                ));
                staged.fpsr |= u64::from(result.exceptions);
                staged.set_vector(destination, u128::from(result.value));
            }
            Aarch64Instruction::SimdFpUnary {
                operation,
                format,
                source,
                destination,
                lanes,
            } => {
                let mut value = 0_u128;
                for lane in 0..lanes {
                    let bits = cpu.vector_lane(source, format.bits(), lane);
                    let result = match operation {
                        FpUnaryOperation::Absolute => bits & !Self::sign_mask(format),
                        FpUnaryOperation::Negate => bits ^ Self::sign_mask(format),
                        _ => unreachable!("vector sign decoder admits only FABS/FNEG"),
                    };
                    value |= u128::from(result) << (u32::from(lane) * u32::from(format.bits()));
                }
                staged.set_vector(destination, value);
            }
            Aarch64Instruction::SimdFpRoundIntegral {
                format,
                source,
                destination,
                lanes,
                rounding,
                exact,
            } => {
                let mut value = 0_u128;
                for lane in 0..lanes {
                    let result = port.evaluate(Self::request(
                        cpu,
                        FpArithmetic::RoundToIntegral { rounding, exact },
                        format,
                        cpu.vector_lane(source, format.bits(), lane),
                        0,
                    ));
                    staged.fpsr |= u64::from(result.exceptions);
                    value |= u128::from(result.value) << (u32::from(lane) * u32::from(format.bits()));
                }
                staged.set_vector(destination, value);
            }
            Aarch64Instruction::FpFormatConvert {
                source_format,
                destination_format,
                source,
                destination,
            } => {
                let result = port.evaluate(Self::request(
                    cpu,
                    FpArithmetic::ConvertFormat {
                        destination: destination_format,
                    },
                    source_format,
                    cpu.vector_lane(source, source_format.bits(), 0),
                    0,
                ));
                staged.fpsr |= u64::from(result.exceptions);
                staged.set_vector(destination, u128::from(result.value));
            }
            Aarch64Instruction::FpCompare {
                format,
                left,
                right,
                signaling,
                condition,
                alternative_nzcv,
            } => {
                let holds = match condition {
                    Some(condition) => Self::condition_holds(cpu.nzcv, condition.0),
                    None => true,
                };
                if holds {
                    let right = Self::comparison_operand(cpu, right, format);
                    Self::compare(
                        &mut staged,
                        format,
                        cpu.vector_lane(left, format.bits(), 0),
                        right,
                        signaling,
                    );
                } else {
                    staged.nzcv = Nzcv::from_bits(u32::from(alternative_nzcv) << 28);
                }
            }
            Aarch64Instruction::SimdFpCompare {
                operation,
                format,
                left,
                right,
                destination,
                lanes,
                absolute,
            } => crate::aarch64_simd_compare::Comparison::execute(
                cpu,
                &mut staged,
                operation,
                format,
                left,
                right,
                destination,
                lanes,
                absolute,
            ),
            Aarch64Instruction::FpGeneralMove {
                format,
                general_to_fp,
                top_half,
                general,
                vector,
            } => match (top_half, general_to_fp, format) {
                (true, true, _) => {
                    staged.set_vector_lane(vector, 64, 1, cpu.register(general));
                }
                (true, false, _) => {
                    staged.set_register(general, cpu.vector_lane(vector, 64, 1));
                }
                (false, true, _) => {
                    staged.set_vector(vector, u128::from(cpu.register(general) & Self::value_mask(format)));
                }
                (false, false, FpFormat::Double) => {
                    staged.set_register(general, cpu.vector_lane(vector, 64, 0));
                }
                (false, false, _) => {
                    staged.set_narrow_register(general, cpu.vector_lane(vector, format.bits(), 0) as u32);
                }
            },
            Aarch64Instruction::FpConvert {
                format,
                fp_to_integer,
                signed,
                integer_wide,
                rounding,
                source,
                destination,
            } => {
                let width = if integer_wide { 64 } else { 32 };
                let operation = if fp_to_integer {
                    FpArithmetic::FloatToInteger {
                        signed,
                        width,
                        rounding,
                    }
                } else {
                    FpArithmetic::IntegerToFloat { signed, width }
                };
                let input = if fp_to_integer {
                    cpu.vector_lane(source, format.bits(), 0)
                } else {
                    cpu.register(source)
                };
                let result = port.evaluate(Self::request(cpu, operation, format, input, 0));
                Self::apply_conversion(&mut staged, result, fp_to_integer, integer_wide, destination);
            }
            Aarch64Instruction::Bf16Convert {
                source,
                destination,
                vector,
                high,
            } => {
                crate::aarch64_simd_bf16::Bf16::execute(cpu, &mut staged, source, destination, vector, high);
            }
            Aarch64Instruction::SimdFpEstimate {
                format,
                reciprocal_sqrt,
                source,
                destination,
                lanes,
            } => {
                crate::aarch64_simd_reciprocal::Reciprocal::estimate(
                    cpu,
                    &mut staged,
                    format,
                    reciprocal_sqrt,
                    source,
                    destination,
                    lanes,
                );
            }
            Aarch64Instruction::SimdFpRecipExponent {
                format,
                source,
                destination,
            } => {
                crate::aarch64_simd_reciprocal::Reciprocal::exponent(cpu, &mut staged, format, source, destination);
            }
            Aarch64Instruction::SimdUnsignedEstimate {
                reciprocal_sqrt,
                source,
                destination,
                wide,
            } => {
                crate::aarch64_simd_reciprocal::Reciprocal::unsigned(
                    cpu,
                    &mut staged,
                    reciprocal_sqrt,
                    source,
                    destination,
                    wide,
                );
            }
            Aarch64Instruction::SimdFpStep {
                format,
                reciprocal_sqrt,
                left,
                right,
                destination,
                lanes,
            } => {
                crate::aarch64_simd_reciprocal::Reciprocal::step(
                    cpu,
                    &mut staged,
                    port,
                    format,
                    reciprocal_sqrt,
                    left,
                    right,
                    destination,
                    lanes,
                );
            }
            Aarch64Instruction::SimdFpNarrowOdd {
                source,
                destination,
                high,
                scalar,
            } => crate::aarch64_simd_narrow::NarrowOdd::execute(
                cpu,
                &mut staged,
                port,
                source,
                destination,
                high,
                scalar,
            ),
            Aarch64Instruction::SimdFpConvert {
                format,
                source,
                destination,
                high,
                widen,
            } => crate::aarch64_simd_convert::Convert::execute(
                cpu,
                &mut staged,
                port,
                format,
                source,
                destination,
                high,
                widen,
            ),
            Aarch64Instruction::SimdFpInteger {
                format,
                lanes,
                signed,
                scale,
                rounding,
                source,
                destination,
            } => {
                let mut value = 0_u128;
                for lane in 0..lanes {
                    let input = cpu.vector_lane(source, format.bits(), lane);
                    let result = port.evaluate(Self::request(
                        cpu,
                        FpArithmetic::FloatToScaled {
                            signed,
                            width: format.bits(),
                            scale,
                            rounding,
                        },
                        format,
                        input,
                        0,
                    ));
                    staged.fpsr |= u64::from(result.exceptions);
                    value |= u128::from(result.value) << (u32::from(lane) * u32::from(format.bits()));
                }
                staged.set_vector(destination, value);
            }
            Aarch64Instruction::SimdIntegerFp {
                format,
                lanes,
                signed,
                source,
                destination,
            } => {
                let mut value = 0_u128;
                for lane in 0..lanes {
                    let input = cpu.vector_lane(source, format.bits(), lane);
                    let result = port.evaluate(Self::request(
                        cpu,
                        FpArithmetic::IntegerToFloat {
                            signed,
                            width: format.bits(),
                        },
                        format,
                        input,
                        0,
                    ));
                    staged.fpsr |= u64::from(result.exceptions);
                    value |= u128::from(result.value) << (u32::from(lane) * u32::from(format.bits()));
                }
                staged.set_vector(destination, value);
            }
            Aarch64Instruction::SimdFpBinary {
                operation,
                format,
                lanes,
                left,
                right,
                destination,
            } => {
                crate::aarch64_simd_arithmetic::Binary::execute(
                    cpu,
                    &mut staged,
                    port,
                    operation,
                    format,
                    lanes,
                    left,
                    right,
                    destination,
                );
            }
            Aarch64Instruction::SimdFpFused {
                format,
                lanes,
                subtract,
                left,
                right,
                index,
                destination,
                scalar: _,
            } => {
                Self::fused(
                    cpu,
                    &mut staged,
                    port,
                    format,
                    lanes,
                    subtract,
                    left,
                    right,
                    index,
                    destination,
                );
            }
            Aarch64Instruction::SimdFpProduct {
                format,
                lanes,
                extended,
                left,
                right,
                index,
                destination,
                scalar: _,
            } => {
                Self::product(
                    cpu,
                    &mut staged,
                    port,
                    format,
                    lanes,
                    extended,
                    left,
                    right,
                    index,
                    destination,
                );
            }
            Aarch64Instruction::SimdFpReduce {
                operation,
                source,
                destination,
            } => {
                let mut value = cpu.vector_lane(source, 32, 0);
                for lane in 1..4 {
                    let result = port.evaluate(Self::request(
                        cpu,
                        FpArithmetic::Binary(operation),
                        FpFormat::Single,
                        value,
                        cpu.vector_lane(source, 32, lane),
                    ));
                    value = result.value;
                    staged.fpsr |= u64::from(result.exceptions);
                }
                staged.set_vector(destination, u128::from(value));
            }
            _ => unreachable!("FP interpreter called for non-FP instruction"),
        }
        *cpu = staged;
        Aarch64ExecutionExit::Continue
    }
}
