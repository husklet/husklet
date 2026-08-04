use super::{
    Container, ContainerId, Deserialize, Disk, Error, Exec, ExecId, File, Network, OpenOptions, Path, PathBuf, Require,
    Result, Serialize, VERSION, Volume, Write, fs,
};

#[derive(Deserialize, Serialize)]
struct ContainerRecord {
    version: u32,
    container: Container,
}

#[derive(Deserialize, Serialize)]
struct ExecRecord {
    version: u32,
    exec: Exec,
}

#[derive(Deserialize, Serialize)]
struct VolumeRecord {
    version: u32,
    volume: Volume,
}
#[derive(Deserialize, Serialize)]
struct NetworkRecord {
    version: u32,
    network: Network,
}

impl Disk {
    pub(super) fn path(&self, id: &ContainerId) -> PathBuf {
        self.directory.join(format!("{}.json", id.as_str()))
    }
    pub(super) fn exec_path(&self, id: &ExecId) -> PathBuf {
        self.execs.join(format!("{}.json", id.as_str()))
    }

    pub(super) fn volume_path(&self, name: &str) -> PathBuf {
        self.volumes.join(format!("{name}.json"))
    }

    pub(super) fn network_path(&self, name: &str) -> PathBuf {
        self.networks.join(format!("{name}.json"))
    }

    pub(super) fn read_network(path: &Path) -> Result<Network> {
        let record: NetworkRecord = serde_json::from_reader(File::open(path)?)?;
        if record.version != VERSION {
            return Err(Error::Corrupt(format!(
                "unsupported network record version {} in {}",
                record.version,
                path.display()
            )));
        }
        record.network.validate()?;
        Ok(record.network)
    }

    pub(super) fn list_networks_sync(&self) -> Result<Vec<Network>> {
        let _guard = self
            .transaction
            .lock()
            .map_err(|_| Error::Corrupt("repository lock poisoned".into()))?;
        let mut values = Vec::new();
        for entry in fs::read_dir(&self.networks)? {
            let path = entry?.path();
            if path.extension() != Some(std::ffi::OsStr::new("json")) {
                continue;
            }
            values.push(Self::read_network(&path)?);
        }
        Ok(values)
    }

    pub(super) fn get_network_sync(&self, name: &str) -> Result<Option<Network>> {
        let _guard = self
            .transaction
            .lock()
            .map_err(|_| Error::Corrupt("repository lock poisoned".into()))?;
        let path = self.network_path(name);
        if !path.exists() {
            return Ok(None);
        }
        Self::read_network(&path).map(Some)
    }

    pub(super) fn write_network_sync(&self, network: &Network, require: Require) -> Result<()> {
        let _guard = self
            .transaction
            .lock()
            .map_err(|_| Error::Corrupt("repository lock poisoned".into()))?;
        let path = self.network_path(&network.name);
        match (path.exists(), require) {
            (true, Require::Absent) => return Err(Error::NetworkConflict(network.name.clone())),
            (false, Require::Present) => return Err(Error::NetworkNotFound(network.name.clone())),
            _ => {}
        }
        let temporary = self
            .networks
            .join(format!(".{}.{}.tmp", network.name, uuid::Uuid::new_v4().simple()));
        let result = (|| {
            let mut file = OpenOptions::new().create_new(true).write(true).open(&temporary)?;
            serde_json::to_writer(
                &mut file,
                &NetworkRecord {
                    version: VERSION,
                    network: network.clone(),
                },
            )?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            fs::rename(&temporary, path)?;
            Self::sync_directory(&self.networks)
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }

    pub(super) fn remove_network_sync(&self, name: &str) -> Result<()> {
        let _guard = self
            .transaction
            .lock()
            .map_err(|_| Error::Corrupt("repository lock poisoned".into()))?;
        fs::remove_file(self.network_path(name)).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Error::NetworkNotFound(name.into())
            } else {
                Error::Io(error)
            }
        })?;
        Self::sync_directory(&self.networks)
    }

    pub(super) fn read_volume(path: &Path) -> Result<Volume> {
        let record: VolumeRecord = serde_json::from_reader(File::open(path)?)?;
        if record.version != VERSION {
            return Err(Error::Corrupt(format!(
                "unsupported volume record version {} in {}",
                record.version,
                path.display()
            )));
        }
        Ok(record.volume)
    }

    pub(super) fn list_volumes_sync(&self) -> Result<Vec<Volume>> {
        let _guard = self
            .transaction
            .lock()
            .map_err(|_| Error::Corrupt("repository lock poisoned".into()))?;
        let mut volumes = Vec::new();
        for entry in fs::read_dir(&self.volumes)? {
            let path = entry?.path();
            if path.extension() != Some(std::ffi::OsStr::new("json")) {
                continue;
            }
            volumes.push(Self::read_volume(&path)?);
        }
        Ok(volumes)
    }

    pub(super) fn get_volume_sync(&self, name: &str) -> Result<Option<Volume>> {
        let _guard = self
            .transaction
            .lock()
            .map_err(|_| Error::Corrupt("repository lock poisoned".into()))?;
        let path = self.volume_path(name);
        if !path.exists() {
            return Ok(None);
        }
        Self::read_volume(&path).map(Some)
    }

    pub(super) fn insert_volume_sync(&self, volume: &Volume) -> Result<()> {
        let _guard = self
            .transaction
            .lock()
            .map_err(|_| Error::Corrupt("repository lock poisoned".into()))?;
        let path = self.volume_path(&volume.name);
        if path.exists() {
            return Err(Error::VolumeConflict(volume.name.clone()));
        }
        let temporary = self
            .volumes
            .join(format!(".{}.{}.tmp", volume.name, uuid::Uuid::new_v4().simple()));
        let result = (|| {
            let mut file = OpenOptions::new().create_new(true).write(true).open(&temporary)?;
            serde_json::to_writer(
                &mut file,
                &VolumeRecord {
                    version: VERSION,
                    volume: volume.clone(),
                },
            )?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            fs::rename(&temporary, path)?;
            Self::sync_directory(&self.volumes)
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }

    pub(super) fn remove_volume_sync(&self, name: &str) -> Result<()> {
        let _guard = self
            .transaction
            .lock()
            .map_err(|_| Error::Corrupt("repository lock poisoned".into()))?;
        fs::remove_file(self.volume_path(name)).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Error::VolumeNotFound(name.into())
            } else {
                Error::Io(error)
            }
        })?;
        Self::sync_directory(&self.volumes)
    }

    pub(super) fn read(path: &Path) -> Result<Container> {
        let record: ContainerRecord = serde_json::from_reader(File::open(path)?)?;
        if record.version != VERSION {
            return Err(Error::Corrupt(format!(
                "unsupported record version {} in {}",
                record.version,
                path.display()
            )));
        }
        Ok(record.container)
    }

    pub(super) fn write(&self, container: &Container, require: Require) -> Result<()> {
        let _guard = self
            .transaction
            .lock()
            .map_err(|_| Error::Corrupt("repository lock poisoned".into()))?;
        let path = self.path(&container.id);
        match (path.exists(), require) {
            (true, Require::Absent) => {
                return Err(Error::Corrupt(format!("duplicate container {}", container.id)));
            }
            (false, Require::Present) => return Err(Error::NotFound(container.id.to_string())),
            _ => {}
        }
        let temporary = self
            .directory
            .join(format!(".{}.{}.tmp", container.id, uuid::Uuid::new_v4().simple()));
        let result = (|| {
            let mut file = OpenOptions::new().create_new(true).write(true).open(&temporary)?;
            serde_json::to_writer(
                &mut file,
                &ContainerRecord {
                    version: VERSION,
                    container: container.clone(),
                },
            )?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            fs::rename(&temporary, &path)?;
            Self::sync_directory(&self.directory)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }

    pub(super) fn list_sync(&self) -> Result<Vec<Container>> {
        let _guard = self
            .transaction
            .lock()
            .map_err(|_| Error::Corrupt("repository lock poisoned".into()))?;
        let mut values = Vec::new();
        for entry in fs::read_dir(&self.directory)? {
            let entry = entry?;
            if entry.path().extension() != Some(std::ffi::OsStr::new("json")) {
                continue;
            }
            values.push(Self::read(&entry.path())?);
        }
        Ok(values)
    }

    pub(super) fn get_sync(&self, id: &ContainerId) -> Result<Option<Container>> {
        let _guard = self
            .transaction
            .lock()
            .map_err(|_| Error::Corrupt("repository lock poisoned".into()))?;
        let path = self.path(id);
        if !path.exists() {
            return Ok(None);
        }
        Self::read(&path).map(Some)
    }

    pub(super) fn remove_sync(&self, id: &ContainerId) -> Result<()> {
        let _guard = self
            .transaction
            .lock()
            .map_err(|_| Error::Corrupt("repository lock poisoned".into()))?;
        fs::remove_file(self.path(id)).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Error::NotFound(id.to_string())
            } else {
                Error::Io(error)
            }
        })?;
        Self::sync_directory(&self.directory)
    }

    pub(super) fn read_exec(path: &Path) -> Result<Exec> {
        let record: ExecRecord = serde_json::from_reader(File::open(path)?)?;
        if record.version != VERSION {
            return Err(Error::Corrupt(format!(
                "unsupported exec record version {} in {}",
                record.version,
                path.display()
            )));
        }
        Ok(record.exec)
    }

    pub(super) fn write_exec(&self, exec: &Exec, require: Require) -> Result<()> {
        let _guard = self
            .transaction
            .lock()
            .map_err(|_| Error::Corrupt("repository lock poisoned".into()))?;
        let path = self.exec_path(&exec.id);
        match (path.exists(), require) {
            (true, Require::Absent) => {
                return Err(Error::Corrupt(format!("duplicate exec {}", exec.id)));
            }
            (false, Require::Present) => return Err(Error::NotFound(exec.id.to_string())),
            _ => {}
        }
        let temporary = self
            .execs
            .join(format!(".{}.{}.tmp", exec.id, uuid::Uuid::new_v4().simple()));
        let result = (|| {
            let mut file = OpenOptions::new().create_new(true).write(true).open(&temporary)?;
            serde_json::to_writer(
                &mut file,
                &ExecRecord {
                    version: VERSION,
                    exec: exec.clone(),
                },
            )?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            fs::rename(&temporary, &path)?;
            Self::sync_directory(&self.execs)
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }

    pub(super) fn list_execs_sync(&self) -> Result<Vec<Exec>> {
        let _guard = self
            .transaction
            .lock()
            .map_err(|_| Error::Corrupt("repository lock poisoned".into()))?;
        let mut values = Vec::new();
        for entry in fs::read_dir(&self.execs)? {
            let path = entry?.path();
            if path.extension() != Some(std::ffi::OsStr::new("json")) {
                continue;
            }
            values.push(Self::read_exec(&path)?);
        }
        Ok(values)
    }

    pub(super) fn get_exec_sync(&self, id: &ExecId) -> Result<Option<Exec>> {
        let _guard = self
            .transaction
            .lock()
            .map_err(|_| Error::Corrupt("repository lock poisoned".into()))?;
        let path = self.exec_path(id);
        if !path.exists() {
            return Ok(None);
        }
        Self::read_exec(&path).map(Some)
    }

    pub(super) fn remove_exec_sync(&self, id: &ExecId) -> Result<()> {
        let _guard = self
            .transaction
            .lock()
            .map_err(|_| Error::Corrupt("repository lock poisoned".into()))?;
        fs::remove_file(self.exec_path(id)).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Error::NotFound(id.to_string())
            } else {
                Error::Io(error)
            }
        })?;
        Self::sync_directory(&self.execs)
    }

    pub(super) fn remove_parent_sync(&self, id: &ContainerId) -> Result<()> {
        let _guard = self
            .transaction
            .lock()
            .map_err(|_| Error::Corrupt("repository lock poisoned".into()))?;
        let mut changed = false;
        for entry in fs::read_dir(&self.execs)? {
            let path = entry?.path();
            if path.extension() != Some(std::ffi::OsStr::new("json")) {
                continue;
            }
            if Self::read_exec(&path)?.container != *id {
                continue;
            }
            fs::remove_file(path)?;
            changed = true;
        }
        if changed {
            Self::sync_directory(&self.execs)?;
        }
        Ok(())
    }
}
