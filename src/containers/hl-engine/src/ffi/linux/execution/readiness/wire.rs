use std::time::Duration;

use hl_linux::Errno;

use super::{EventPort, POLL_INVALID, POLL_LIMIT, PollEntry};

impl EventPort {
    pub(super) fn entries(&self, address: u64, count: u64) -> Result<Vec<PollEntry>, Errno> {
        let count = usize::try_from(count).map_err(|_| Errno::EINVAL)?;
        if count > POLL_LIMIT {
            return Err(Errno::EINVAL);
        }
        if count == 0 {
            return Ok(Vec::new());
        }
        let length = count.checked_mul(8).ok_or(Errno::EINVAL)?;
        let mut bytes = vec![0; length];
        self.memory.read(address, &mut bytes).map_err(|_| Errno::EFAULT)?;
        self.memory
            .probe_write(address, length as u64)
            .map_err(|_| Errno::EFAULT)?;
        Ok(bytes
            .chunks_exact(8)
            .map(|record| {
                let descriptor = i32::from_le_bytes(record[..4].try_into().unwrap());
                let events = i16::from_le_bytes(record[4..6].try_into().unwrap());
                let slot = self.descriptors.slot(descriptor);
                let snapshot = self.descriptors.snapshot(descriptor).ok();
                let valid = descriptor < 0 || snapshot.is_some();
                PollEntry {
                    descriptor: slot.map_or(-1, |slot| slot.native),
                    events,
                    returned: if valid {
                        self.descriptors.readiness(descriptor, events).unwrap_or(0)
                    } else {
                        POLL_INVALID
                    },
                    guest: descriptor,
                    generation: snapshot.map(|value| u64::from(value.descriptor_generation)),
                }
            })
            .collect())
    }

    pub(super) fn timeout(&self, address: u64) -> Result<Option<Duration>, Errno> {
        if address == 0 {
            return Ok(None);
        }
        let mut bytes = [0; 16];
        self.memory.read(address, &mut bytes).map_err(|_| Errno::EFAULT)?;
        let seconds = i64::from_le_bytes(bytes[..8].try_into().unwrap());
        let nanoseconds = i64::from_le_bytes(bytes[8..].try_into().unwrap());
        if seconds < 0 || !(0..1_000_000_000).contains(&nanoseconds) {
            return Err(Errno::EINVAL);
        }
        Ok(Some(Duration::new(seconds as u64, nanoseconds as u32)))
    }

    pub(super) fn copyout(
        &self,
        address: u64,
        entries: &[PollEntry],
        timeout: Option<(u64, [u8; 16])>,
    ) -> Result<(), ()> {
        let mut records = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let offset = (index as u64).checked_mul(8).ok_or(())?;
                let destination = address.checked_add(offset + 6).ok_or(())?;
                Ok((destination, entry.returned.to_le_bytes().to_vec()))
            })
            .collect::<Result<Vec<_>, ()>>()?;
        if let Some((destination, bytes)) = timeout {
            records.push((destination, bytes.to_vec()));
        }
        let writes = records
            .iter()
            .map(|(destination, bytes)| (*destination, bytes.as_slice()))
            .collect::<Vec<_>>();
        self.memory.write_scatter(&writes).map_err(|_| ())
    }

    pub(super) fn timespec(duration: Duration) -> [u8; 16] {
        let mut bytes = [0; 16];
        bytes[..8].copy_from_slice(&(duration.as_secs() as i64).to_le_bytes());
        bytes[8..].copy_from_slice(&i64::from(duration.subsec_nanos()).to_le_bytes());
        bytes
    }
}
