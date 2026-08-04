use std::sync::Mutex;

use super::{MqAttributes as Attributes, MqError as Error, MqNotify as Notify};
use crate::{GuestAccess, GuestFault, GuestMemory, MqAbi};

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
fn wire_values() {
    let memory = Memory(Mutex::new(vec![0; 256]));
    memory.write(8, b"queue\0").unwrap();
    let attributes = Attributes {
        flags: 0x800,
        maximum_messages: 10,
        message_bytes: 8192,
        current_messages: 3,
    };
    let abi = MqAbi::new(&memory);
    abi.stage_attributes(32, attributes).unwrap().commit().unwrap();
    assert_eq!(abi.name(8), Ok(b"queue".to_vec()));
    assert_eq!(abi.attributes(32), Ok(attributes));

    memory.write(80, &7_u64.to_ne_bytes()).unwrap();
    memory.write(88, &12_i32.to_ne_bytes()).unwrap();
    memory.write(92, &0_i32.to_ne_bytes()).unwrap();
    let event = abi.event(80).unwrap();
    assert_eq!((event.notify, event.signal, event.value), (Notify::Signal, 12, 7));
}

#[test]
fn timeout_validation() {
    let memory = Memory(Mutex::new(vec![0; 64]));
    memory.write(8, &2_i64.to_ne_bytes()).unwrap();
    memory.write(16, &3_i64.to_ne_bytes()).unwrap();
    assert_eq!(MqAbi::new(&memory).timeout(8).unwrap().unwrap().nanoseconds, 3);
    memory.write(16, &1_000_000_000_i64.to_ne_bytes()).unwrap();
    assert_eq!(MqAbi::new(&memory).timeout(8), Err(Error::Invalid));
    assert_eq!(MqAbi::new(&memory).timeout(60), Err(Error::Fault));
}

#[test]
fn receive_preflight() {
    let memory = Memory(Mutex::new(vec![0xaa; 64]));
    let abi = MqAbi::new(&memory);
    assert!(abi.stage_receive(8, 4, 63).is_err());
    assert_eq!(&memory.0.lock().unwrap()[8..12], &[0xaa; 4]);
    abi.stage_receive(8, 4, 16).unwrap().commit(b"data", 9).unwrap();
    assert_eq!(&memory.0.lock().unwrap()[8..12], b"data");
    assert_eq!(&memory.0.lock().unwrap()[16..20], &9_u32.to_ne_bytes());
}

#[test]
fn bounded_inputs() {
    let memory = Memory(Mutex::new(vec![b'a'; 300]));
    assert_eq!(MqAbi::new(&memory).name(0), Err(Error::Fault));
    assert_eq!(MqAbi::new(&memory).name(1), Err(Error::NameTooLong));
    assert_eq!(MqAbi::<Memory>::priority(32_768), Err(Error::Invalid));
    assert_eq!(MqAbi::new(&memory).message(299, 2, 1), Err(Error::MessageTooBig));
    assert_eq!(MqAbi::new(&memory).message(299, 2, 2), Err(Error::Fault));
}
