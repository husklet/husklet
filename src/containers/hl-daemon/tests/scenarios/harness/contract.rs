//! Typed scenario contracts and migration registry.

use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, time::Duration};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    Arm64,
    Amd64,
}

impl Target {
    pub const LINUX: [Self; 2] = [Self::Arm64, Self::Amd64];

    pub fn from_env() -> Result<Self, String> {
        match std::env::var("HL_SCENARIO_TARGET").as_deref() {
            Ok("amd64") => Ok(Self::Amd64),
            Ok("arm64") | Err(std::env::VarError::NotPresent) => Ok(Self::Arm64),
            Ok(value) => Err(format!("unsupported scenario target {value:?}")),
            Err(error) => Err(format!("invalid scenario target: {error}")),
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Arm64 => "arm64",
            Self::Amd64 => "amd64",
        }
    }

    pub fn platform(self) -> hl_images::Platform {
        match self {
            Self::Arm64 => hl_images::Platform::linux_arm64(),
            Self::Amd64 => hl_images::Platform::linux_amd64(),
        }
    }

    pub const fn guest(self) -> hl_container::Guest {
        match self {
            Self::Arm64 => hl_container::Guest::Aarch64,
            Self::Amd64 => hl_container::Guest::X86_64,
        }
    }
}

pub(crate) fn test_target_routing() {
    assert_eq!(Target::Arm64.platform(), hl_images::Platform::linux_arm64());
    assert_eq!(Target::Amd64.platform(), hl_images::Platform::linux_amd64());
    assert_eq!(Target::Arm64.guest(), hl_container::Guest::Aarch64);
    assert_eq!(Target::Amd64.guest(), hl_container::Guest::X86_64);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Class {
    Quick,
    Long,
}

/// Host-side resources that bound safe scenario concurrency.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Resource {
    DiskHeavy,
    Registry,
    Network,
    Pty,
    HostPort,
    ImageMutation,
    ProcessHeavy,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Step {
    Run(Vec<String>),
    Exec(String),
    Host(String),
    Api(String),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Check {
    Contains(String),
    Equals(String),
    Exit(i32),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Service {
    pub startup: String,
    pub probe: String,
    pub attempts: u32,
    pub delay_ms: u64,
    pub logs: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Scenario {
    pub id: &'static str,
    pub image: &'static str,
    pub step: Step,
    pub targets: Vec<Target>,
    pub class: Class,
    pub checks: Vec<Check>,
    pub expected_failures: Vec<Target>,
    pub timeout_seconds: u64,
    pub terminal: bool,
    pub environment: BTreeMap<String, String>,
    pub resources: Vec<Resource>,
    pub service: Option<Service>,
}

#[allow(
    dead_code,
    reason = "builders are consumed as scenario groups transfer"
)]
impl Scenario {
    pub fn new(id: &'static str, image: &'static str) -> Self {
        Self {
            id,
            image,
            step: Step::Run(Vec::new()),
            targets: Target::LINUX.to_vec(),
            class: Class::Quick,
            checks: Vec::new(),
            expected_failures: Vec::new(),
            timeout_seconds: 120,
            terminal: false,
            environment: BTreeMap::new(),
            resources: Vec::new(),
            service: None,
        }
    }

    pub fn resource(mut self, resource: Resource) -> Self {
        if !self.resources.contains(&resource) {
            self.resources.push(resource);
        }
        self
    }

    pub fn run(mut self, arguments: &[&str]) -> Self {
        self.step = Step::Run(arguments.iter().map(ToString::to_string).collect());
        self
    }

    pub fn exec(mut self, command: &str) -> Self {
        self.step = Step::Exec(command.into());
        self
    }

    pub fn host(mut self, command: &str) -> Self {
        self.step = Step::Host(command.into());
        self
    }

    pub fn api(mut self, operation: &str) -> Self {
        self.step = Step::Api(operation.into());
        self
    }

    pub fn contains(mut self, value: &str) -> Self {
        self.checks.push(Check::Contains(value.into()));
        self
    }

    pub fn equals(mut self, value: &str) -> Self {
        self.checks.push(Check::Equals(value.into()));
        self
    }

    pub fn exit(mut self, value: i32) -> Self {
        self.checks.push(Check::Exit(value));
        self
    }

    pub fn long(mut self) -> Self {
        self.class = Class::Long;
        self
    }

    pub fn only(mut self, targets: &[Target]) -> Self {
        self.targets = targets.to_vec();
        self
    }

    pub fn xfail(mut self, targets: &[Target]) -> Self {
        self.expected_failures = targets.to_vec();
        self
    }

    pub fn timeout(mut self, seconds: u64) -> Self {
        self.timeout_seconds = seconds;
        self
    }

    pub fn operation_timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_seconds)
    }

    pub fn terminal(mut self) -> Self {
        self.terminal = true;
        self.resources.push(Resource::Pty);
        self
    }

    pub fn env(mut self, key: &str, value: &str) -> Self {
        self.environment.insert(key.into(), value.into());
        self
    }

    pub fn service(
        mut self,
        startup: &str,
        probe: &str,
        attempts: u32,
        delay_ms: u64,
        logs: &[&str],
    ) -> Self {
        self.service = Some(Service {
            startup: startup.into(),
            probe: probe.into(),
            attempts,
            delay_ms,
            logs: logs.iter().map(|path| (*path).into()).collect(),
        });
        self
    }
}

pub struct Group {
    pub name: &'static str,
    pub scenarios: Vec<Scenario>,
}

impl Group {
    pub fn new(name: &'static str, scenarios: Vec<Scenario>) -> Self {
        Self { name, scenarios }
    }
}

#[derive(Default)]
pub struct Registry {
    scenarios: BTreeMap<&'static str, Scenario>,
}

impl Registry {
    pub fn add(&mut self, group: Group) {
        for scenario in group.scenarios {
            assert!(
                self.scenarios.insert(scenario.id, scenario).is_none(),
                "duplicate scenario ID in group {:?}",
                group.name,
            );
        }
    }

    pub fn scenarios(&self) -> impl Iterator<Item = &Scenario> {
        self.scenarios.values()
    }

    pub fn snapshot(&self) -> String {
        let mut output = String::new();
        for scenario in self.scenarios.values() {
            output.push_str(&serde_json::to_string(&Contract::from(scenario)).unwrap());
            output.push('\n');
        }
        output
    }

    pub fn verify(&self, expected: &str) -> Result<(), String> {
        let actual = self.snapshot();
        if actual == expected {
            return Ok(());
        }
        let line = actual
            .lines()
            .zip(expected.lines())
            .position(|(actual, expected)| actual != expected)
            .unwrap_or_else(|| actual.lines().count().min(expected.lines().count()));
        Err(format!(
            "scenario contract drift at line {}\nexpected: {}\nactual:   {}",
            line + 1,
            expected.lines().nth(line).unwrap_or("<missing>"),
            actual.lines().nth(line).unwrap_or("<missing>")
        ))
    }
}

#[derive(Serialize)]
struct Contract<'a> {
    id: &'a str,
    image: &'a str,
    step: &'a Step,
    targets: &'a [Target],
    class: Class,
    checks: &'a [Check],
    xfail: &'a [Target],
    timeout: u64,
    terminal: bool,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    environment: &'a BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    service: &'a Option<Service>,
}

impl<'a> From<&'a Scenario> for Contract<'a> {
    fn from(scenario: &'a Scenario) -> Self {
        Self {
            id: scenario.id,
            image: scenario.image,
            step: &scenario.step,
            targets: &scenario.targets,
            class: scenario.class,
            checks: &scenario.checks,
            xfail: &scenario.expected_failures,
            timeout: scenario.timeout_seconds,
            terminal: scenario.terminal,
            environment: &scenario.environment,
            service: &scenario.service,
        }
    }
}

pub fn test_firewall() -> Result<(), String> {
    fn fixture() -> Scenario {
        Scenario::new("firewall/case", "alpine:3.20")
            .run(&["echo", "hello"])
            .contains("hello")
            .only(&[Target::Arm64])
            .xfail(&[Target::Amd64])
            .timeout(7)
            .terminal()
    }
    fn registry(scenario: Scenario) -> Registry {
        let mut registry = Registry::default();
        registry.add(Group::new("firewall", vec![scenario]));
        registry
    }

    let expected = registry(fixture()).snapshot();
    registry(fixture()).verify(&expected)?;
    let mut variants = Vec::new();
    let mut value = fixture();
    value.image = "alpine:3.21";
    variants.push(value);
    let mut value = fixture();
    value.step = Step::Run(vec!["echo".into(), "changed".into()]);
    variants.push(value);
    let mut value = fixture();
    value.step = Step::Exec("echo hello".into());
    variants.push(value);
    let mut value = fixture();
    value.targets = vec![Target::Amd64];
    variants.push(value);
    let mut value = fixture();
    value.class = Class::Long;
    variants.push(value);
    let mut value = fixture();
    value.checks = vec![Check::Equals("hello".into())];
    variants.push(value);
    let mut value = fixture();
    value.expected_failures.clear();
    variants.push(value);
    let mut value = fixture();
    value.timeout_seconds = 8;
    variants.push(value);
    let mut value = fixture();
    value.terminal = false;
    variants.push(value);
    let mut value = fixture();
    value.environment.insert("TERM".into(), "screen".into());
    variants.push(value);
    if variants
        .into_iter()
        .all(|variant| registry(variant).verify(&expected).is_err())
    {
        Ok(())
    } else {
        Err("scenario firewall accepted behavioral drift".into())
    }
}
