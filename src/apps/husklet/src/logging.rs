use hl_log::{Config, Level, TagList, Tags};

/// Apply Husklet's logging configuration at the composition boundary.
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
            logging: tags("HL_LOG"),
            level: std::env::var("HL_LOG_LEVEL")
                .ok()
                .and_then(|value| Level::from_name(&value))
                .unwrap_or(Level::Warn),
            profiling: tags("HL_LOG_COUNTERS"),
        }
    }
}

/// Read a tag-list variable, and say so when it was set to something that opens nothing.
///
/// `Tags::from_str` ignores names it does not recognise, deliberately, so a typo or a LEVEL name written
/// into the TAG variable yields an empty mask that is indistinguishable from never having asked. That has
/// cost this project twice: a conformance harness reported a silent subject while its reasons sat behind
/// the mask, and `apps/gl-diff` shipped `HL_LOG=debug` — not a tag — beside a comment claiming the gate
/// was open, so a context loss there reported no reason for as long as the setting existed.
///
/// The warning ANNOUNCES rather than refuses: an older binary must still tolerate a newer configuration.
///
/// It is a plain `eprintln!` and must stay one. A diagnostic about the tag mask cannot be gated by the
/// tag mask — routing it through `hl_error!` would suppress it in exactly the case it exists to report,
/// which is the misconfiguration itself. It is also emitted before `Config::apply`, so there is no sink
/// to route it to yet.
fn tags(variable: &str) -> Tags {
    let Ok(value) = std::env::var(variable) else {
        return Tags::NONE;
    };
    let list = TagList::from(value.as_str());
    if list.tags().bits() == 0 {
        if !list.unrecognised().is_empty() {
            eprintln!(
                "husklet: {variable}={value:?} opened no logging: {} is not a tag name. \
                 Tag lists go in {variable}; a severity goes in {variable}_LEVEL.",
                list.unrecognised().join(", ")
            );
        }
    }
    list.tags()
}
