use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Write,
    path::{Component, Path, PathBuf},
};

use crate::{Digest, Error, Result};

#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(transparent)]
struct Entries(BTreeMap<String, String>);

/// How an existing host directory already spells the requested guest name.
enum Sibling {
    Exact(PathBuf),
    Collision,
    None,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Name(String);

impl Name {
    pub(super) fn normalize(path: &Path) -> PathBuf {
        let mut parts = Vec::new();
        for component in path.components() {
            match component {
                Component::Normal(part) => parts.push(part.to_owned()),
                Component::ParentDir => {
                    parts.pop();
                }
                _ => {}
            }
        }
        parts.into_iter().collect()
    }

    pub(super) fn relative(from: &Path, to: &Path) -> PathBuf {
        let from = from.components().collect::<Vec<_>>();
        let to = to.components().collect::<Vec<_>>();
        let shared = from.iter().zip(&to).take_while(|(left, right)| left == right).count();
        let mut path = PathBuf::new();
        for _ in shared..from.len() {
            path.push("..");
        }
        for component in &to[shared..] {
            path.push(component.as_os_str());
        }
        path
    }

    fn from_path(path: &Path) -> Result<Self> {
        let value = path
            .to_str()
            .ok_or_else(|| Error::InvalidMetadata("snapshot name path is not UTF-8".into()))?;
        if value.is_empty() || path.is_absolute() || path.components().any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(Error::InvalidMetadata(format!(
                "unsafe snapshot name path {}",
                path.display()
            )));
        }
        Ok(Self(value.to_owned()))
    }

    fn encoded(&self, salt: u32) -> Self {
        let material = if salt == 0 {
            self.0.clone()
        } else {
            format!("{}\0{salt}", self.0)
        };
        let hash = Digest::sha256(material.as_bytes());
        let encoded = format!(".hl-name-{}", hash.encoded());
        let parent = self.as_path().parent().and_then(Path::to_str).unwrap_or("");
        if parent.is_empty() {
            Self(encoded)
        } else {
            Self(format!("{parent}/{encoded}"))
        }
    }

    fn as_path(&self) -> &Path {
        Path::new(&self.0)
    }

    fn into_string(self) -> String {
        self.0
    }
}

/// Guest-to-physical names for entries encoded to preserve case-distinct siblings.
///
/// Literal entries are intentionally absent: the host path is already the guest path.
#[derive(Clone, Debug)]
pub struct Names {
    path: Option<PathBuf>,
    entries: Entries,
}

impl Names {
    #[must_use]
    pub fn memory() -> Self {
        Self {
            path: None,
            entries: Entries::default(),
        }
    }

    pub(super) fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub(super) fn create(path: PathBuf) -> Result<Self> {
        let names = Self {
            path: Some(path),
            entries: Entries::default(),
        };
        names.save()?;
        Ok(names)
    }

    pub(super) fn open(path: PathBuf) -> Result<Self> {
        let bytes = fs::read(&path).map_err(|source| Error::LayerFilesystem {
            operation: "read snapshot names",
            path: path.clone(),
            source,
        })?;
        let entries: Entries = serde_json::from_slice(&bytes).map_err(|error| {
            Error::InvalidMetadata(format!("malformed snapshot names sidecar {}: {error}", path.display()))
        })?;
        for (physical, guest) in &entries.0 {
            let physical = Name::from_path(Path::new(physical))?;
            Name::from_path(Path::new(guest))?;
            if !physical
                .as_path()
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(".hl-name-"))
            {
                return Err(Error::InvalidMetadata(
                    "encoded physical name lacks reserved prefix".into(),
                ));
            }
        }
        Ok(Self {
            path: Some(path),
            entries,
        })
    }

    pub(super) fn fork(source: &Path, path: PathBuf) -> Result<Self> {
        let mut names = Self::open(source.to_owned())?;
        names.path = Some(path);
        names.save()?;
        Ok(names)
    }

    /// Resolve a guest-relative path to its physical snapshot-relative path.
    #[must_use]
    pub fn physical<'a>(&'a self, guest: &'a Path) -> &'a Path {
        self.entries
            .0
            .iter()
            .find_map(|(physical, mapped)| (Path::new(mapped) == guest).then(|| Path::new(physical)))
            .unwrap_or(guest)
    }

    /// Resolve a physical snapshot-relative path to its guest-visible path.
    #[must_use]
    pub fn guest<'a>(&'a self, physical: &'a Path) -> &'a Path {
        self.entries
            .0
            .get(physical.to_str().unwrap_or_default())
            .map_or(physical, Path::new)
    }

    /// Iterate encoded mappings as `(physical, guest)` pairs in deterministic order.
    pub fn iter(&self) -> impl Iterator<Item = (&Path, &Path)> {
        self.entries
            .0
            .iter()
            .map(|(physical, guest)| (Path::new(physical), Path::new(guest)))
    }

    /// Persist a stable physical path for a guest name that collides on the host.
    ///
    /// # Errors
    /// Returns an error for unsafe paths or sidecar persistence failures.
    pub fn encode(&mut self, guest: &Path) -> Result<PathBuf> {
        let guest = Name::from_path(guest)?;
        if self.physical(guest.as_path()) != guest.as_path() {
            return Ok(self.physical(guest.as_path()).to_owned());
        }
        let physical = guest.encoded(0);
        let path = physical.as_path().to_owned();
        self.entries.0.insert(physical.into_string(), guest.into_string());
        self.save()?;
        Ok(path)
    }

    /// Resolve an archive guest path, encoding only a host-colliding sibling.
    pub(crate) fn resolve(&mut self, root: &Path, guest: &Path) -> Result<PathBuf> {
        let guest = Name::from_path(guest)?;
        if self.physical(guest.as_path()) != guest.as_path() {
            return Ok(self.physical(guest.as_path()).to_owned());
        }
        let guest_parent = guest.as_path().parent().unwrap_or(Path::new(""));
        let physical_parent = if guest_parent.as_os_str().is_empty() {
            PathBuf::new()
        } else {
            // A case-distinct ancestor may itself be encoded. Resolve it first so
            // descendants cannot fall back to the host's case-insensitive alias
            // and overwrite the sibling that caused the encoding.
            self.resolve(root, guest_parent)?
        };
        let name = guest
            .as_path()
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                Error::InvalidMetadata(format!("snapshot name is not UTF-8: {}", guest.as_path().display()))
            })?;
        let directory = root.join(&physical_parent);
        if !directory.is_dir() {
            return Ok(physical_parent.join(name));
        }
        match self.sibling(&directory, &physical_parent, name)? {
            Sibling::Exact(physical) => Ok(physical),
            Sibling::Collision => self.encode_sibling(root, &physical_parent, guest),
            Sibling::None => Ok(physical_parent.join(name)),
        }
    }

    /// Classifies the existing host siblings against the requested guest name.
    fn sibling(&self, directory: &Path, physical_parent: &Path, name: &str) -> Result<Sibling> {
        let mut collides = false;
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let physical = physical_parent.join(entry.file_name());
            let visible = self.guest(&physical);
            let Some(visible_name) = visible.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            if visible_name == name {
                return Ok(Sibling::Exact(physical));
            }
            collides |= visible_name.eq_ignore_ascii_case(name);
        }
        Ok(if collides { Sibling::Collision } else { Sibling::None })
    }

    fn encode_sibling(&mut self, root: &Path, physical_parent: &Path, guest: Name) -> Result<PathBuf> {
        for salt in 0..u32::MAX {
            let encoded = guest.encoded(salt);
            let candidate = Name::from_path(
                &physical_parent.join(encoded.as_path().file_name().expect("encoded name has a leaf")),
            )?;
            if !root.join(candidate.as_path()).exists() || self.guest(candidate.as_path()) == guest.as_path() {
                let path = candidate.as_path().to_owned();
                self.entries.0.insert(candidate.into_string(), guest.into_string());
                self.save()?;
                return Ok(path);
            }
        }
        Err(Error::InvalidMetadata("snapshot name encoding space exhausted".into()))
    }

    pub(super) fn relocate(&mut self, path: PathBuf) {
        self.path = Some(path);
    }

    fn save(&self) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let parent = path
            .parent()
            .ok_or_else(|| Error::InvalidMetadata("snapshot names sidecar has no parent".into()))?;
        fs::create_dir_all(parent)?;
        let temporary = parent.join(format!(
            ".{}.{}.tmp",
            path.file_name().and_then(|name| name.to_str()).unwrap_or("names"),
            uuid::Uuid::new_v4().simple()
        ));
        let result = (|| -> Result<()> {
            let mut file = File::create(&temporary)?;
            serde_json::to_writer(&mut file, &self.entries)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            fs::rename(&temporary, path)?;
            File::open(parent)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}
