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
        capture: Option<hl::runtime::gpu::CaptureOptions>,
    },
    Compositor {
        socket: PathBuf,
        gpu: hl::runtime::compositor::NativeGpuConfiguration,
    },
    GpuReplay {
        capture: PathBuf,
        output: PathBuf,
    },
}

impl Operation {
    fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Option<Self>, String> {
        let mut arguments = arguments.into_iter();
        match arguments.next().as_deref() {
            None => Ok(None),
            Some("__gpu-replay") => {
                let capture = arguments
                    .next()
                    .filter(|value| !value.is_empty())
                    .map(PathBuf::from)
                    .ok_or_else(|| "GPU replay requires a capture path".to_owned())?;
                let output = arguments
                    .next()
                    .filter(|value| !value.is_empty())
                    .map(PathBuf::from)
                    .ok_or_else(|| "GPU replay requires an output directory".to_owned())?;
                if arguments.next().is_some() {
                    return Err("GPU replay received unexpected arguments".to_owned());
                }
                Ok(Some(Self::GpuReplay { capture, output }))
            }
            Some("__compositor") => {
                let mut socket = None;
                let mut gpu_socket = None;
                let mut backend = None;
                let mut trace = None;
                while let Some(flag) = arguments.next() {
                    let value = arguments.next().ok_or_else(|| {
                        format!("native compositor argument {flag} requires a value")
                    })?;
                    if value.is_empty() {
                        return Err(format!("native compositor argument {flag} is empty"));
                    }
                    let target = match flag.as_str() {
                        "--socket" => &mut socket,
                        "--gpu-socket" => &mut gpu_socket,
                        "--gpu-backend" => &mut backend,
                        "--gpu-trace" => &mut trace,
                        _ => {
                            return Err(format!(
                                "native compositor received unexpected argument {flag:?}"
                            ))
                        }
                    };
                    if target.replace(value).is_some() {
                        return Err(format!(
                            "native compositor argument {flag} was provided more than once"
                        ));
                    }
                }
                let socket = socket
                    .map(PathBuf::from)
                    .ok_or_else(|| "native compositor requires --socket <path>".to_owned())?;
                let gpu_socket = gpu_socket
                    .map(PathBuf::from)
                    .ok_or_else(|| "native compositor requires --gpu-socket <path>".to_owned())?;
                let backend = backend
                    .ok_or_else(|| "native compositor requires --gpu-backend <backend>".to_owned())?
                    .parse::<hl::runtime::gpu::Backend>()?;
                let trace = match trace.as_deref() {
                    Some("on") => true,
                    Some("off") => false,
                    Some(value) => {
                        return Err(format!(
                            "invalid --gpu-trace value {value:?}; expected on or off"
                        ))
                    }
                    None => {
                        return Err("native compositor requires --gpu-trace <on|off>".to_owned())
                    }
                };
                let gpu = hl::runtime::compositor::NativeGpuConfiguration::new(
                    gpu_socket, backend, trace,
                )
                .map_err(|error| error.to_string())?;
                Ok(Some(Self::Compositor { socket, gpu }))
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
                        let capture = hl::runtime::gpu::CaptureOptions::from_worker(
                            arguments.collect::<Vec<_>>(),
                        )?;
                        Ok(Some(Self::Domain { name, capture }))
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
            Operation::Domain { name, capture } => {
                if let Some(capture) = capture {
                    capture.apply();
                }
                match hl::runtime::worker::Worker::domain(&name) {
                    Ok(()) => 0,
                    Err(error) => {
                        eprintln!("workspace execution domain failed: {error}");
                        1
                    }
                }
            }
            Operation::Compositor { socket, gpu } => Self::compositor(&socket, gpu),
            Operation::GpuReplay { capture, output } => {
                match hl::runtime::gpu::replay::Replay::run(&capture, &output) {
                    Ok(frames) => {
                        println!(
                            "replayed {} frame(s) into {}",
                            frames.len(),
                            output.display()
                        );
                        0
                    }
                    Err(error) => {
                        eprintln!("GPU replay failed: {error}");
                        1
                    }
                }
            }
        })
    }

    #[cfg(target_os = "macos")]
    fn compositor(
        socket: &std::path::Path,
        gpu: hl::runtime::compositor::NativeGpuConfiguration,
    ) -> i32 {
        let configuration = hl::runtime::compositor::NativeConfiguration::configured(gpu);
        match hl::runtime::compositor::Service::run_native(socket, configuration) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("native compositor failed: {error}");
                1
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn compositor(
        _socket: &std::path::Path,
        _gpu: hl::runtime::compositor::NativeGpuConfiguration,
    ) -> i32 {
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
        let operation = parse(&[
            "__compositor",
            "--socket",
            "/tmp/wayland-0",
            "--gpu-socket",
            "/tmp/gpu.sock",
            "--gpu-backend",
            "wgpu",
            "--gpu-trace",
            "off",
        ])
        .unwrap()
        .expect("private operation");
        assert!(matches!(
            operation,
            Operation::Compositor { socket, .. }
                if socket == std::path::Path::new("/tmp/wayland-0")
        ));
    }

    #[test]
    fn parses_gpu_replay_invocation() {
        let operation = parse(&["__gpu-replay", "/tmp/frame.hgpu", "/tmp/frames"])
            .unwrap()
            .expect("private operation");
        assert!(matches!(
            operation,
            Operation::GpuReplay { capture, output }
                if capture == std::path::Path::new("/tmp/frame.hgpu")
                    && output == std::path::Path::new("/tmp/frames")
        ));
        assert!(parse(&["__gpu-replay", "/tmp/frame.hgpu"]).is_err());
    }

    #[test]
    fn rejects_incomplete_or_ambiguous_private_invocations() {
        assert!(parse(&["__compositor", "/tmp/wayland-0"]).is_err());
        assert!(parse(&["__compositor", "--socket", ""]).is_err());
        assert!(parse(&[
            "__compositor",
            "--socket",
            "/tmp/wayland-0",
            "--socket",
            "/tmp/wayland-1",
            "--gpu-socket",
            "/tmp/gpu.sock",
            "--gpu-backend",
            "wgpu",
            "--gpu-trace",
            "off",
        ])
        .is_err());
        assert!(parse(&[
            "__compositor",
            "--socket",
            "/tmp/wayland-0",
            "--gpu-socket",
            "/tmp/gpu.sock",
            "--gpu-backend",
            "invalid",
            "--gpu-trace",
            "off",
        ])
        .is_err());
        assert!(parse(&[
            "__compositor",
            "--socket",
            "/tmp/wayland-0",
            "--gpu-socket",
            "/tmp/gpu.sock",
            "--gpu-backend",
            "wgpu",
            "--gpu-trace",
            "maybe",
        ])
        .is_err());
        assert!(parse(&[
            "__compositor",
            "--socket",
            "/tmp/wayland-0",
            "--gpu-socket",
            "/tmp/gpu.sock",
            "--gpu-backend",
            "wgpu",
            "--gpu-trace",
            "off",
            "--surprise",
            "value",
        ])
        .is_err());
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
    fn domain_worker_parses_forwarded_gpu_capture_configuration() {
        assert!(matches!(
            parse(&["--worker", "domain", "runtime"]),
            Ok(Some(Operation::Domain {
                name,
                capture: None
            })) if name == "runtime"
        ));
        let operation = parse(&[
            "--worker",
            "domain",
            "runtime",
            "--gpu-capture-dir",
            "/tmp/gpu",
            "--gpu-capture-batches",
            "8",
            "--gpu-capture-bytes",
            "4096",
            "--gpu-capture-presents",
            "1",
        ])
        .unwrap()
        .expect("domain worker");
        assert!(matches!(
            operation,
            Operation::Domain {
                name,
                capture: Some(capture)
            } if name == "runtime"
                && capture.worker_arguments().unwrap()
                    == [
                        "--gpu-capture-dir", "/tmp/gpu",
                        "--gpu-capture-batches", "8",
                        "--gpu-capture-bytes", "4096",
                        "--gpu-capture-presents", "1",
                    ]
        ));
        assert!(parse(&["--worker", "domain", "runtime", "extra"]).is_err());
    }
}
