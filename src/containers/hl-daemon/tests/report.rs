#![allow(dead_code, reason = "standalone harness stubs the scenario contract")]

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

#[test]
fn every_contract_has_stable_report_metadata() {
    let mut ids = std::collections::BTreeSet::new();
    let mut categories = std::collections::BTreeSet::new();
    for line in include_str!("scenarios/golden/legacy.contracts").lines() {
        let value: serde_json::Value = serde_json::from_str(line).unwrap();
        let id = value["id"].as_str().unwrap().to_owned();
        assert!(ids.insert(id.clone()), "duplicate report key {id}");
        categories.insert(id.split('/').next().unwrap().to_owned());
        assert!(!value["image"].as_str().unwrap().is_empty());
        assert!(!value["targets"].as_array().unwrap().is_empty());
        assert!(value["timeout"].as_u64().unwrap() > 0);
        assert!(value.get("step").is_some());
        assert!(value.get("checks").is_some());
    }
    // The catalog only grows; guard against truncation/regression rather than pinning an exact
    // count (a hard-coded total breaks on every legitimate coverage addition).
    assert!(
        ids.len() >= 607,
        "contract catalog shrank below the established floor: {} < 607",
        ids.len()
    );
    assert!(
        categories.len() >= 25,
        "category count shrank below the established floor: {} < 25",
        categories.len()
    );
}
