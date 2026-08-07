// Each test target includes this module and uses only the builders it needs.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

/// Freestanding `_start` guests issue raw syscalls; hosted guests link the C library statically.
pub const FREESTANDING: &[&str] = &[
    "-nostdlib",
    "-static",
    "-no-pie",
    "-O2",
    "-ffreestanding",
    "-fno-stack-protector",
    "-fno-ident",
    "-Wl,-e,_start",
    "-Wl,--build-id=none",
];
pub const HOSTED: &[&str] = &["-static", "-no-pie", "-O2", "-std=gnu11"];

fn repository(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..").join(relative)
}

/// Cross-compiles a guest source into `destination` for `isa`.
pub fn compile(source: &Path, isa: &str, destination: &Path, flags: &[&str]) {
    let compiler = std::env::var(format!("HL_TEST_GUEST_CC_{}", isa.to_uppercase()))
        .unwrap_or_else(|_| format!("{isa}-linux-gnu-gcc"));
    let output = Command::new(&compiler)
        .arg("-o")
        .arg(destination)
        .arg(source)
        .args(flags)
        .output()
        .unwrap_or_else(|error| panic!("{compiler} must build {} for {isa}: {error}", source.display()));
    assert!(
        output.status.success(),
        "{compiler} failed on {} for {isa}: {}",
        source.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Builds one of this directory's projection guests.
pub fn projection(name: &str, isa: &str, destination: &Path) {
    let flags = if name == "projected_directory.c" {
        HOSTED
    } else {
        FREESTANDING
    };
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/guest_fixture")
        .join(name);
    compile(&source, isa, destination, flags);
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
