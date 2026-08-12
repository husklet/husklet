use super::{
    Aarch64CpuState, AccessKind, CpuState, ExclusiveReservation, ExecutionCpuSnapshot, ExecutionSnapshot,
    ExecutionStateError, ExtendedClass, ExtendedReal, FlagState, MappingGeneration, MemoryFault, Nzcv,
};

pub(super) struct SnapshotCodec;

impl SnapshotCodec {
    pub(super) fn encode(snapshot: &ExecutionSnapshot) -> Result<Vec<u8>, ExecutionStateError> {
        snapshot.validate()?;
        let mut output = Vec::with_capacity(1024);
        Self::u32(&mut output, snapshot.version);
        output.push(match snapshot.cpu {
            ExecutionCpuSnapshot::Aarch64(_) => 1,
            ExecutionCpuSnapshot::X86_64(_) => 2,
        });
        output.push(u8::from(snapshot.fault.is_some()));
        output.push(snapshot.fault.map_or(0, |fault| Self::access(fault.access)));
        output.push(match &snapshot.cpu {
            ExecutionCpuSnapshot::Aarch64(cpu) => u8::from(cpu.exclusive.is_some()),
            ExecutionCpuSnapshot::X86_64(_) => 0,
        });
        Self::u64(&mut output, snapshot.cache_epoch);
        Self::u64(&mut output, snapshot.fault.map_or(0, |fault| fault.instruction));
        Self::u64(&mut output, snapshot.fault.map_or(0, |fault| fault.address));
        match &snapshot.cpu {
            ExecutionCpuSnapshot::Aarch64(cpu) => Self::aarch64(&mut output, cpu),
            ExecutionCpuSnapshot::X86_64(cpu) => Self::x86(&mut output, cpu),
        }
        Ok(output)
    }

    pub(super) fn decode(bytes: &[u8]) -> Result<ExecutionSnapshot, ExecutionStateError> {
        let mut input = Input { bytes, offset: 0 };
        let version = input.u32()?;
        let architecture = input.u8()?;
        let has_fault = input.boolean()?;
        let access = input.u8()?;
        let has_exclusive = input.boolean()?;
        let cache_epoch = input.u64()?;
        let instruction = input.u64()?;
        let address = input.u64()?;
        let fault = if has_fault {
            Some(MemoryFault {
                instruction,
                address,
                access: Self::decode_access(access)?,
            })
        } else {
            if access != 0 || instruction != 0 || address != 0 {
                return Err(ExecutionStateError::InvalidSnapshot);
            }
            None
        };
        let cpu = match architecture {
            1 => ExecutionCpuSnapshot::Aarch64(Self::decode_aarch64(&mut input, has_exclusive)?),
            2 if !has_exclusive => ExecutionCpuSnapshot::X86_64(Self::decode_x86(&mut input)?),
            _ => return Err(ExecutionStateError::InvalidSnapshot),
        };
        if input.offset != bytes.len() {
            return Err(ExecutionStateError::InvalidSnapshot);
        }
        let snapshot = ExecutionSnapshot {
            version,
            cpu,
            cache_epoch,
            fault,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }

    fn aarch64(output: &mut Vec<u8>, cpu: &Aarch64CpuState) {
        for value in cpu.registers {
            Self::u64(output, value);
        }
        for value in cpu.vectors {
            Self::u128(output, value);
        }
        for value in [cpu.sp, cpu.pc, cpu.tls, cpu.fpcr, cpu.fpsr] {
            Self::u64(output, value);
        }
        Self::u32(output, cpu.nzcv.bits());
        if let Some(reservation) = cpu.exclusive {
            Self::u64(output, reservation.address());
            output.push(reservation.element_bytes());
            output.push(u8::from(reservation.pair()));
            output.extend_from_slice(&[0; 6]);
            Self::u64(output, reservation.generation().value());
        } else {
            output.extend_from_slice(&[0; 24]);
        }
    }

    fn x86(output: &mut Vec<u8>, cpu: &CpuState) {
        for value in cpu.registers {
            Self::u64(output, value);
        }
        for value in cpu.vectors {
            Self::u128(output, value);
        }
        for value in cpu.vector_upper {
            Self::u128(output, value);
        }
        for value in [cpu.rip, cpu.fs_base, cpu.gs_base] {
            Self::u64(output, value);
        }
        output.extend_from_slice(&cpu.flags.bits().to_le_bytes());
        output.extend_from_slice(&cpu.x87_control.to_le_bytes());
        output.extend_from_slice(&cpu.x87_status.to_le_bytes());
        for value in cpu.x87_values {
            Self::u128(output, value.bits());
        }
        for class in cpu.x87_classes {
            output.push(Self::x87_class(class));
        }
        Self::u32(output, cpu.mxcsr);
        output.push(u8::from(cpu.direction));
        output.push(u8::from(cpu.id_flag));
        output.push(u8::from(cpu.alignment_check));
        output.push(0);
    }

    fn decode_aarch64(input: &mut Input<'_>, has_exclusive: bool) -> Result<Aarch64CpuState, ExecutionStateError> {
        let mut cpu = Aarch64CpuState::default();
        for value in &mut cpu.registers {
            *value = input.u64()?;
        }
        for value in &mut cpu.vectors {
            *value = input.u128()?;
        }
        cpu.sp = input.u64()?;
        cpu.pc = input.u64()?;
        cpu.tls = input.u64()?;
        cpu.fpcr = input.u64()?;
        cpu.fpsr = input.u64()?;
        cpu.nzcv = Nzcv::from_bits(input.u32()?);
        let address = input.u64()?;
        let element_bytes = input.u8()?;
        let pair = input.boolean()?;
        input.zeroes(6)?;
        let generation = input.u64()?;
        if has_exclusive {
            if !matches!(element_bytes, 1 | 2 | 4 | 8) || generation == 0 {
                return Err(ExecutionStateError::InvalidSnapshot);
            }
            cpu.exclusive = Some(ExclusiveReservation::new(
                address,
                element_bytes,
                pair,
                MappingGeneration::new(generation),
            ));
        } else if address != 0 || element_bytes != 0 || pair || generation != 0 {
            return Err(ExecutionStateError::InvalidSnapshot);
        }
        Ok(cpu)
    }

    fn decode_x86(input: &mut Input<'_>) -> Result<CpuState, ExecutionStateError> {
        let mut cpu = CpuState::default();
        for value in &mut cpu.registers {
            *value = input.u64()?;
        }
        for value in &mut cpu.vectors {
            *value = input.u128()?;
        }
        for value in &mut cpu.vector_upper {
            *value = input.u128()?;
        }
        cpu.rip = input.u64()?;
        cpu.fs_base = input.u64()?;
        cpu.gs_base = input.u64()?;
        cpu.flags = FlagState::from_bits(input.u16()?);
        cpu.x87_control = input.u16()?;
        cpu.x87_status = input.u16()?;
        for value in &mut cpu.x87_values {
            *value = ExtendedReal::from_bits(input.u128()?);
        }
        for class in &mut cpu.x87_classes {
            *class = Self::decode_x87(input.u8()?)?;
        }
        cpu.mxcsr = input.u32()?;
        cpu.direction = input.boolean()?;
        cpu.id_flag = input.boolean()?;
        cpu.alignment_check = input.boolean()?;
        input.zeroes(1)?;
        Ok(cpu)
    }

    const fn x87_class(class: ExtendedClass) -> u8 {
        match class {
            ExtendedClass::Empty => 0,
            ExtendedClass::Zero => 1,
            ExtendedClass::Denormal => 2,
            ExtendedClass::Normal => 3,
            ExtendedClass::Infinity => 4,
            ExtendedClass::QuietNan => 5,
            ExtendedClass::SignalingNan => 6,
            ExtendedClass::Unsupported => 7,
        }
    }

    fn decode_x87(value: u8) -> Result<ExtendedClass, ExecutionStateError> {
        match value {
            0 => Ok(ExtendedClass::Empty),
            1 => Ok(ExtendedClass::Zero),
            2 => Ok(ExtendedClass::Denormal),
            3 => Ok(ExtendedClass::Normal),
            4 => Ok(ExtendedClass::Infinity),
            5 => Ok(ExtendedClass::QuietNan),
            6 => Ok(ExtendedClass::SignalingNan),
            7 => Ok(ExtendedClass::Unsupported),
            _ => Err(ExecutionStateError::InvalidSnapshot),
        }
    }

    const fn access(access: AccessKind) -> u8 {
        match access {
            AccessKind::Read => 1,
            AccessKind::Write => 2,
            AccessKind::Execute => 3,
        }
    }

    fn decode_access(value: u8) -> Result<AccessKind, ExecutionStateError> {
        match value {
            1 => Ok(AccessKind::Read),
            2 => Ok(AccessKind::Write),
            3 => Ok(AccessKind::Execute),
            _ => Err(ExecutionStateError::InvalidSnapshot),
        }
    }

    fn u32(output: &mut Vec<u8>, value: u32) {
        output.extend_from_slice(&value.to_le_bytes());
    }

    fn u64(output: &mut Vec<u8>, value: u64) {
        output.extend_from_slice(&value.to_le_bytes());
    }

    fn u128(output: &mut Vec<u8>, value: u128) {
        output.extend_from_slice(&value.to_le_bytes());
    }
}

struct Input<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl Input<'_> {
    fn take(&mut self, count: usize) -> Result<&[u8], ExecutionStateError> {
        let end = self
            .offset
            .checked_add(count)
            .ok_or(ExecutionStateError::ResourceLimit)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(ExecutionStateError::InvalidSnapshot)?;
        self.offset = end;
        Ok(value)
    }

    fn u8(&mut self) -> Result<u8, ExecutionStateError> {
        Ok(self.take(1)?[0])
    }

    fn boolean(&mut self) -> Result<bool, ExecutionStateError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(ExecutionStateError::InvalidSnapshot),
        }
    }

    fn u16(&mut self) -> Result<u16, ExecutionStateError> {
        Ok(u16::from_le_bytes(
            self.take(2)?
                .try_into()
                .map_err(|_| ExecutionStateError::InvalidSnapshot)?,
        ))
    }

    fn u32(&mut self) -> Result<u32, ExecutionStateError> {
        Ok(u32::from_le_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| ExecutionStateError::InvalidSnapshot)?,
        ))
    }

    fn u64(&mut self) -> Result<u64, ExecutionStateError> {
        Ok(u64::from_le_bytes(
            self.take(8)?
                .try_into()
                .map_err(|_| ExecutionStateError::InvalidSnapshot)?,
        ))
    }

    fn u128(&mut self) -> Result<u128, ExecutionStateError> {
        Ok(u128::from_le_bytes(
            self.take(16)?
                .try_into()
                .map_err(|_| ExecutionStateError::InvalidSnapshot)?,
        ))
    }

    fn zeroes(&mut self, count: usize) -> Result<(), ExecutionStateError> {
        if self.take(count)?.iter().any(|byte| *byte != 0) {
            return Err(ExecutionStateError::InvalidSnapshot);
        }
        Ok(())
    }
}
