use super::{EngineHost, EnvironmentEntry, HostExclusion, ManifestPath, environment};
use crate::{
    runtime::scheduler,
    suite::{Commands, Error, Execution, Target},
};
use serde::Deserialize;
use std::{collections::BTreeSet, path::PathBuf};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Document {
    pub(super) targets: BTreeSet<Target>,
    pub(super) image: String,
    #[serde(default)]
    pub(super) execution: Execution,
    pub(super) artifact: Option<Artifact>,
    pub(super) build: Build,
    pub(super) oracle: Option<Oracle>,
    pub(super) cases: Vec<Case>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Oracle {
    pub(super) provider: OracleProvider,
    pub(super) commands: Commands,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum OracleProvider {
    Native,
    Qemu,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Artifact {
    destination: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Build {
    source: Option<ManifestPath>,
    output: Option<String>,
    pub(super) compiler: Commands,
    #[serde(default)]
    flags: Vec<String>,
    #[serde(default)]
    inputs: Vec<ManifestPath>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CaseBuild {
    source: ManifestPath,
    output: String,
    flags: Vec<String>,
    #[serde(default)]
    inputs: Vec<ManifestPath>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Case {
    pub(super) id: String,
    pub(super) build: Option<CaseBuild>,
    pub(super) artifact: Option<Artifact>,
    #[serde(default)]
    pub(super) targets: BTreeSet<Target>,
    pub(super) status: Status,
    pub(super) compat: Compat,
    pub(super) soak: Option<scheduler::Plan>,
    pub(super) run: Vec<String>,
    #[serde(default)]
    #[serde(deserialize_with = "environment::environment")]
    pub(super) environment: Vec<EnvironmentEntry>,
    #[serde(default = "timeout")]
    pub(super) timeout: u64,
    pub(super) expect: Expectation,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Status {
    Active,
    Broken(Evidence),
    Unsupported(Evidence),
    HostExcluded(HostExclusion),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Evidence {
    reason: String,
    evidence: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Compat {
    pub(super) class: CompatClass,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CompatClass {
    Smoke,
    Compatibility,
    Soak,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Expectation {
    pub(super) exit: i32,
    pub(super) stdout: PathBuf,
}

impl Build {
    pub(super) fn resolve(
        &self,
        case: Option<CaseBuild>,
        artifact: Option<Artifact>,
        default_artifact: Option<&Artifact>,
    ) -> Result<(ManifestPath, String, Vec<String>, Vec<ManifestPath>, String), Error> {
        match (case, artifact) {
            (Some(build), Some(artifact)) => Ok((
                build.source,
                build.output,
                build.flags,
                build.inputs,
                artifact.destination,
            )),
            (None, None) => Ok((
                self.source.clone().ok_or("document build has no default source")?,
                self.output.clone().ok_or("document build has no default output")?,
                self.flags.clone(),
                self.inputs.clone(),
                default_artifact
                    .ok_or("document defines no default artifact")?
                    .destination
                    .clone(),
            )),
            _ => Err("case build and artifact must be declared together".into()),
        }
    }
}

impl Status {
    pub(super) fn validate(&self) -> Result<(), Error> {
        if let Self::Broken(evidence) | Self::Unsupported(evidence) = self
            && (evidence.reason.trim().is_empty() || evidence.evidence.trim().is_empty())
        {
            return Err("non-active status requires non-empty reason and evidence".into());
        }
        if let Self::HostExcluded(exclusion) = self {
            exclusion.validate()?;
        }
        Ok(())
    }

    pub(super) fn inactive(&self, host: EngineHost) -> Option<(&'static str, &str, &str)> {
        match self {
            Self::Active => None,
            Self::Broken(evidence) => Some(("BROKEN", &evidence.reason, &evidence.evidence)),
            Self::Unsupported(evidence) => Some(("UNSUPPORTED", &evidence.reason, &evidence.evidence)),
            Self::HostExcluded(exclusion) => exclusion.inactive(host),
        }
    }

    pub(super) fn oracle_inactive(&self) -> Option<(&'static str, &str, &str)> {
        match self {
            Self::Active | Self::HostExcluded(_) => None,
            Self::Broken(evidence) => Some(("BROKEN", &evidence.reason, &evidence.evidence)),
            Self::Unsupported(evidence) => Some(("UNSUPPORTED", &evidence.reason, &evidence.evidence)),
        }
    }
}

const fn timeout() -> u64 {
    30
}
