#![allow(
    dead_code,
    reason = "the standalone harness exercises analyzer internals"
)]

#[path = "harness/analyze.rs"]
mod analyze;
mod contract {
    use serde::Serialize;
    #[derive(Clone, Debug, PartialEq, Serialize)]
    pub enum Target {
        Arm64,
        Amd64,
    }
    impl Target {
        pub fn from_env() -> Result<Self, String> {
            match std::env::var("HL_SCENARIO_TARGET").as_deref() {
                Ok("amd64") => Ok(Self::Amd64),
                Ok("arm64") | Err(std::env::VarError::NotPresent) => Ok(Self::Arm64),
                Ok(value) => Err(format!("unsupported scenario target {value:?}")),
                Err(error) => Err(format!("invalid scenario target: {error}")),
            }
        }

        pub const fn name(&self) -> &'static str {
            match self {
                Self::Arm64 => "arm64",
                Self::Amd64 => "amd64",
            }
        }
    }
    #[derive(Clone, Debug, Serialize)]
    pub enum Step {
        Run(Vec<String>),
    }
    #[derive(Clone, Debug)]
    pub enum Check {
        Exit(i32),
    }
    pub struct Scenario {
        pub id: &'static str,
        pub image: &'static str,
        pub step: Step,
        pub timeout_seconds: u64,
        pub checks: Vec<Check>,
        pub expected_failures: Vec<Target>,
    }
    impl Scenario {
        pub fn new(id: &'static str, image: &'static str) -> Self {
            Self {
                id,
                image,
                step: Step::Run(Vec::new()),
                timeout_seconds: 120,
                checks: Vec::new(),
                expected_failures: Vec::new(),
            }
        }
    }
}
#[path = "scenarios/harness/report.rs"]
mod report;
