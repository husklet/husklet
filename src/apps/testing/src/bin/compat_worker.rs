use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::Duration;

fn main() {
    let arguments = std::env::args().collect::<Vec<_>>();
    if arguments.len() != 5 && arguments.len() != 9 && arguments.len() != 10 && arguments.len() != 11 {
        std::process::exit(64);
    }
    let isa = &arguments[1];
    let guest = PathBuf::from(&arguments[2]);
    let environment = &arguments[3];
    let report = PathBuf::from(&arguments[4]);
    let fixture = arguments
        .get(5)
        .map(String::as_str)
        .unwrap_or("executable")
        .parse::<Fixture>()
        .unwrap_or_else(|()| std::process::exit(64));
    let launch_arguments = arguments.get(6).map(String::as_str).unwrap_or("-");
    let side_files = arguments.get(7).map(String::as_str).unwrap_or("-");
    let rootfs = arguments.get(8).map(String::as_str).unwrap_or("-");
    let guest_executable = arguments.get(9).filter(|value| value.as_str() != "trace");
    let trace = arguments.iter().skip(9).any(|value| value == "trace");
    if run(
        isa,
        &guest,
        environment,
        &report,
        fixture,
        launch_arguments,
        side_files,
        rootfs,
        guest_executable.map(String::as_str),
        trace,
    )
    .is_err()
    {
        std::process::exit(125);
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Fixture {
    Executable,
    Network,
    Process,
    SideFile,
    Directory,
    Symlink,
    RootExecutable,
    RootTree,
    RootInterpreter,
    Device,
}

impl std::str::FromStr for Fixture {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "executable" => Ok(Self::Executable),
            "network-sandbox" => Ok(Self::Network),
            "multi-process-service" => Ok(Self::Process),
            "side-file" => Ok(Self::SideFile),
            "directory-tree" => Ok(Self::Directory),
            "entry-symlink" => Ok(Self::Symlink),
            "rootfs-executable" => Ok(Self::RootExecutable),
            "rootfs-tree" => Ok(Self::RootTree),
            "rootfs-interpreter" => Ok(Self::RootInterpreter),
            "special-device" => Ok(Self::Device),
            _ => Err(()),
        }
    }
}

#[cfg(test)]
mod test {
    use super::Fixture;

    #[test]
    fn process_is_typed() {
        assert!(matches!(
            "multi-process-service".parse::<Fixture>(),
            Ok(Fixture::Process)
        ));
        assert!("external-process-service".parse::<Fixture>().is_err());
    }

    #[test]
    fn device_is_typed() {
        assert!(matches!("special-device".parse::<Fixture>(), Ok(Fixture::Device)));
        assert!("host-device".parse::<Fixture>().is_err());
    }
}

// hl-lint: visual-section
fn run(
    isa: &str,
    guest: &PathBuf,
    environment: &str,
    report: &PathBuf,
    fixture: Fixture,
    launch_arguments: &str,
    side_files: &str,
    rootfs: &str,
    guest_executable: Option<&str>,
    trace: bool,
) -> Result<(), ()> {
    let isa = match isa {
        "aarch64" => hl_engine::activation::GuestIsa::Aarch64,
        "x86_64" => hl_engine::activation::GuestIsa::X86_64,
        _ => return Err(()),
    };
    let mut builder = options(hl_engine::runtime::Builder::new(isa, guest), environment, fixture)?;
    if let Some(path) = guest_executable {
        builder = builder.with_guest_executable(path);
    }
    let mut builder = fixture_input(builder, guest, fixture, side_files)?;
    builder = rootfs_input(builder, isa, guest, rootfs)?;
    if launch_arguments != "-" {
        builder = builder.with_argument(launch_arguments.as_bytes().to_vec());
    }
    let trace_path = report.with_extension("trace");
    if trace {
        builder = builder.with_trace(&trace_path);
    }
    let _signals = hl_engine::native::TerminationSignals::install().map_err(|_| ())?;
    let engine = Arc::new(builder.build().map_err(|_| ())?);
    let workspace = engine.workspace().to_owned();
    engine.start().map_err(|_| ())?;
    let exit = Waiter::run(&engine)?;
    engine.destroy().map_err(|_| ())?;
    let cleaned = !workspace.exists();
    let mut output = fs::File::create(report).map_err(|_| ())?;
    writeln!(
        output,
        "{}\n{cleaned}\n{exit:?}",
        hl_engine::program::Program::exit_status(exit)
    )
    .map_err(|_| ())?;
    if trace {
        let records = fs::read_to_string(trace_path).map_err(|_| ())?;
        write!(output, "{records}").map_err(|_| ())?;
    }
    Ok(())
}

struct Waiter;

impl Waiter {
    fn run(engine: &Arc<hl_engine::runtime::Engine>) -> Result<hl_engine::engine::EngineExit, ()> {
        let (sender, receiver) = mpsc::sync_channel(1);
        let wait_engine = Arc::clone(engine);
        std::thread::Builder::new()
            .name("compat-wait".into())
            .spawn(move || {
                let _ = sender.send(wait_engine.wait());
            })
            .map_err(|_| ())?;
        loop {
            if let Some(exit) = Self::poll(engine, &receiver)? {
                return exit.map_err(|_| ());
            }
        }
    }

    fn poll(
        engine: &hl_engine::runtime::Engine,
        receiver: &mpsc::Receiver<Result<hl_engine::engine::EngineExit, hl_engine::engine::EngineError>>,
    ) -> Result<Option<Result<hl_engine::engine::EngineExit, hl_engine::engine::EngineError>>, ()> {
        match receiver.recv_timeout(Duration::from_millis(10)) {
            Ok(exit) => Ok(Some(exit)),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if hl_engine::native::TerminationSignals::pending().is_some() {
                    let _ = engine.stop(hl_engine::engine::StopRequest::Force);
                }
                Ok(None)
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(()),
        }
    }
}

// hl-lint: visual-section
fn rootfs_input(
    builder: hl_engine::runtime::Builder,
    isa: hl_engine::activation::GuestIsa,
    guest: &PathBuf,
    category: &str,
) -> Result<hl_engine::runtime::Builder, ()> {
    use hl_engine::runtime::{Input, Rootfs};
    if category == "-" {
        return Ok(builder);
    }
    let mut rootfs = Rootfs::scratch("guest");
    match category {
        "scratch-rootfs" => {}
        "mapping-data-rootfs" => {
            rootfs = rootfs.with_input(Input::File {
                source: guest.clone(),
                relative: PathBuf::from("data"),
                executable: false,
            });
        }
        "alpine-rootfs" => {
            for relative in [
                "tmp",
                "proc",
                "proc/self",
                "sys",
                "sys/fs",
                "sys/fs/cgroup",
                "dev",
                "dev/shm",
                "dev/pts",
                "etc",
            ] {
                rootfs = rootfs.with_input(Input::Directory {
                    source: None,
                    relative: PathBuf::from(relative),
                });
            }
        }
        "dynamic-rootfs" => {
            let runtime =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../tests/runtime/legacy/artifacts/runtime");
            let resources = match isa {
                hl_engine::activation::GuestIsa::Aarch64 => [
                    ("aarch64/lib/ld-linux-aarch64.so.1", "lib/ld-linux-aarch64.so.1"),
                    (
                        "aarch64/lib/aarch64-linux-gnu/libc.so.6",
                        "lib/aarch64-linux-gnu/libc.so.6",
                    ),
                ],
                hl_engine::activation::GuestIsa::X86_64 => [
                    ("x86_64/lib64/ld-linux-x86-64.so.2", "lib64/ld-linux-x86-64.so.2"),
                    (
                        "x86_64/lib/x86_64-linux-gnu/libc.so.6",
                        "lib/x86_64-linux-gnu/libc.so.6",
                    ),
                ],
            };
            for (source, relative) in resources {
                rootfs = rootfs.with_input(Input::File {
                    source: runtime.join(source),
                    relative: PathBuf::from(relative),
                    executable: true,
                });
            }
        }
        _ => return Err(()),
    }
    Ok(builder.with_rootfs(rootfs))
}

// hl-lint: visual-section
fn fixture_input(
    builder: hl_engine::runtime::Builder,
    guest: &PathBuf,
    fixture: Fixture,
    side_files: &str,
) -> Result<hl_engine::runtime::Builder, ()> {
    if matches!(
        fixture,
        Fixture::Executable
            | Fixture::Network
            | Fixture::Process
            | Fixture::RootExecutable
            | Fixture::RootTree
            | Fixture::RootInterpreter
            | Fixture::Device
    ) {
        return Ok(builder);
    }
    match fixture {
        Fixture::SideFile => {
            return Ok(builder.with_input(hl_engine::runtime::Input::File {
                source: guest
                    .parent()
                    .and_then(|path| path.parent())
                    .and_then(|path| path.parent())
                    .and_then(|path| path.parent())
                    .and_then(|path| path.parent())
                    .ok_or(())?
                    .join(side_files),
                relative: PathBuf::from("tmp/hl_pclib_blob.bin"),
                executable: false,
            }));
        }
        Fixture::Directory => {
            return Ok(builder.with_input(hl_engine::runtime::Input::Directory {
                source: None,
                relative: PathBuf::from("volume"),
            }));
        }
        Fixture::Symlink => {
            return Ok(builder
                .with_input(hl_engine::runtime::Input::Symlink {
                    relative: PathBuf::from("entry"),
                    target: PathBuf::from("guest"),
                })
                .with_entry("entry"));
        }
        Fixture::Executable
        | Fixture::Network
        | Fixture::Process
        | Fixture::RootExecutable
        | Fixture::RootTree
        | Fixture::RootInterpreter
        | Fixture::Device => return Err(()),
    }
}

fn options(
    mut builder: hl_engine::runtime::Builder,
    environment: &str,
    fixture: Fixture,
) -> Result<hl_engine::runtime::Builder, ()> {
    if fixture == Fixture::Network {
        builder = builder.with_option("HL_NET_HOST", "1");
    }
    if environment == "-" {
        return Ok(builder);
    }
    for assignment in environment.split(';') {
        let (name, value) = assignment.split_once('=').ok_or(())?;
        let value = if fixture == Fixture::Directory {
            value.replace("/tmp", "{workspace}/volume")
        } else {
            value.to_owned()
        };
        if hl_engine::options::Options::defines(name) {
            builder = builder.with_option(name, value);
        } else {
            builder = builder.with_environment(name.as_bytes(), value.as_bytes());
        }
    }
    Ok(builder)
}
