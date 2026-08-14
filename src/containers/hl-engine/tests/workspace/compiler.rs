use std::path::Path;
use std::process::Command;

/// Hosted guests link the C library statically.
pub const HOSTED: &[&str] = &["-static", "-no-pie", "-O2", "-std=gnu11"];

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
