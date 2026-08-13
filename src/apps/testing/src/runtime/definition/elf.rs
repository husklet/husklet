use crate::suite::{Error, Target};
use serde::Deserialize;
use std::{fs, io::Read as _, io::Seek as _, io::SeekFrom, path::Path};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Expectation {
    #[serde(rename = "type")]
    kind: Type,
    pub(crate) interpreter: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
enum Type {
    Exec,
    Dyn,
}

pub(crate) fn verify(path: &Path, target: Target, expected: Expectation) -> Result<(), Error> {
    const ELF_HEADER_SIZE: usize = 64;
    const PROGRAM_HEADER_SIZE: usize = 56;
    const PT_INTERP: u32 = 3;
    let mut file = fs::File::open(path)?;
    let mut header = [0_u8; ELF_HEADER_SIZE];
    file.read_exact(&mut header)
        .map_err(|error| format!("read ELF header {}: {error}", path.display()))?;
    if &header[..4] != b"\x7fELF" || header[4] != 2 || header[5] != 1 || header[6] != 1 {
        return Err(format!("{} is not a little-endian ELF64 artifact", path.display()).into());
    }
    let actual_type = u16::from_le_bytes([header[16], header[17]]);
    let expected_type = match expected.kind {
        Type::Exec => 2,
        Type::Dyn => 3,
    };
    if actual_type != expected_type {
        return Err(format!(
            "{} ELF type is {actual_type}, expected {expected_type} ({:?})",
            path.display(),
            expected.kind
        )
        .into());
    }
    let machine = u16::from_le_bytes([header[18], header[19]]);
    let expected_machine = match target {
        Target::Arm64 => 183,
        Target::Amd64 => 62,
    };
    if machine != expected_machine {
        return Err(format!(
            "{} ELF machine is {machine}, expected {expected_machine} for {}",
            path.display(),
            target.name()
        )
        .into());
    }
    let program_offset = u64::from_le_bytes(header[32..40].try_into().expect("fixed ELF field"));
    let entry_size = u16::from_le_bytes([header[54], header[55]]) as usize;
    let entry_count = u16::from_le_bytes([header[56], header[57]]) as usize;
    if entry_count != 0 && entry_size < PROGRAM_HEADER_SIZE {
        return Err(format!("{} has an undersized ELF program header", path.display()).into());
    }
    let mut has_interpreter = false;
    let mut program = [0_u8; PROGRAM_HEADER_SIZE];
    for index in 0..entry_count {
        let relative = index
            .checked_mul(entry_size)
            .ok_or_else(|| format!("{} ELF program table overflows", path.display()))?;
        let offset = program_offset
            .checked_add(relative as u64)
            .ok_or_else(|| format!("{} ELF program table overflows", path.display()))?;
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(&mut program)
            .map_err(|error| format!("read ELF program header {}: {error}", path.display()))?;
        has_interpreter |= u32::from_le_bytes(program[..4].try_into().expect("fixed ELF field")) == PT_INTERP;
    }
    if has_interpreter != expected.interpreter {
        return Err(format!(
            "{} ELF PT_INTERP presence is {has_interpreter}, expected {}",
            path.display(),
            expected.interpreter
        )
        .into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Expectation, Type, verify};
    use std::fs;

    fn fixture(kind: u16, machine: u16, interpreter: bool) -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut bytes = vec![0_u8; 64 + 56];
        bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        bytes[16..18].copy_from_slice(&kind.to_le_bytes());
        bytes[18..20].copy_from_slice(&machine.to_le_bytes());
        bytes[32..40].copy_from_slice(&64_u64.to_le_bytes());
        bytes[52..54].copy_from_slice(&64_u16.to_le_bytes());
        bytes[54..56].copy_from_slice(&56_u16.to_le_bytes());
        bytes[56..58].copy_from_slice(&1_u16.to_le_bytes());
        bytes[64..68].copy_from_slice(&(if interpreter { 3_u32 } else { 1_u32 }).to_le_bytes());
        fs::write(file.path(), bytes).unwrap();
        file
    }

    #[test]
    fn checks_type_machine_and_interpreter_independently() {
        let dynamic = Expectation {
            kind: Type::Dyn,
            interpreter: true,
        };
        let file = fixture(3, 62, true);
        verify(file.path(), crate::suite::Target::Amd64, dynamic).unwrap();
        assert!(
            verify(file.path(), crate::suite::Target::Arm64, dynamic)
                .unwrap_err()
                .to_string()
                .contains("ELF machine")
        );
        assert!(
            verify(
                file.path(),
                crate::suite::Target::Amd64,
                Expectation {
                    kind: Type::Exec,
                    interpreter: true,
                },
            )
            .unwrap_err()
            .to_string()
            .contains("ELF type")
        );
        assert!(
            verify(
                file.path(),
                crate::suite::Target::Amd64,
                Expectation {
                    kind: Type::Dyn,
                    interpreter: false,
                },
            )
            .unwrap_err()
            .to_string()
            .contains("PT_INTERP")
        );
    }
}
