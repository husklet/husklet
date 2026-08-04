//! Deterministic guest-page storage and protection adapter.

mod memory;

pub use memory::{GuestPageStore, PAGE_SIZE, Protection, WriteReservation};
