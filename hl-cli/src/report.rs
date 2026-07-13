//! Small shared reporting helpers: status lines, success/failure notes, and exit-code plumbing.

use std::process::Command;

pub(crate) fn line(ok: bool, msg: &str) {
    println!("[{}] {msg}", if ok { "✓" } else { "✗" });
}

/// Report success/failure of an action whose Ok payload we don't need to show.
pub(crate) fn report<T, E: std::fmt::Display>(what: &str, r: Result<T, E>) -> i32 {
    match r {
        Ok(_) => {
            println!("✓ {what}");
            0
        }
        Err(e) => {
            eprintln!("✗ {what}: {e}");
            1
        }
    }
}

/// Like [`report`] but prints the Ok payload (a human note) too.
pub(crate) fn note<E: std::fmt::Display>(what: &str, r: Result<String, E>) -> i32 {
    match r {
        Ok(n) => {
            println!("✓ {what}: {n}");
            0
        }
        Err(e) => {
            eprintln!("✗ {what}: {e}");
            1
        }
    }
}

pub(crate) fn run_status(cmd: &mut Command) -> i32 {
    match cmd.status() {
        Ok(s) => s.code().unwrap_or(0),
        Err(e) => {
            eprintln!("failed to run: {e}");
            1
        }
    }
}
