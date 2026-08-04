use crate::x86::real::Conversion;
use crate::{
    AccessKind, CpuState, DecodedInstruction, EffectiveAddress, ExecutionExit, ExtendedClass, ExtendedReal, FloatWidth,
    GuestOperandMemory, ScalarInstruction, ScalarIrError, X87StackOperation,
};

pub(crate) struct ExtendedMemory;

impl ExtendedMemory {
    pub(crate) fn decode(decoded: &DecodedInstruction) -> Result<ScalarInstruction, ScalarIrError> {
        if decoded.raw_mod == Some(3) {
            let group = decoded.raw_reg.ok_or(ScalarIrError::Invalid)?;
            let source = decoded.raw_rm.ok_or(ScalarIrError::Invalid)?;
            if matches!(decoded.opcode, 0xd8 | 0xdc | 0xde) && !matches!(group, 2 | 3) {
                return Ok(ScalarInstruction::X87Arithmetic {
                    address: None,
                    source,
                    operation: group,
                    destination_source: decoded.opcode != 0xd8,
                    pop: decoded.opcode == 0xde,
                    format: FloatWidth::Double,
                    integer_bytes: 0,
                });
            }
            if matches!(decoded.opcode, 0xd8 | 0xdc | 0xde) && matches!(group, 2 | 3) {
                return Ok(ScalarInstruction::X87StatusCompare {
                    address: None,
                    source,
                    pop: if group == 3 {
                        1 + u8::from(decoded.opcode == 0xde && source == 1)
                    } else {
                        0
                    },
                    format: FloatWidth::Double,
                    ordered: true,
                });
            }
            if decoded.opcode == 0xdd && matches!(group, 4 | 5) {
                return Ok(ScalarInstruction::X87StatusCompare {
                    address: None,
                    source,
                    pop: u8::from(group == 5),
                    format: FloatWidth::Double,
                    ordered: false,
                });
            }
            if matches!(decoded.opcode, 0xda | 0xdb) && group <= 3 {
                return Ok(ScalarInstruction::X87ConditionalMove {
                    source,
                    condition: group,
                    negate: decoded.opcode == 0xdb,
                });
            }
            match (decoded.opcode, group, source) {
                (0xdb, 4, 3) => return Ok(ScalarInstruction::X87Initialize),
                (0xdf, 4, 0) => return Ok(ScalarInstruction::X87Status),
                (0xd9, 5, constant @ 0..=6) => return Ok(ScalarInstruction::X87Constant { constant }),
                _ => {}
            }
            if decoded.opcode == 0xdb && group == 4 {
                return Ok(ScalarInstruction::X87Unary { operation: 9, source });
            }
            if decoded.opcode == 0xd9
                && matches!(
                    (group, source),
                    (2, 0) | (4, 0 | 1 | 5) | (6, 0..=7) | (7, 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7)
                )
            {
                return Ok(ScalarInstruction::X87Unary {
                    operation: group,
                    source,
                });
            }
            if decoded.opcode == 0xdd && group == 0 {
                return Ok(ScalarInstruction::X87Unary { operation: 8, source });
            }
            let operation = match (decoded.opcode, group) {
                (0xd9, 0) => Some(X87StackOperation::Load),
                (0xd9, 1) => Some(X87StackOperation::Exchange),
                (0xdd, 2) => Some(X87StackOperation::Store),
                (0xdd, 3) => Some(X87StackOperation::StorePop),
                _ => None,
            };
            if let Some(operation) = operation {
                return Ok(ScalarInstruction::X87Stack { source, operation });
            }
            if matches!(decoded.opcode, 0xdb | 0xdf) && matches!(group, 5 | 6) {
                return Ok(ScalarInstruction::X87Compare {
                    source,
                    ordered: group == 6,
                    pop: decoded.opcode == 0xdf,
                });
            }
            return Err(ScalarIrError::Unsupported);
        }
        let address = decoded.address.ok_or(ScalarIrError::Invalid)?;
        match (decoded.opcode, decoded.raw_reg) {
            (0xdd, Some(7)) => Ok(ScalarInstruction::X87StatusStore { address }),
            (0xdd, Some(group @ (4 | 6))) => Ok(ScalarInstruction::X87Save {
                address,
                load: group == 4,
            }),
            (0xdb, Some(group @ 0..=3)) => Ok(ScalarInstruction::X87Integer {
                address,
                bytes: 4,
                load: group == 0,
                pop: group != 0 && group != 2,
                truncate: group == 1,
            }),
            (0xdd, Some(1)) => Ok(ScalarInstruction::X87Integer {
                address,
                bytes: 8,
                load: false,
                pop: true,
                truncate: true,
            }),
            (0xdf, Some(group @ (0..=3 | 5 | 7))) => Ok(ScalarInstruction::X87Integer {
                address,
                bytes: if matches!(group, 5 | 7) { 8 } else { 2 },
                load: matches!(group, 0 | 5),
                pop: !matches!(group, 0 | 2 | 5),
                truncate: group == 1,
            }),
            (opcode @ (0xd8 | 0xdc), Some(group @ (2 | 3))) => Ok(ScalarInstruction::X87StatusCompare {
                address: Some(address),
                source: 0,
                pop: u8::from(group == 3),
                format: if opcode == 0xd8 {
                    FloatWidth::Single
                } else {
                    FloatWidth::Double
                },
                ordered: true,
            }),
            (opcode @ (0xd8 | 0xdc), Some(operation)) if !matches!(operation, 2 | 3) => {
                Ok(ScalarInstruction::X87Arithmetic {
                    address: Some(address),
                    source: 0,
                    operation,
                    destination_source: false,
                    pop: false,
                    format: if opcode == 0xd8 {
                        FloatWidth::Single
                    } else {
                        FloatWidth::Double
                    },
                    integer_bytes: 0,
                })
            }
            (opcode @ (0xda | 0xde), Some(operation)) if !matches!(operation, 2 | 3) => {
                Ok(ScalarInstruction::X87Arithmetic {
                    address: Some(address),
                    source: 0,
                    operation,
                    destination_source: false,
                    pop: false,
                    format: FloatWidth::Double,
                    integer_bytes: if opcode == 0xda { 4 } else { 2 },
                })
            }
            (0xd9, Some(group @ 4 | group @ 6)) => Ok(ScalarInstruction::X87Environment {
                address,
                load: group == 4,
            }),
            (0xd9, Some(5 | 7)) => super::Control::decode(decoded),
            (0xdb, Some(5 | 7)) => Ok(ScalarInstruction::X87Extended {
                address,
                load: decoded.raw_reg == Some(5),
            }),
            (0xd9, Some(group @ 0 | group @ 2 | group @ 3)) => Ok(ScalarInstruction::X87Float {
                address,
                format: FloatWidth::Single,
                store: group != 0,
                pop: group == 3,
            }),
            (0xdd, Some(group @ 0 | group @ 2 | group @ 3)) => Ok(ScalarInstruction::X87Float {
                address,
                format: FloatWidth::Double,
                store: group != 0,
                pop: group == 3,
            }),
            _ => Err(ScalarIrError::Unsupported),
        }
    }

    pub(crate) fn save<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        effective: EffectiveAddress,
        load: bool,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let address = effective.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
        let access = if load { AccessKind::Read } else { AccessKind::Write };
        if !Self::canonical(address) || !Self::canonical(address.wrapping_add(107)) {
            return ExecutionExit::NonCanonical {
                instruction,
                address,
                access,
            };
        }
        if load {
            let mut environment = [0_u64; 7];
            for (index, value) in environment.iter_mut().enumerate() {
                let field = address + (index as u64) * 4;
                *value = match memory.read(field, 4) {
                    Ok(value) => value,
                    Err(()) => return Self::fault(instruction, field, access, 108),
                };
            }
            let mut registers = [ExtendedReal::from_bits(0); 8];
            for (index, value) in registers.iter_mut().enumerate() {
                let field = address + 28 + (index as u64) * 10;
                let low = match memory.read(field, 8) {
                    Ok(value) => value,
                    Err(()) => return Self::fault(instruction, field, access, 108),
                };
                let high = match memory.read(field + 8, 2) {
                    Ok(value) => value,
                    Err(()) => return Self::fault(instruction, field + 8, access, 108),
                };
                *value = ExtendedReal::from_bits(u128::from(low) | u128::from(high) << 64);
            }
            let mut staged = cpu.clone();
            staged.x87_control = environment[0] as u16 & 0x1f3f | 0x0040;
            staged.x87_status = environment[1] as u16;
            let tags = environment[2] as u16;
            let top = usize::from((staged.x87_status >> 11) & 7);
            for logical in 0..8 {
                let physical = (top + logical) & 7;
                staged.x87_values[physical] = registers[logical];
                staged.x87_classes[physical] = if tags >> (physical * 2) & 3 == 3 {
                    ExtendedClass::Empty
                } else {
                    registers[logical].class()
                };
            }
            staged.rip = next;
            *cpu = staged;
            return ExecutionExit::Continue;
        }
        let top = usize::from((cpu.x87_status >> 11) & 7);
        let mut tags = 0_u16;
        for (physical, class) in cpu.x87_classes.iter().copied().enumerate() {
            let tag = match class {
                ExtendedClass::Normal => 0,
                ExtendedClass::Zero => 1,
                ExtendedClass::Empty => 3,
                _ => 2,
            };
            tags |= tag << (physical * 2);
        }
        let mut writes = Vec::with_capacity(23);
        let mut values = Vec::with_capacity(23);
        for (index, value) in [
            0xffff_0000 | u64::from(cpu.x87_control),
            0xffff_0000 | u64::from(cpu.x87_status),
            0xffff_0000 | u64::from(tags),
            0,
            0,
            0,
            0xffff_0000,
        ]
        .into_iter()
        .enumerate()
        {
            writes.push((address + (index as u64) * 4, 4));
            values.push(value);
        }
        for logical in 0..8 {
            let bits = cpu.x87_values[(top + logical) & 7].bits();
            let field = address + 28 + (logical as u64) * 10;
            writes.push((field, 8));
            values.push(bits as u64);
            writes.push((field + 8, 2));
            values.push((bits >> 64) as u64);
        }
        let reservation = match memory.reserve_write_batch(&writes) {
            Ok(value) => value,
            Err(fault) => return Self::fault(instruction, fault, access, 108),
        };
        if memory.commit_write_batch(reservation, &values).is_err() {
            return Self::fault(instruction, address, access, 108);
        }
        cpu.x87_control = 0x037f;
        cpu.x87_status = 0;
        cpu.x87_classes.fill(ExtendedClass::Empty);
        cpu.rip = next;
        ExecutionExit::Continue
    }

    pub(crate) fn unary(cpu: &mut CpuState, operation: u8, source: u8, instruction: u64, next: u64) -> ExecutionExit {
        let top = usize::from((cpu.x87_status >> 11) & 7);
        let mut staged = cpu.clone();
        staged.x87_status &= !(1 << 9);
        if matches!((operation, source), (4, 0 | 1)) && staged.x87_classes[top] == ExtendedClass::Empty {
            if Self::raise_stack(&mut staged, false) {
                *cpu = staged;
                return ExecutionExit::UndefinedInstruction { instruction };
            }
            staged.x87_values[top] = ExtendedReal::INDEFINITE;
            staged.x87_classes[top] = ExtendedClass::QuietNan;
            staged.rip = next;
            *cpu = staged;
            return ExecutionExit::Continue;
        }
        match (operation, source) {
            (2, 0) => {}
            (4, 0) => staged.x87_values[top] = ExtendedReal::from_bits(staged.x87_values[top].bits() ^ (1_u128 << 79)),
            (4, 1) => staged.x87_values[top] = ExtendedReal::from_bits(staged.x87_values[top].bits() & !(1_u128 << 79)),
            (4, 5) => {
                let code = match staged.x87_classes[top] {
                    ExtendedClass::Unsupported => 0,
                    ExtendedClass::QuietNan | ExtendedClass::SignalingNan => 1,
                    ExtendedClass::Normal => 2,
                    ExtendedClass::Infinity => 3,
                    ExtendedClass::Zero => 4,
                    ExtendedClass::Empty => 5,
                    ExtendedClass::Denormal => 6,
                };
                staged.x87_status &= !((1 << 14) | (1 << 10) | (1 << 8));
                staged.x87_status |=
                    ((code >> 2) as u16) << 14 | (((code >> 1) & 1) as u16) << 10 | ((code & 1) as u16) << 8;
                if staged.x87_values[top].bits() >> 79 != 0 {
                    staged.x87_status |= 1 << 9;
                }
            }
            (6, 6 | 7) => {
                let current = (staged.x87_status >> 11) & 7;
                let adjusted = if source == 6 {
                    current.wrapping_sub(1) & 7
                } else {
                    current.wrapping_add(1) & 7
                };
                staged.x87_status = (staged.x87_status & !0x3800) | adjusted << 11;
            }
            (7, 2) => {
                if staged.x87_classes[top] == ExtendedClass::Empty {
                    if Self::raise_stack(&mut staged, false) {
                        *cpu = staged;
                        return ExecutionExit::UndefinedInstruction { instruction };
                    }
                    staged.x87_values[top] = ExtendedReal::INDEFINITE;
                    staged.x87_classes[top] = ExtendedClass::QuietNan;
                } else {
                    let value = f64::from_bits(
                        Conversion::narrow(staged.x87_values[top], staged.x87_classes[top], FloatWidth::Double, 0).bits,
                    );
                    let invalid = staged.x87_classes[top] == ExtendedClass::SignalingNan
                        || (value.is_sign_negative() && value != 0.0);
                    if invalid && Self::raise(&mut staged, 1) {
                        staged.rip = next;
                        *cpu = staged;
                        return ExecutionExit::Continue;
                    }
                    let mut environment = hl_softfloat::Environment::default();
                    environment.rounding = match staged.x87_control >> 10 & 3 {
                        0 => hl_softfloat::RoundingMode::NearestEven,
                        1 => hl_softfloat::RoundingMode::TowardNegative,
                        2 => hl_softfloat::RoundingMode::TowardPositive,
                        _ => hl_softfloat::RoundingMode::TowardZero,
                    };
                    let result = environment.square_root(hl_softfloat::Value::from_bits(
                        hl_softfloat::Format::Binary64,
                        value.to_bits(),
                    ));
                    let mut bits = result.value.bits();
                    let mut flags = crate::x86::scalar::arithmetic::Arithmetic::exceptions(result.flags) as u16;
                    let (narrowed, precision) = Self::precision(bits, staged.x87_control);
                    bits = narrowed;
                    flags |= precision;
                    if flags != 0 {
                        Self::raise(&mut staged, flags);
                    }
                    let (expanded, class) = Self::computed(bits, &[staged.x87_classes[top]]);
                    staged.x87_values[top] = expanded;
                    staged.x87_classes[top] = class;
                }
            }
            (6, 5) | (7, 0) => {
                let other = (top + 1) & 7;
                if staged.x87_classes[top] == ExtendedClass::Empty || staged.x87_classes[other] == ExtendedClass::Empty
                {
                    if Self::raise_stack(&mut staged, false) {
                        *cpu = staged;
                        return ExecutionExit::UndefinedInstruction { instruction };
                    }
                    staged.x87_values[top] = ExtendedReal::INDEFINITE;
                    staged.x87_classes[top] = ExtendedClass::QuietNan;
                } else {
                    let left = f64::from_bits(
                        Conversion::narrow(staged.x87_values[top], staged.x87_classes[top], FloatWidth::Double, 0).bits,
                    );
                    let right = f64::from_bits(
                        Conversion::narrow(
                            staged.x87_values[other],
                            staged.x87_classes[other],
                            FloatWidth::Double,
                            0,
                        )
                        .bits,
                    );
                    if !left.is_nan() && !right.is_nan() && (left.is_infinite() || right == 0.0) {
                        if Self::raise(&mut staged, 1) {
                            *cpu = staged;
                            return ExecutionExit::UndefinedInstruction { instruction };
                        }
                        staged.x87_values[top] = ExtendedReal::INDEFINITE;
                        staged.x87_classes[top] = ExtendedClass::QuietNan;
                        staged.x87_status &= !((1 << 14) | (1 << 10) | (1 << 9) | (1 << 8));
                    } else {
                        let ieee = operation == 6;
                        let spread = left.abs().log2().floor() - right.abs().log2().floor();
                        let (result, quotient, partial) =
                            if left.is_finite() && right.is_finite() && left != 0.0 && right != 0.0 && spread >= 64.0 {
                                let scaled = right * 8.0;
                                (if scaled.is_infinite() { left } else { left % scaled }, 0_i64, true)
                            } else {
                                let left_magnitude = left.abs();
                                let right_magnitude = right.abs();
                                let scaled = right_magnitude * 8.0;
                                let reduced = if scaled > left_magnitude {
                                    left_magnitude
                                } else {
                                    left_magnitude % scaled
                                };
                                let mut quotient = ((reduced / right_magnitude) as i64).min(7);
                                let truncated = left % right;
                                let result = if ieee {
                                    let half = right_magnitude * 0.5;
                                    if truncated.abs() > half || truncated.abs() == half && quotient & 1 != 0 {
                                        quotient += 1;
                                        truncated - right_magnitude.copysign(left)
                                    } else {
                                        truncated
                                    }
                                } else {
                                    truncated
                                };
                                (result, quotient & 7, false)
                            };
                        let (value, class) = Conversion::expand(result.to_bits(), FloatWidth::Double);
                        staged.x87_values[top] = value;
                        staged.x87_classes[top] = class;
                        staged.x87_status &= !((1 << 14) | (1 << 10) | (1 << 9) | (1 << 8));
                        if partial {
                            staged.x87_status |= 1 << 10;
                        } else {
                            staged.x87_status |= (((quotient >> 1) & 1) as u16) << 14;
                            staged.x87_status |= ((quotient & 1) as u16) << 9;
                            staged.x87_status |= (((quotient >> 2) & 1) as u16) << 8;
                        }
                    }
                }
            }
            (6, 4) => {
                let destination = top.wrapping_sub(1) & 7;
                let empty = staged.x87_classes[top] == ExtendedClass::Empty;
                if empty || staged.x87_classes[destination] != ExtendedClass::Empty {
                    if Self::raise_stack(&mut staged, !empty) {
                        *cpu = staged;
                        return ExecutionExit::UndefinedInstruction { instruction };
                    }
                    staged.x87_values[top] = ExtendedReal::INDEFINITE;
                    staged.x87_classes[top] = ExtendedClass::QuietNan;
                    staged.x87_values[destination] = ExtendedReal::INDEFINITE;
                    staged.x87_classes[destination] = ExtendedClass::QuietNan;
                } else {
                    let value = f64::from_bits(
                        Conversion::narrow(staged.x87_values[top], staged.x87_classes[top], FloatWidth::Double, 0).bits,
                    );
                    let bits = value.to_bits();
                    let exponent = ((bits >> 52 & 0x7ff) as i64 - 1023) as f64;
                    let significand = f64::from_bits((bits & !(0x7ff_u64 << 52)) | (1023_u64 << 52));
                    let (exponent, exponent_class) = Conversion::expand(exponent.to_bits(), FloatWidth::Double);
                    let (significand, significand_class) =
                        Conversion::expand(significand.to_bits(), FloatWidth::Double);
                    staged.x87_values[top] = exponent;
                    staged.x87_classes[top] = exponent_class;
                    staged.x87_values[destination] = significand;
                    staged.x87_classes[destination] = significand_class;
                }
                staged.x87_status = (staged.x87_status & !0x3800) | (destination as u16) << 11;
            }
            (7, 5) => {
                let other = (top + 1) & 7;
                if staged.x87_classes[top] == ExtendedClass::Empty || staged.x87_classes[other] == ExtendedClass::Empty
                {
                    if Self::raise_stack(&mut staged, false) {
                        *cpu = staged;
                        return ExecutionExit::UndefinedInstruction { instruction };
                    }
                    staged.x87_values[top] = ExtendedReal::INDEFINITE;
                    staged.x87_classes[top] = ExtendedClass::QuietNan;
                } else {
                    let left = f64::from_bits(
                        Conversion::narrow(staged.x87_values[top], staged.x87_classes[top], FloatWidth::Double, 0).bits,
                    );
                    let scale = f64::from_bits(
                        Conversion::narrow(
                            staged.x87_values[other],
                            staged.x87_classes[other],
                            FloatWidth::Double,
                            0,
                        )
                        .bits,
                    )
                    .trunc();
                    let result = Self::scale(left, scale);
                    let (value, class) =
                        Self::computed(result.to_bits(), &[staged.x87_classes[top], staged.x87_classes[other]]);
                    staged.x87_values[top] = value;
                    staged.x87_classes[top] = class;
                }
            }
            (6, 0) | (7, 6 | 7) | (7, 4) => {
                if staged.x87_classes[top] == ExtendedClass::Empty {
                    if Self::raise_stack(&mut staged, false) {
                        *cpu = staged;
                        return ExecutionExit::UndefinedInstruction { instruction };
                    }
                    staged.x87_values[top] = ExtendedReal::INDEFINITE;
                    staged.x87_classes[top] = ExtendedClass::QuietNan;
                } else {
                    let value = f64::from_bits(
                        Conversion::narrow(staged.x87_values[top], staged.x87_classes[top], FloatWidth::Double, 0).bits,
                    );
                    let result = match (operation, source) {
                        (6, 0) => value.exp2() - 1.0,
                        (7, 4) => match (staged.x87_control >> 10) & 3 {
                            0 => value.round_ties_even(),
                            1 => value.floor(),
                            2 => value.ceil(),
                            _ => value.trunc(),
                        },
                        (7, 6) => value.sin(),
                        (7, 7) => value.cos(),
                        _ => unreachable!(),
                    };
                    if (operation, source) == (7, 4) && result.abs() > value.abs() {
                        staged.x87_status |= 1 << 9;
                    }
                    let (result, class) = Conversion::expand(result.to_bits(), FloatWidth::Double);
                    staged.x87_values[top] = result;
                    staged.x87_classes[top] = class;
                }
            }
            (6, 1 | 3) | (7, 1) => {
                let other = (top + 1) & 7;
                if staged.x87_classes[top] == ExtendedClass::Empty || staged.x87_classes[other] == ExtendedClass::Empty
                {
                    Self::raise_stack(&mut staged, false);
                    staged.x87_values[other] = ExtendedReal::INDEFINITE;
                    staged.x87_classes[other] = ExtendedClass::QuietNan;
                } else {
                    let left = f64::from_bits(
                        Conversion::narrow(staged.x87_values[top], staged.x87_classes[top], FloatWidth::Double, 0).bits,
                    );
                    let right = f64::from_bits(
                        Conversion::narrow(
                            staged.x87_values[other],
                            staged.x87_classes[other],
                            FloatWidth::Double,
                            0,
                        )
                        .bits,
                    );
                    let result = match (operation, source) {
                        (6, 1) => right * left.log2(),
                        (6, 3) => right.atan2(left),
                        (7, 1) => right * (left + 1.0).log2(),
                        _ => unreachable!(),
                    };
                    let (result, class) = Conversion::expand(result.to_bits(), FloatWidth::Double);
                    staged.x87_values[other] = result;
                    staged.x87_classes[other] = class;
                }
                staged.x87_classes[top] = ExtendedClass::Empty;
                staged.x87_status = (staged.x87_status & !0x3800) | (other as u16) << 11;
            }
            (6, 2) | (7, 3) => {
                let destination = top.wrapping_sub(1) & 7;
                if staged.x87_classes[top] == ExtendedClass::Empty
                    || staged.x87_classes[destination] != ExtendedClass::Empty
                {
                    let overflow = staged.x87_classes[destination] != ExtendedClass::Empty;
                    Self::raise_stack(&mut staged, overflow);
                    staged.x87_values[top] = ExtendedReal::INDEFINITE;
                    staged.x87_classes[top] = ExtendedClass::QuietNan;
                    staged.x87_values[destination] = ExtendedReal::INDEFINITE;
                    staged.x87_classes[destination] = ExtendedClass::QuietNan;
                } else {
                    let value = f64::from_bits(
                        Conversion::narrow(staged.x87_values[top], staged.x87_classes[top], FloatWidth::Double, 0).bits,
                    );
                    if value.abs() >= 2_f64.powi(63) {
                        staged.x87_status |= 1 << 10;
                        staged.rip = next;
                        *cpu = staged;
                        return ExecutionExit::Continue;
                    }
                    let (old_result, pushed) = if operation == 6 {
                        (value.tan(), 1.0)
                    } else {
                        (value.sin(), value.cos())
                    };
                    let (old_result, old_class) = Conversion::expand(old_result.to_bits(), FloatWidth::Double);
                    let (pushed, pushed_class) = Conversion::expand(pushed.to_bits(), FloatWidth::Double);
                    staged.x87_values[top] = old_result;
                    staged.x87_classes[top] = old_class;
                    staged.x87_values[destination] = pushed;
                    staged.x87_classes[destination] = pushed_class;
                }
                staged.x87_status = (staged.x87_status & !0x3800) | (destination as u16) << 11;
            }
            (8, _) => staged.x87_classes[(top + usize::from(source)) & 7] = ExtendedClass::Empty,
            (9, 2) => staged.x87_status &= !0x80ff,
            (9, _) => {}
            _ => return ExecutionExit::UndefinedInstruction { instruction },
        }
        staged.rip = next;
        *cpu = staged;
        ExecutionExit::Continue
    }

    pub(crate) fn integer<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        effective: EffectiveAddress,
        bytes: u8,
        load: bool,
        pop: bool,
        truncate: bool,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let address = effective.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
        let access = if load { AccessKind::Read } else { AccessKind::Write };
        if !Self::canonical(address) || !Self::canonical(address.wrapping_add(u64::from(bytes) - 1)) {
            return ExecutionExit::NonCanonical {
                instruction,
                address,
                access,
            };
        }
        if load {
            let bits = match memory.read(address, bytes) {
                Ok(value) => value,
                Err(()) => return Self::fault(instruction, address, access, bytes),
            };
            let signed = match bytes {
                2 => i64::from(bits as i16),
                4 => i64::from(bits as i32),
                8 => bits as i64,
                _ => return ExecutionExit::UndefinedInstruction { instruction },
            };
            let (value, class) = Conversion::expand((signed as f64).to_bits(), FloatWidth::Double);
            let mut staged = cpu.clone();
            let destination = usize::from(((staged.x87_status >> 11) as u8).wrapping_sub(1) & 7);
            if staged.x87_classes[destination] != ExtendedClass::Empty {
                return Self::stack_fault(cpu, staged, destination, instruction, next, true);
            }
            staged.x87_status = (staged.x87_status & !0x3a00) | (destination as u16) << 11;
            staged.x87_values[destination] = value;
            staged.x87_classes[destination] = class;
            staged.rip = next;
            *cpu = staged;
            return ExecutionExit::Continue;
        }
        let reservation = match memory.reserve_write(address, bytes) {
            Ok(value) => value,
            Err(()) => return Self::fault(instruction, address, access, bytes),
        };
        let top = usize::from((cpu.x87_status >> 11) & 7);
        let mut staged = cpu.clone();
        let empty = staged.x87_classes[top] == ExtendedClass::Empty;
        let mut invalid = empty;
        let mut rounded_up = false;
        let stored = if empty {
            Self::raise_stack(&mut staged, false);
            1_u64 << (u32::from(bytes) * 8 - 1)
        } else {
            let value = f64::from_bits(
                Conversion::narrow(staged.x87_values[top], staged.x87_classes[top], FloatWidth::Double, 0).bits,
            );
            let rounded = if truncate {
                value.trunc()
            } else {
                match (staged.x87_control >> 10) & 3 {
                    0 => value.round_ties_even(),
                    1 => value.floor(),
                    2 => value.ceil(),
                    _ => value.trunc(),
                }
            };
            rounded_up = !rounded.is_nan() && !value.is_nan() && rounded.abs() > value.abs();
            let (minimum, maximum) = match bytes {
                2 => (i16::MIN as f64, i16::MAX as f64),
                4 => (i32::MIN as f64, i32::MAX as f64),
                8 => (i64::MIN as f64, i64::MAX as f64),
                _ => return ExecutionExit::UndefinedInstruction { instruction },
            };
            if !rounded.is_finite() || rounded < minimum || rounded > maximum {
                invalid = true;
                1_u64 << (u32::from(bytes) * 8 - 1)
            } else {
                rounded as i64 as u64
            }
        };
        if invalid && Self::raise(&mut staged, 1) {
            *cpu = staged;
            return ExecutionExit::UndefinedInstruction { instruction };
        }
        if memory.commit_write(reservation, stored).is_err() {
            return Self::fault(instruction, address, access, bytes);
        }
        staged.x87_status = (staged.x87_status & !(1 << 9)) | u16::from(rounded_up) << 9;
        if pop {
            staged.x87_classes[top] = ExtendedClass::Empty;
            staged.x87_status = (staged.x87_status & !0x3800) | (((top + 1) & 7) as u16) << 11;
        }
        staged.rip = next;
        *cpu = staged;
        ExecutionExit::Continue
    }

    pub(crate) fn status_compare<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &M,
        effective: Option<EffectiveAddress>,
        source: u8,
        pop: u8,
        format: FloatWidth,
        ordered: bool,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let top = usize::from((cpu.x87_status >> 11) & 7);
        let (right, right_class) = if let Some(effective) = effective {
            let address = effective.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
            let bytes = if format == FloatWidth::Single { 4 } else { 8 };
            let bits = match memory.read(address, bytes as u8) {
                Ok(value) => value,
                Err(()) => return Self::fault(instruction, address, AccessKind::Read, bytes as u8),
            };
            Conversion::expand(bits, format)
        } else {
            let index = (top + usize::from(source)) & 7;
            (cpu.x87_values[index], cpu.x87_classes[index])
        };
        let left_class = cpu.x87_classes[top];
        let stack_fault = left_class == ExtendedClass::Empty || right_class == ExtendedClass::Empty;
        let invalid =
            stack_fault || Self::invalid_compare(left_class, ordered) || Self::invalid_compare(right_class, ordered);
        let mut staged = cpu.clone();
        let relation = if invalid {
            if stack_fault {
                staged.x87_status |= 1 << 6;
            }
            if Self::raise(&mut staged, 1) {
                *cpu = staged;
                return ExecutionExit::UndefinedInstruction { instruction };
            }
            None
        } else {
            Self::relation(cpu.x87_values[top], left_class, right, right_class)
        };
        staged.x87_status &= !((1 << 14) | (1 << 10) | (1 << 9) | (1 << 8));
        if relation.is_none() || relation == Some(std::cmp::Ordering::Equal) {
            staged.x87_status |= 1 << 14;
        }
        if relation.is_none() {
            staged.x87_status |= 1 << 10;
        }
        if relation.is_none() || relation == Some(std::cmp::Ordering::Less) {
            staged.x87_status |= 1 << 8;
        }
        for _ in 0..pop {
            let current = usize::from((staged.x87_status >> 11) & 7);
            staged.x87_classes[current] = ExtendedClass::Empty;
            staged.x87_status = (staged.x87_status & !0x3800) | (((current + 1) & 7) as u16) << 11;
        }
        staged.rip = next;
        *cpu = staged;
        ExecutionExit::Continue
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn arithmetic<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &M,
        effective: Option<EffectiveAddress>,
        source: u8,
        mut operation: u8,
        destination_source: bool,
        pop: bool,
        format: FloatWidth,
        integer_bytes: u8,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let top = usize::from((cpu.x87_status >> 11) & 7);
        let source_index = (top + usize::from(source)) & 7;
        let (right, right_class) = if let Some(effective) = effective {
            let address = effective.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
            let bytes = if integer_bytes != 0 {
                u64::from(integer_bytes)
            } else if format == FloatWidth::Single {
                4
            } else {
                8
            };
            if !Self::canonical(address) || !Self::canonical(address.wrapping_add(bytes - 1)) {
                return ExecutionExit::NonCanonical {
                    instruction,
                    address,
                    access: AccessKind::Read,
                };
            }
            let bits = match memory.read(address, bytes as u8) {
                Ok(value) => value,
                Err(()) => return Self::fault(instruction, address, AccessKind::Read, bytes as u8),
            };
            if integer_bytes != 0 {
                let signed = if integer_bytes == 2 {
                    i64::from(bits as i16)
                } else {
                    i64::from(bits as i32)
                };
                Conversion::expand((signed as f64).to_bits(), FloatWidth::Double)
            } else {
                Conversion::expand(bits, format)
            }
        } else {
            (
                cpu.x87_values[if destination_source { top } else { source_index }],
                cpu.x87_classes[if destination_source { top } else { source_index }],
            )
        };
        let destination = if destination_source { source_index } else { top };
        let left = cpu.x87_values[destination];
        let left_class = cpu.x87_classes[destination];
        let mut staged = cpu.clone();
        staged.x87_status &= !(1 << 9);
        if left_class == ExtendedClass::Empty || right_class == ExtendedClass::Empty {
            if Self::raise_stack(&mut staged, false) {
                *cpu = staged;
                return ExecutionExit::UndefinedInstruction { instruction };
            }
            staged.x87_values[destination] = ExtendedReal::INDEFINITE;
            staged.x87_classes[destination] = ExtendedClass::QuietNan;
        } else {
            if destination_source && matches!(operation, 4..=7) {
                operation ^= 1;
            }
            let left_value = left;
            let right_value = right;
            let left = f64::from_bits(Conversion::narrow(left_value, left_class, FloatWidth::Double, 0).bits);
            let right = f64::from_bits(Conversion::narrow(right_value, right_class, FloatWidth::Double, 0).bits);
            let mut environment = hl_softfloat::Environment::default();
            environment.rounding = match cpu.x87_control >> 10 & 3 {
                0 => hl_softfloat::RoundingMode::NearestEven,
                1 => hl_softfloat::RoundingMode::TowardNegative,
                2 => hl_softfloat::RoundingMode::TowardPositive,
                _ => hl_softfloat::RoundingMode::TowardZero,
            };
            let left_soft = hl_softfloat::Value::from_bits(hl_softfloat::Format::Binary64, left.to_bits());
            let right_soft = hl_softfloat::Value::from_bits(hl_softfloat::Format::Binary64, right.to_bits());
            let soft = match operation {
                0 => environment.add(left_soft, right_soft),
                1 => environment.multiply(left_soft, right_soft),
                4 => environment.subtract(left_soft, right_soft),
                5 => environment.subtract(right_soft, left_soft),
                6 => environment.divide(left_soft, right_soft),
                7 => environment.divide(right_soft, left_soft),
                _ => return ExecutionExit::UndefinedInstruction { instruction },
            };
            let result_bits = soft.value.bits();
            let mut soft_flags = crate::x86::scalar::arithmetic::Arithmetic::exceptions(soft.flags) as u16;
            let (result_bits, precision) = Self::precision(result_bits, cpu.x87_control);
            soft_flags |= precision;
            let result = f64::from_bits(result_bits);
            let invalid = matches!(operation, 0 | 4 | 5)
                && result.is_nan()
                && left_class != ExtendedClass::QuietNan
                && right_class != ExtendedClass::QuietNan
                || operation == 1 && ((left == 0.0 && right.is_infinite()) || (right == 0.0 && left.is_infinite()))
                || matches!(operation, 6 | 7)
                    && ((left == 0.0 && right == 0.0) || (left.is_infinite() && right.is_infinite()))
                || left_class == ExtendedClass::SignalingNan
                || right_class == ExtendedClass::SignalingNan;
            let divisor = if operation == 6 { right } else { left };
            let dividend = if operation == 6 { left } else { right };
            let divide_zero = matches!(operation, 6 | 7) && divisor == 0.0 && dividend != 0.0 && dividend.is_finite();
            let inexact = matches!(operation, 6 | 7)
                && !invalid
                && !divide_zero
                && result.is_finite()
                && Self::division_inexact(
                    if operation == 6 { left_value } else { right_value },
                    if operation == 6 { right_value } else { left_value },
                );
            let flags = soft_flags
                | u16::from(invalid)
                | u16::from(left_class == ExtendedClass::Denormal || right_class == ExtendedClass::Denormal) << 1
                | u16::from(divide_zero) << 2
                | u16::from(inexact) << 5;
            if flags != 0 && Self::raise(&mut staged, flags) {
                staged.rip = next;
                *cpu = staged;
                return ExecutionExit::Continue;
            }
            let (value, class) = Self::computed(result.to_bits(), &[left_class, right_class]);
            staged.x87_values[destination] = value;
            staged.x87_classes[destination] = class;
        }
        if pop {
            staged.x87_classes[top] = ExtendedClass::Empty;
            staged.x87_status = (staged.x87_status & !0x3800) | (((top + 1) & 7) as u16) << 11;
        }
        staged.rip = next;
        *cpu = staged;
        ExecutionExit::Continue
    }

    pub(crate) fn environment<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        effective: EffectiveAddress,
        load: bool,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let address = effective.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
        let access = if load { AccessKind::Read } else { AccessKind::Write };
        if !Self::canonical(address) || !Self::canonical(address.wrapping_add(27)) {
            return ExecutionExit::NonCanonical {
                instruction,
                address,
                access,
            };
        }
        if load {
            let mut values = [0_u64; 7];
            for (index, value) in values.iter_mut().enumerate() {
                let field = address + (index as u64) * 4;
                *value = match memory.read(field, 4) {
                    Ok(value) => value,
                    Err(()) => return Self::fault(instruction, field, access, 28),
                };
            }
            let mut staged = cpu.clone();
            staged.x87_control = values[0] as u16 & 0x1f3f | 0x0040;
            staged.x87_status = values[1] as u16;
            let tags = values[2] as u16;
            for physical in 0..8 {
                staged.x87_classes[physical] = if tags >> (physical * 2) & 3 == 3 {
                    ExtendedClass::Empty
                } else {
                    staged.x87_values[physical].class()
                };
            }
            staged.rip = next;
            *cpu = staged;
            return ExecutionExit::Continue;
        }
        let mut tags = 0_u16;
        for (physical, class) in cpu.x87_classes.iter().copied().enumerate() {
            let tag = match class {
                ExtendedClass::Normal => 0,
                ExtendedClass::Zero => 1,
                ExtendedClass::Empty => 3,
                _ => 2,
            };
            tags |= tag << (physical * 2);
        }
        let values = [
            0xffff_0000 | u64::from(cpu.x87_control),
            0xffff_0000 | u64::from(cpu.x87_status),
            0xffff_0000 | u64::from(tags),
            0,
            0,
            0,
            0xffff_0000,
        ];
        let writes: [(u64, u8); 7] = std::array::from_fn(|index| (address + (index as u64) * 4, 4));
        let reservation = match memory.reserve_write_batch(&writes) {
            Ok(value) => value,
            Err(fault) => return Self::fault(instruction, fault, access, 28),
        };
        if memory.commit_write_batch(reservation, &values).is_err() {
            return Self::fault(instruction, address, access, 28);
        }
        cpu.x87_control |= 0x3f;
        cpu.rip = next;
        ExecutionExit::Continue
    }

    pub(crate) fn initialize(cpu: &mut CpuState, next: u64) -> ExecutionExit {
        cpu.x87_control = 0x037f;
        cpu.x87_status = 0;
        cpu.x87_classes.fill(ExtendedClass::Empty);
        cpu.rip = next;
        ExecutionExit::Continue
    }

    pub(crate) fn status(cpu: &mut CpuState, next: u64) -> ExecutionExit {
        cpu.write_register(
            crate::ScalarRegister::General(0),
            crate::ScalarWidth::Word,
            u64::from(cpu.x87_status),
        );
        cpu.rip = next;
        ExecutionExit::Continue
    }

    pub(crate) fn store_status<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        effective: EffectiveAddress,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let address = effective.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
        if !Self::canonical(address) || !Self::canonical(address.wrapping_add(1)) {
            return ExecutionExit::NonCanonical {
                instruction,
                address,
                access: AccessKind::Write,
            };
        }
        let reservation = match memory.reserve_write(address, 2) {
            Ok(value) => value,
            Err(()) => return Self::fault(instruction, address, AccessKind::Write, 2),
        };
        if memory.commit_write(reservation, u64::from(cpu.x87_status)).is_err() {
            return Self::fault(instruction, address, AccessKind::Write, 2);
        }
        cpu.rip = next;
        ExecutionExit::Continue
    }

    pub(crate) fn constant(cpu: &mut CpuState, constant: u8, instruction: u64, next: u64) -> ExecutionExit {
        const BITS: [u64; 7] = [
            0x3ff0_0000_0000_0000,
            0x400a_934f_0979_a371,
            0x3ff7_1547_652b_82fe,
            0x4009_21fb_5444_2d18,
            0x3fd3_4413_509f_79ff,
            0x3fe6_2e42_fefa_39ef,
            0,
        ];
        let (value, class) = Conversion::expand(BITS[usize::from(constant)], FloatWidth::Double);
        let mut staged = cpu.clone();
        let destination = usize::from(((staged.x87_status >> 11) as u8).wrapping_sub(1) & 7);
        if staged.x87_classes[destination] != ExtendedClass::Empty {
            return Self::stack_fault(cpu, staged, destination, instruction, next, true);
        }
        staged.x87_status = (staged.x87_status & !0x3800) | (destination as u16) << 11;
        staged.x87_values[destination] = value;
        staged.x87_classes[destination] = class;
        staged.rip = next;
        *cpu = staged;
        ExecutionExit::Continue
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn float<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        effective: EffectiveAddress,
        format: FloatWidth,
        store: bool,
        pop: bool,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let address = effective.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
        let bytes = match format {
            FloatWidth::Single => 4,
            FloatWidth::Double => 8,
        };
        let access = if store { AccessKind::Write } else { AccessKind::Read };
        if !Self::canonical(address) || !Self::canonical(address.wrapping_add(bytes - 1)) {
            return ExecutionExit::NonCanonical {
                instruction,
                address,
                access,
            };
        }
        if store {
            Self::store_float(cpu, memory, address, format, pop, instruction, next)
        } else {
            Self::load_float(cpu, memory, address, format, instruction, next)
        }
    }

    pub(crate) fn compare(
        cpu: &mut CpuState,
        source: u8,
        ordered: bool,
        pop: bool,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let top = usize::from((cpu.x87_status >> 11) & 7);
        let right = (top + usize::from(source)) & 7;
        let left_class = cpu.x87_classes[top];
        let right_class = cpu.x87_classes[right];
        let stack_fault = left_class == ExtendedClass::Empty || right_class == ExtendedClass::Empty;
        let invalid =
            stack_fault || Self::invalid_compare(left_class, ordered) || Self::invalid_compare(right_class, ordered);
        let mut staged = cpu.clone();
        let relation = if invalid {
            if stack_fault {
                staged.x87_status |= 1 << 6;
                staged.x87_status &= !(1 << 9);
            }
            if Self::raise(&mut staged, 1) {
                *cpu = staged;
                return ExecutionExit::UndefinedInstruction { instruction };
            }
            None
        } else {
            Self::relation(cpu.x87_values[top], left_class, cpu.x87_values[right], right_class)
        };
        staged.flags = staged
            .flags
            .with(
                crate::Flag::Zero,
                relation.is_none() || relation == Some(std::cmp::Ordering::Equal),
            )
            .with(crate::Flag::Parity, relation.is_none())
            .with(
                crate::Flag::Carry,
                relation.is_none() || relation == Some(std::cmp::Ordering::Less),
            )
            .with(crate::Flag::Overflow, false)
            .with(crate::Flag::Sign, false)
            .with(crate::Flag::Auxiliary, false);
        if pop {
            staged.x87_classes[top] = ExtendedClass::Empty;
            staged.x87_status = (staged.x87_status & !0x3800) | (((top + 1) & 7) as u16) << 11;
        }
        staged.rip = next;
        *cpu = staged;
        ExecutionExit::Continue
    }

    pub(crate) fn conditional_move(
        cpu: &mut CpuState,
        source: u8,
        condition: u8,
        negate: bool,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let selected = match condition {
            0 => cpu.flags.contains(crate::Flag::Carry),
            1 => cpu.flags.contains(crate::Flag::Zero),
            2 => cpu.flags.contains(crate::Flag::Carry) || cpu.flags.contains(crate::Flag::Zero),
            3 => cpu.flags.contains(crate::Flag::Parity),
            _ => return ExecutionExit::UndefinedInstruction { instruction },
        };
        let top = usize::from((cpu.x87_status >> 11) & 7);
        let source = (top + usize::from(source)) & 7;
        let mut staged = cpu.clone();
        if selected != negate {
            staged.x87_values[top] = staged.x87_values[source];
            staged.x87_classes[top] = staged.x87_classes[source];
        }
        staged.rip = next;
        *cpu = staged;
        ExecutionExit::Continue
    }

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

    pub(crate) fn execute<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        effective: EffectiveAddress,
        load: bool,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let address = effective.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
        let access = if load { AccessKind::Read } else { AccessKind::Write };
        if !Self::canonical(address) || !Self::canonical(address.wrapping_add(9)) {
            return ExecutionExit::NonCanonical {
                instruction,
                address,
                access,
            };
        }
        if load {
            Self::load(cpu, memory, address, instruction, next)
        } else {
            Self::store(cpu, memory, address, instruction, next)
        }
    }

    fn load<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &M,
        address: u64,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let low = match memory.read(address, 8) {
            Ok(value) => value,
            Err(()) => return Self::fault(instruction, address, AccessKind::Read, 10),
        };
        let high = match memory.read(address + 8, 2) {
            Ok(value) => value,
            Err(()) => return Self::fault(instruction, address + 8, AccessKind::Read, 10),
        };
        let source = ExtendedReal::from_bits(u128::from(low) | u128::from(high) << 64);
        let mut staged = cpu.clone();
        let index = usize::from(((staged.x87_status >> 11) as u8).wrapping_sub(1) & 7);
        if staged.x87_classes[index] != ExtendedClass::Empty {
            return Self::stack_fault(cpu, staged, index, instruction, next, true);
        }
        let (value, class, exception) = match source.class() {
            ExtendedClass::Denormal => (source, ExtendedClass::Denormal, Some(1_u16 << 1)),
            ExtendedClass::SignalingNan => {
                let quiet = ExtendedReal::from_bits(source.bits() | (1_u128 << 62));
                (quiet, ExtendedClass::QuietNan, Some(1))
            }
            ExtendedClass::Unsupported => (ExtendedReal::INDEFINITE, ExtendedClass::QuietNan, Some(1)),
            class => (source, class, None),
        };
        if let Some(flag) = exception {
            staged.x87_status |= flag;
            if staged.x87_control & flag == 0 {
                staged.x87_status |= (1 << 7) | (1 << 15);
                *cpu = staged;
                return ExecutionExit::UndefinedInstruction { instruction };
            }
        }
        staged.x87_status = (staged.x87_status & !0x3800) | (index as u16) << 11;
        staged.x87_values[index] = value;
        staged.x87_classes[index] = class;
        staged.rip = next;
        *cpu = staged;
        ExecutionExit::Continue
    }

    fn store<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        address: u64,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let index = usize::from((cpu.x87_status >> 11) & 7);
        let (value, empty) = if cpu.x87_classes[index] == ExtendedClass::Empty {
            (ExtendedReal::INDEFINITE, true)
        } else {
            (cpu.x87_values[index], false)
        };
        if empty && cpu.x87_control & 1 == 0 {
            let mut staged = cpu.clone();
            Self::raise_stack(&mut staged, false);
            *cpu = staged;
            return ExecutionExit::UndefinedInstruction { instruction };
        }
        let writes = [(address, 8), (address + 8, 2)];
        let reservation = match memory.reserve_write_batch(&writes) {
            Ok(value) => value,
            Err(fault) => return Self::fault(instruction, fault, AccessKind::Write, 10),
        };
        let bits = value.bits();
        if memory
            .commit_write_batch(reservation, &[bits as u64, (bits >> 64) as u64])
            .is_err()
        {
            return Self::fault(instruction, address, AccessKind::Write, 10);
        }
        let mut staged = cpu.clone();
        if empty {
            Self::raise_stack(&mut staged, false);
        }
        staged.x87_classes[index] = ExtendedClass::Empty;
        staged.x87_status = (staged.x87_status & !0x3800) | (((index + 1) & 7) as u16) << 11;
        staged.rip = next;
        *cpu = staged;
        ExecutionExit::Continue
    }

    fn load_float<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &M,
        address: u64,
        format: FloatWidth,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let bytes = match format {
            FloatWidth::Single => 4,
            FloatWidth::Double => 8,
        };
        let bits = match memory.read(address, bytes) {
            Ok(value) => value,
            Err(()) => return Self::fault(instruction, address, AccessKind::Read, bytes),
        };
        let (mut value, mut class) = Conversion::expand(bits, format);
        let mut staged = cpu.clone();
        let index = usize::from(((staged.x87_status >> 11) as u8).wrapping_sub(1) & 7);
        if staged.x87_classes[index] != ExtendedClass::Empty {
            return Self::stack_fault(cpu, staged, index, instruction, next, true);
        }
        let flag = match class {
            ExtendedClass::Denormal => 1 << 1,
            ExtendedClass::SignalingNan | ExtendedClass::Unsupported => 1,
            _ => 0,
        };
        if class == ExtendedClass::SignalingNan {
            value = ExtendedReal::from_bits(value.bits() | (1_u128 << 62));
            class = ExtendedClass::QuietNan;
        } else if class == ExtendedClass::Unsupported {
            value = ExtendedReal::INDEFINITE;
            class = ExtendedClass::QuietNan;
        }
        if flag != 0 && Self::raise(&mut staged, flag) {
            *cpu = staged;
            return ExecutionExit::UndefinedInstruction { instruction };
        }
        staged.x87_status = (staged.x87_status & !0x3800) | (index as u16) << 11;
        staged.x87_values[index] = value;
        staged.x87_classes[index] = class;
        staged.rip = next;
        *cpu = staged;
        ExecutionExit::Continue
    }

    fn store_float<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        address: u64,
        format: FloatWidth,
        pop: bool,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let index = usize::from((cpu.x87_status >> 11) & 7);
        let empty = cpu.x87_classes[index] == ExtendedClass::Empty;
        let bytes = match format {
            FloatWidth::Single => 4,
            FloatWidth::Double => 8,
        };
        let reservation = match memory.reserve_write(address, bytes) {
            Ok(value) => value,
            Err(()) => return Self::fault(instruction, address, AccessKind::Write, bytes),
        };
        let mut staged = cpu.clone();
        let converted = if empty {
            if Self::raise_stack(&mut staged, false) {
                *cpu = staged;
                return ExecutionExit::UndefinedInstruction { instruction };
            }
            Conversion::indefinite(format)
        } else {
            let result = Conversion::narrow(
                cpu.x87_values[index],
                cpu.x87_classes[index],
                format,
                (cpu.x87_control >> 10) & 3,
            );
            if result.flags != 0 && Self::raise(&mut staged, result.flags) {
                *cpu = staged;
                return ExecutionExit::UndefinedInstruction { instruction };
            }
            result.bits
        };
        if memory.commit_write(reservation, converted).is_err() {
            return Self::fault(instruction, address, AccessKind::Write, bytes);
        }
        if pop {
            staged.x87_classes[index] = ExtendedClass::Empty;
            staged.x87_status = (staged.x87_status & !0x3800) | (((index + 1) & 7) as u16) << 11;
        }
        staged.rip = next;
        *cpu = staged;
        ExecutionExit::Continue
    }

    fn stack_fault(
        cpu: &mut CpuState,
        mut staged: CpuState,
        index: usize,
        instruction: u64,
        next: u64,
        overflow: bool,
    ) -> ExecutionExit {
        Self::raise_stack(&mut staged, overflow);
        if staged.x87_control & 1 == 0 {
            *cpu = staged;
            return ExecutionExit::UndefinedInstruction { instruction };
        }
        staged.x87_status = (staged.x87_status & !0x3800) | (index as u16) << 11;
        staged.x87_values[index] = ExtendedReal::INDEFINITE;
        staged.x87_classes[index] = ExtendedClass::QuietNan;
        staged.rip = next;
        *cpu = staged;
        ExecutionExit::Continue
    }

    fn raise_stack(cpu: &mut CpuState, overflow: bool) -> bool {
        cpu.x87_status |= 1 << 6;
        if overflow {
            cpu.x87_status |= 1 << 9;
        } else {
            cpu.x87_status &= !(1 << 9);
        }
        Self::raise(cpu, 1)
    }

    fn raise(cpu: &mut CpuState, flags: u16) -> bool {
        cpu.x87_status |= flags;
        let unmasked = flags & !cpu.x87_control & 0x3f != 0;
        if unmasked {
            cpu.x87_status |= (1 << 7) | (1 << 15);
        }
        unmasked
    }

    fn computed(bits: u64, inputs: &[ExtendedClass]) -> (ExtendedReal, ExtendedClass) {
        let (value, class) = Conversion::expand(bits, FloatWidth::Double);
        let input_nan = inputs
            .iter()
            .any(|class| matches!(class, ExtendedClass::QuietNan | ExtendedClass::SignalingNan));
        if !input_nan && matches!(class, ExtendedClass::QuietNan | ExtendedClass::SignalingNan) {
            (ExtendedReal::INDEFINITE, ExtendedClass::QuietNan)
        } else {
            (value, class)
        }
    }

    fn scale(value: f64, exponent: f64) -> f64 {
        if !value.is_finite() || value == 0.0 {
            return value;
        }
        if exponent.is_nan() {
            return value + exponent;
        }
        let exponent = exponent.clamp(-4096.0, 4096.0);
        let biased = (exponent.trunc() as i64 + 1023).clamp(0, 2047) as u64;
        value * f64::from_bits(biased << 52)
    }

    fn precision(bits: u64, control: u16) -> (u64, u16) {
        if control & 0x0300 != 0 || bits >> 52 & 0x7ff == 0x7ff {
            return (bits, 0);
        }
        let dropped = bits & ((1_u64 << 29) - 1);
        if dropped == 0 {
            return (bits, 0);
        }
        let negative = bits >> 63 != 0;
        let mut kept = bits - dropped;
        let increment = match control >> 10 & 3 {
            0 => dropped > 1_u64 << 28 || dropped == 1_u64 << 28 && kept & (1_u64 << 29) != 0,
            1 => negative,
            2 => !negative,
            _ => false,
        };
        if increment {
            kept += 1_u64 << 29;
        }
        (kept, 1 << 5)
    }

    fn division_inexact(dividend: ExtendedReal, divisor: ExtendedReal) -> bool {
        let mut left = dividend.bits() as u64;
        let mut right = divisor.bits() as u64;
        while right != 0 {
            (left, right) = (right, left % right);
        }
        let reduced_divisor = (divisor.bits() as u64) / left;
        !reduced_divisor.is_power_of_two()
    }

    const fn canonical(address: u64) -> bool {
        (((address << 16) as i64) >> 16) as u64 == address
    }
    const fn fault(instruction: u64, address: u64, access: AccessKind, length: u8) -> ExecutionExit {
        ExecutionExit::OperandFault(crate::FaultAccess::operand(instruction, address, access, length as u64))
    }
}
