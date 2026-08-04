use crate::StackError;

const GUEST_WORD_BYTES: usize = 8;
const STACK_ALIGNMENT: u64 = 16;

struct StackWrite {
    address: u64,
    bytes: Vec<u8>,
}

pub(crate) struct StackLayout {
    top: u64,
    cursor: u64,
    max_image_bytes: usize,
    writes: Vec<StackWrite>,
}

impl StackLayout {
    pub(crate) fn new(top: u64, max_image_bytes: usize, write_capacity: usize) -> Self {
        Self {
            top,
            cursor: top,
            max_image_bytes,
            writes: Vec::with_capacity(write_capacity.min(8192)),
        }
    }

    pub(crate) fn place_address_order(&mut self, strings: &[&[u8]]) -> Result<Vec<u64>, StackError> {
        let mut addresses = vec![0; strings.len()];
        for index in (0..strings.len()).rev() {
            addresses[index] = self.place_nul_terminated(strings[index])?;
        }
        Ok(addresses)
    }

    pub(crate) fn place_top_down(&mut self, strings: &[&[u8]]) -> Result<Vec<u64>, StackError> {
        strings.iter().map(|value| self.place_nul_terminated(value)).collect()
    }

    pub(crate) fn place_nul_terminated(&mut self, value: &[u8]) -> Result<u64, StackError> {
        let mut bytes = Vec::with_capacity(value.len() + 1);
        bytes.extend_from_slice(value);
        bytes.push(0);
        self.place_owned(bytes)
    }

    pub(crate) fn place_bytes(&mut self, value: &[u8]) -> Result<u64, StackError> {
        self.place_owned(value.to_vec())
    }

    fn place_owned(&mut self, bytes: Vec<u8>) -> Result<u64, StackError> {
        self.cursor = self
            .cursor
            .checked_sub(bytes.len() as u64)
            .ok_or(StackError::AddressOverflow)?;
        let address = self.cursor;
        self.writes.push(StackWrite { address, bytes });
        Ok(address)
    }

    pub(crate) fn align_cursor(&mut self) -> Result<(), StackError> {
        self.cursor &= !(STACK_ALIGNMENT - 1);
        self.check_current_size()
    }

    pub(crate) fn reserve_table(
        &self,
        argument_count: usize,
        environment_count: usize,
        auxiliary_count: usize,
    ) -> Result<u64, StackError> {
        let argument_slots = argument_count.checked_add(1).ok_or(StackError::StackImageTooLarge)?;
        let environment_slots = environment_count.checked_add(1).ok_or(StackError::StackImageTooLarge)?;
        let auxiliary_slots = auxiliary_count.checked_mul(2).ok_or(StackError::StackImageTooLarge)?;
        let slots = 1_usize
            .checked_add(argument_slots)
            .and_then(|count| count.checked_add(environment_slots))
            .and_then(|count| count.checked_add(auxiliary_slots))
            .ok_or(StackError::StackImageTooLarge)?;
        let bytes = slots
            .checked_mul(GUEST_WORD_BYTES)
            .ok_or(StackError::StackImageTooLarge)?;
        self.cursor
            .checked_sub(bytes as u64)
            .map(|address| address & !(STACK_ALIGNMENT - 1))
            .ok_or(StackError::AddressOverflow)
    }

    pub(crate) fn materialize(self, start: u64) -> Result<Vec<u8>, StackError> {
        let size = self.top.checked_sub(start).ok_or(StackError::AddressOverflow)?;
        let size = usize::try_from(size).map_err(|_| StackError::StackImageTooLarge)?;
        if size > self.max_image_bytes {
            return Err(StackError::StackImageTooLarge);
        }
        let mut output = vec![0; size];
        for write in self.writes {
            let offset = usize::try_from(write.address - start).map_err(|_| StackError::StackImageTooLarge)?;
            output[offset..offset + write.bytes.len()].copy_from_slice(&write.bytes);
        }
        Ok(output)
    }

    fn check_current_size(&self) -> Result<(), StackError> {
        if self.top - self.cursor > self.max_image_bytes as u64 {
            Err(StackError::StackImageTooLarge)
        } else {
            Ok(())
        }
    }
}
