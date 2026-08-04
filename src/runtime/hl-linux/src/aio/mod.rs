mod abi;

pub use abi::{Abi, ControlBlock, Event, IOCB_FLAG_RESFD, MarshalError, Opcode, StagedEvents};

#[cfg(test)]
mod test;
