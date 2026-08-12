//! Deterministic guest-page storage and protection adapter.

mod memory;

pub use memory::{
    FetchError, GuestOperandMemory, GuestPageStore, InstructionFetch, PAGE_SIZE, Protection, WriteReservation,
};
