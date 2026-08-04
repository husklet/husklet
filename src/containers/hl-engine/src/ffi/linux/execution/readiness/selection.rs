use std::time::{Duration, Instant};

use hl_linux::{Errno, LinuxResult};

use super::{EventPort, POLL_LIMIT, PollEntry};

impl EventPort {
    pub(super) fn pselect(&mut self, arguments: [u64; 6]) -> LinuxResult {
        let (count, sets) = match self.selection_sets(arguments) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        let timeout = match self.timeout(arguments[4]) {
            Ok(timeout) => timeout,
            Err(error) => return LinuxResult::Error(error),
        };
        let mask = match self.selection_mask(arguments[5]) {
            Ok(mask) => mask,
            Err(error) => return LinuxResult::Error(error),
        };
        self.selection_wait(count, sets, timeout, mask, TimeoutCopy::Nanos(arguments[4]))
    }

    pub(super) fn select(&mut self, arguments: [u64; 6]) -> LinuxResult {
        let (count, sets) = match self.selection_sets(arguments) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        let timeout = match self.timeval(arguments[4]) {
            Ok(timeout) => timeout,
            Err(error) => return LinuxResult::Error(error),
        };
        self.selection_wait(count, sets, timeout, None, TimeoutCopy::Micros(arguments[4]))
    }

    fn selection_sets(&self, arguments: [u64; 6]) -> Result<(usize, Vec<(u64, Vec<u8>)>), Errno> {
        let count = usize::try_from(arguments[0]).map_err(|_| Errno::EINVAL)?;
        if count > POLL_LIMIT {
            return Err(Errno::EINVAL);
        }
        let length = count.div_ceil(64).saturating_mul(8);
        let mut sets = Vec::with_capacity(3);
        for address in arguments[1..4].iter().copied() {
            sets.push((address, self.selection_set(address, length)?));
        }
        Ok((count, sets))
    }

    fn selection_wait(
        &mut self,
        count: usize,
        sets: Vec<(u64, Vec<u8>)>,
        timeout: Option<Duration>,
        mask: Option<u64>,
        copy: TimeoutCopy,
    ) -> LinuxResult {
        let length = count.div_ceil(64).saturating_mul(8);
        let mut entries = Vec::new();
        for descriptor in 0..count {
            let events = Self::selected_events(&sets, descriptor / 8, 1 << (descriptor % 8));
            if events == 0 {
                continue;
            }
            let descriptor = descriptor as i32;
            let snapshot = match self.descriptors.snapshot(descriptor) {
                Ok(snapshot) => snapshot,
                Err(_) => return LinuxResult::Error(Errno::EBADF),
            };
            entries.push(PollEntry {
                descriptor: self.descriptors.slot(descriptor).map_or(-1, |slot| slot.native),
                events,
                returned: self.descriptors.readiness(descriptor, events).unwrap_or(0),
                guest: descriptor,
                generation: Some(u64::from(snapshot.descriptor_generation)),
            });
        }
        let started = Instant::now();
        let masks = std::sync::Arc::clone(&self.masks);
        let _scope = mask.map(|bits| masks.replace(bits));
        self.revalidate(&mut entries);
        self.wake.drain();
        let mut immediate = entries.iter().any(|entry| entry.returned != 0);
        let subscriptions = if immediate {
            Vec::new()
        } else {
            match self.subscriptions(&entries) {
                Ok(subscriptions) => subscriptions,
                Err(error) => return LinuxResult::Error(error),
            }
        };
        self.revalidate(&mut entries);
        immediate |= entries.iter().any(|entry| entry.returned != 0);
        if let Err(error) = self.wait_revalidating(&mut entries, timeout, started, immediate, mask) {
            return LinuxResult::Error(error);
        }
        let _subscriptions = subscriptions;
        let mut output = [vec![0_u8; length], vec![0_u8; length], vec![0_u8; length]];
        for entry in &entries {
            Self::mark_selected(&mut output, entry);
        }
        let mut records = sets
            .iter()
            .zip(&output)
            .filter(|((address, _), _)| *address != 0)
            .map(|((address, _), bytes)| (*address, bytes.clone()))
            .collect::<Vec<_>>();
        if let Some(duration) = timeout {
            let remaining = duration.saturating_sub(started.elapsed());
            if let Some((address, bytes)) = copy.encode(remaining) {
                records.push((address, bytes));
            }
        }
        let writes = records
            .iter()
            .map(|(address, bytes)| (*address, bytes.as_slice()))
            .collect::<Vec<_>>();
        if self.memory.write_scatter(&writes).is_err() {
            return LinuxResult::Error(Errno::EFAULT);
        }
        LinuxResult::Value(Self::selected_count(&output))
    }

    fn selection_set(&self, address: u64, length: usize) -> Result<Vec<u8>, Errno> {
        if address == 0 || length == 0 {
            return Ok(vec![0; length]);
        }
        let mut bytes = vec![0; length];
        self.memory.read(address, &mut bytes).map_err(|_| Errno::EFAULT)?;
        self.memory
            .probe_write(address, length as u64)
            .map_err(|_| Errno::EFAULT)?;
        Ok(bytes)
    }

    fn selected_events(sets: &[(u64, Vec<u8>)], byte: usize, bit: u8) -> i16 {
        let mut events = 0;
        for (index, event) in [1_i16, 4, 2].into_iter().enumerate() {
            if sets[index].1[byte] & bit != 0 {
                events |= event;
            }
        }
        events
    }

    fn mark_selected(output: &mut [Vec<u8>; 3], entry: &PollEntry) {
        let byte = entry.guest as usize / 8;
        let bit = 1_u8 << (entry.guest as usize % 8);
        for (index, event) in [1_i16, 4, 2].into_iter().enumerate() {
            if entry.returned & event != 0 {
                output[index][byte] |= bit;
            }
        }
    }

    fn selected_count(output: &[Vec<u8>; 3]) -> u64 {
        output
            .iter()
            .flat_map(|set| set.iter())
            .map(|byte| u64::from(byte.count_ones()))
            .sum()
    }

    fn selection_mask(&self, address: u64) -> Result<Option<u64>, Errno> {
        if address == 0 {
            return Ok(None);
        }
        let mut bytes = [0_u8; 16];
        self.memory.read(address, &mut bytes).map_err(|_| Errno::EFAULT)?;
        self.mask(
            u64::from_le_bytes(bytes[..8].try_into().unwrap()),
            u64::from_le_bytes(bytes[8..].try_into().unwrap()),
        )
    }

    fn timeval(&self, address: u64) -> Result<Option<Duration>, Errno> {
        if address == 0 {
            return Ok(None);
        }
        let mut bytes = [0; 16];
        self.memory.read(address, &mut bytes).map_err(|_| Errno::EFAULT)?;
        let seconds = i64::from_le_bytes(bytes[..8].try_into().unwrap());
        let microseconds = i64::from_le_bytes(bytes[8..].try_into().unwrap());
        if seconds < 0 || !(0..1_000_000).contains(&microseconds) {
            return Err(Errno::EINVAL);
        }
        Ok(Some(Duration::new(seconds as u64, microseconds as u32 * 1_000)))
    }
}

#[cfg(test)]
mod test {
    use super::EventPort;

    #[test]
    fn selected_count_includes_each_set() {
        let sets = [vec![0b11], vec![0b10], vec![0]];
        assert_eq!(EventPort::selected_count(&sets), 3);
    }
}

#[derive(Clone, Copy)]
enum TimeoutCopy {
    Nanos(u64),
    Micros(u64),
}

impl TimeoutCopy {
    fn encode(self, duration: Duration) -> Option<(u64, Vec<u8>)> {
        let (address, fraction) = match self {
            Self::Nanos(address) => (address, u64::from(duration.subsec_nanos())),
            Self::Micros(address) => (address, u64::from(duration.subsec_micros())),
        };
        if address == 0 {
            return None;
        }
        let mut bytes = Vec::with_capacity(16);
        bytes.extend_from_slice(&(duration.as_secs() as i64).to_le_bytes());
        bytes.extend_from_slice(&(fraction as i64).to_le_bytes());
        Some((address, bytes))
    }
}
