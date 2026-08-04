//! Host-neutral pseudoterminal ownership and line discipline.

#![forbid(unsafe_code)]

mod endpoint;
mod pty;
mod termios;

pub use endpoint::{Bindings, Description, Endpoint, Handle, SignalSink};
pub use pty::{Catalog, CatalogError, ForegroundGroup, Pair, PairId, ReadError, Signal, Window, WriteOutcome};
pub use termios::{Control, Input, Local, Output, Settings, WireError};
