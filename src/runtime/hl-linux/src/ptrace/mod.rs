//! Linux ptrace request decoding without task or execution ownership.

mod request;

pub use request::{NT_PRSTATUS, Options, Plan, Request, Resume};

#[cfg(test)]
mod test;
