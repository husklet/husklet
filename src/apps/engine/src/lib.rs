//! Process adapters for the packaged engine executables.

use clap::Parser;
use sha2::{Digest, Sha256};
use std::io::Read as _;

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
    #[arg(long = "engine-option")]
    engine_options: Vec<String>,
    #[arg(long = "guest-isa")]
    guest: Option<String>,
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
        if arguments.get(1).map(String::as_str) == Some("--c-worker") {
            let descriptor = |name: &str| {
                let value = std::env::var(name).ok()?;
                (!value.is_empty() && value.len() <= 10 && value.bytes().all(|byte| byte.is_ascii_digit()))
                    .then(|| value.parse::<i32>().ok())
                    .flatten()
                    .filter(|value| *value >= 3)
            };
            let status = match (
                descriptor("HL_C_PLAN_FD"),
                descriptor("HL_C_CONTROL_FD"),
                descriptor("HL_C_PROVIDER_FD"),
            ) {
                (Some(plan), Some(control), provider)
                    if plan != control && provider.is_none_or(|provider| provider != plan && provider != control) =>
                {
                    hl_engine::retained_worker::run_with_provider(plan, control, provider).unwrap_or_else(|error| {
                        eprintln!("{}: retained worker failed: {error:?}", guest.program());
                        error.status()
                    })
                }
                _ => {
                    eprintln!("{}: retained worker descriptors are invalid", guest.program());
                    64
                }
            };
            std::process::exit(status);
        }
        let mut environment = hl_engine::environment::BootstrapEnvironment::capture(std::env::vars());
        let authority = descriptor(environment.take_authority_descriptor());
        let health = descriptor(environment.take_authority_health());
        let result = hl_engine::program::Program::run_authorized(arguments, authority, health);
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
        let status = result.map_or_else(
            hl_engine::program::ProgramError::status,
            hl_engine::program::Program::exit_status,
        );
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

#[cfg(target_os = "linux")]
pub fn backend_receipt(arguments: &[String], forced_guest: Option<Guest>) -> Result<String, ()> {
    if arguments.get(1).map(String::as_str) != Some("--backend-receipt") {
        return Err(());
    }
    let mut options = hl_engine::options::Options::default();
    let parsed = BackendReceiptArguments::try_parse_from(
        std::iter::once("backend-receipt").chain(arguments[2..].iter().map(String::as_str)),
    )
    .map_err(|_| ())?;
    for assignment in parsed.engine_options {
        let (name, value) = assignment.split_once('=').ok_or(())?;
        options.set(name, value, true).map_err(|_| ())?;
    }
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
    let plan = hl_engine::launch_plan::RuntimePlan {
        rootfs: None,
        executable_host: None,
        arguments: vec![b"backend-receipt".to_vec()],
        environment: Vec::new(),
        result_path: None,
        options,
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

#[cfg(not(target_os = "linux"))]
pub fn backend_receipt(_: &[String], _: Option<Guest>) -> Result<String, ()> {
    Err(())
}

fn descriptor(value: hl_engine::environment::AuthorityDescriptor) -> Option<i32> {
    match value {
        hl_engine::environment::AuthorityDescriptor::Present(value) => i32::try_from(value).ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{Guest, backend_receipt};

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
    fn backend_receipt_honors_production_selection() {
        let arguments = |backend: &str| {
            vec![
                "hl-aarch64".into(),
                "--backend-receipt".into(),
                "--engine-option".into(),
                format!("HL_EXECUTION_BACKEND={backend}"),
            ]
        };
        assert!(backend_receipt(&arguments("c"), Some(Guest::Aarch64)).is_ok());
        assert!(backend_receipt(&arguments("rust"), Some(Guest::Aarch64)).is_err());
        assert!(backend_receipt(&arguments("bogus"), Some(Guest::Aarch64)).is_err());
        assert!(backend_receipt(&arguments("c"), Some(Guest::X86_64)).is_ok());
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
}
