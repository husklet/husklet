use super::identity::verify_artifact;
use crate::{platform::HostProcess, record::FramedIdentity, suite::Error};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

const SCHEMA: &str = "husklet-benchmark-v1";
// A smoke executes the campaign's actual factor-bound command. Large calibrated factors can take
// longer than the former ten-second startup allowance on a DBT arm, even though they remain well
// inside the workload timeout declared by the campaign.
const SMOKE_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Campaign {
    pub(super) schema: String,
    pub rounds: u32,
    pub samples_per_row: u32,
    pub rootfs: Artifact,
    pub arms: BTreeMap<String, Arm>,
    pub layouts: BTreeMap<String, Layout>,
    pub workloads: BTreeMap<String, Workload>,
    #[serde(default)]
    pub invariant_phases: Vec<String>,
}

pub(super) const CAMPAIGN_SCHEMA: &str = SCHEMA;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Artifact {
    pub path: PathBuf,
    pub sha256: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Arm {
    pub primary: Profile,
    #[serde(default)]
    pub independent_null: Option<Profile>,
    pub null_unqualified_reason: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Profile {
    pub command: Vec<String>,
    pub artifacts: BTreeMap<String, Artifact>,
    pub smoke: Vec<String>,
    pub guest_path: GuestPath,
    #[serde(default)]
    pub guest_map: BTreeMap<PathBuf, PathBuf>,
    pub build: BuildReceipt,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BuildReceipt {
    pub command: Vec<String>,
    pub inputs: BTreeMap<String, String>,
    pub outputs: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum ProfileKind {
    Primary,
    IndependentNull,
}

impl ProfileKind {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::IndependentNull => "independent-null",
        }
    }
}

impl Arm {
    pub(super) fn profile(&self, kind: ProfileKind) -> Option<&Profile> {
        match kind {
            ProfileKind::Primary => Some(&self.primary),
            ProfileKind::IndependentNull => self.independent_null.as_ref(),
        }
    }
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum GuestPath {
    HostAbsolute,
    RootfsAbsolute,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Layout {
    pub phases: Vec<String>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Workload {
    pub commands: BTreeMap<String, Vec<String>>,
    /// Exact phases emitted by each layout. The judged `phases` are their union.
    pub layout_phases: BTreeMap<String, Vec<String>>,
    /// Per-layout compatibility evidence. E and I must be available; R may be classified incompatible.
    pub arm_support: BTreeMap<String, BTreeMap<String, ArmSupport>>,
    pub phases: Vec<String>,
    pub timeout_seconds: u64,
    #[serde(default)]
    pub wall_time: bool,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub(super) enum ArmSupport {
    Available,
    Incompatible {
        status: i32,
        stderr: String,
        artifact_sha256: String,
    },
}

impl ArmSupport {
    pub(super) fn available(&self) -> bool {
        matches!(self, Self::Available)
    }

    fn valid(&self) -> bool {
        match self {
            Self::Available => true,
            Self::Incompatible {
                stderr,
                artifact_sha256,
                ..
            } => {
                !stderr.is_empty()
                    && artifact_sha256.len() == 64
                    && artifact_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
            }
        }
    }
}

impl Campaign {
    pub fn load(path: &Path) -> Result<Self, Error> {
        let value: Self = serde_yaml::from_str(&fs::read_to_string(path)?)?;
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), Error> {
        if self.schema != SCHEMA || self.rounds < 4 || !self.rounds.is_multiple_of(4) || self.samples_per_row < 3 {
            return Err("benchmark schema, rounds, or samples_per_row is invalid".into());
        }
        if self.arms.keys().map(String::as_str).collect::<Vec<_>>() != ["E", "I", "R"] {
            return Err("benchmark arms must be exactly E, I, and R".into());
        }
        if !command_profiles_distinct(self.arms.values().map(|arm| &arm.primary.command)) {
            return Err("benchmark E, I, and R command profiles must be distinct".into());
        }
        validate_arm_profiles(&self.arms, &self.rootfs.path)?;
        if !self.layouts.contains_key("plain") || !self.layouts.contains_key("sqlite") || self.layouts.len() != 2 {
            return Err("benchmark layouts must be exactly plain and sqlite".into());
        }
        if self.workloads.keys().map(String::as_str).collect::<Vec<_>>() != ["malloc", "python", "sqlite"] {
            return Err("benchmark workloads must be exactly malloc, python, and sqlite".into());
        }
        for (name, arm) in &self.arms {
            validate_profile(name, "primary", &arm.primary)?;
            if let Some(profile) = &arm.independent_null {
                validate_profile(name, "independent-null", profile)?;
                if arm.null_unqualified_reason.is_some()
                    || profile.build.inputs != arm.primary.build.inputs
                    || profile.command == arm.primary.command
                    || !independent_outputs(&arm.primary, profile)
                {
                    return Err(format!("benchmark arm {name} has invalid independent-null provenance").into());
                }
            } else if arm
                .null_unqualified_reason
                .as_deref()
                .is_none_or(|reason| reason.trim().is_empty())
            {
                return Err(format!("benchmark arm {name} must explain its missing independent null").into());
            }
        }
        if self.arms["E"].independent_null.is_none()
            || self.arms["I"].independent_null.is_none()
            || self.arms["R"].independent_null.is_some()
        {
            return Err("benchmark requires independent nulls for E and I and an explicit retained exclusion".into());
        }
        for (name, layout) in &self.layouts {
            if !phase_names_valid(&layout.phases, false) {
                return Err(format!("benchmark layout {name} is incomplete").into());
            }
        }
        for (name, workload) in &self.workloads {
            if workload.commands.is_empty()
                || !phase_names_valid(&workload.phases, false)
                || workload.layout_phases.keys().collect::<Vec<_>>() != workload.commands.keys().collect::<Vec<_>>()
                || workload.arm_support.keys().collect::<Vec<_>>() != workload.commands.keys().collect::<Vec<_>>()
                || workload.arm_support.values().any(|support| {
                    support.keys().map(String::as_str).collect::<Vec<_>>() != ["E", "I", "R"]
                        || !support["E"].available()
                        || !support["I"].available()
                        || support.values().any(|entry| !entry.valid())
                })
                || workload.layout_phases.values().any(|phases| {
                    !phase_names_valid(phases, false) || phases.iter().any(|phase| !workload.phases.contains(phase))
                })
                || workload
                    .phases
                    .iter()
                    .any(|phase| !workload.layout_phases.values().any(|phases| phases.contains(phase)))
                || !(1..=3600).contains(&workload.timeout_seconds)
                || workload
                    .commands
                    .iter()
                    .any(|(layout, command)| !self.layouts.contains_key(layout) || command.is_empty())
            {
                return Err(format!("benchmark workload {name} has invalid layouts or phases").into());
            }
            for command in workload.commands.values() {
                let guest = Path::new(&command[0]);
                if !guest_is_hashed(&self.rootfs.path, guest) {
                    return Err(format!("benchmark workload {name} guest is not a rootfs-contained file").into());
                }
            }
            if !workload_judgments_covered(name, workload, &self.layouts) {
                return Err(format!("benchmark workload {name} judges a phase absent from one of its layouts").into());
            }
        }
        let declared = self
            .layouts
            .values()
            .flat_map(|layout| &layout.phases)
            .chain(self.workloads.values().flat_map(|workload| &workload.phases))
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        if !invariant_phases_valid(&self.invariant_phases, &declared) {
            return Err("benchmark requires at least one unique declared invariant phase".into());
        }
        if self.workloads["malloc"].commands.keys().collect::<Vec<_>>() != self.layouts.keys().collect::<Vec<_>>() {
            return Err("malloc must run the full sequence on both plain and sqlite layouts".into());
        }
        if !self.workloads["sqlite"].commands.contains_key("sqlite") {
            return Err("sqlite must run on the sqlite-linked layout".into());
        }
        let guests = self
            .workloads
            .values()
            .flat_map(|workload| workload.commands.values())
            .map(|command| PathBuf::from(&command[0]))
            .collect::<BTreeSet<_>>();
        if [
            &self.arms["E"].primary,
            self.arms["E"].independent_null.as_ref().unwrap(),
        ]
        .into_iter()
        .any(|profile| profile.guest_map.keys().collect::<BTreeSet<_>>() != guests.iter().collect())
        {
            return Err("native/Rosetta arm must map every Linux guest to its hashed macOS equivalent".into());
        }
        Ok(())
    }

    pub fn identity(&self) -> Result<String, Error> {
        let bytes = serde_json::to_vec(self)?;
        let mut framed = FramedIdentity::new(b"husklet-benchmark-campaign-v1")?;
        framed.field(&bytes)?;
        Ok(framed.finish())
    }

    pub fn verify_artifacts(&self) -> Result<(), Error> {
        verify_artifact("rootfs", &self.rootfs, true)?;
        for (label, arm) in &self.arms {
            for (kind, profile) in [
                ("primary", Some(&arm.primary)),
                ("independent-null", arm.independent_null.as_ref()),
            ] {
                let Some(profile) = profile else { continue };
                verify_profile(label, kind, profile)?;
            }
        }
        Ok(())
    }

    pub fn guest(&self, arm: &str, profile: ProfileKind, guest: &Path) -> Result<PathBuf, Error> {
        let definition = self.arms[arm]
            .profile(profile)
            .ok_or("benchmark profile is unavailable")?;
        if let Some(mapped) = definition.guest_map.get(guest) {
            return Ok(mapped.clone());
        }
        match definition.guest_path {
            GuestPath::HostAbsolute => Ok(guest.to_owned()),
            // The production engine CLI confines guest names beneath --rootfs and
            // therefore accepts root-relative names without a host-leading slash.
            GuestPath::RootfsAbsolute => Ok(guest.strip_prefix(&self.rootfs.path)?.to_owned()),
        }
    }

    /// Whether both sides of an arm's null are independently produced artifacts for this guest.
    pub(super) fn independent_null_qualified(&self, arm: &str, guest: &Path) -> bool {
        let Some(independent) = self.arms[arm].independent_null.as_ref() else {
            return false;
        };
        if arm != "E" {
            return independent.build.outputs.contains_key("engine")
                && self.arms[arm].primary.build.outputs.contains_key("engine");
        }
        let primary = &self.arms[arm].primary;
        [primary, independent].into_iter().all(|profile| {
            let Some(mapped) = profile.guest_map.get(guest) else {
                return false;
            };
            profile.artifacts.iter().any(|(name, artifact)| {
                artifact.path == *mapped
                    && profile
                        .build
                        .outputs
                        .get(name)
                        .is_some_and(|digest| *digest == artifact.sha256)
            })
        })
    }
}

/// Verifies that one arm profile still measures the hashed artifacts its receipt names.
fn verify_profile(label: &str, kind: &str, profile: &Profile) -> Result<(), Error> {
    for (name, artifact) in &profile.artifacts {
        verify_artifact(&format!("arm {label} {kind} artifact {name}"), artifact, false)?;
    }
    let executable = Path::new(&profile.command[0]);
    if !executable.is_absolute()
        || !executable.is_file()
        || !smoke_binds_profile(&profile.command, &profile.smoke)
        || !command_is_hashed(executable, profile.artifacts.values())
    {
        return Err(format!("arm {label} {kind} command is not bound to a hashed artifact").into());
    }
    let outcome = HostProcess::bounded(&profile.smoke[0], &profile.smoke[1..], SMOKE_TIMEOUT)?;
    if outcome != hl_process::Outcome::Exited(Some(0)) {
        return Err(format!("arm {label} {kind} smoke execution failed with {outcome:?}").into());
    }
    Ok(())
}

fn independent_outputs(primary: &Profile, null: &Profile) -> bool {
    if primary.build.outputs.keys().collect::<BTreeSet<_>>() != null.build.outputs.keys().collect::<BTreeSet<_>>() {
        return false;
    }
    primary.build.outputs.keys().all(|name| {
        let left = &primary.artifacts[name].path;
        let right = &null.artifacts[name].path;
        if left == right {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            let identities = fs::metadata(left).ok().zip(fs::metadata(right).ok());
            if identities.is_some_and(|(left, right)| left.dev() == right.dev() && left.ino() == right.ino()) {
                return false;
            }
        }
        true
    })
}

fn sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn validate_profile(arm: &str, kind: &str, profile: &Profile) -> Result<(), Error> {
    if profile.command.is_empty()
        || profile.artifacts.is_empty()
        || profile.smoke.is_empty()
        || profile.build.command.is_empty()
        || profile.build.inputs.is_empty()
        || profile.build.outputs.is_empty()
        || profile
            .build
            .inputs
            .keys()
            .chain(profile.build.outputs.keys())
            .any(String::is_empty)
        || profile
            .build
            .inputs
            .values()
            .chain(profile.build.outputs.values())
            .any(|digest| !sha256(digest))
        || profile.build.outputs.iter().any(|(name, digest)| {
            profile
                .artifacts
                .get(name)
                .is_none_or(|artifact| artifact.sha256 != *digest)
        })
    {
        return Err(format!("benchmark arm {arm} {kind} profile or build receipt is incomplete").into());
    }
    if profile.guest_map.values().any(|guest| {
        !guest.is_absolute() || !guest.is_file() || !profile.artifacts.values().any(|artifact| artifact.path == *guest)
    }) {
        return Err(format!("benchmark arm {arm} {kind} guest map is not bound to hashed artifacts").into());
    }
    Ok(())
}

fn command_is_hashed<'a>(executable: &Path, artifacts: impl IntoIterator<Item = &'a Artifact>) -> bool {
    let Ok(executable) = fs::canonicalize(executable) else {
        return false;
    };
    artifacts
        .into_iter()
        .any(|artifact| fs::canonicalize(&artifact.path).is_ok_and(|path| path == executable))
}

fn smoke_binds_profile(command: &[String], smoke: &[String]) -> bool {
    !command.is_empty() && smoke.starts_with(command) && smoke.len() > command.len()
}

fn command_profiles_distinct<'a>(commands: impl IntoIterator<Item = &'a Vec<String>>) -> bool {
    let commands = commands.into_iter().collect::<Vec<_>>();
    commands.len() == 3 && commands.iter().collect::<BTreeSet<_>>().len() == 3
}

fn validate_arm_profiles(arms: &BTreeMap<String, Arm>, rootfs: &Path) -> Result<(), Error> {
    for external in [Some(&arms["E"].primary), arms["E"].independent_null.as_ref()]
        .into_iter()
        .flatten()
    {
        if external.command.len() != 3
            || external.command[2] != "-x86_64"
            || !matches!(external.guest_path, GuestPath::HostAbsolute)
            || !profile_argument_matches_artifact(&external.command[0], external.artifacts.get("command"))
            || !profile_argument_matches_artifact(&external.command[1], external.artifacts.get("arch"))
        {
            return Err("benchmark E arm must contain hashed macOS x86-64 native/Rosetta profiles".into());
        }
    }
    for label in ["I", "R"] {
        for arm in [Some(&arms[label].primary), arms[label].independent_null.as_ref()]
            .into_iter()
            .flatten()
        {
            if arm.command.len() != 4
                || arm.command[2] != "--rootfs"
                || Path::new(&arm.command[3]) != rootfs
                || !matches!(arm.guest_path, GuestPath::RootfsAbsolute)
                || !arm.guest_map.is_empty()
                || !profile_argument_matches_artifact(&arm.command[0], arm.artifacts.get("command"))
                || !profile_argument_matches_artifact(&arm.command[1], arm.artifacts.get("engine"))
            {
                return Err(format!("benchmark {label} arm is not bound to its hashed rootfs engine profiles").into());
            }
        }
    }
    Ok(())
}

fn profile_argument_matches_artifact(argument: &str, artifact: Option<&Artifact>) -> bool {
    let argument = Path::new(argument);
    let path = argument
        .strip_prefix("/mnt/mac")
        .map_or_else(|_| argument.to_owned(), |suffix| Path::new("/").join(suffix));
    artifact.is_some_and(|artifact| artifact.path == path)
}

fn workload_judgments_covered(name: &str, workload: &Workload, layouts: &BTreeMap<String, Layout>) -> bool {
    name == "python"
        || workload.commands.keys().all(|layout| {
            workload.layout_phases[layout]
                .iter()
                .all(|phase| layouts[layout].phases.contains(phase))
        })
}

fn phase_names_valid(phases: &[String], allow_empty: bool) -> bool {
    (allow_empty || !phases.is_empty())
        && phases.iter().all(|phase| !phase.is_empty())
        && phases.iter().collect::<BTreeSet<_>>().len() == phases.len()
}

fn invariant_phases_valid(invariants: &[String], declared: &BTreeSet<&str>) -> bool {
    phase_names_valid(invariants, false) && invariants.iter().all(|phase| declared.contains(phase.as_str()))
}

fn guest_is_hashed(rootfs: &Path, guest: &Path) -> bool {
    guest.is_absolute()
        && guest.starts_with(rootfs)
        && guest.is_file()
        && fs::canonicalize(rootfs)
            .ok()
            .zip(fs::canonicalize(guest).ok())
            .is_some_and(|(rootfs, guest)| guest.starts_with(rootfs))
}

#[cfg(test)]
mod tests {
    use super::{
        Arm, ArmSupport, Artifact, BuildReceipt, Campaign, GuestPath, Layout, Profile, ProfileKind, Workload,
        command_profiles_distinct, guest_is_hashed, independent_outputs, invariant_phases_valid, phase_names_valid,
        profile_argument_matches_artifact, smoke_binds_profile, validate_arm_profiles, workload_judgments_covered,
    };
    use std::{collections::BTreeMap, fs, path::Path};

    fn test_receipt(artifacts: &BTreeMap<String, Artifact>) -> BuildReceipt {
        BuildReceipt {
            command: vec!["test-build".into()],
            inputs: BTreeMap::from([("source".into(), "a".repeat(64))]),
            outputs: artifacts
                .iter()
                .map(|(name, artifact)| (name.clone(), artifact.sha256.clone()))
                .collect(),
        }
    }

    fn primary(profile: Profile) -> Arm {
        Arm {
            primary: profile,
            independent_null: None,
            null_unqualified_reason: Some("unit fixture has no null".into()),
        }
    }

    fn output_profile(path: &Path) -> Profile {
        let artifact = Artifact {
            path: path.to_owned(),
            sha256: crate::benchmark::identity::file_hash_without_attributes(path).unwrap(),
        };
        let artifacts = BTreeMap::from([("engine".into(), artifact)]);
        Profile {
            command: vec![path.display().to_string()],
            build: test_receipt(&artifacts),
            artifacts,
            smoke: vec![path.display().to_string(), "--smoke".into()],
            guest_path: GuestPath::RootfsAbsolute,
            guest_map: BTreeMap::new(),
        }
    }

    #[cfg(unix)]
    #[test]
    fn independent_outputs_reject_shared_paths_and_hardlinks() {
        let directory = tempfile::tempdir().unwrap();
        let primary_path = directory.path().join("primary");
        let copied_path = directory.path().join("copied");
        let linked_path = directory.path().join("linked");
        fs::write(&primary_path, b"same deterministic output").unwrap();
        fs::copy(&primary_path, &copied_path).unwrap();
        fs::hard_link(&primary_path, &linked_path).unwrap();
        let primary = output_profile(&primary_path);
        assert!(independent_outputs(&primary, &output_profile(&copied_path)));
        assert!(!independent_outputs(&primary, &output_profile(&primary_path)));
        assert!(!independent_outputs(&primary, &output_profile(&linked_path)));
    }

    #[test]
    fn arm_can_map_shared_linux_guest_to_hashed_host_native_equivalent() {
        let temporary = tempfile::tempdir().unwrap();
        let rootfs = temporary.path().join("rootfs");
        fs::create_dir(&rootfs).unwrap();
        let linux = rootfs.join("bench");
        let native = temporary.path().join("bench-macho-x86_64");
        fs::write(&linux, b"linux elf").unwrap();
        fs::write(&native, b"mach-o x86_64").unwrap();
        let artifacts = BTreeMap::from([(
            "guest".into(),
            Artifact {
                path: native.clone(),
                sha256: crate::benchmark::identity::file_hash_without_attributes(&native).unwrap(),
            },
        )]);
        let arm = primary(Profile {
            command: vec!["/usr/bin/arch".into(), "-x86_64".into()],
            build: test_receipt(&artifacts),
            artifacts,
            smoke: vec!["/usr/bin/arch".into(), "-x86_64".into(), native.display().to_string()],
            guest_path: GuestPath::HostAbsolute,
            guest_map: BTreeMap::from([(linux.clone(), native.clone())]),
        });
        let campaign = Campaign {
            schema: super::SCHEMA.into(),
            rounds: 4,
            samples_per_row: 3,
            rootfs: Artifact {
                path: rootfs,
                sha256: String::new(),
            },
            arms: BTreeMap::from([("E".into(), arm)]),
            layouts: BTreeMap::new(),
            workloads: BTreeMap::new(),
            invariant_phases: Vec::new(),
        };
        assert_eq!(campaign.guest("E", ProfileKind::Primary, &linux).unwrap(), native);
    }

    #[test]
    fn rootfs_guest_is_passed_as_a_confined_relative_name() {
        let temporary = tempfile::tempdir().unwrap();
        let rootfs = temporary.path().join("rootfs");
        let guest = rootfs.join("benchmark/malloc-plain");
        let arm = primary(Profile {
            command: vec!["/engine".into()],
            artifacts: BTreeMap::new(),
            smoke: vec!["/engine".into(), "--smoke".into()],
            guest_path: GuestPath::RootfsAbsolute,
            guest_map: BTreeMap::new(),
            build: BuildReceipt {
                command: vec!["test-build".into()],
                inputs: BTreeMap::from([("source".into(), "a".repeat(64))]),
                outputs: BTreeMap::new(),
            },
        });
        let campaign = Campaign {
            schema: super::SCHEMA.into(),
            rounds: 4,
            samples_per_row: 3,
            rootfs: Artifact {
                path: rootfs,
                sha256: String::new(),
            },
            arms: BTreeMap::from([("I".into(), arm)]),
            layouts: BTreeMap::new(),
            workloads: BTreeMap::new(),
            invariant_phases: Vec::new(),
        };
        assert_eq!(
            campaign.guest("I", ProfileKind::Primary, &guest).unwrap(),
            Path::new("benchmark/malloc-plain")
        );
    }

    #[test]
    fn smoke_executes_the_exact_measured_profile_prefix() {
        let command = ["/engine".into(), "--profile".into(), "integrated".into()];
        let smoke = [
            "/engine".into(),
            "--profile".into(),
            "integrated".into(),
            "/guest/smoke".into(),
        ];
        assert!(smoke_binds_profile(&command, &smoke));
        assert!(!smoke_binds_profile(&command, &["/engine".into(), "--help".into()]));
        assert!(!smoke_binds_profile(
            &command,
            &[
                "/engine".into(),
                "--profile".into(),
                "retained".into(),
                "/guest/smoke".into()
            ]
        ));
        assert!(!smoke_binds_profile(&command, &command));
    }

    #[cfg(unix)]
    #[test]
    fn command_symlink_is_bound_to_its_hashed_regular_target() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let target = temporary.path().join("mac-real");
        let command = temporary.path().join("mac");
        fs::write(&target, b"proxy").unwrap();
        symlink(&target, &command).unwrap();
        let artifact = Artifact {
            path: target,
            sha256: String::new(),
        };
        assert!(super::command_is_hashed(&command, [&artifact]));
        fs::write(temporary.path().join("other"), b"other").unwrap();
        fs::remove_file(&command).unwrap();
        symlink(temporary.path().join("other"), &command).unwrap();
        assert!(!super::command_is_hashed(&command, [&artifact]));
    }

    #[test]
    fn engine_labels_cannot_alias_one_command_profile() {
        let distinct = [
            vec!["/external".into()],
            vec!["/testing".into(), "--backend=integrated".into()],
            vec!["/testing".into(), "--backend=retained".into()],
        ];
        assert!(command_profiles_distinct(distinct.iter()));

        let aliased = [
            vec!["/external".into()],
            vec!["/testing".into(), "--backend=integrated".into()],
            vec!["/testing".into(), "--backend=integrated".into()],
        ];
        assert!(!command_profiles_distinct(aliased.iter()));
        assert!(!command_profiles_distinct(distinct[..2].iter()));
    }

    #[test]
    fn measured_profile_arguments_must_name_hashed_host_artifacts() {
        let artifacts = BTreeMap::from([(
            "engine".to_owned(),
            Artifact {
                path: "/Users/test/stage/hl-x86_64".into(),
                sha256: "a".repeat(64),
            },
        )]);
        assert!(profile_argument_matches_artifact(
            "/mnt/mac/Users/test/stage/hl-x86_64",
            artifacts.get("engine")
        ));
        assert!(!profile_argument_matches_artifact(
            "/mnt/mac/Users/test/stage/unhashed-engine",
            artifacts.get("engine")
        ));
        assert!(!profile_argument_matches_artifact(
            "/mnt/macaroni/Users/test/stage/hl-x86_64",
            artifacts.get("engine")
        ));
    }

    #[test]
    fn arm_roles_are_bound_to_rosetta_and_rootfs_execution_profiles() {
        let artifact = |path: &str| Artifact {
            path: path.into(),
            sha256: "a".repeat(64),
        };
        let proxy = artifact("/stage/mac");
        let mut arms = BTreeMap::from([
            (
                "E".into(),
                primary(Profile {
                    command: vec!["/stage/mac".into(), "/mnt/mac/stage/arch".into(), "-x86_64".into()],
                    artifacts: BTreeMap::from([
                        ("command".into(), proxy.clone()),
                        ("arch".into(), artifact("/stage/arch")),
                    ]),
                    smoke: vec!["unused".into()],
                    guest_path: GuestPath::HostAbsolute,
                    guest_map: BTreeMap::new(),
                    build: BuildReceipt {
                        command: vec!["build-E".into()],
                        inputs: BTreeMap::from([("source".into(), "a".repeat(64))]),
                        outputs: BTreeMap::new(),
                    },
                }),
            ),
            (
                "I".into(),
                primary(Profile {
                    command: vec![
                        "/stage/mac".into(),
                        "/mnt/mac/stage/integrated".into(),
                        "--rootfs".into(),
                        "/stage/rootfs".into(),
                    ],
                    artifacts: BTreeMap::from([
                        ("command".into(), proxy.clone()),
                        ("engine".into(), artifact("/stage/integrated")),
                    ]),
                    smoke: vec!["unused".into()],
                    guest_path: GuestPath::RootfsAbsolute,
                    guest_map: BTreeMap::new(),
                    build: BuildReceipt {
                        command: vec!["build-I".into()],
                        inputs: BTreeMap::from([("source".into(), "a".repeat(64))]),
                        outputs: BTreeMap::new(),
                    },
                }),
            ),
            (
                "R".into(),
                primary(Profile {
                    command: vec![
                        "/stage/mac".into(),
                        "/mnt/mac/stage/retained".into(),
                        "--rootfs".into(),
                        "/stage/rootfs".into(),
                    ],
                    artifacts: BTreeMap::from([
                        ("command".into(), proxy),
                        ("engine".into(), artifact("/stage/retained")),
                    ]),
                    smoke: vec!["unused".into()],
                    guest_path: GuestPath::RootfsAbsolute,
                    guest_map: BTreeMap::new(),
                    build: BuildReceipt {
                        command: vec!["build-R".into()],
                        inputs: BTreeMap::from([("source".into(), "a".repeat(64))]),
                        outputs: BTreeMap::new(),
                    },
                }),
            ),
        ]);
        validate_arm_profiles(&arms, Path::new("/stage/rootfs")).unwrap();
        arms.get_mut("E").unwrap().primary.command[2] = "-arm64".into();
        assert!(validate_arm_profiles(&arms, Path::new("/stage/rootfs")).is_err());
        arms.get_mut("E").unwrap().primary.command[2] = "-x86_64".into();
        let arch = arms["E"].primary.artifacts["arch"].clone();
        arms.get_mut("E")
            .unwrap()
            .primary
            .artifacts
            .insert("command".into(), arch);
        assert!(validate_arm_profiles(&arms, Path::new("/stage/rootfs")).is_err());
        arms.get_mut("E")
            .unwrap()
            .primary
            .artifacts
            .insert("command".into(), artifact("/stage/mac"));
        arms.get_mut("E").unwrap().primary.artifacts.remove("arch");
        assert!(validate_arm_profiles(&arms, Path::new("/stage/rootfs")).is_err());
        arms.get_mut("E")
            .unwrap()
            .primary
            .artifacts
            .insert("arch".into(), artifact("/stage/arch"));
        arms.get_mut("I").unwrap().primary.command[1] = "/mnt/mac/stage/unhashed".into();
        assert!(validate_arm_profiles(&arms, Path::new("/stage/rootfs")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn guest_symlink_cannot_escape_the_hashed_rootfs() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let rootfs = directory.path().join("rootfs");
        fs::create_dir(&rootfs).unwrap();
        let outside = directory.path().join("outside-guest");
        fs::write(&outside, b"guest").unwrap();
        let escaping = rootfs.join("escaping-guest");
        symlink(&outside, &escaping).unwrap();
        assert!(!guest_is_hashed(&rootfs, &escaping));

        let inside = rootfs.join("inside-guest");
        fs::write(&inside, b"guest").unwrap();
        let contained = rootfs.join("contained-guest");
        symlink(&inside, &contained).unwrap();
        assert!(guest_is_hashed(&rootfs, &contained));
    }

    #[test]
    fn phase_declarations_are_unique_and_named() {
        assert!(phase_names_valid(&["compute".into(), "malloc".into()], false));
        assert!(!phase_names_valid(&[], false));
        assert!(!phase_names_valid(&["compute".into(), "compute".into()], false));
        assert!(!phase_names_valid(&[String::new()], false));
        assert!(phase_names_valid(&[], true));

        let declared = ["compute", "malloc"].into_iter().collect();
        assert!(invariant_phases_valid(&["compute".into()], &declared));
        assert!(!invariant_phases_valid(&[], &declared));
        assert!(!invariant_phases_valid(&["typo".into()], &declared));
    }

    #[test]
    fn incompatible_arm_requires_a_bound_failure_record() {
        assert!(ArmSupport::Available.valid());
        assert!(
            ArmSupport::Incompatible {
                status: 1,
                stderr: "_PySys_Create: failed to create a module object".into(),
                artifact_sha256: "a".repeat(64),
            }
            .valid()
        );
        assert!(
            !ArmSupport::Incompatible {
                status: 1,
                stderr: String::new(),
                artifact_sha256: "a".repeat(64),
            }
            .valid()
        );
        assert!(
            !ArmSupport::Incompatible {
                status: 1,
                stderr: "failure".into(),
                artifact_sha256: "unbound".into(),
            }
            .valid()
        );
    }

    #[test]
    fn judged_phases_exist_in_every_workload_layout() {
        let layouts = [
            (
                "plain".into(),
                Layout {
                    phases: vec!["malloc".into()],
                },
            ),
            (
                "sqlite".into(),
                Layout {
                    phases: vec!["malloc".into(), "sqlite".into()],
                },
            ),
        ]
        .into();
        let mut workload = Workload {
            commands: [
                ("plain".into(), vec!["guest".into()]),
                ("sqlite".into(), vec!["guest".into()]),
            ]
            .into(),
            layout_phases: [
                ("plain".into(), vec!["malloc".into()]),
                ("sqlite".into(), vec!["malloc".into()]),
            ]
            .into(),
            arm_support: ["plain", "sqlite"]
                .into_iter()
                .map(|layout| {
                    (
                        layout.into(),
                        ["E", "I", "R"]
                            .into_iter()
                            .map(|arm| (arm.into(), ArmSupport::Available))
                            .collect(),
                    )
                })
                .collect(),
            phases: vec!["malloc".into()],
            timeout_seconds: 1,
            wall_time: false,
        };
        assert!(workload_judgments_covered("malloc", &workload, &layouts));
        workload.phases = vec!["sqlite".into()];
        workload.layout_phases.get_mut("plain").unwrap()[0] = "sqlite".into();
        assert!(!workload_judgments_covered("malloc", &workload, &layouts));
        assert!(workload_judgments_covered("python", &workload, &layouts));
    }
}
