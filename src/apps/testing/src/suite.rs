use clap::ValueEnum;
use serde::Deserialize;

pub(crate) type Error = Box<dyn std::error::Error>;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Target {
    Arm64,
    Amd64,
}

impl Target {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Arm64 => "arm64",
            Self::Amd64 => "amd64",
        }
    }

    /// The target a journal or manifest spelled, when it names one.
    pub(crate) fn named(name: &str) -> Option<Self> {
        match name {
            "arm64" => Some(Self::Arm64),
            "amd64" => Some(Self::Amd64),
            _ => None,
        }
    }

    pub(crate) const fn guest(self) -> hl_container::Guest {
        match self {
            Self::Arm64 => hl_container::Guest::Aarch64,
            Self::Amd64 => hl_container::Guest::X86_64,
        }
    }

    pub(crate) fn platform(self) -> hl_images::Platform {
        match self {
            Self::Arm64 => hl_images::Platform::linux_arm64(),
            Self::Amd64 => hl_images::Platform::linux_amd64(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Execution {
    #[serde(default)]
    native: bool,
    #[serde(default)]
    diagnostics: bool,
    #[serde(default)]
    retained_c: bool,
    #[serde(default)]
    retained_c_diagnostics: bool,
}

impl Execution {
    /// Whether the engine is asked for the counters a case can assert on.
    pub(crate) const fn emits_diagnostics(self) -> bool {
        self.native && self.diagnostics
    }

    pub(crate) fn container(self) -> Result<hl_container::Execution, Error> {
        if self.diagnostics && !self.native {
            return Err("native diagnostics require native execution".into());
        }
        if self.retained_c_diagnostics && !self.retained_c {
            return Err("retained C diagnostics require retained C execution".into());
        }
        if self.retained_c {
            return Ok(if self.retained_c_diagnostics {
                hl_container::Execution::retained_c_diagnostics()
            } else {
                hl_container::Execution::retained_c()
            });
        }
        Ok(if self.native {
            hl_container::Execution::native(self.diagnostics)
        } else {
            hl_container::Execution::default()
        })
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Commands {
    arm64: String,
    amd64: String,
}

impl Commands {
    pub(crate) fn for_target(&self, target: Target) -> &str {
        match target {
            Target::Arm64 => &self.arm64,
            Target::Amd64 => &self.amd64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Commands, Execution, Target, parse};
    use clap::ValueEnum;

    #[test]
    fn jobs_and_results_are_bounded() {
        assert_eq!(parse::jobs("1"), Ok(1));
        assert_eq!(parse::jobs("256"), Ok(256));
        for invalid in ["0", "257", "many"] {
            assert!(parse::jobs(invalid).is_err(), "accepted {invalid}");
        }
        assert!((1..=256).contains(&parse::logical_jobs()));
        assert!(parse::results("target/bench.tsv").is_ok());
        assert!(parse::results("../bench.tsv").is_err());
        assert!(parse::results("/absolute.tsv").is_err());
        assert!(parse::results("").is_err());
    }

    #[test]
    fn definition_paths_reject_traversal_nul_and_unbounded_fields() {
        use super::SafePath as _;
        use std::path::Path;

        assert!(Path::new("cases/a.txt").safe_relative().is_ok());
        for unsafe_path in ["", "/absolute", "../escape", "a/\0/b"] {
            assert!(
                Path::new(unsafe_path).safe_relative().is_err(),
                "accepted {unsafe_path:?}"
            );
        }
        assert!(Path::new(&"x".repeat(4097)).safe_relative().is_err());
        assert!(Path::new("/guest/bin").safe_absolute().is_ok());
        for unsafe_path in ["relative", "/guest/../escape", "/guest/\0"] {
            assert!(
                Path::new(unsafe_path).safe_absolute().is_err(),
                "accepted {unsafe_path:?}"
            );
        }
    }

    #[test]
    fn targets_preserve_guest_and_platform_routing() {
        assert!(Target::from_str("x86", false).is_err());
        assert_eq!(Target::from_str("arm64", false), Ok(Target::Arm64));
        assert_eq!(Target::from_str("amd64", false), Ok(Target::Amd64));
        assert_eq!(Target::Arm64.guest(), hl_container::Guest::Aarch64);
        assert_eq!(Target::Amd64.guest(), hl_container::Guest::X86_64);
        assert_eq!(Target::Arm64.platform(), hl_images::Platform::linux_arm64());
        assert_eq!(Target::Amd64.platform(), hl_images::Platform::linux_amd64());
    }

    #[test]
    fn execution_preserves_native_diagnostic_validation() {
        let enabled: Execution = serde_yaml::from_str("native: true\ndiagnostics: true\n").unwrap();
        let enabled = enabled.container().unwrap();
        assert!(enabled.is_native());
        assert!(enabled.diagnostics());
        let invalid: Execution = serde_yaml::from_str("diagnostics: true\n").unwrap();
        assert!(invalid.container().is_err());
    }

    #[test]
    fn commands_select_the_existing_isa_spelling() {
        let commands: Commands = serde_yaml::from_str("arm64: arm-cc\namd64: amd-cc\n").unwrap();
        assert_eq!(commands.for_target(Target::Arm64), "arm-cc");
        assert_eq!(commands.for_target(Target::Amd64), "amd-cc");
    }
}

/// The case-selection and concurrency arguments the runtime and scenario sweeps share.
/// Their durable result paths stay per-command, since each defaults to its own directory.
#[derive(clap::Args)]
pub(crate) struct Selection {
    /// Run only the case whose complete ID exactly matches this value.
    #[arg(long = "case", value_name = "FULL_ID")]
    pub(crate) case: Option<String>,
    /// Run only one guest ISA.
    #[arg(long = "isa", visible_alias = "target", value_enum)]
    pub(crate) target: Option<Target>,
    /// Maximum number of concurrently executing cases.
    #[arg(long, env = "HL_COMPAT_JOBS", default_value_t = parse::logical_jobs(), value_parser = parse::jobs)]
    pub(crate) jobs: usize,
    /// Resume exact completed case/target keys from the synchronized partial result.
    #[arg(long, env = "HL_COMPAT_RESUME", default_value_t = false)]
    pub(crate) resume: bool,
}

impl Selection {
    /// One named case on one ISA, run alone: what an in-process worker selects.
    pub(crate) fn exact(case: String, target: Target) -> Self {
        Self {
            case: Some(case),
            target: Some(target),
            jobs: 1,
            resume: false,
        }
    }

    pub(crate) fn targets(&self) -> Vec<Target> {
        self.target
            .map_or_else(|| vec![Target::Arm64, Target::Amd64], |value| vec![value])
    }
}

/// Command-line value parsers every harness shares. clap dictates the free
/// `fn(&str) -> Result<T, String>` shape, so they live here once.
pub(crate) mod parse {
    use std::path::{Component, PathBuf};

    #[hl_design::adapter]
    pub(crate) fn jobs(value: &str) -> Result<usize, String> {
        let jobs = value
            .parse::<usize>()
            .map_err(|_| "jobs must be an integer".to_owned())?;
        (1..=256)
            .contains(&jobs)
            .then_some(jobs)
            .ok_or_else(|| "jobs must be between 1 and 256".to_owned())
    }

    #[hl_design::adapter]
    pub(crate) fn results(value: &str) -> Result<PathBuf, String> {
        let path = PathBuf::from(value);
        if value.is_empty() || path.is_absolute() || path.components().any(|part| matches!(part, Component::ParentDir))
        {
            Err("results must be a safe relative path".to_owned())
        } else {
            Ok(path)
        }
    }

    /// The default worker count: available parallelism, within the accepted range.
    pub(crate) fn logical_jobs() -> usize {
        std::thread::available_parallelism()
            .map_or(1, std::num::NonZero::get)
            .min(256)
    }
}

/// The output one case is allowed to capture before the harness abandons it.
pub(crate) struct Capture;

impl Capture {
    pub(crate) const LIMIT: usize = 1024 * 1024;

    pub(crate) fn bounded(stdout: usize, stderr: usize) -> Result<(), Error> {
        let captured = stdout.checked_add(stderr).ok_or("captured output size overflow")?;
        if captured > Self::LIMIT {
            Err(format!("captured output exceeded {} bytes", Self::LIMIT).into())
        } else {
            Ok(())
        }
    }
}

/// Reports whether a container's captured logs stayed inside the shared bound.
pub(crate) trait BoundedCapture {
    fn bounded(&self) -> Result<(), Error>;
}

impl BoundedCapture for hl_container::Logs {
    fn bounded(&self) -> Result<(), Error> {
        Capture::bounded(self.stdout.len(), self.stderr.len())
    }
}

/// Admissibility of a path spelled by an untrusted definition. Every harness
/// validated the same thing with a different subset of these checks.
pub(crate) trait SafePath {
    /// The longest path field a definition may spell.
    const MAX_FIELD: usize = 4096;

    fn safe_relative(&self) -> Result<(), Error>;
    fn safe_absolute(&self) -> Result<(), Error>;
}

impl SafePath for std::path::Path {
    fn safe_relative(&self) -> Result<(), Error> {
        if self.as_os_str().is_empty() || self.is_absolute() || !self.is_bounded() {
            Err(format!("unsafe relative path {}", self.display()).into())
        } else {
            Ok(())
        }
    }

    fn safe_absolute(&self) -> Result<(), Error> {
        if !self.is_absolute() || !self.is_bounded() {
            Err(format!("unsafe guest path {}", self.display()).into())
        } else {
            Ok(())
        }
    }
}

trait BoundedPath {
    fn is_bounded(&self) -> bool;
}

impl BoundedPath for std::path::Path {
    fn is_bounded(&self) -> bool {
        self.as_os_str().len() <= <Self as SafePath>::MAX_FIELD
            && !self.as_os_str().to_string_lossy().contains('\0')
            && !self
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
    }
}
