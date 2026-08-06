use super::{ControlError, ControlMessage, ControlWord, RIGHTS_MAXIMUM};

const CONTROL_MAXIMUM: usize = 65_536;

pub struct ControlCodec;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControlEncoding {
    pub bytes: Vec<u8>,
    pub truncated: bool,
}

impl ControlCodec {
    pub fn encode(
        controls: &[ControlMessage],
        word: ControlWord,
        capacity: usize,
    ) -> Result<ControlEncoding, ControlError> {
        if capacity > CONTROL_MAXIMUM {
            return Err(ControlError::TooBig);
        }
        let mut bytes = Vec::new();
        let mut truncated = false;
        for (index, control) in controls.iter().enumerate() {
            let (level, kind, data) = Self::parts(control);
            let full = data.len();
            let available = capacity.saturating_sub(bytes.len());
            let Some(data) = Self::fitting_data(control, &data, word, available) else {
                truncated = true;
                break;
            };
            truncated |= data.len() < full;
            let raw_end = bytes
                .len()
                .checked_add(Self::header_size(word))
                .and_then(|length| length.checked_add(data.len()))
                .ok_or(ControlError::TooBig)?;
            Self::append(&mut bytes, word, level, kind, data)?;
            if bytes.len() > capacity {
                bytes.truncate(raw_end);
                truncated |= index + 1 < controls.len();
                break;
            }
            if truncated {
                break;
            }
        }
        Ok(ControlEncoding { bytes, truncated })
    }

    fn fitting_data<'data>(
        control: &ControlMessage,
        data: &'data [u8],
        word: ControlWord,
        available: usize,
    ) -> Option<&'data [u8]> {
        let header = Self::header_size(word);
        if header + data.len() <= available {
            return Some(data);
        }
        if !matches!(control, ControlMessage::Rights(_)) || available < header + 4 {
            return None;
        }
        let mut count = (available - header) / 4;
        while count > 0 && header + count * 4 > available {
            count -= 1;
        }
        (count > 0).then_some(&data[..count * 4])
    }

    pub fn decode(bytes: &[u8], word: ControlWord) -> Result<Vec<ControlMessage>, ControlError> {
        if bytes.len() > CONTROL_MAXIMUM {
            return Err(ControlError::TooBig);
        }
        let header = Self::header_size(word);
        let alignment = Self::alignment(word);
        let mut offset = 0;
        let mut controls = Vec::new();
        while offset < bytes.len() {
            if bytes.len() - offset < header {
                return Err(ControlError::Invalid);
            }
            let length = Self::read_length(bytes, offset, word);
            if length < header || length > bytes.len() - offset {
                return Err(ControlError::Invalid);
            }
            let level_offset = offset + Self::length_size(word);
            let level = i32::from_le_bytes(bytes[level_offset..level_offset + 4].try_into().unwrap());
            let kind = i32::from_le_bytes(bytes[level_offset + 4..level_offset + 8].try_into().unwrap());
            controls.push(Self::control(level, kind, &bytes[offset + header..offset + length])?);
            offset = Self::next_offset(offset, length, bytes.len(), alignment)?;
        }
        Ok(controls)
    }

    fn next_offset(offset: usize, length: usize, total: usize, alignment: usize) -> Result<usize, ControlError> {
        if length == total - offset {
            return Ok(total);
        }
        let next = offset
            .checked_add(Self::align(length, alignment))
            .ok_or(ControlError::Invalid)?;
        (next <= total).then_some(next).ok_or(ControlError::Invalid)
    }

    fn read_length(bytes: &[u8], offset: usize, word: ControlWord) -> usize {
        match word {
            ControlWord::Four => u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize,
            ControlWord::Eight => u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap()) as usize,
        }
    }

    fn align(value: usize, alignment: usize) -> usize {
        value.saturating_add(alignment - 1) & !(alignment - 1)
    }

    fn alignment(word: ControlWord) -> usize {
        match word {
            ControlWord::Four => 4,
            ControlWord::Eight => 8,
        }
    }

    fn length_size(word: ControlWord) -> usize {
        match word {
            ControlWord::Four => 4,
            ControlWord::Eight => 8,
        }
    }

    fn header_size(word: ControlWord) -> usize {
        Self::length_size(word) + 8
    }

    fn parts(control: &ControlMessage) -> (i32, i32, Vec<u8>) {
        match control {
            ControlMessage::Rights(numbers) => (1, 1, numbers.iter().flat_map(|number| number.to_le_bytes()).collect()),
            ControlMessage::Credentials { process, user, group } => (
                1,
                2,
                [*process, *user, *group]
                    .into_iter()
                    .flat_map(u32::to_le_bytes)
                    .collect(),
            ),
            ControlMessage::Unknown { level, kind, data } => (*level, *kind, data.clone()),
        }
    }

    fn append(output: &mut Vec<u8>, word: ControlWord, level: i32, kind: i32, data: &[u8]) -> Result<(), ControlError> {
        let length = Self::header_size(word)
            .checked_add(data.len())
            .ok_or(ControlError::TooBig)?;
        let aligned = Self::align(length, Self::alignment(word));
        if output.len().checked_add(aligned).ok_or(ControlError::TooBig)? > CONTROL_MAXIMUM {
            return Err(ControlError::TooBig);
        }
        match word {
            ControlWord::Four => {
                output.extend_from_slice(&u32::try_from(length).map_err(|_| ControlError::TooBig)?.to_le_bytes());
            }
            ControlWord::Eight => {
                output.extend_from_slice(&(length as u64).to_le_bytes());
            }
        }
        output.extend_from_slice(&level.to_le_bytes());
        output.extend_from_slice(&kind.to_le_bytes());
        output.extend_from_slice(data);
        output.resize(output.len() + aligned - length, 0);
        Ok(())
    }

    fn control(level: i32, kind: i32, data: &[u8]) -> Result<ControlMessage, ControlError> {
        if level == 1 && kind == 2 {
            if data.len() != 12 {
                return Err(ControlError::Invalid);
            }
            return Ok(ControlMessage::Credentials {
                process: u32::from_le_bytes(data[0..4].try_into().unwrap()),
                user: u32::from_le_bytes(data[4..8].try_into().unwrap()),
                group: u32::from_le_bytes(data[8..12].try_into().unwrap()),
            });
        }
        if level != 1 || kind != 1 {
            return Ok(ControlMessage::Unknown {
                level,
                kind,
                data: data.to_vec(),
            });
        }
        if !data.len().is_multiple_of(4) || data.len() / 4 > RIGHTS_MAXIMUM {
            return Err(ControlError::Invalid);
        }
        Ok(ControlMessage::Rights(
            data.chunks_exact(4)
                .map(|chunk| i32::from_le_bytes(chunk.try_into().expect("four-byte right")))
                .collect(),
        ))
    }
}
