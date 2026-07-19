use hl_log::{Config, Level, Tags};

/// Apply Husklet's environment compatibility at the composition boundary.
pub fn configure() {
    let environment = Environment::read();
    Config {
        logging: environment.logging,
        level: environment.level,
        profiling: environment.profiling,
    }
    .apply();
}

struct Environment {
    logging: Tags,
    level: Level,
    profiling: Tags,
}

impl Environment {
    fn read() -> Self {
        Self {
            logging: std::env::var("HL_LOG")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(Tags::NONE),
            level: std::env::var("HL_LOG_LEVEL")
                .ok()
                .and_then(|value| Level::from_name(&value))
                .unwrap_or(Level::Warn),
            profiling: std::env::var("HL_LOG_COUNTERS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(Tags::NONE),
        }
    }
}
