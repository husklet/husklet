// The workspace integration test uses these guest builders.
#![allow(dead_code)]

#[path = "compiler.rs"]
mod compiler;

use compiler::{compile, FREESTANDING, HOSTED};
use std::path::{Path, PathBuf};

fn fixture(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/guest")
        .join(relative)
}

fn repository(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..").join(relative)
}

/// Builds one of this directory's projection guests.
pub fn projection(name: &str, isa: &str, destination: &Path) {
    let flags = if name == "projected_directory.c" {
        HOSTED
    } else {
        FREESTANDING
    };
    let source = fixture(name);
    compile(&source, isa, destination, flags);
}

/// Builds the generic static `ET_EXEC` displacement fixture.
pub fn displaced_et_exec(isa: &str, destination: &Path) {
    compile(&fixture("elf/displaced.c"), isa, destination, FREESTANDING);
}

/// Builds a self-contained `ET_DYN` guest using the requested PIE linker mode.
pub fn pie_exec(isa: &str, destination: &Path, static_pie: bool) {
    let (source, flags) = if static_pie {
        (
            "elf/static_pie.c",
            vec!["-static-pie", "-fPIE", "-O2", "-std=gnu11", "-Wl,--build-id=none"],
        )
    } else {
        (
            "elf/pie.c",
            vec![
                "-nostdlib",
                "-fPIE",
                "-pie",
                "-O2",
                "-ffreestanding",
                "-fno-stack-protector",
                "-fno-ident",
                "-Wl,-e,_start",
                "-Wl,--build-id=none",
                "-Wl,--no-dynamic-linker",
            ],
        )
    };
    compile(&fixture(source), isa, destination, &flags);
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
