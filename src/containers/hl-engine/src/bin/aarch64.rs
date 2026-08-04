fn main() {
    Worker::run();
}

struct Worker;

impl Worker {
    fn run() {
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
        let status = hl_engine::program::Program::run_authorized(arguments, authority, health).map_or_else(
            hl_engine::program::ProgramError::status,
            hl_engine::program::Program::exit_status,
        );
        std::process::exit(status);
    }
}
