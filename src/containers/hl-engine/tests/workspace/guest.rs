#[path = "compiler.rs"]
mod compiler;

use compiler::{HOSTED, compile};
use std::path::Path;

/// Builds the engine lifecycle fixture used to verify isolated force-stop authority.
pub fn socket_stop(isa: &str, destination: &Path) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/socket_stop.c");
    compile(&source, isa, destination, HOSTED);
}
