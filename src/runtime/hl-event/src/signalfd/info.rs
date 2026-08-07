use super::SIGNALFD_RECORD_SIZE;

/// The `signalfd_siginfo` record a reader observes for one queued signal.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SignalInfo {
    pub signal: u32,
    pub error: i32,
    pub code: i32,
    pub process_id: u32,
    pub user_id: u32,
    pub file_descriptor: i32,
    pub thread_id: u32,
    pub band: u32,
    pub overrun: u32,
    pub trap_number: u32,
    pub status: i32,
    pub integer: i32,
    pub pointer: u64,
    pub user_time: u64,
    pub system_time: u64,
    pub address: u64,
    pub address_lsb: u16,
    pub syscall: i32,
    pub call_address: u64,
    pub architecture: u32,
}

impl SignalInfo {
    #[must_use]
    pub fn encode(self) -> [u8; SIGNALFD_RECORD_SIZE] {
        let mut record = [0_u8; SIGNALFD_RECORD_SIZE];
        Self::put_u32(&mut record, 0, self.signal);
        Self::put_i32(&mut record, 4, self.error);
        Self::put_i32(&mut record, 8, self.code);
        Self::put_u32(&mut record, 12, self.process_id);
        Self::put_u32(&mut record, 16, self.user_id);
        Self::put_i32(&mut record, 20, self.file_descriptor);
        Self::put_u32(&mut record, 24, self.thread_id);
        Self::put_u32(&mut record, 28, self.band);
        Self::put_u32(&mut record, 32, self.overrun);
        Self::put_u32(&mut record, 36, self.trap_number);
        Self::put_i32(&mut record, 40, self.status);
        Self::put_i32(&mut record, 44, self.integer);
        Self::put_u64(&mut record, 48, self.pointer);
        Self::put_u64(&mut record, 56, self.user_time);
        Self::put_u64(&mut record, 64, self.system_time);
        Self::put_u64(&mut record, 72, self.address);
        record[80..82].copy_from_slice(&self.address_lsb.to_le_bytes());
        Self::put_i32(&mut record, 84, self.syscall);
        Self::put_u64(&mut record, 88, self.call_address);
        Self::put_u32(&mut record, 96, self.architecture);
        record
    }
    fn put_i32(output: &mut [u8], offset: usize, value: i32) {
        output[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u32(output: &mut [u8], offset: usize, value: u32) {
        output[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u64(output: &mut [u8], offset: usize, value: u64) {
        output[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }
}
