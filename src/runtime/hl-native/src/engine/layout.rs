use std::io::{Read, Seek, SeekFrom};

pub(super) struct ProgramLayout {
    pub(super) load_start: u64,
    pub(super) load_end: u64,
    pub(super) interpreter: Option<Vec<u8>>,
    entry_is_executable: bool,
}

pub(super) fn validate_elf_image(
    file: &mut (impl Read + Seek),
    image_length: u64,
    isa: u32,
) -> Result<(u32, ProgramLayout), i32> {
    file.seek(SeekFrom::Start(0)).map_err(|_| 1)?;
    if image_length < 64 {
        return Err(1);
    }
    let mut header = [0_u8; 64];
    file.read_exact(&mut header).map_err(|_| 1)?;
    if &header[..7] != b"\x7fELF\x02\x01\x01" || !matches!(header[7], 0 | 3) {
        return Err(1);
    }
    let word16 = |offset| u16::from_le_bytes(header[offset..offset + 2].try_into().expect("fixed header"));
    let word32 = |offset| u32::from_le_bytes(header[offset..offset + 4].try_into().expect("fixed header"));
    let word64 = |offset| u64::from_le_bytes(header[offset..offset + 8].try_into().expect("fixed header"));
    let kind = match word16(16) {
        2 => 1,
        3 => 2,
        _ => return Err(1),
    };
    let machine = match isa {
        1 => 0xb7,
        2 => 0x3e,
        _ => return Err(1),
    };
    if word16(18) != machine || word32(20) != 1 || word16(52) != 64 {
        return Err(1);
    }
    let entry = word64(24);
    if isa == 1 && !entry.is_multiple_of(4) {
        return Err(1);
    }
    let layout = ProgramLayout::inspect(file, image_length, entry, word64(32), u64::from(word16(54)), word16(56))?;
    if !layout.entry_is_executable {
        return Err(1);
    }
    Ok((kind, layout))
}

impl ProgramLayout {
    fn inspect(
        file: &mut (impl Read + Seek),
        image_length: u64,
        entry: u64,
        phoff: u64,
        phentsize: u64,
        phnum: u16,
    ) -> Result<Self, i32> {
        const PROGRAM_HEADER_SIZE: u64 = 56;
        const MAX_PROGRAM_HEADERS: u16 = 1024;
        const MAX_LOAD_SEGMENTS: u16 = 128;
        if phentsize != PROGRAM_HEADER_SIZE || phnum == 0 || phnum > MAX_PROGRAM_HEADERS {
            return Err(1);
        }
        let table_size = phentsize.checked_mul(u64::from(phnum)).ok_or(1)?;
        if phoff.checked_add(table_size).is_none_or(|end| end > image_length) {
            return Err(1);
        }
        let mut first = u64::MAX;
        let mut last = 0_u64;
        let mut interpreter = None;
        let mut loads = 0_u16;
        let mut entry_is_executable = false;
        for index in 0..phnum {
            let offset = phoff
                .checked_add(u64::from(index).checked_mul(phentsize).ok_or(1)?)
                .ok_or(1)?;
            file.seek(SeekFrom::Start(offset)).map_err(|_| 1)?;
            let mut program = [0_u8; 56];
            file.read_exact(&mut program).map_err(|_| 1)?;
            let u32_at = |offset| u32::from_le_bytes(program[offset..offset + 4].try_into().expect("program header"));
            let u64_at = |offset| u64::from_le_bytes(program[offset..offset + 8].try_into().expect("program header"));
            match u32_at(0) {
                1 => {
                    loads = loads.checked_add(1).ok_or(1)?;
                    if loads > MAX_LOAD_SEGMENTS {
                        return Err(1);
                    }
                    let file_offset = u64_at(8);
                    let start = u64_at(16);
                    let file_size = u64_at(32);
                    let memory_size = u64_at(40);
                    let alignment = u64_at(48);
                    if file_size > memory_size
                        || (file_size != 0 && file_offset.checked_add(file_size).is_none_or(|end| end > image_length))
                        || (alignment > 1
                            && (!alignment.is_power_of_two() || start % alignment != file_offset % alignment))
                    {
                        return Err(1);
                    }
                    let end = start.checked_add(memory_size).ok_or(1)?;
                    first = first.min(start);
                    last = last.max(end);
                    entry_is_executable |= u32_at(4) & 1 != 0 && entry >= start && entry < end;
                }
                3 => {
                    if interpreter.is_some() {
                        return Err(1);
                    }
                    interpreter = Some(read_interpreter(file, u64_at(8), u64_at(32))?);
                }
                _ => {}
            }
        }
        if first == u64::MAX {
            return Err(1);
        }
        Ok(Self {
            load_start: first,
            load_end: last,
            interpreter,
            entry_is_executable,
        })
    }
}

fn read_interpreter(file: &mut (impl Read + Seek), offset: u64, encoded_size: u64) -> Result<Vec<u8>, i32> {
    let size = usize::try_from(encoded_size).map_err(|_| 1)?;
    if size == 0 || size > 4096 {
        return Err(1);
    }
    let mut path = vec![0; size];
    file.seek(SeekFrom::Start(offset)).map_err(|_| 1)?;
    file.read_exact(&mut path).map_err(|_| 1)?;
    if path.last() != Some(&0) || path[..path.len() - 1].contains(&0) {
        return Err(1);
    }
    path.pop();
    Ok(path)
}
