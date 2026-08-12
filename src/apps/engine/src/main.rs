use clap::Parser;
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;

const C_WORKER_ARGUMENT: &str = "--c-worker";
const C_PLAN_DESCRIPTOR: &str = "HL_C_PLAN_FD";
const C_CONTROL_DESCRIPTOR: &str = "HL_C_CONTROL_FD";

fn main() {
    let mut arguments = std::env::args().collect::<Vec<_>>();
    if arguments.get(1).map(String::as_str) == Some("--backend-receipt") {
        match engine::backend_receipt(&arguments, None) {
            Ok(receipt) => {
                println!("{receipt}");
                std::process::exit(0);
            }
            Err(_) => std::process::exit(125),
        }
    }
    if let Some(worker) = CWorker::capture(&arguments) {
        // The worker inherits the guest's stderr. Host diagnostics belong to the
        // supervising parent and travel over the bounded control protocol.
        hl_log::Output::global().set(Box::new(hl_log::DiscardSink));
        let status = match worker {
            Ok(worker) => {
                hl_engine::retained_worker::run(worker.plan, worker.control).unwrap_or_else(|error| error.status())
            }
            Err(error) => error.status(),
        };
        std::process::exit(status);
    }
    let environment =
        Environment::try_parse_from(["hl-engine"]).expect("engine environment contains valid Unicode values");
    let logging = hl_log::EnvironmentConfig::parse(hl_log::Config::default(), environment.logging());
    for warning in logging.warnings() {
        eprintln!("hl-engine: {warning}");
    }
    logging.apply();
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CWorker {
    plan: i32,
    control: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CWorkerConfigurationError {
    MissingPlan,
    InvalidPlan,
    MissingControl,
    InvalidControl,
    AliasedDescriptors,
}

impl CWorker {
    fn capture(arguments: &[String]) -> Option<Result<Self, CWorkerConfigurationError>> {
        Self::parse(arguments, |name| std::env::var_os(name))
    }

    fn parse(
        arguments: &[String],
        environment: impl Fn(&str) -> Option<OsString>,
    ) -> Option<Result<Self, CWorkerConfigurationError>> {
        if arguments.get(1).map(String::as_str) != Some(C_WORKER_ARGUMENT) {
            return None;
        }
        let plan = match environment(C_PLAN_DESCRIPTOR) {
            Some(value) => match descriptor(&value) {
                Some(value) => value,
                None => return Some(Err(CWorkerConfigurationError::InvalidPlan)),
            },
            None => return Some(Err(CWorkerConfigurationError::MissingPlan)),
        };
        let control = match environment(C_CONTROL_DESCRIPTOR) {
            Some(value) => match descriptor(&value) {
                Some(value) => value,
                None => return Some(Err(CWorkerConfigurationError::InvalidControl)),
            },
            None => return Some(Err(CWorkerConfigurationError::MissingControl)),
        };
        Some(if plan == control {
            Err(CWorkerConfigurationError::AliasedDescriptors)
        } else {
            Ok(Self { plan, control })
        })
    }
}

fn descriptor(value: &OsStr) -> Option<i32> {
    let value = value.to_str()?;
    (!value.is_empty() && value.len() <= 10 && value.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| value.parse::<i32>().ok())
        .flatten()
        .filter(|value| *value >= 3)
}

impl CWorkerConfigurationError {
    const fn status(self) -> i32 {
        64
    }
}

impl std::fmt::Display for CWorkerConfigurationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::MissingPlan => "HL_C_PLAN_FD is missing",
            Self::InvalidPlan => "HL_C_PLAN_FD must be a decimal descriptor of 3 or greater",
            Self::MissingControl => "HL_C_CONTROL_FD is missing",
            Self::InvalidControl => "HL_C_CONTROL_FD must be a decimal descriptor of 3 or greater",
            Self::AliasedDescriptors => "HL_C_PLAN_FD and HL_C_CONTROL_FD must be different",
        })
    }
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
    use super::{CWorker, CWorkerConfigurationError, Environment, ExitReport, descriptor};
    use clap::Parser;
    use std::ffi::{OsStr, OsString};

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

    #[test]
    fn c_worker_is_hidden_exactly_at_first_argument() {
        let environment = |name: &str| match name {
            "HL_C_PLAN_FD" => Some(OsString::from("7")),
            "HL_C_CONTROL_FD" => Some(OsString::from("8")),
            _ => None,
        };
        assert_eq!(
            CWorker::parse(&["hl-engine".into(), "--c-worker".into()], environment),
            Some(Ok(CWorker { plan: 7, control: 8 }))
        );
        assert_eq!(
            CWorker::parse(&["hl-engine".into(), "guest".into(), "--c-worker".into()], environment),
            None
        );
    }

    #[test]
    fn c_worker_requires_distinct_bounded_descriptors() {
        let arguments = ["hl-engine".into(), "--c-worker".into()];
        assert_eq!(
            CWorker::parse(&arguments, |_| None),
            Some(Err(CWorkerConfigurationError::MissingPlan))
        );
        assert_eq!(
            CWorker::parse(&arguments, |name| match name {
                "HL_C_PLAN_FD" => Some(OsString::from("2")),
                "HL_C_CONTROL_FD" => Some(OsString::from("4")),
                _ => None,
            }),
            Some(Err(CWorkerConfigurationError::InvalidPlan))
        );
        assert_eq!(
            CWorker::parse(&arguments, |_| Some(OsString::from("9"))),
            Some(Err(CWorkerConfigurationError::AliasedDescriptors))
        );
        for value in ["", "+3", "-3", "3 ", "0x3", "2147483648", "12345678901"] {
            assert_eq!(descriptor(OsStr::new(value)), None, "{value:?}");
        }
        assert_eq!(descriptor(OsStr::new("3")), Some(3));
        assert_eq!(descriptor(OsStr::new("2147483647")), Some(i32::MAX));
    }
}
