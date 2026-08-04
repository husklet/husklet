#![forbid(unsafe_code)]

mod bench;
mod runtime;
mod scenario;

#[tokio::main]
async fn main() {
    hl_log::Config {
        logging: hl_log::tag::EXEC.into(),
        level: hl_log::Level::Error,
        profiling: hl_log::Tags::NONE,
    }
    .apply();
    if let Err(error) = run().await {
        eprintln!("testing: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    match arguments.first().map(String::as_str) {
        Some("runtime") => runtime::run(&arguments[1..]).await,
        Some("oracle") => runtime::oracle(&arguments[1..]),
        Some("scenarios") => scenario::run(&arguments[1..]).await,
        Some("bench") => bench::run(&arguments[1..]).await,
        Some(command) => Err(format!("unknown testing command {command:?}").into()),
        None => Err("usage: testing <runtime|oracle|scenarios|bench> [options]".into()),
    }
}
