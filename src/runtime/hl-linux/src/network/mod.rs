//! Linux socket address, message, and option ABI marshalling.

mod abi;
pub use abi::*;

#[cfg(test)]
mod test;
