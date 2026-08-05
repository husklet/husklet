//! Process adapters for the packaged engine executables.

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
}

/// Runs one architecture-specific engine worker process.
pub struct Worker;

impl Worker {
    pub fn run(guest: Guest) -> ! {
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
        let arguments = std::env::args().collect::<Vec<_>>();
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

fn descriptor(value: hl_engine::environment::AuthorityDescriptor) -> Option<i32> {
    match value {
        hl_engine::environment::AuthorityDescriptor::Present(value) => i32::try_from(value).ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::Guest;

    #[test]
    fn worker_identity_is_architecture_specific() {
        assert_eq!(Guest::Aarch64.name(), "aarch64");
        assert_eq!(Guest::Aarch64.program(), "hl-aarch64");
        assert_eq!(Guest::X86_64.name(), "x86_64");
        assert_eq!(Guest::X86_64.program(), "hl-x86_64");
    }
}
