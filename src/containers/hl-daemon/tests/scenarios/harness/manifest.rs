//! Validated declarative compatibility-case manifests.

use crate::contract::{Check, Class, Resource, Scenario, Service, Step, Target};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

type Error = Box<dyn std::error::Error>;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    cases: Vec<Case>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    image: String,
    #[serde(default)]
    platform: Vec<Platform>,
    #[serde(default = "default_timeout")]
    timeout: u64,
    #[serde(default)]
    class: ManifestClass,
    #[serde(default)]
    resources: Vec<Resource>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    xfail: Vec<Platform>,
    #[serde(default)]
    service: Option<ServiceSpec>,
    run: Run,
    expect: Expect,
}

/// Readiness probe for stateful scenarios, mirroring `Scenario::service`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ServiceSpec {
    startup: String,
    probe: String,
    attempts: u32,
    delay_ms: u64,
    #[serde(default)]
    logs: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Platform {
    Arm64,
    Amd64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ManifestClass {
    #[default]
    Quick,
    Long,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Run {
    Shell { shell: String },
    Argv { argv: Vec<String> },
    // `run: {entrypoint: true}` launches the image's own ENTRYPOINT/CMD, producing an
    // empty argv (`Step::Run(vec![])`). Distinct from a bare `argv: []`, which stays rejected.
    Default { entrypoint: bool },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Expect {
    #[serde(default)]
    exit: Option<i32>,
    #[serde(default)]
    stdout_contains: Vec<String>,
    stdout_exact: Option<String>,
}

const fn default_timeout() -> u64 {
    120
}

pub(crate) fn load(source: &'static str) -> Result<Vec<Scenario>, Error> {
    let document: Document = serde_yaml::from_str(source)?;
    if document.cases.is_empty() {
        return Err("manifest contains no cases".into());
    }
    let mut ids = BTreeSet::new();
    document
        .cases
        .into_iter()
        .map(|case| {
            validate(&case, &mut ids)?;
            let id = Box::leak(case.id.into_boxed_str());
            let image = Box::leak(case.image.into_boxed_str());
            let step = match case.run {
                Run::Shell { shell } => Step::Exec(shell),
                Run::Argv { argv } => Step::Run(argv),
                Run::Default { .. } => Step::Run(Vec::new()),
            };
            let targets = if case.platform.is_empty() {
                Target::LINUX.to_vec()
            } else {
                case.platform
                    .into_iter()
                    .map(|value| match value {
                        Platform::Arm64 => Target::Arm64,
                        Platform::Amd64 => Target::Amd64,
                    })
                    .collect()
            };
            let mut checks = case
                .expect
                .stdout_contains
                .into_iter()
                .map(Check::Contains)
                .collect::<Vec<_>>();
            if let Some(exact) = case.expect.stdout_exact {
                checks.push(Check::Equals(exact));
            }
            if let Some(exit) = case.expect.exit {
                checks.push(Check::Exit(exit));
            }
            Ok(Scenario {
                id,
                image,
                step,
                targets,
                class: match case.class {
                    ManifestClass::Quick => Class::Quick,
                    ManifestClass::Long => Class::Long,
                },
                checks,
                expected_failures: case
                    .xfail
                    .into_iter()
                    .map(|value| match value {
                        Platform::Arm64 => Target::Arm64,
                        Platform::Amd64 => Target::Amd64,
                    })
                    .collect(),
                timeout_seconds: case.timeout,
                terminal: case
                    .resources
                    .iter()
                    .any(|value| matches!(value, Resource::Pty)),
                environment: case.env,
                resources: case.resources,
                service: case.service.map(|spec| Service {
                    startup: spec.startup,
                    probe: spec.probe,
                    attempts: spec.attempts,
                    delay_ms: spec.delay_ms,
                    logs: spec.logs,
                }),
            })
        })
        .collect()
}

fn validate(case: &Case, ids: &mut BTreeSet<String>) -> Result<(), Error> {
    if !ids.insert(case.id.clone()) {
        return Err(format!("duplicate scenario id {:?}", case.id).into());
    }
    if !case.id.contains('/') {
        return Err(format!("scenario id {:?} lacks a domain", case.id).into());
    }
    if case.image.trim().is_empty() {
        return Err(format!("scenario {} has no image", case.id).into());
    }
    if !(1..=3600).contains(&case.timeout) {
        return Err(format!("scenario {} timeout is outside 1..=3600", case.id).into());
    }
    match &case.run {
        Run::Shell { shell } if shell.trim().is_empty() => {
            return Err(format!("scenario {} has an empty shell program", case.id).into());
        }
        Run::Argv { argv }
            if argv.first().is_none_or(String::is_empty)
                || argv.iter().any(|value| value.contains('\0')) =>
        {
            return Err(format!("scenario {} has invalid argv", case.id).into());
        }
        Run::Default { entrypoint } if !entrypoint => {
            return Err(format!("scenario {} must set entrypoint: true", case.id).into());
        }
        _ => {}
    }
    if let Some(service) = &case.service {
        if service.startup.trim().is_empty() {
            return Err(format!("scenario {} has an empty service startup", case.id).into());
        }
        if service.probe.trim().is_empty() {
            return Err(format!("scenario {} has an empty service probe", case.id).into());
        }
        if service.attempts == 0 {
            return Err(format!("scenario {} has zero service attempts", case.id).into());
        }
    }
    Ok(())
}

pub(crate) fn test_validation() -> Result<(), Error> {
    let valid = load(
        "cases:\n  - id: schema/ok\n    image: alpine\n    resources: [network, host_port]\n    run: {argv: [echo, ok]}\n    expect: {stdout_contains: [ok]}\n",
    )?;
    if valid[0].resources != [Resource::Network, Resource::HostPort] {
        return Err("manifest resources were not preserved".into());
    }
    let rich = load(
        "cases:\n  - id: schema/rich\n    image: alpine\n    env: {FOO: bar}\n    xfail: [amd64]\n    service: {startup: 'daemon --fork', probe: 'ping', attempts: 3, delay_ms: 500, logs: [/tmp/d.log]}\n    run: {entrypoint: true}\n    expect: {}\n",
    )?;
    if rich[0].environment.get("FOO").map(String::as_str) != Some("bar") {
        return Err("manifest env was not preserved".into());
    }
    if rich[0].expected_failures != [Target::Amd64] {
        return Err("manifest xfail was not preserved".into());
    }
    if rich[0].service.as_ref().map(|service| service.attempts) != Some(3) {
        return Err("manifest service was not preserved".into());
    }
    if rich[0].step != Step::Run(Vec::new()) {
        return Err("manifest default entrypoint was not preserved".into());
    }
    for invalid in [
        "cases: []\n",
        "cases:\n  - id: schema/x\n    image: alpine\n    mystery: true\n    run: {argv: [true]}\n    expect: {}\n",
        "cases:\n  - id: schema/x\n    image: alpine\n    run: {argv: [true]}\n    expect: {}\n  - id: schema/x\n    image: alpine\n    run: {argv: [true]}\n    expect: {}\n",
        "cases:\n  - id: schema/x\n    image: alpine\n    run: {argv: []}\n    expect: {}\n",
        "cases:\n  - id: schema/x\n    image: alpine\n    run: {argv: ['', arg]}\n    expect: {}\n",
        "cases:\n  - id: schema/x\n    image: alpine\n    run: {shell: '   '}\n    expect: {}\n",
        "cases:\n  - id: schema/x\n    image: alpine\n    service: {startup: '  ', probe: ping, attempts: 1, delay_ms: 1}\n    run: {argv: [true]}\n    expect: {}\n",
        "cases:\n  - id: schema/x\n    image: alpine\n    service: {startup: daemon, probe: ping, attempts: 0, delay_ms: 1}\n    run: {argv: [true]}\n    expect: {}\n",
    ] {
        if load(invalid).is_ok() {
            return Err(format!("invalid manifest was accepted: {invalid:?}").into());
        }
    }
    Ok(())
}
