fn main() {
    let logging = hl_log::EnvironmentConfig::parse(hl_log::Config::default(), std::env::vars());
    for warning in logging.warnings() {
        eprintln!("hl-x86_64: {warning}");
    }
    logging.apply();
    Worker::run();
}

struct Worker;

impl Worker {
    fn run() {
        hl_log::hl_info!(hl_log::tag::EXEC, "engine process starting isa=x86_64");
        hl_log::hl_event!(
            hl_log::tag::EXEC,
            hl_log::Level::Info,
            "engine.process.starting",
            isa = "x86_64"
        );
        let arguments = std::env::args().collect::<Vec<_>>();
        let mut environment = hl_engine::environment::BootstrapEnvironment::capture(std::env::vars());
        let authority = match environment.take_authority_descriptor() {
            hl_engine::environment::AuthorityDescriptor::Present(value) => i32::try_from(value).ok(),
            _ => None,
        };
        let health = match environment.take_authority_health() {
            hl_engine::environment::AuthorityDescriptor::Present(value) => i32::try_from(value).ok(),
            _ => None,
        };
        let result = hl_engine::program::Program::run_authorized(arguments, authority, health);
        if let Err(error) = result {
            hl_log::hl_error!(hl_log::tag::EXEC, "engine process failed isa=x86_64 reason={error:?}");
            hl_log::hl_event!(
                hl_log::tag::EXEC,
                hl_log::Level::Error,
                "engine.process.failed",
                isa = "x86_64",
                reason = ?error
            );
            if std::env::var_os("RUST_BACKTRACE").is_some() {
                eprintln!("{error:?}");
            }
        }
        let status = result.map_or_else(
            hl_engine::program::ProgramError::status,
            hl_engine::program::Program::exit_status,
        );
        if result.is_ok() {
            hl_log::hl_info!(hl_log::tag::EXEC, "engine process exited isa=x86_64 status={status}");
            hl_log::hl_event!(
                hl_log::tag::EXEC,
                hl_log::Level::Info,
                "engine.process.exited",
                isa = "x86_64",
                status = status
            );
        }
        std::process::exit(status);
    }
}
