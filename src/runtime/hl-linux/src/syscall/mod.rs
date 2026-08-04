//! Syscall frame, routing ports, and canonical number tables.

mod frame;
mod ports;
mod table;

pub use frame::*;
pub use ports::*;
pub use table::*;

#[cfg(test)]
mod frame_test;
#[cfg(test)]
mod table_test;
