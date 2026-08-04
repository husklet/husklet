#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrctlPlan {
    SetParentDeathSignal(u32),
    GetParentDeathSignal { destination: u64 },
    SetDumpable(bool),
    GetDumpable,
    SetName([u8; 16]),
    GetName { destination: u64 },
    SetNoNewPrivileges,
    GetNoNewPrivileges,
    SetKeepCapabilities(bool),
    GetKeepCapabilities,
    ReadCapability(u32),
    DropCapability(u32),
    GetSecureBits,
    SetSecureBits(u32),
    SetTimerSlack(u64),
    GetTimerSlack,
    SetSubreaper(bool),
    GetSubreaper { destination: u64 },
    SetThp(bool),
    GetThp,
    AmbientRead(u32),
    AmbientRaise(u32),
    AmbientLower(u32),
    AmbientClear,
    GetSpeculation(u32),
    SetTiming,
    GetSeccomp,
    SetSeccompStrict,
    SetSeccompFilter { address: u64 },
    SetMcePolicy(u32),
    GetMcePolicy,
    TogglePerfEvents,
    SetMemoryLayout,
}

impl<M: crate::GuestMemory + ?Sized> crate::ProcessAbi<'_, M> {
    pub fn prctl(&self, arguments: [u64; 6]) -> Result<PrctlPlan, crate::ProcessMarshalError> {
        let option = arguments[0] as u32;
        let argument = arguments[1];
        let unused = |start: usize| arguments[start..5].iter().all(|value| *value == 0);
        match option {
            1 if argument <= 64 => Ok(PrctlPlan::SetParentDeathSignal(argument as u32)),
            2 => Ok(PrctlPlan::GetParentDeathSignal { destination: argument }),
            3 => Ok(PrctlPlan::GetDumpable),
            4 if argument <= 1 && unused(2) => Ok(PrctlPlan::SetDumpable(argument != 0)),
            15 => self.set_name(argument),
            16 => Ok(PrctlPlan::GetName { destination: argument }),
            7 if argument == 0 && unused(2) => Ok(PrctlPlan::GetKeepCapabilities),
            8 if argument <= 1 && unused(2) => Ok(PrctlPlan::SetKeepCapabilities(argument != 0)),
            14 if argument == 0 && unused(2) => Ok(PrctlPlan::SetTiming),
            21 if unused(1) => Ok(PrctlPlan::GetSeccomp),
            22 if argument == 1 && unused(2) => Ok(PrctlPlan::SetSeccompStrict),
            22 if argument == 2 && unused(3) => Ok(PrctlPlan::SetSeccompFilter { address: arguments[2] }),
            23 if argument < 41 && unused(2) => Ok(PrctlPlan::ReadCapability(argument as u32)),
            24 if argument < 41 && unused(2) => Ok(PrctlPlan::DropCapability(argument as u32)),
            27 if argument == 0 && unused(2) => Ok(PrctlPlan::GetSecureBits),
            28 if argument <= 0xff && unused(2) => Ok(PrctlPlan::SetSecureBits(argument as u32)),
            29 if unused(2) => Ok(PrctlPlan::SetTimerSlack(argument)),
            30 if unused(1) => Ok(PrctlPlan::GetTimerSlack),
            31 | 32 => Ok(PrctlPlan::TogglePerfEvents),
            33 if argument == 0 && unused(2) => Ok(PrctlPlan::SetMcePolicy(2)),
            33 if argument == 1 && arguments[2] <= 2 && unused(3) => {
                Ok(PrctlPlan::SetMcePolicy(arguments[2] as u32))
            }
            34 if unused(1) => Ok(PrctlPlan::GetMcePolicy),
            35 => Ok(PrctlPlan::SetMemoryLayout),
            36 if argument <= 1 => Ok(PrctlPlan::SetSubreaper(argument != 0)),
            37 => Ok(PrctlPlan::GetSubreaper { destination: argument }),
            38 if argument == 1 && unused(2) => Ok(PrctlPlan::SetNoNewPrivileges),
            39 if argument == 0 && unused(2) => Ok(PrctlPlan::GetNoNewPrivileges),
            41 if argument <= 1 && unused(2) => Ok(PrctlPlan::SetThp(argument != 0)),
            42 if argument == 0 && unused(2) => Ok(PrctlPlan::GetThp),
            47 if unused(5) => Self::ambient(arguments),
            52 if unused(2) => Ok(PrctlPlan::GetSpeculation(argument as u32)),
            _ => Err(crate::ProcessMarshalError::Invalid),
        }
    }

    fn ambient(arguments: [u64; 6]) -> Result<PrctlPlan, crate::ProcessMarshalError> {
        let command = arguments[1];
        let capability = arguments[2];
        if arguments[3] != 0 || arguments[4] != 0 {
            return Err(crate::ProcessMarshalError::Invalid);
        }
        match command {
            1 if capability < 41 => Ok(PrctlPlan::AmbientRead(capability as u32)),
            2 if capability < 41 => Ok(PrctlPlan::AmbientRaise(capability as u32)),
            3 if capability < 41 => Ok(PrctlPlan::AmbientLower(capability as u32)),
            4 if capability == 0 => Ok(PrctlPlan::AmbientClear),
            _ => Err(crate::ProcessMarshalError::Invalid),
        }
    }

    fn set_name(&self, address: u64) -> Result<PrctlPlan, crate::ProcessMarshalError> {
        let mut name = [0; 16];
        for index in 0..15 {
            let target = &mut name[index..index + 1];
            if self
                .marshaller
                .copy_from(address + index as u64, target)
                .fault
                .is_some()
            {
                return Err(crate::ProcessMarshalError::Fault);
            }
            if target[0] == 0 {
                break;
            }
        }
        Ok(PrctlPlan::SetName(name))
    }
}
