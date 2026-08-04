//! Transactional VFS namespace mutation plans and service.

mod create;
mod model;
mod service;
mod transaction;

pub use model::*;
pub use service::VfsMutations;

#[cfg(test)]
mod service_test;
