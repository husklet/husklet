use crate::{Level, Logging, Profiling, Tags};

/// Complete process-wide logging and profiling configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Config {
    pub logging: Tags,
    pub level: Level,
    pub profiling: Tags,
}

impl Config {
    /// Apply this configuration atomically per setting.
    pub fn apply(self) {
        Logging::global().set(self.logging);
        Logging::global().set_level(self.level);
        Profiling::global().set(self.profiling);
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            logging: Tags::NONE,
            level: Level::Warn,
            profiling: Tags::NONE,
        }
    }
}
