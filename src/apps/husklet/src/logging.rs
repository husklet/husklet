use hl_log::{Config, EnvironmentConfig};

/// Apply Husklet's logging configuration at the composition boundary.
pub fn configure() {
    let parsed = EnvironmentConfig::parse(Config::default(), std::env::vars());
    for warning in parsed.warnings() {
        eprintln!("husklet: {warning}");
    }
    parsed.apply();
}
