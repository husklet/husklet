use super::{EngineHost, EnvironmentEntry, HostExclusion, ManifestPath, environment};
use crate::{
    runtime::scheduler,
    suite::{Commands, Error, Execution, SafePath as _, Target},
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
    #[serde(default)]
    pub(super) guest: Guest,
    pub(super) expect: Expectation,
}

/// Guest-side state a case needs before its binary runs, which the image alone does not supply.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Guest {
    #[serde(default)]
    files: Vec<GuestFile>,
    /// Host files a dynamically linked case needs inside the image, such as its `PT_INTERP` loader.
    #[serde(default)]
    libraries: Vec<GuestLibrary>,
    cwd: Option<String>,
}

/// A host file copied into the case root filesystem at mode 0755, with both sides chosen per target
/// because a cross toolchain names its loader and sysroot differently for each guest ISA.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GuestLibrary {
    host: Commands,
    guest: Commands,
}

impl GuestLibrary {
    pub(crate) fn host(&self, target: Target) -> &str {
        self.host.for_target(target)
    }

    pub(crate) fn guest(&self, target: Target) -> &str {
        self.guest.for_target(target)
    }
}

/// A regular file staged into the case root filesystem at mode 0600, filled with one repeated byte.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GuestFile {
    path: String,
    size: u64,
    fill: u8,
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
    /// Declared stderr line patterns; an absent list keeps the default that stderr must be empty.
    #[serde(default)]
    pub(super) stderr: Vec<String>,
}

/// Guest files are bounded so a manifest typo cannot ask the harness to write an unbounded fixture.
const GUEST_FILE_LIMIT: u64 = 64 * 1024 * 1024;

impl GuestFile {
    pub(crate) fn path(&self) -> &str {
        &self.path
    }

    pub(crate) fn contents(&self) -> Vec<u8> {
        vec![self.fill; usize::try_from(self.size).unwrap_or(usize::MAX)]
    }
}

impl Guest {
    pub(super) fn validate(self) -> Result<(Vec<GuestFile>, Vec<GuestLibrary>, Option<String>), Error> {
        let mut seen = BTreeSet::new();
        for library in &self.libraries {
            for target in [Target::Arm64, Target::Amd64] {
                std::path::Path::new(library.guest(target)).safe_absolute()?;
                if !seen.insert(library.guest(target).to_owned()) {
                    return Err(format!("guest library {:?} is declared twice", library.guest(target)).into());
                }
            }
        }
        for file in &self.files {
            std::path::Path::new(&file.path).safe_absolute()?;
            if !(1..=GUEST_FILE_LIMIT).contains(&file.size) {
                return Err(format!("guest file {:?} has an out-of-range size", file.path).into());
            }
            if !seen.insert(file.path.clone()) {
                return Err(format!("guest file {:?} is declared twice", file.path).into());
            }
        }
        if let Some(cwd) = &self.cwd {
            std::path::Path::new(cwd).safe_absolute()?;
        }
        Ok((self.files, self.libraries, self.cwd))
    }
}

/// A pattern that matched nothing is a stale declaration, so patterns are validated as text here and
/// enforced as a two-way match at run time.
pub(super) fn stderr_patterns(patterns: Vec<String>) -> Result<Vec<String>, Error> {
    if patterns.iter().any(|pattern| pattern.trim().is_empty()) {
        return Err("an expected stderr pattern is empty".into());
    }
    Ok(patterns)
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
