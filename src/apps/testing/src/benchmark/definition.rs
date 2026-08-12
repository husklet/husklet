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
        }
        let declared = self
            .layouts
            .values()
            .flat_map(|layout| &layout.phases)
            .chain(self.workloads.values().flat_map(|workload| &workload.phases))
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        if !invariant_phases_valid(&self.invariant_phases, &declared) {
            return Err("benchmark invariant phases must be unique declared phases".into());
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
                || arm.smoke[0] != arm.command[0]
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

fn phase_names_valid(phases: &[String], allow_empty: bool) -> bool {
    (allow_empty || !phases.is_empty())
        && phases.iter().all(|phase| !phase.is_empty())
        && phases.iter().collect::<BTreeSet<_>>().len() == phases.len()
}

fn invariant_phases_valid(invariants: &[String], declared: &BTreeSet<&str>) -> bool {
    phase_names_valid(invariants, true) && invariants.iter().all(|phase| declared.contains(phase.as_str()))
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
        FramedIdentity::of(&fs::read(&artifact.path)?)
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
    fn walk(root: &Path, directory: &Path, identity: &mut FramedIdentity) -> Result<(), Error> {
        let mut entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let relative = path.strip_prefix(root)?;
            identity.field(relative.as_os_str().as_encoded_bytes())?;
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                identity.field(b"L")?;
                identity.field(fs::read_link(path)?.as_os_str().as_encoded_bytes())?;
            } else if metadata.is_dir() {
                identity.field(b"D")?;
                walk(root, &path, identity)?;
            } else if metadata.is_file() {
                identity.field(b"F")?;
                identity.field(&fs::read(path)?)?;
            } else {
                return Err("rootfs contains an unsupported entry type".into());
            }
        }
        Ok(())
    }
    let mut identity = FramedIdentity::new(b"husklet-rootfs-tree-v1")?;
    walk(root, root, &mut identity)?;
    Ok(identity.finish())
}

pub(super) fn artifact_identity(path: &Path) -> Result<String, Error> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() {
        tree_hash(path)
    } else if metadata.is_file() {
        Ok(FramedIdentity::of(&fs::read(path)?))
    } else {
        Err("benchmark artifact is neither a regular file nor a directory".into())
    }
}

#[cfg(test)]
mod tests {
    use super::{Artifact, guest_is_hashed, invariant_phases_valid, phase_names_valid, verify_artifact};
    use crate::record::FramedIdentity;
    use std::fs;

    #[test]
    fn regular_file_artifact_is_accepted() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("engine");
        fs::write(&path, b"engine").unwrap();
        let artifact = Artifact {
            path,
            sha256: FramedIdentity::of(b"engine"),
        };
        verify_artifact("engine", &artifact, false).unwrap();
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
            sha256: FramedIdentity::of(b"engine"),
        };
        assert!(verify_artifact("engine", &artifact, false).is_err());
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
        assert!(!invariant_phases_valid(&["typo".into()], &declared));
    }
}
