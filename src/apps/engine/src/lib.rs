//! Process adapters for the packaged engine executables.

use clap::Parser;
use sha2::{Digest, Sha256};
use std::io::Read as _;
use std::path::PathBuf;

/// The fixed guest architecture selected by a worker executable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Guest {
    Aarch64,
    X86_64,
}

impl Guest {
    const fn name(self) -> &'static str {
        match self {
            Self::Aarch64 => "aarch64",
            Self::X86_64 => "x86_64",
        }
    }

    const fn program(self) -> &'static str {
        match self {
            Self::Aarch64 => "hl-aarch64",
            Self::X86_64 => "hl-x86_64",
        }
    }

    const fn isa(self) -> hl_engine::activation::GuestIsa {
        match self {
            Self::Aarch64 => hl_engine::activation::GuestIsa::Aarch64,
            Self::X86_64 => hl_engine::activation::GuestIsa::X86_64,
        }
    }

    fn named(value: &str) -> Option<Self> {
        match value {
            "aarch64" | "arm64" => Some(Self::Aarch64),
            "x86_64" | "amd64" => Some(Self::X86_64),
            _ => None,
        }
    }
}

/// Runs one architecture-specific engine worker process.
pub struct Worker;

#[derive(Parser)]
struct BackendReceiptArguments {
    #[arg(long = "guest-isa")]
    guest: Option<String>,
}

#[derive(Parser)]
#[command(trailing_var_arg = true)]
struct LaunchArguments {
    #[arg(long = "guest-isa")]
    guest: Option<String>,
    #[arg(long)]
    report_exit: bool,
    /// Existing container root used to resolve the guest entry and PT_INTERP.
    #[arg(long)]
    rootfs: Option<PathBuf>,
    executable: PathBuf,
    #[arg(allow_hyphen_values = true)]
    arguments: Vec<String>,
}

impl Worker {
    pub fn run(guest: Guest) -> ! {
        let arguments = std::env::args().collect::<Vec<_>>();
        if arguments.get(1).map(String::as_str) == Some("--backend-receipt") {
            match backend_receipt(&arguments, Some(guest)) {
                Ok(receipt) => {
                    println!("{receipt}");
                    std::process::exit(0);
                }
                Err(()) => std::process::exit(125),
            }
        }
        let logging = hl_log::EnvironmentConfig::parse(hl_log::Config::default(), std::env::vars());
        for warning in logging.warnings() {
            eprintln!("{}: {warning}", guest.program());
        }
        logging.apply();

        let isa = guest.name();
        hl_log::hl_info!(hl_log::tag::EXEC, "engine process starting isa={isa}");
        hl_log::hl_event!(
            hl_log::tag::EXEC,
            hl_log::Level::Info,
            "engine.process.starting",
            isa = isa
        );
        let launch = LaunchArguments::try_parse_from(&arguments).unwrap_or_else(|_| std::process::exit(2));
        let report = launch.report_exit;
        let result = execute(guest, &launch);
        if let Err(error) = result {
            hl_log::hl_error!(hl_log::tag::EXEC, "engine process failed isa={isa} reason={error:?}");
            hl_log::hl_event!(
                hl_log::tag::EXEC,
                hl_log::Level::Error,
                "engine.process.failed",
                isa = isa,
                reason = ?error
            );
            // Preserve the retained x86 worker's opt-in diagnostic output.
            if guest == Guest::X86_64 && std::env::var_os("RUST_BACKTRACE").is_some() {
                eprintln!("{error:?}");
            }
        }
        let status = result.map_or(125, hl_engine::engine::EngineExit::process_status);
        if report && let Ok(exit) = result {
            eprintln!("[hl-exit]\t{:?}\t{}\t{:#x}", exit.kind, exit.guest_status, exit.detail);
        }
        if result.is_ok() {
            hl_log::hl_info!(hl_log::tag::EXEC, "engine process exited isa={isa} status={status}");
            hl_log::hl_event!(
                hl_log::tag::EXEC,
                hl_log::Level::Info,
                "engine.process.exited",
                isa = isa,
                status = status
            );
        }
        std::process::exit(status);
    }
}

fn execute(
    guest: Guest,
    launch: &LaunchArguments,
) -> Result<hl_engine::engine::EngineExit, hl_engine::engine::EngineError> {
    if let Some(selected) = launch.guest.as_deref()
        && Guest::named(selected) != Some(guest)
    {
        return Err(hl_engine::engine::EngineError::LaunchFailed);
    }
    let engine = if let Some(rootfs) = &launch.rootfs {
        if launch.executable.is_absolute()
            || launch
                .executable
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(hl_engine::engine::EngineError::LaunchFailed);
        }
        let entry = &launch.executable;
        let host = rootfs.join(entry);
        let guest_entry = std::path::Path::new("/").join(entry);
        let plan = hl_engine::launcher::plan::RuntimePlan {
            rootfs: Some(rootfs.as_os_str().as_encoded_bytes().to_vec()),
            executable_host: Some(host.as_os_str().as_encoded_bytes().to_vec()),
            arguments: std::iter::once(guest_entry.as_os_str().as_encoded_bytes().to_vec())
                .chain(launch.arguments.iter().map(|argument| argument.as_bytes().to_vec()))
                .collect(),
            environment: Vec::new(),
            result_path: None,
            options: hl_engine::options::Options::default(),
        };
        hl_engine::runtime::Engine::from_plan(guest.isa(), plan)?
    } else {
        let mut builder = hl_engine::runtime::Builder::new(guest.isa(), &launch.executable);
        for argument in &launch.arguments {
            builder = builder.with_argument(argument.as_bytes().to_vec());
        }
        builder.build()?
    };
    engine.start()?;
    let exit = engine.wait()?;
    engine.destroy()?;
    Ok(exit)
}

pub fn backend_receipt(arguments: &[String], forced_guest: Option<Guest>) -> Result<String, ()> {
    if arguments.get(1).map(String::as_str) != Some("--backend-receipt") {
        return Err(());
    }
    let parsed = BackendReceiptArguments::try_parse_from(
        std::iter::once("backend-receipt").chain(arguments[2..].iter().map(String::as_str)),
    )
    .map_err(|_| ())?;
    let selected = match (forced_guest, parsed.guest.as_deref()) {
        (Some(_), Some(_)) => return Err(()),
        (Some(guest), None) => Some(guest),
        (None, Some(guest)) => Some(Guest::named(guest).ok_or(())?),
        (None, None) => None,
    };
    let guest = selected.unwrap_or(if cfg!(target_arch = "aarch64") {
        Guest::Aarch64
    } else {
        Guest::X86_64
    });
    let plan = hl_engine::launcher::plan::RuntimePlan {
        rootfs: None,
        executable_host: None,
        arguments: vec![b"backend-receipt".to_vec()],
        environment: Vec::new(),
        result_path: None,
        options: hl_engine::options::Options::default(),
    };
    // This is the production selector itself.  A receipt is emitted only when
    // it constructs the backend named below for the requested guest ISA.
    let selected = hl_engine::runtime::Engine::from_plan(guest.isa(), plan).map_err(|_| ())?;
    drop(selected);

    let executable = std::env::current_exe().map_err(|_| ())?;
    let mut file = std::fs::File::open(executable).map_err(|_| ())?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|_| ())?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    let hash = digest.finalize();
    let mut hex = String::with_capacity(64);
    for byte in hash {
        use std::fmt::Write as _;
        write!(hex, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(format!(
        "{{\"schema\":\"husklet-engine-backend-v1\",\"backend\":\"retained-c\",\"engine_sha256\":\"{hex}\"}}"
    ))
}

#[cfg(test)]
mod tests {
    use super::{Guest, LaunchArguments, backend_receipt};
    use clap::Parser;

    #[test]
    fn worker_identity_is_architecture_specific() {
        assert_eq!(Guest::Aarch64.name(), "aarch64");
        assert_eq!(Guest::Aarch64.program(), "hl-aarch64");
        assert_eq!(Guest::X86_64.name(), "x86_64");
        assert_eq!(Guest::X86_64.program(), "hl-x86_64");
    }

    #[test]
    fn backend_receipt_is_exact_and_hash_bound() {
        let receipt =
            backend_receipt(&["hl-aarch64".into(), "--backend-receipt".into()], Some(Guest::Aarch64)).unwrap();
        assert!(
            receipt.starts_with(
                "{\"schema\":\"husklet-engine-backend-v1\",\"backend\":\"retained-c\",\"engine_sha256\":\""
            )
        );
        assert!(receipt.ends_with("\"}"));
        let hash = receipt
            .strip_prefix("{\"schema\":\"husklet-engine-backend-v1\",\"backend\":\"retained-c\",\"engine_sha256\":\"")
            .unwrap()
            .strip_suffix("\"}")
            .unwrap();
        assert_eq!(hash.len(), 64);
        assert!(
            hash.bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        );
    }

    #[test]
    fn backend_receipt_rejects_an_explicit_unknown_guest() {
        assert!(
            backend_receipt(
                &[
                    "hl-engine".into(),
                    "--backend-receipt".into(),
                    "--guest-isa".into(),
                    "riscv64".into(),
                ],
                None,
            )
            .is_err()
        );
    }

    #[test]
    fn launch_parser_owns_rootfs_and_trailing_guest_arguments() {
        let launch = LaunchArguments::try_parse_from([
            "hl-x86_64",
            "--rootfs",
            "/staged/rootfs",
            "usr/local/bin/python3",
            "-c",
            "print(42)",
        ])
        .unwrap();
        assert_eq!(launch.rootfs.unwrap(), std::path::Path::new("/staged/rootfs"));
        assert_eq!(launch.executable, std::path::Path::new("usr/local/bin/python3"));
        assert_eq!(launch.arguments, ["-c", "print(42)"]);
    }
}
