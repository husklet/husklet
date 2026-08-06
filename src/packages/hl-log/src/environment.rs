use std::fmt::{self, Display, Formatter};

use crate::{Config, Level, TagList};

/// Stable environment variable names understood by process composition roots.
pub const LOG_TAGS: &str = "HL_LOG";
pub const LOG_LEVEL: &str = "HL_LOG_LEVEL";
pub const PROFILE_TAGS: &str = "HL_LOG_COUNTERS";

/// A non-fatal configuration problem retained for the composition root to report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigurationWarning {
    variable: &'static str,
    value: String,
    names: Vec<String>,
}

impl Display for ConfigurationWarning {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}={:?} ignored unknown {}: {}",
            self.variable,
            self.value,
            if self.names.len() == 1 { "name" } else { "names" },
            self.names.join(", ")
        )
    }
}

/// Parsed logging configuration and any forward-compatible parsing warnings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnvironmentConfig {
    config: Config,
    warnings: Vec<ConfigurationWarning>,
}

impl EnvironmentConfig {
    /// Parse an explicitly captured environment, preserving `defaults` for absent variables.
    ///
    /// Unknown names do not prevent startup: callers can report [`Self::warnings`] before
    /// applying the usable portion. Capturing the ambient environment remains the composition
    /// root's responsibility.
    pub fn parse<I, K, V>(defaults: Config, variables: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let mut config = defaults;
        let mut warnings = Vec::new();
        for (name, value) in variables {
            let name = name.as_ref();
            let value = value.as_ref();
            match name {
                LOG_TAGS => config.logging = parse_tags(LOG_TAGS, value, &mut warnings),
                PROFILE_TAGS => config.profiling = parse_tags(PROFILE_TAGS, value, &mut warnings),
                LOG_LEVEL => match Level::from_name(value) {
                    Some(level) => config.level = level,
                    None => warnings.push(ConfigurationWarning {
                        variable: LOG_LEVEL,
                        value: value.to_owned(),
                        names: vec![value.to_owned()],
                    }),
                },
                _ => {}
            }
        }
        Self { config, warnings }
    }

    #[must_use]
    pub const fn config(&self) -> Config {
        self.config
    }

    #[must_use]
    pub fn warnings(&self) -> &[ConfigurationWarning] {
        &self.warnings
    }

    pub fn apply(self) {
        self.config.apply();
    }
}

fn parse_tags(variable: &'static str, value: &str, warnings: &mut Vec<ConfigurationWarning>) -> crate::Tags {
    let list = TagList::from(value);
    if !list.unrecognised().is_empty() {
        warnings.push(ConfigurationWarning {
            variable,
            value: value.to_owned(),
            names: list.unrecognised().iter().map(|name| (*name).to_owned()).collect(),
        });
    }
    list.tags()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{tag, Tags};

    #[test]
    fn absent_values_preserve_intentional_defaults() {
        let defaults = Config {
            logging: tag::EXEC.into(),
            level: Level::Error,
            profiling: Tags::NONE,
        };
        assert_eq!(
            EnvironmentConfig::parse(defaults, [] as [(&str, &str); 0]).config(),
            defaults
        );
    }

    #[test]
    fn parses_all_three_controls() {
        let parsed = EnvironmentConfig::parse(
            Config::default(),
            [
                (LOG_TAGS, "daemon,container"),
                (LOG_LEVEL, "debug"),
                (PROFILE_TAGS, "image"),
            ],
        );
        assert_eq!(parsed.config().logging, tag::DAEMON | tag::CONTAINER);
        assert_eq!(parsed.config().level, Level::Debug);
        assert_eq!(parsed.config().profiling, tag::IMAGE.into());
        assert!(parsed.warnings().is_empty());
    }

    #[test]
    fn unknown_tag_is_observable_without_discarding_known_tags() {
        let parsed = EnvironmentConfig::parse(Config::default(), [(LOG_TAGS, "daemon,daemno")]);
        assert_eq!(parsed.config().logging, tag::DAEMON.into());
        assert_eq!(parsed.warnings().len(), 1);
        assert!(parsed.warnings()[0].to_string().contains("daemno"));
    }
}
