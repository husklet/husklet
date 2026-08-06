//! Host-neutral pseudoterminal ownership and line discipline.

#![forbid(unsafe_code)]

mod endpoint;
mod pty;
mod pty_catalog;
mod termios;

pub use endpoint::{Bindings, Description, Endpoint, Handle, SignalSink};
pub use pty::{ForegroundGroup, Pair, PairId, ReadError, Signal, Window, WriteOutcome};
pub use pty_catalog::{Catalog, CatalogError};
pub use termios::{Control, Input, Local, Output, Settings, WireError};
