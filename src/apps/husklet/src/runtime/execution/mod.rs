//! Workspace execution through the standalone container domain.

use crate::config::WorkspaceConfig;
use crate::paths;
use hl_container::{Config, Console, ContainerSpec, Containers, Guest, Mount, Size};
use hl_images::remote::{Auth, Registry};
use hl_images::{Images, Platform, Reference, RuntimeOverrides};
use hl_ws::Arch;
use hl_ws_term::PtyBackend;
use std::collections::BTreeMap;
use std::io;

mod process;

use process::{ContainerPty, Hostname, Shell};

struct LauncherError;

impl LauncherError {
    fn io(error: impl std::fmt::Display) -> io::Error {
        io::Error::other(error.to_string())
    }
}

pub fn launch(
    workspace: &WorkspaceConfig,
    columns: u16,
    rows: u16,
    cwd: Option<&str>,
    slot: Option<&str>,
) -> io::Result<Box<dyn PtyBackend>> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let root = workspace.storage_dir(&paths::hl_root()).join("containers");
    let images = Images::open(paths::images_dir()).map_err(LauncherError::io)?;
    let containers = runtime
        .block_on(
            Containers::builder(Config::new(root))
                .images(images.clone())
                .build(),
        )
        .map_err(LauncherError::io)?;
    let platform = match workspace.arch {
        Arch::Arm64 => Platform::linux_arm64(),
        Arch::Amd64 => Platform::linux_amd64(),
    };
    let reference: Reference = workspace.image.parse().map_err(LauncherError::io)?;
    let image = match images.resolve(&reference).map_err(LauncherError::io)? {
        Some(image) => image,
        None => runtime
            .block_on(images.pull(&Registry::new(Auth::Anonymous), reference, &platform))
            .map_err(LauncherError::io)?,
    };
    let unpacked = images
        .unpack(&image, &platform)
        .map_err(LauncherError::io)?;
    let injection = crate::runtime::gpu::Injection::for_workspace(workspace)?;

    let start_dir = cwd
        .map(str::trim)
        .filter(|value| value.starts_with('/') && !value.is_empty())
        .unwrap_or("/root");
    let base = workspace
        .shell
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map_or_else(
            || {
                "if command -v bash >/dev/null 2>&1; then exec bash -il; else exec sh -i; fi"
                    .to_owned()
            },
            |shell| format!("exec {shell}"),
        );
    let command = format!("cd {} 2>/dev/null; {base}", Shell::quote(start_dir));
    let mut environment = BTreeMap::from([
        ("TERM".to_owned(), "xterm-256color".to_owned()),
        ("HOME".to_owned(), "/root".to_owned()),
        (
            "PATH".to_owned(),
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_owned(),
        ),
    ]);
    environment.extend(workspace.env.iter().cloned());
    environment.extend(injection.environment.iter().cloned());
    if let Some(library) = &injection.library_path {
        let value = environment
            .get("LD_LIBRARY_PATH")
            .filter(|value| !value.is_empty())
            .map(|value| format!("{library}:{value}"))
            .unwrap_or_else(|| library.clone());
        environment.insert("LD_LIBRARY_PATH".to_owned(), value);
    }
    let daemon_socket = workspace
        .docker_sock
        .then(|| crate::runtime::resources::Daemon::new(&workspace.name).ensure())
        .transpose()?;
    if daemon_socket.is_some() {
        environment.insert(
            "DOCKER_HOST".to_owned(),
            "unix:///run/docker.sock".to_owned(),
        );
    }
    let overrides = RuntimeOverrides {
        entrypoint: Some(vec!["/bin/sh".to_owned()]),
        command: Some(vec!["-c".to_owned(), command]),
        environment,
        working_directory: Some(start_dir.to_owned()),
        user: Some("0:0".to_owned()),
    };
    let name = format!(
        "{}-{}-{}",
        Hostname::sanitize(&workspace.name),
        slot.map(Hostname::sanitize)
            .unwrap_or_else(|| "terminal".to_owned()),
        std::process::id()
    );
    let size = Size::new(rows.max(1), columns.max(1)).map_err(LauncherError::io)?;
    let container = runtime
        .block_on(
            containers.create_image(&unpacked, overrides, |mut spec: ContainerSpec| {
                spec = spec
                    .name(name.clone())
                    .hostname(Hostname::sanitize(&workspace.name))
                    .guest(match workspace.arch {
                        Arch::Arm64 => Guest::Aarch64,
                        Arch::Amd64 => Guest::X86_64,
                    });
                spec.process.console = Console {
                    stdin: true,
                    terminal: Some(size),
                };
                for mount in &workspace.mounts {
                    spec = spec.mount(if mount.ro {
                        Mount::read_only(&mount.host, &mount.container)
                    } else {
                        Mount::read_write(&mount.host, &mount.container)
                    });
                }
                for mount in &injection.mounts {
                    spec = spec.mount(mount.clone());
                }
                if let Some(socket) = &daemon_socket {
                    spec = spec.mount(Mount::read_write(socket, "/run/docker.sock"));
                }
                spec
            }),
        )
        .map_err(LauncherError::io)?;
    let name = container
        .spec
        .name
        .as_deref()
        .unwrap_or(container.id.as_str())
        .to_owned();
    let mut session = runtime
        .block_on(containers.attach(&name))
        .map_err(LauncherError::io)?;
    let input = session.input();
    runtime
        .block_on(containers.start(&name))
        .map_err(LauncherError::io)?;

    let (output_tx, output) = std::sync::mpsc::channel();
    let exited = std::sync::Arc::new(std::sync::Mutex::new(None));
    runtime.spawn(async move {
        while let Ok(Some(entry)) = session.next().await {
            if output_tx.send(entry.bytes).is_err() {
                return;
            }
        }
    });
    let waiting = containers.clone();
    let waiting_name = name.clone();
    let exit = std::sync::Arc::clone(&exited);
    runtime.spawn(async move {
        if let Ok(status) = waiting.wait(&waiting_name).await {
            let code = match status {
                hl_container::ExitStatus::Code(code) => code,
                hl_container::ExitStatus::Signal(signal) => 128 + signal,
                hl_container::ExitStatus::Fault { status, .. } => status,
            };
            *exit.lock().expect("container exit status") = Some(code);
        }
    });

    Ok(Box::new(ContainerPty {
        runtime,
        containers,
        name,
        input,
        output,
        pending: Default::default(),
        exited,
        _gpu_service: injection.service,
        _compositor_service: injection.compositor,
    }))
}
