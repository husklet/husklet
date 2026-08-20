use hl_log::{Config, EnvironmentConfig, Level, Tags};

/// Apply Husklet's logging configuration at the composition boundary.
///
/// The base is every tag at `Error` and nothing above it. `Config::default()` is `Tags::NONE`, and the
/// tag mask gates `hl_error!` exactly as it gates `hl_trace!` -- so with the default an operation that
/// failed and named its reason produced no output at all. That is how a checkpoint refusal reached the
/// user as `CaptureRefused` with an empty log while the broker had recorded exactly why, and three lanes
/// in a row read the resulting silence as evidence about the engine. An error is never ordinary business
/// (see hl-log's level contract), so it is on by default and the environment only widens from here.
pub fn configure() {
    let base = Config {
        logging: Tags::ALL,
        level: Level::Error,
        ..Config::default()
    };
    let parsed = EnvironmentConfig::parse(base, std::env::vars());
    for warning in parsed.warnings() {
        eprintln!("husklet: {warning}");
    }
    parsed.apply();
}
