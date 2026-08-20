//! Runs the reference extension against the socket the host provided.

use std::os::unix::net::UnixStream;

use extension::Extension;

/// Where the host mounts the socket inside an extension's container.
const SOCKET: &str = "HUSKLET_EXTENSION_SOCKET";

fn main() -> std::process::ExitCode {
    let Ok(path) = std::env::var(SOCKET) else {
        eprintln!("[extension] {SOCKET} is not set; this runs inside a workspace");
        return std::process::ExitCode::FAILURE;
    };
    let stream = match UnixStream::connect(&path) {
        Ok(stream) => stream,
        Err(error) => {
            eprintln!("[extension] cannot reach {path}: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    match extension::serve(stream, Extension::new()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("[extension] {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
