use std::sync::Mutex;
use std::time::Duration;

use super::{Abi, MarshalError, Opcode};
use crate::{GuestAccess, GuestFault, GuestMemory};

struct Memory(Mutex<Vec<u8>>);
impl GuestMemory for Memory {
    fn probe(&self, address: u64, length: usize, access: GuestAccess) -> Result<usize, GuestFault> {
        if (address as usize)
            .checked_add(length)
            .is_some_and(|end| end <= self.0.lock().unwrap().len())
        {
            Ok(length)
        } else {
            Err(GuestFault { address, access })
        }
    }
    fn read(&self, address: u64, output: &mut [u8]) -> Result<usize, GuestFault> {
        self.probe(address, output.len(), GuestAccess::Read)?;
        output.copy_from_slice(&self.0.lock().unwrap()[address as usize..address as usize + output.len()]);
        Ok(output.len())
    }
    fn write(&self, address: u64, input: &[u8]) -> Result<usize, GuestFault> {
        self.probe(address, input.len(), GuestAccess::Write)?;
        self.0.lock().unwrap()[address as usize..address as usize + input.len()].copy_from_slice(input);
        Ok(input.len())
    }
}

#[test]
fn decodes_control_array() {
    let memory = Memory(Mutex::new(vec![0; 256]));
    let mut control = [0_u8; 64];
    control[0..8].copy_from_slice(&9_u64.to_ne_bytes());
    control[16..18].copy_from_slice(&0_u16.to_ne_bytes());
    control[20..24].copy_from_slice(&4_u32.to_ne_bytes());
    control[24..32].copy_from_slice(&128_u64.to_ne_bytes());
    control[32..40].copy_from_slice(&12_u64.to_ne_bytes());
    memory.write(64, &control).unwrap();
    memory.write(16, &64_u64.to_ne_bytes()).unwrap();
    let decoded = Abi::new(&memory).controls(16, 1).unwrap();
    assert_eq!(decoded[0].opcode, Opcode::Pread);
    assert_eq!((decoded[0].data, decoded[0].descriptor, decoded[0].count), (9, 4, 12));
}

#[test]
fn validates_timeout() {
    let memory = Memory(Mutex::new(vec![0; 64]));
    memory.write(8, &2_i64.to_ne_bytes()).unwrap();
    memory.write(16, &3_i64.to_ne_bytes()).unwrap();
    assert_eq!(Abi::new(&memory).timeout(8), Ok(Some(Duration::new(2, 3))));
    memory.write(16, &1_000_000_000_i64.to_ne_bytes()).unwrap();
    assert_eq!(Abi::new(&memory).timeout(8), Err(MarshalError::Invalid));
}
