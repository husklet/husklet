#[path = "compiler.rs"]
mod compiler;

use compiler::{HOSTED, compile};
use std::path::{Path, PathBuf};

fn repository(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..").join(relative)
}

/// Builds the runtime socket-stop application, whose folder case cannot drive an external stop.
pub fn socket_stop(isa: &str, destination: &Path) {
    compile(
        &repository("tests/runtime/socket-stop/main.c"),
        isa,
        destination,
        HOSTED,
    );
}
