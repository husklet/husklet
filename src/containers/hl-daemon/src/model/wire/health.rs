use serde::{Deserialize, Serialize};

/// Image `HEALTHCHECK` / `docker run --health-*` (Config.Healthcheck). Durations are nanoseconds (Go
/// `time.Duration`, exactly how the OCI config + docker API encode them). `test` is docker's form:
/// `["NONE"]` disables, `["CMD", argv…]` execs directly, `["CMD-SHELL", script]` runs via `/bin/sh -c`.
/// Serde-renamed so it deserializes from the create body AND round-trips through inspect Config.Healthcheck.
#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct HealthConfig {
    #[serde(rename = "Test", default)]
    pub(crate) test: Vec<String>,
    #[serde(rename = "Interval", default)]
    pub(crate) interval: i64, // ns between probes (0 ⇒ 30s)
    #[serde(rename = "Timeout", default)]
    pub(crate) timeout: i64, // ns a probe may run (0 ⇒ 30s)
    #[serde(rename = "Retries", default)]
    pub(crate) retries: i64, // consecutive failures ⇒ unhealthy (0 ⇒ 3)
    #[serde(rename = "StartPeriod", default)]
    pub(crate) start_period: i64, // ns grace where a failure doesn't count
}

/// A container's live health, surfaced as inspect `State.Health`. `status` is starting/healthy/unhealthy;
/// `failing_streak` is the current run of consecutive failing probes; `log` keeps the most recent probe
/// results (docker caps this at 5). Mirrors Moby `container.Health`.
#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct HealthState {
    #[serde(rename = "Status", default)]
    pub(crate) status: String,
    #[serde(rename = "FailingStreak", default)]
    pub(crate) failing_streak: i64,
    #[serde(rename = "Log", default)]
    pub(crate) log: Vec<HealthLog>,
}

/// One probe result in `State.Health.Log[]` (RFC3339 start/end, the probe's exit code + captured output).
#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct HealthLog {
    #[serde(rename = "Start", default)]
    pub(crate) start: String,
    #[serde(rename = "End", default)]
    pub(crate) end: String,
    #[serde(rename = "ExitCode", default)]
    pub(crate) exit_code: i64,
    #[serde(rename = "Output", default)]
    pub(crate) output: String,
}
