#![allow(
    dead_code,
    reason = "the standalone harness exercises analyzer internals"
)]

#[path = "scenarios/harness/analyze.rs"]
mod analyze;
mod contract {
    use serde::Serialize;
    #[derive(Clone, Debug, PartialEq, Serialize)]
    pub enum Target {
        Arm64,
        Amd64,
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
