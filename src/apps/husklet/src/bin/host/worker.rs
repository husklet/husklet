use std::path::PathBuf;

pub(crate) struct Worker;

enum Operation {
    Launch {
        name: String,
        slot: Option<String>,
        cwd: Option<String>,
        diagnostics: Option<PathBuf>,
    },
    Daemon {
        name: String,
    },
    Domain {
        name: String,
    },
    Compositor {
        socket: PathBuf,
    },
}

impl Operation {
    fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Option<Self>, String> {
        let mut arguments = arguments.into_iter();
        match arguments.next().as_deref() {
            None => Ok(None),
            Some("__compositor") => {
                if arguments.next().as_deref() != Some("--socket") {
                    return Err("native compositor requires --socket <path>".into());
                }
                let socket = arguments
                    .next()
                    .filter(|value| !value.is_empty())
                    .map(PathBuf::from)
                    .ok_or_else(|| "native compositor socket path is missing".to_owned())?;
                if arguments.next().is_some() {
                    return Err("native compositor received unexpected arguments".into());
                }
                Ok(Some(Self::Compositor { socket }))
            }
            Some("--worker") => {
                let operation = arguments.next().unwrap_or_default();
                let name = arguments.next().filter(|value| !value.is_empty());
                match operation.as_str() {
                    "launch" => {
                        let name = name.ok_or_else(|| "workspace name is missing".to_owned())?;
                        let slot = arguments.next().filter(|value| !value.is_empty());
                        let diagnostics = arguments
                            .next()
                            .filter(|value| !value.is_empty())
                            .map(PathBuf::from);
                        let cwd = arguments.next().filter(|value| !value.is_empty());
                        if arguments.next().is_some() {
                            return Err("workspace launch received unexpected arguments".into());
                        }
                        Ok(Some(Self::Launch {
                            name,
                            slot,
                            cwd,
                            diagnostics,
                        }))
                    }
                    "daemon" => {
                        let name = name.ok_or_else(|| "workspace name is missing".to_owned())?;
                        if arguments.next().is_some() {
                            return Err("workspace daemon received unexpected arguments".into());
                        }
                        Ok(Some(Self::Daemon { name }))
                    }
                    "domain" => {
                        let name = name.ok_or_else(|| "workspace name is missing".to_owned())?;
                        if arguments.next().is_some() {
                            return Err("workspace domain received unexpected arguments".into());
                        }
                        Ok(Some(Self::Domain { name }))
                    }
                    _ => Err(format!("invalid Husklet worker operation {operation:?}")),
                }
            }
            Some(_) => Ok(None),
        }
    }
}

impl Worker {
    pub(crate) fn run() -> Option<i32> {
        let operation = match Operation::parse(std::env::args().skip(1)) {
            Ok(operation) => operation?,
            Err(error) => {
                eprintln!("{error}");
                return Some(2);
            }
        };
        Some(match operation {
            Operation::Launch {
                name,
                slot,
                cwd,
                diagnostics,
            } => hl::runtime::worker::Worker::launch(
                &name,
                cwd.as_deref(),
                slot.as_deref(),
                diagnostics.as_deref(),
            ),
            Operation::Daemon { name } => match hl::runtime::worker::Worker::daemon(&name) {
                Ok(socket) => {
                    println!("{}", socket.display());
                    0
                }
                Err(error) => {
                    eprintln!("workspace resources unavailable: {error}");
                    1
                }
            },
            Operation::Domain { name } => match hl::runtime::worker::Worker::domain(&name) {
                Ok(()) => 0,
                Err(error) => {
                    eprintln!("workspace execution domain failed: {error}");
                    1
                }
            },
            Operation::Compositor { socket } => Self::compositor(&socket),
        })
    }

    #[cfg(target_os = "macos")]
    fn compositor(socket: &std::path::Path) -> i32 {
        let configuration = hl::runtime::compositor::NativeConfiguration::configured();
        match hl::runtime::compositor::Service::run_native(socket, configuration) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("native compositor failed: {error}");
                1
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn compositor(_socket: &std::path::Path) -> i32 {
        eprintln!("native compositor is supported only on macOS");
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(arguments: &[&str]) -> Result<Option<Operation>, String> {
        Operation::parse(arguments.iter().map(|value| (*value).to_owned()))
    }

    #[test]
    fn parses_native_compositor_invocation() {
        let operation = parse(&["__compositor", "--socket", "/tmp/wayland-0"])
            .unwrap()
            .expect("private operation");
        assert!(matches!(
            operation,
            Operation::Compositor { socket } if socket == std::path::Path::new("/tmp/wayland-0")
        ));
    }

    #[test]
    fn rejects_incomplete_or_ambiguous_private_invocations() {
        assert!(parse(&["__compositor", "/tmp/wayland-0"]).is_err());
        assert!(parse(&["__compositor", "--socket", ""]).is_err());
        assert!(parse(&["--worker", "launch"]).is_err());
        assert!(parse(&["--worker", "unknown", "workspace"]).is_err());
    }

    #[test]
    fn leaves_application_arguments_to_gtk() {
        assert!(parse(&[]).unwrap().is_none());
        assert!(parse(&["--display", ":0"]).unwrap().is_none());
    }

    #[test]
    fn parses_worker_diagnostics_without_redirecting_terminal_streams() {
        let operation = parse(&[
            "--worker",
            "launch",
            "runtime",
            "pane-0",
            "/tmp/worker.log",
            "/root",
        ])
        .unwrap()
        .expect("private operation");
        assert!(matches!(
            operation,
            Operation::Launch {
                name,
                slot: Some(slot),
                cwd: Some(cwd),
                diagnostics: Some(diagnostics),
            } if name == "runtime"
                && slot == "pane-0"
                && cwd == "/root"
                && diagnostics == std::path::Path::new("/tmp/worker.log")
        ));
    }

    #[test]
    fn daemon_worker_accepts_only_its_workspace_name() {
        assert!(matches!(
            parse(&["--worker", "daemon", "runtime"]),
            Ok(Some(Operation::Daemon { name })) if name == "runtime"
        ));
        assert!(parse(&["--worker", "daemon", "runtime", ""]).is_err());
    }

    #[test]
    fn domain_worker_accepts_only_its_workspace_name() {
        assert!(matches!(
            parse(&["--worker", "domain", "runtime"]),
            Ok(Some(Operation::Domain { name })) if name == "runtime"
        ));
        assert!(parse(&["--worker", "domain", "runtime", "extra"]).is_err());
    }
}
