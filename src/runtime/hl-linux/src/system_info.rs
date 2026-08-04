pub const SYSTEM_INFO_SIZE: usize = 112;

/// Host-neutral values encoded into Linux's LP64 `struct sysinfo`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SystemInfo {
    pub uptime_seconds: u64,
    pub loads: [u64; 3],
    pub total_ram: u64,
    pub free_ram: u64,
    pub shared_ram: u64,
    pub buffer_ram: u64,
    pub total_swap: u64,
    pub free_swap: u64,
    pub processes: u16,
    pub total_high: u64,
    pub free_high: u64,
}

impl SystemInfo {
    #[must_use]
    pub fn encode(self) -> [u8; SYSTEM_INFO_SIZE] {
        let mut output = [0; SYSTEM_INFO_SIZE];
        Self::put_u64(&mut output, 0, self.uptime_seconds);
        for (index, load) in self.loads.into_iter().enumerate() {
            Self::put_u64(&mut output, 8 + index * 8, load);
        }
        for (offset, value) in [
            (32, self.total_ram),
            (40, self.free_ram),
            (48, self.shared_ram),
            (56, self.buffer_ram),
            (64, self.total_swap),
            (72, self.free_swap),
            (88, self.total_high),
            (96, self.free_high),
        ] {
            Self::put_u64(&mut output, offset, value);
        }
        output[80..82].copy_from_slice(&self.processes.to_le_bytes());
        output[104..108].copy_from_slice(&1_u32.to_le_bytes());
        output
    }

    fn put_u64(output: &mut [u8; SYSTEM_INFO_SIZE], offset: usize, value: u64) {
        output[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }
}
