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
    schema: String,
    pub rounds: u32,
    pub samples_per_row: u32,
    pub rootfs: Artifact,
    pub arms: BTreeMap<String, Arm>,
    pub layouts: BTreeMap<String, Layout>,
    pub workloads: BTreeMap<String, Workload>,
    #[serde(default)]
    pub invariant_phases: Vec<String>,
}

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
    pub phases: Vec<String>,
    pub timeout_seconds: u64,
    #[serde(default)]
    pub wall_time: bool,
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
        }
        for (name, layout) in &self.layouts {
            if !phase_names_valid(&layout.phases, false) {
                return Err(format!("benchmark layout {name} is incomplete").into());
            }
        }
        for (name, workload) in &self.workloads {
            if workload.commands.is_empty()
                || !phase_names_valid(&workload.phases, false)
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
                || !arm.artifacts.values().any(|artifact| artifact.path == executable)
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
            workload
                .phases
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
    let mut identity = FramedIdentity::new(b"husklet-rootfs-tree-v3")?;
    permissions(&fs::symlink_metadata(root)?, &mut identity)?;
    walk(root, root, &mut identity, &mut BTreeMap::new())?;
    Ok(identity.finish())
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
    let mut identity = FramedIdentity::new(b"husklet-benchmark-file-v1")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        unix_attributes(
            metadata.permissions().mode(),
            metadata.uid(),
            metadata.gid(),
            &mut identity,
        )?;
    }
    #[cfg(not(unix))]
    identity.field(&[u8::from(metadata.permissions().readonly())])?;
    identity.field(&fs::read(path)?)?;
    Ok(identity.finish())
}

#[cfg(test)]
mod tests {
    use super::{
        Artifact, Layout, Workload, command_profiles_distinct, guest_is_hashed, invariant_phases_valid,
        phase_names_valid, smoke_binds_profile, verify_artifact, workload_judgments_covered,
    };
    use crate::record::FramedIdentity;
    use std::fs;

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
            phases: vec!["malloc".into()],
            timeout_seconds: 1,
            wall_time: false,
        };
        assert!(workload_judgments_covered("malloc", &workload, &layouts));
        workload.phases = vec!["sqlite".into()];
        assert!(!workload_judgments_covered("malloc", &workload, &layouts));
        assert!(workload_judgments_covered("python", &workload, &layouts));
    }
}
