#[path = "compiler.rs"]
mod compiler;

use compiler::{HOSTED, compile};
use std::path::{Path, PathBuf};

fn fixture(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/guest")
        .join(relative)
}

/// Builds the engine lifecycle fixture used to verify isolated force-stop authority.
pub fn socket_stop(isa: &str, destination: &Path) {
    compile(&fixture("socket_stop.c"), isa, destination, HOSTED);
}
