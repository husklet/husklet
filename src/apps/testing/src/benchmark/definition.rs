use crate::{platform::HostProcess, record::FramedIdentity, suite::Error};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

const SCHEMA: &str = "husklet-benchmark-v1";
const SMOKE_TIMEOUT: Duration = Duration::from_secs(10);

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
    pub command: Vec<String>,
    pub artifacts: BTreeMap<String, Artifact>,
    pub smoke: Vec<String>,
    pub guest_path: GuestPath,
    #[serde(default)]
    pub guest_map: BTreeMap<PathBuf, PathBuf>,
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
        if !command_profiles_distinct(self.arms.values().map(|arm| &arm.command)) {
            return Err("benchmark E, I, and R command profiles must be distinct".into());
        }
        if !self.layouts.contains_key("plain") || !self.layouts.contains_key("sqlite") || self.layouts.len() != 2 {
            return Err("benchmark layouts must be exactly plain and sqlite".into());
        }
        if self.workloads.keys().map(String::as_str).collect::<Vec<_>>() != ["malloc", "python", "sqlite"] {
            return Err("benchmark workloads must be exactly malloc, python, and sqlite".into());
        }
        for (name, arm) in &self.arms {
            if arm.command.is_empty() || arm.artifacts.is_empty() || arm.smoke.is_empty() {
                return Err(format!("benchmark arm {name} is incomplete").into());
            }
            if arm.guest_map.values().any(|guest| {
                !guest.is_absolute()
                    || !guest.is_file()
                    || !arm.artifacts.values().any(|artifact| artifact.path == *guest)
            }) {
                return Err(format!("benchmark arm {name} guest map is not bound to hashed artifacts").into());
            }
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
                    !phase_names_valid(phases, false)
                        || phases.iter().any(|phase| !workload.phases.contains(phase))
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
            for (name, artifact) in &arm.artifacts {
                verify_artifact(&format!("arm {label} artifact {name}"), artifact, false)?;
            }
            let executable = Path::new(&arm.command[0]);
            if !executable.is_absolute()
                || !executable.is_file()
                || !smoke_binds_profile(&arm.command, &arm.smoke)
                || !command_is_hashed(executable, arm.artifacts.values())
            {
                return Err(format!("arm {label} command is not bound to a hashed artifact").into());
            }
            let outcome = HostProcess::bounded(&arm.smoke[0], &arm.smoke[1..], SMOKE_TIMEOUT)?;
            if outcome != hl_process::Outcome::Exited(Some(0)) {
                return Err(format!("arm {label} smoke execution failed with {outcome:?}").into());
            }
        }
        Ok(())
    }

    pub fn guest(&self, arm: &str, guest: &Path) -> Result<PathBuf, Error> {
        let definition = &self.arms[arm];
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

fn verify_artifact(label: &str, artifact: &Artifact, directory: bool) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(&artifact.path)?;
    let expected_type = if directory {
        metadata.is_dir()
    } else {
        metadata.is_file()
    };
    if !expected_type {
        return Err(format!("{label} has the wrong file type").into());
    }
    let observed = if directory {
        tree_hash(&artifact.path)?
    } else {
        file_hash(&artifact.path)?
    };
    if observed != artifact.sha256 {
        return Err(format!(
            "{label} sha256 changed: expected {}, observed {observed}",
            artifact.sha256
        )
        .into());
    }
    Ok(())
}

fn tree_hash(root: &Path) -> Result<String, Error> {
    fn permissions(metadata: &fs::Metadata, identity: &mut FramedIdentity) -> Result<(), Error> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
            unix_attributes(metadata.permissions().mode(), metadata.uid(), metadata.gid(), identity)?;
        }
        #[cfg(not(unix))]
        identity.field(&[u8::from(metadata.permissions().readonly())])?;
        Ok(())
    }

    fn walk(
        root: &Path,
        directory: &Path,
        identity: &mut FramedIdentity,
        links: &mut BTreeMap<(u64, u64), PathBuf>,
    ) -> Result<(), Error> {
        let mut entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let relative = path.strip_prefix(root)?;
            identity.field(relative.as_os_str().as_encoded_bytes())?;
            let metadata = fs::symlink_metadata(&path)?;
            permissions(&metadata, identity)?;
            if !metadata.file_type().is_symlink() {
                attributes(&path, identity)?;
            }
            if metadata.file_type().is_symlink() {
                identity.field(b"L")?;
                identity.field(fs::read_link(path)?.as_os_str().as_encoded_bytes())?;
            } else if metadata.is_dir() {
                identity.field(b"D")?;
                walk(root, &path, identity, links)?;
            } else if metadata.is_file() {
                identity.field(b"F")?;
                hardlink(relative, &metadata, identity, links)?;
                identity.field(&fs::read(path)?)?;
            } else {
                return Err("rootfs contains an unsupported entry type".into());
            }
        }
        Ok(())
    }
    let mut identity = FramedIdentity::new(b"husklet-rootfs-tree-v4")?;
    permissions(&fs::symlink_metadata(root)?, &mut identity)?;
    attributes(root, &mut identity)?;
    walk(root, root, &mut identity, &mut BTreeMap::new())?;
    Ok(identity.finish())
}

fn attributes(path: &Path, identity: &mut FramedIdentity) -> Result<(), Error> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        let mut names = xattr::list(path)?.collect::<Vec<_>>();
        names.sort();
        identity.field(&(names.len() as u64).to_le_bytes())?;
        for name in names {
            identity.field(name.as_bytes())?;
            let value = xattr::get(path, &name)?.ok_or("rootfs xattr disappeared while hashing")?;
            identity.field(&value)?;
        }
    }
    #[cfg(not(unix))]
    identity.field(&0_u64.to_le_bytes())?;
    Ok(())
}

fn hardlink(
    relative: &Path,
    metadata: &fs::Metadata,
    identity: &mut FramedIdentity,
    links: &mut BTreeMap<(u64, u64), PathBuf>,
) -> Result<(), Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.nlink() > 1 {
            let first = links
                .entry((metadata.dev(), metadata.ino()))
                .or_insert_with(|| relative.to_owned());
            identity.field(b"H")?;
            identity.field(first.as_os_str().as_encoded_bytes())?;
            return Ok(());
        }
    }
    let _ = (relative, metadata, links);
    identity.field(b"U")?;
    Ok(())
}

#[cfg(unix)]
fn unix_attributes(mode: u32, uid: u32, gid: u32, identity: &mut FramedIdentity) -> Result<(), Error> {
    identity.field(&(mode & 0o7777).to_le_bytes())?;
    identity.field(&uid.to_le_bytes())?;
    identity.field(&gid.to_le_bytes())?;
    Ok(())
}

pub(super) fn artifact_identity(path: &Path) -> Result<String, Error> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() {
        tree_hash(path)
    } else if metadata.is_file() {
        file_hash(path)
    } else {
        Err("benchmark artifact is neither a regular file nor a directory".into())
    }
}

fn file_hash(path: &Path) -> Result<String, Error> {
    let metadata = fs::symlink_metadata(path)?;
    let mut identity = FramedIdentity::new(b"husklet-benchmark-file-v3")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        unix_attributes(
            metadata.permissions().mode(),
            metadata.uid(),
            metadata.gid(),
            &mut identity,
        )?;
        identity.field(&metadata.nlink().to_le_bytes())?;
    }
    #[cfg(not(unix))]
    identity.field(&[u8::from(metadata.permissions().readonly())])?;
    attributes(path, &mut identity)?;
    identity.field(&fs::read(path)?)?;
    Ok(identity.finish())
}

#[cfg(test)]
mod tests {
    use super::{
        Arm, ArmSupport, Artifact, Campaign, GuestPath, Layout, Workload, command_profiles_distinct, guest_is_hashed,
        invariant_phases_valid, phase_names_valid, smoke_binds_profile, verify_artifact, workload_judgments_covered,
    };
    use crate::record::FramedIdentity;
    use std::{collections::BTreeMap, fs, path::Path};

    #[test]
    fn arm_can_map_shared_linux_guest_to_hashed_host_native_equivalent() {
        let temporary = tempfile::tempdir().unwrap();
        let rootfs = temporary.path().join("rootfs");
        fs::create_dir(&rootfs).unwrap();
        let linux = rootfs.join("bench");
        let native = temporary.path().join("bench-macho-x86_64");
        fs::write(&linux, b"linux elf").unwrap();
        fs::write(&native, b"mach-o x86_64").unwrap();
        let arm = Arm {
            command: vec!["/usr/bin/arch".into(), "-x86_64".into()],
            artifacts: BTreeMap::from([(
                "guest".into(),
                Artifact {
                    path: native.clone(),
                    sha256: super::file_hash(&native).unwrap(),
                },
            )]),
            smoke: vec!["/usr/bin/arch".into(), "-x86_64".into(), native.display().to_string()],
            guest_path: GuestPath::HostAbsolute,
            guest_map: BTreeMap::from([(linux.clone(), native.clone())]),
        };
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
        assert_eq!(campaign.guest("E", &linux).unwrap(), native);
    }

    #[test]
    fn rootfs_guest_is_passed_as_a_confined_relative_name() {
        let temporary = tempfile::tempdir().unwrap();
        let rootfs = temporary.path().join("rootfs");
        let guest = rootfs.join("benchmark/malloc-plain");
        let arm = Arm {
            command: vec!["/engine".into()],
            artifacts: BTreeMap::new(),
            smoke: vec!["/engine".into(), "--smoke".into()],
            guest_path: GuestPath::RootfsAbsolute,
            guest_map: BTreeMap::new(),
        };
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
        assert_eq!(campaign.guest("I", &guest).unwrap(), Path::new("benchmark/malloc-plain"));
    }

    #[test]
    fn regular_file_artifact_is_accepted() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("engine");
        fs::write(&path, b"engine").unwrap();
        let artifact = Artifact {
            sha256: super::file_hash(&path).unwrap(),
            path,
        };
        verify_artifact("engine", &artifact, false).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn executable_artifact_identity_includes_mode() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("engine");
        fs::write(&path, b"same engine bytes").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let before = super::file_hash(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        assert_ne!(before, super::file_hash(&path).unwrap());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn executable_artifact_identity_includes_extended_attributes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("engine");
        fs::write(&path, b"same engine bytes").unwrap();
        let before = super::file_hash(&path).unwrap();
        xattr::set(&path, "user.husklet-benchmark", b"changed capability").unwrap();
        assert_ne!(before, super::file_hash(&path).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn executable_artifact_identity_includes_hardlink_aliases() {
        let temporary = tempfile::tempdir().unwrap();
        let executable = temporary.path().join("engine");
        fs::write(&executable, b"engine").unwrap();
        let before = super::file_hash(&executable).unwrap();
        fs::hard_link(&executable, temporary.path().join("engine-alias")).unwrap();
        let after = super::file_hash(&executable).unwrap();
        assert_ne!(before, after, "a hard-link alias must change executable identity");
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

    #[cfg(unix)]
    #[test]
    fn executable_artifact_cannot_be_a_symlink() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("engine-real");
        let path = directory.path().join("engine");
        fs::write(&target, b"engine").unwrap();
        symlink(&target, &path).unwrap();
        let artifact = Artifact {
            path,
            sha256: super::file_hash(&target).unwrap(),
        };
        assert!(verify_artifact("engine", &artifact, false).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rootfs_identity_includes_executable_permissions() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().unwrap();
        let guest = directory.path().join("guest");
        fs::write(&guest, b"same bytes").unwrap();
        fs::set_permissions(&guest, fs::Permissions::from_mode(0o644)).unwrap();
        let before = super::tree_hash(directory.path()).unwrap();
        fs::set_permissions(&guest, fs::Permissions::from_mode(0o755)).unwrap();
        let after = super::tree_hash(directory.path()).unwrap();
        assert_ne!(before, after, "chmod must change the rootfs artifact identity");
    }

    #[cfg(unix)]
    #[test]
    fn rootfs_identity_includes_ownership() {
        let attributes = |uid, gid| {
            let mut identity = FramedIdentity::new(b"ownership-test").unwrap();
            super::unix_attributes(0o755, uid, gid, &mut identity).unwrap();
            identity.finish()
        };
        assert_ne!(attributes(1000, 1000), attributes(1001, 1000));
        assert_ne!(attributes(1000, 1000), attributes(1000, 1001));
    }

    #[cfg(unix)]
    #[test]
    fn rootfs_identity_includes_hardlink_topology() {
        use std::os::unix::fs::PermissionsExt as _;

        let linked = tempfile::tempdir().unwrap();
        fs::write(linked.path().join("a"), b"same bytes").unwrap();
        fs::hard_link(linked.path().join("a"), linked.path().join("b")).unwrap();

        let copied = tempfile::tempdir().unwrap();
        fs::write(copied.path().join("a"), b"same bytes").unwrap();
        fs::write(copied.path().join("b"), b"same bytes").unwrap();
        for root in [linked.path(), copied.path()] {
            fs::set_permissions(root.join("a"), fs::Permissions::from_mode(0o644)).unwrap();
            fs::set_permissions(root.join("b"), fs::Permissions::from_mode(0o644)).unwrap();
        }
        assert_ne!(
            super::tree_hash(linked.path()).unwrap(),
            super::tree_hash(copied.path()).unwrap()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn rootfs_identity_includes_extended_attributes() {
        let directory = tempfile::tempdir().unwrap();
        let guest = directory.path().join("guest");
        fs::write(&guest, b"same bytes").unwrap();
        let before = super::tree_hash(directory.path()).unwrap();
        xattr::set(&guest, "user.husklet-benchmark", b"changed behavior").unwrap();
        assert_ne!(before, super::tree_hash(directory.path()).unwrap());
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
