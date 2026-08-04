fn main() {
    let logging = hl_log::EnvironmentConfig::parse(hl_log::Config::default(), std::env::vars());
    for warning in logging.warnings() {
        eprintln!("hl-engine: {warning}");
    }
    logging.apply();
    let mut arguments = std::env::args().collect::<Vec<_>>();
    let mut environment = hl_engine::environment::BootstrapEnvironment::capture(std::env::vars());
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

struct ExitReport;

impl ExitReport {
    fn isa(arguments: &[String]) -> String {
        arguments
            .windows(2)
            .find(|pair| pair[0] == "--guest-isa")
            .map_or_else(|| "unknown".into(), |pair| pair[1].clone())
    }

    fn error(isa: &str, error: hl_engine::program::ProgramError) {
        eprintln!("[hl-exit]\tError\t0\t{isa}\t0x0\t-\t{error:?}");
    }

    fn write(exit: hl_engine::engine::EngineExit) {
        let Some(fault) = exit.fault else {
            eprintln!("[hl-exit]\t{:?}\t{}\t{:#x}", exit.kind, exit.guest_status, exit.detail);
            return;
        };
        let opcode = fault.opcode[..usize::from(fault.opcode_len)]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
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
