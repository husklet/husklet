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
    AttachContainer {
        name: String,
        container: String,
        command: Vec<String>,
    },
}

impl Operation {
    fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Option<Self>, String> {
        let mut arguments = arguments.into_iter();
        let leading = arguments.next();
        match leading.as_deref() {
            Some("--worker") => Self::parse_worker(arguments),
            _ => Ok(None),
        }
    }

    fn parse_worker(mut arguments: impl Iterator<Item = String>) -> Result<Option<Self>, String> {
        let operation = arguments.next().unwrap_or_default();
        let name = arguments.next().filter(|value| !value.is_empty());
        match operation.as_str() {
            "launch" => Self::parse_launch(name, arguments),
            "daemon" => Ok(Some(Self::Daemon {
                name: Self::only_name(name, arguments, "daemon")?,
            })),
            "domain" => Ok(Some(Self::Domain {
                name: Self::only_name(name, arguments, "domain")?,
            })),
            "attach-container" => {
                let name = name.ok_or_else(|| "workspace name is missing".to_owned())?;
                let container = arguments
                    .next()
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| "container identity is missing".to_owned())?;
                let command: Vec<String> = arguments.collect();
                if command.is_empty() {
                    return Err("container attachment command is missing".into());
                }
                Ok(Some(Self::AttachContainer {
                    name,
                    container,
                    command,
                }))
            }
            _ => Err(format!("invalid Husklet worker operation {operation:?}")),
        }
    }

    fn parse_launch(name: Option<String>, mut arguments: impl Iterator<Item = String>) -> Result<Option<Self>, String> {
        let name = name.ok_or_else(|| "workspace name is missing".to_owned())?;
        let slot = arguments.next().filter(|value| !value.is_empty());
        let diagnostics = arguments.next().filter(|value| !value.is_empty()).map(PathBuf::from);
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

    fn only_name(
        name: Option<String>,
        mut arguments: impl Iterator<Item = String>,
        operation: &str,
    ) -> Result<String, String> {
        let name = name.ok_or_else(|| "workspace name is missing".to_owned())?;
        if arguments.next().is_some() {
            return Err(format!("workspace {operation} received unexpected arguments"));
        }
        Ok(name)
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
            } => hl::runtime::worker::Worker::launch(&name, cwd.as_deref(), slot.as_deref(), diagnostics.as_deref()),
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
            Operation::AttachContainer {
                name,
                container,
                command,
            } => hl::runtime::worker::Worker::attach_container(&name, &container, &command),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(arguments: &[&str]) -> Result<Option<Operation>, String> {
        Operation::parse(arguments.iter().map(|value| (*value).to_owned()))
    }

    #[test]
    fn ignores_regular_application_arguments() {
        assert!(parse(&[]).unwrap().is_none());
        assert!(parse(&["--help"]).unwrap().is_none());
    }

    #[test]
    fn parses_launch_worker() {
        let operation = parse(&["--worker", "launch", "demo", "pane-1", "/tmp/diagnostics", "/work"])
            .unwrap()
            .expect("private operation");
        assert!(matches!(
            operation,
            Operation::Launch { name, slot: Some(slot), .. }
                if name == "demo" && slot == "pane-1"
        ));
    }

    #[test]
    fn parses_daemon_and_domain_workers() {
        assert!(matches!(
            parse(&["--worker", "daemon", "runtime"]),
            Ok(Some(Operation::Daemon { name })) if name == "runtime"
        ));
        assert!(matches!(
            parse(&["--worker", "domain", "runtime"]),
            Ok(Some(Operation::Domain { name })) if name == "runtime"
        ));
    }

    #[test]
    fn parses_container_attachment_without_shell_joining_argv() {
        let operation = parse(&[
            "--worker",
            "attach-container",
            "dev",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "sh",
            "-lc",
            "printf '%s' \"$HOME\"",
        ])
        .unwrap()
        .unwrap();
        assert!(matches!(
            operation,
            Operation::AttachContainer { name, container, command }
                if name == "dev"
                    && container == "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    && command == ["sh", "-lc", "printf '%s' \"$HOME\""]
        ));
    }

    #[test]
    fn rejects_missing_or_extra_worker_arguments() {
        assert!(parse(&["--worker", "domain"]).is_err());
        assert!(parse(&["--worker", "domain", "demo", "extra"]).is_err());
        assert!(parse(&["--worker", "unknown", "demo"]).is_err());
    }
}
