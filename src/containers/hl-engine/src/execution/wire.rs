use crate::{activation::GuestIsa, engine::EngineError, launch_plan::RuntimeLaunchPlan, options::Options};

const MAGIC: [u8; 8] = *b"HLCPLAN\0";
const VERSION: u32 = 1;
const MAXIMUM_WIRE: usize = 64 * 1024 * 1024;
const MAXIMUM_RECORDS: usize = 4096;
const MAXIMUM_FIELD: usize = 16 * 1024 * 1024;

pub(super) fn encode(isa: GuestIsa, plan: &RuntimeLaunchPlan) -> Result<Vec<u8>, EngineError> {
    let mut output = Vec::new();
    output.extend(MAGIC);
    word(&mut output, VERSION)?;
    word(&mut output, isa as u32)?;
    optional(&mut output, plan.rootfs.as_deref())?;
    optional(&mut output, plan.executable_host.as_deref())?;
    optional(&mut output, plan.result_path.as_deref())?;
    records(&mut output, &plan.arguments)?;
    records(&mut output, &plan.environment)?;
    let options = plan.options.iter().collect::<Vec<_>>();
    count(&mut output, options.len())?;
    for (name, value) in options {
        blob(&mut output, name.as_bytes())?;
        blob(&mut output, value)?;
    }
    (output.len() <= MAXIMUM_WIRE)
        .then_some(output)
        .ok_or(EngineError::LaunchFailed)
}

pub(super) fn decode(input: &[u8]) -> Result<(GuestIsa, RuntimeLaunchPlan), EngineError> {
    if input.len() > MAXIMUM_WIRE {
        return Err(EngineError::LaunchFailed);
    }
    let mut input = Input {
        bytes: input,
        offset: 0,
    };
    if input.take(MAGIC.len())? != MAGIC || input.word()? != VERSION {
        return Err(EngineError::LaunchFailed);
    }
    let isa = GuestIsa::from_abi(input.word()?).ok_or(EngineError::LaunchFailed)?;
    let rootfs = input.optional()?;
    let executable_host = input.optional()?;
    let result_path = input.optional()?;
    let arguments = input.records()?;
    let environment = input.records()?;
    let option_count = input.count()?;
    let mut options = Options::default();
    for _ in 0..option_count {
        let name = input.blob()?;
        let value = input.blob()?;
        let name = std::str::from_utf8(&name).map_err(|_| EngineError::LaunchFailed)?;
        options
            .set_bytes(name, &value, true)
            .map_err(|_| EngineError::LaunchFailed)?;
    }
    if input.offset != input.bytes.len() {
        return Err(EngineError::LaunchFailed);
    }
    Ok((
        isa,
        RuntimeLaunchPlan {
            rootfs,
            executable_host,
            arguments,
            environment,
            result_path,
            options,
        },
    ))
}

fn records(output: &mut Vec<u8>, values: &[Vec<u8>]) -> Result<(), EngineError> {
    count(output, values.len())?;
    for value in values {
        blob(output, value)?;
    }
    Ok(())
}

fn optional(output: &mut Vec<u8>, value: Option<&[u8]>) -> Result<(), EngineError> {
    match value {
        Some(value) => {
            word(output, 1)?;
            blob(output, value)
        }
        None => word(output, 0),
    }
}

fn count(output: &mut Vec<u8>, value: usize) -> Result<(), EngineError> {
    if value > MAXIMUM_RECORDS {
        return Err(EngineError::LaunchFailed);
    }
    word(output, u32::try_from(value).map_err(|_| EngineError::LaunchFailed)?)
}

fn blob(output: &mut Vec<u8>, value: &[u8]) -> Result<(), EngineError> {
    if value.len() > MAXIMUM_FIELD || output.len().saturating_add(value.len()).saturating_add(4) > MAXIMUM_WIRE {
        return Err(EngineError::LaunchFailed);
    }
    word(
        output,
        u32::try_from(value.len()).map_err(|_| EngineError::LaunchFailed)?,
    )?;
    output.extend(value);
    Ok(())
}

fn word(output: &mut Vec<u8>, value: u32) -> Result<(), EngineError> {
    if output.len().saturating_add(4) > MAXIMUM_WIRE {
        return Err(EngineError::LaunchFailed);
    }
    output.extend(value.to_le_bytes());
    Ok(())
}

struct Input<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl Input<'_> {
    fn take(&mut self, length: usize) -> Result<&[u8], EngineError> {
        let end = self.offset.checked_add(length).ok_or(EngineError::LaunchFailed)?;
        let value = self.bytes.get(self.offset..end).ok_or(EngineError::LaunchFailed)?;
        self.offset = end;
        Ok(value)
    }

    fn word(&mut self) -> Result<u32, EngineError> {
        self.take(4)?
            .try_into()
            .map(u32::from_le_bytes)
            .map_err(|_| EngineError::LaunchFailed)
    }

    fn count(&mut self) -> Result<usize, EngineError> {
        let count = self.word()? as usize;
        (count <= MAXIMUM_RECORDS)
            .then_some(count)
            .ok_or(EngineError::LaunchFailed)
    }

    fn blob(&mut self) -> Result<Vec<u8>, EngineError> {
        let length = self.word()? as usize;
        if length > MAXIMUM_FIELD {
            return Err(EngineError::LaunchFailed);
        }
        self.take(length).map(<[u8]>::to_vec)
    }

    fn optional(&mut self) -> Result<Option<Vec<u8>>, EngineError> {
        match self.word()? {
            0 => Ok(None),
            1 => self.blob().map(Some),
            _ => Err(EngineError::LaunchFailed),
        }
    }

    fn records(&mut self) -> Result<Vec<Vec<u8>>, EngineError> {
        let count = self.count()?;
        (0..count).map(|_| self.blob()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> RuntimeLaunchPlan {
        let mut options = Options::default();
        options.set("HL_CWD", "/work", true).unwrap();
        RuntimeLaunchPlan {
            rootfs: Some(b"/root".to_vec()),
            executable_host: None,
            arguments: vec![b"/bin/sh".to_vec(), b"-c".to_vec(), b"echo ok".to_vec()],
            environment: vec![b"PATH=/bin".to_vec(), b"EMPTY=".to_vec()],
            result_path: None,
            options,
        }
    }

    #[test]
    fn round_trip_preserves_the_exact_plan() {
        let original = plan();
        let encoded = encode(GuestIsa::Aarch64, &original).unwrap();
        let (isa, decoded) = decode(&encoded).unwrap();
        assert_eq!(isa, GuestIsa::Aarch64);
        assert_eq!(decoded.rootfs, original.rootfs);
        assert_eq!(decoded.executable_host, original.executable_host);
        assert_eq!(decoded.arguments, original.arguments);
        assert_eq!(decoded.environment, original.environment);
        assert_eq!(decoded.result_path, original.result_path);
        assert_eq!(
            decoded.options.iter().collect::<Vec<_>>(),
            original.options.iter().collect::<Vec<_>>()
        );
    }

    #[test]
    fn malformed_and_trailing_frames_are_rejected() {
        let encoded = encode(GuestIsa::Aarch64, &plan()).unwrap();
        for length in 0..encoded.len() {
            assert!(decode(&encoded[..length]).is_err(), "accepted truncation at {length}");
        }
        let mut trailing = encoded;
        trailing.push(0);
        assert!(decode(&trailing).is_err());
    }

    #[test]
    fn record_counts_and_fields_are_bounded_before_allocation() {
        let mut encoded = Vec::new();
        encoded.extend(MAGIC);
        encoded.extend(VERSION.to_le_bytes());
        encoded.extend((GuestIsa::Aarch64 as u32).to_le_bytes());
        encoded.extend(0_u32.to_le_bytes());
        encoded.extend(0_u32.to_le_bytes());
        encoded.extend(0_u32.to_le_bytes());
        encoded.extend(u32::MAX.to_le_bytes());
        assert!(decode(&encoded).is_err());
    }
}
