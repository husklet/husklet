//! Runs the reference extension against the socket the host provided.

/// Where the host mounts the socket inside an extension's container.
const SOCKET: &str = "HUSKLET_EXTENSION_SOCKET";

/// Connects to the host's socket and serves the extension over it.
///
/// The rendezvous is an `AF_UNIX` socket at a filesystem path, because this program runs
/// *inside* the workspace container -- a Linux guest, whatever the host the workspace is
/// opened from -- and that is the path the host mounts in. `std` binds Unix-domain sockets
/// only on Unix.
#[cfg(unix)]
fn connect(path: &str) -> std::io::Result<std::os::unix::net::UnixStream> {
    std::os::unix::net::UnixStream::connect(path)
}

fn main() -> std::process::ExitCode {
    let Ok(path) = std::env::var(SOCKET) else {
        eprintln!("[extension] {SOCKET} is not set; this runs inside a workspace");
        return std::process::ExitCode::FAILURE;
    };
    // A host that is not Unix can still run this binary's tests and build it; it cannot run it,
    // because there is no host on which a Windows build of this program would be the right
    // program. An extension is delivered as a Linux container image and speaks to the host across
    // the socket mounted into it, so the target that matters is always the guest's. Saying so is
    // the whole non-Unix arm: refusing at the connect is the last point where the reason is still
    // legible, and it is better than a build that omits the binary and explains nothing.
    #[cfg(not(unix))]
    {
        let _ = &path;
        eprintln!(
            "[extension] this is a guest-side program: it reaches its host over the AF_UNIX socket \
             at {SOCKET}, which is mounted into the Linux container it runs in. Build it for the \
             workspace's Linux target, not for this host."
        );
        return std::process::ExitCode::FAILURE;
    }
    #[cfg(unix)]
    {
        let stream = match connect(&path) {
            Ok(stream) => stream,
            Err(error) => {
                eprintln!("[extension] cannot reach {path}: {error}");
                return std::process::ExitCode::FAILURE;
            }
        };
        match extension::serve(stream, extension::Extension::new()) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("[extension] {error}");
                std::process::ExitCode::FAILURE
            }
        }
    }
}
