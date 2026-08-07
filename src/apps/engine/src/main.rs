use clap::Parser;
use std::fmt::Write as _;

fn main() {
    let environment =
        Environment::try_parse_from(["hl-engine"]).expect("engine environment contains valid Unicode values");
    let logging = hl_log::EnvironmentConfig::parse(hl_log::Config::default(), environment.logging());
    for warning in logging.warnings() {
        eprintln!("hl-engine: {warning}");
    }
    logging.apply();
    let mut arguments = std::env::args().collect::<Vec<_>>();
    let mut environment = environment.bootstrap();
    let authority = match environment.take_authority_descriptor() {
        hl_engine::environment::AuthorityDescriptor::Absent => None,
        hl_engine::environment::AuthorityDescriptor::Present(value) => i32::try_from(value).ok(),
        hl_engine::environment::AuthorityDescriptor::Invalid => None,
    };
    let health = match environment.take_authority_health() {
        hl_engine::environment::AuthorityDescriptor::Absent => None,
        hl_engine::environment::AuthorityDescriptor::Present(value) => i32::try_from(value).ok(),
        hl_engine::environment::AuthorityDescriptor::Invalid => None,
    };
    let report = arguments.iter().position(|value| value == "--report-exit");
    if let Some(index) = report {
        arguments.remove(index);
    }
    let isa = ExitReport::isa(&arguments);
    let status = match hl_engine::program::Program::run_authorized(arguments, authority, health) {
        Ok(exit) => {
            if report.is_some() {
                ExitReport::write(exit);
            }
            hl_engine::program::Program::exit_status(exit)
        }
        Err(error) => {
            if report.is_some() {
                ExitReport::error(&isa, error);
            }
            error.status()
        }
    };
    std::process::exit(status);
}

#[derive(Debug, Default, Parser)]
struct Environment {
    #[arg(long, env = "HL_LOG", hide = true)]
    log: Option<String>,
    #[arg(long, env = "HL_LOG_LEVEL", hide = true)]
    log_level: Option<String>,
    #[arg(long, env = "HL_LOG_COUNTERS", hide = true)]
    log_counters: Option<String>,
    #[arg(long, env = "HL_AUTHORITY_FD", hide = true)]
    authority: Option<String>,
    #[arg(long, env = "HL_AUTHORITY_HEALTH_FD", hide = true)]
    authority_health: Option<String>,
}

impl Environment {
    fn logging(&self) -> impl Iterator<Item = (&'static str, &str)> {
        [
            (hl_log::LOG_TAGS, self.log.as_deref()),
            (hl_log::LOG_LEVEL, self.log_level.as_deref()),
            (hl_log::PROFILE_TAGS, self.log_counters.as_deref()),
        ]
        .into_iter()
        .filter_map(|(name, value)| value.map(|value| (name, value)))
    }

    fn bootstrap(self) -> hl_engine::environment::BootstrapEnvironment {
        hl_engine::environment::BootstrapEnvironment::capture(
            [
                (hl_engine::environment::AUTHORITY_DESCRIPTOR_NAME, self.authority),
                (hl_engine::environment::AUTHORITY_HEALTH_NAME, self.authority_health),
            ]
            .into_iter()
            .filter_map(|(name, value)| value.map(|value| (name, value))),
        )
    }
}

struct ExitReport;

impl ExitReport {
    fn isa(arguments: &[String]) -> String {
        arguments
            .windows(2)
            .find(|pair| pair[0] == "--guest-isa")
            .map_or_else(|| "unknown".into(), |pair| pair[1].clone())
    }

    fn error(isa: &str, error: hl_engine::program::ProgramError) {
        eprintln!("{}", Self::error_line(isa, error));
    }

    fn error_line(isa: &str, error: hl_engine::program::ProgramError) -> String {
        format!("[hl-exit]\tError\t0\t{isa}\t0x0\t-\t{error:?}")
    }

    fn write(exit: hl_engine::engine::EngineExit) {
        let Some(fault) = exit.fault else {
            eprintln!("[hl-exit]\t{:?}\t{}\t{:#x}", exit.kind, exit.guest_status, exit.detail);
            return;
        };
        let opcode = fault.opcode[..usize::from(fault.opcode_len)]
            .iter()
            .fold(String::new(), |mut text, byte| {
                let _ = write!(text, "{byte:02x}");
                text
            });
        let address = fault
            .address
            .map_or_else(|| "-".to_string(), |value| format!("{value:#x}"));
        let access = fault
            .access
            .map_or_else(|| "-".to_string(), |value| format!("{value:?}"));
        eprintln!(
            "[hl-exit]\tFault\t{}\t{:?}\t{:#x}\t{}\t{}\t{}\t{:?}",
            exit.guest_status, fault.isa, fault.pc, opcode, address, access, fault.reason,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{Environment, ExitReport};
    use clap::Parser;

    #[test]
    fn environment_routes() {
        let environment = Environment::try_parse_from([
            "hl-engine",
            "--log",
            "exec",
            "--log-level",
            "debug",
            "--log-counters",
            "syscall",
            "--authority",
            "12",
            "--authority-health",
            "13",
        ])
        .unwrap();
        let logging = environment.logging().collect::<Vec<_>>();
        assert_eq!(
            logging,
            [
                (hl_log::LOG_TAGS, "exec"),
                (hl_log::LOG_LEVEL, "debug"),
                (hl_log::PROFILE_TAGS, "syscall"),
            ]
        );

        let mut bootstrap = environment.bootstrap();
        assert_eq!(
            bootstrap.take_authority_descriptor(),
            hl_engine::environment::AuthorityDescriptor::Present(12)
        );
        assert_eq!(
            bootstrap.take_authority_health(),
            hl_engine::environment::AuthorityDescriptor::Present(13)
        );
    }

    #[test]
    fn exit_report_renders_bounded_construction_cause() {
        let error = hl_engine::program::ProgramError::Engine(hl_engine::engine::EngineError::Construction(
            hl_engine::composition::ConstructionError::Memory,
        ));
        assert_eq!(
            ExitReport::error_line("aarch64", error),
            "[hl-exit]\tError\t0\taarch64\t0x0\t-\tEngine(Construction(Memory))",
        );
    }
}
