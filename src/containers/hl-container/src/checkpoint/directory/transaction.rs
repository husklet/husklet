use super::storage;
use super::{DirectoryGeneration, DirectoryImage, DirectoryImageState};
use crate::checkpoint::{CheckpointError, CheckpointImage};
#[cfg(not(unix))]
use std::fs::OpenOptions;
use std::num::NonZeroU64;

impl CheckpointImage for DirectoryImage {
    fn begin_until(&self, deadline: std::time::Instant) -> Result<NonZeroU64, CheckpointError> {
        if std::time::Instant::now() >= deadline {
            return Err(CheckpointError::deadline());
        }
        let mut state = self.state_until(deadline)?;
        if let Some((_, lease)) = state.transaction {
            if std::time::Instant::now() < lease {
                return Err(CheckpointError::busy());
            }
            state = self.abort_state(state, deadline)?;
        }
        let transaction = Self::next_transaction();
        state.transaction = Some((transaction, deadline));
        Ok(transaction)
    }

    fn put_until(
        &self,
        transaction: NonZeroU64,
        name: &str,
        bytes: &[u8],
        deadline: std::time::Instant,
    ) -> Result<(), CheckpointError> {
        let state = self.state_until(deadline)?;
        Self::validate_transaction(&state, transaction, deadline)?;
        #[cfg(unix)]
        Self::replace_at(&self.directory, &format!("{}/{name}", state.generation), bytes)?;
        #[cfg(not(unix))]
        Self::replace(&Self::path(&self.root.join(&state.generation), name)?, bytes)?;
        (std::time::Instant::now() < deadline)
            .then_some(())
            .ok_or_else(CheckpointError::deadline)
    }

    fn abort_until(&self, transaction: NonZeroU64, deadline: std::time::Instant) -> Result<(), CheckpointError> {
        if std::time::Instant::now() >= deadline {
            return Err(CheckpointError::deadline());
        }
        let state = self.state_until(deadline)?;
        if !matches!(state.transaction, Some((active, _)) if active == transaction) {
            return Err(CheckpointError::new("checkpoint transaction is not owned"));
        }
        self.abort_state(state, deadline).map(|_| ())
    }

    fn get(&self, name: &str) -> Result<Vec<u8>, CheckpointError> {
        let state = self.state()?;
        let current = state
            .current
            .as_ref()
            .ok_or_else(|| CheckpointError::new("checkpoint has no committed generation"))?;
        #[cfg(unix)]
        let bytes = Self::read(&self.hold_generation(current)?, name)?;
        #[cfg(not(unix))]
        let bytes = std::fs::read(Self::path(&self.generation_path(current), name)?)
            .map_err(|error| CheckpointError::new(format!("read checkpoint object: {error}")))?;
        Ok(bytes)
    }

    fn get_until(&self, name: &str, deadline: std::time::Instant) -> Result<Vec<u8>, CheckpointError> {
        let state = self.state_until(deadline)?;
        let current = state
            .current
            .as_ref()
            .ok_or_else(|| CheckpointError::new("checkpoint has no committed generation"))?;
        #[cfg(unix)]
        let bytes = Self::read(&self.hold_generation(current)?, name)?;
        #[cfg(not(unix))]
        let bytes = std::fs::read(Self::path(&self.generation_path(current), name)?)
            .map_err(|error| CheckpointError::new(format!("read checkpoint object: {error}")))?;
        (std::time::Instant::now() < deadline)
            .then_some(bytes)
            .ok_or_else(CheckpointError::deadline)
    }

    fn list(&self) -> Result<Vec<String>, CheckpointError> {
        let state = self.state()?;
        let Some(current) = &state.current else {
            return Ok(Vec::new());
        };
        let mut objects = Vec::new();
        #[cfg(unix)]
        {
            Self::collect_held(
                self.hold_generation(current)?,
                "",
                matches!(current, DirectoryGeneration::Namespace),
                &mut objects,
            )?;
        }
        #[cfg(not(unix))]
        {
            let current = self.generation_path(current);
            Self::collect(
                &current,
                &current,
                matches!(state.current, Some(DirectoryGeneration::Namespace)),
                &mut objects,
            )?;
        }
        objects.sort();
        Ok(objects)
    }

    fn list_until(&self, deadline: std::time::Instant) -> Result<Vec<String>, CheckpointError> {
        let state = self.state_until(deadline)?;
        let Some(current) = &state.current else {
            return (std::time::Instant::now() < deadline)
                .then(Vec::new)
                .ok_or_else(CheckpointError::deadline);
        };
        let mut objects = Vec::new();
        #[cfg(unix)]
        Self::collect_held_until(
            self.hold_generation(current)?,
            "",
            matches!(current, DirectoryGeneration::Namespace),
            &mut objects,
            Some(deadline),
        )?;
        #[cfg(not(unix))]
        {
            let current = self.generation_path(current);
            Self::collect_until(
                &current,
                &current,
                matches!(state.current, Some(DirectoryGeneration::Namespace)),
                &mut objects,
                Some(deadline),
            )?;
        }
        objects.sort();
        (std::time::Instant::now() < deadline)
            .then_some(objects)
            .ok_or_else(CheckpointError::deadline)
    }

    fn commit_until(
        &self,
        transaction: NonZeroU64,
        manifest: &[u8],
        deadline: std::time::Instant,
    ) -> Result<(), CheckpointError> {
        self.commit_inner(transaction, manifest, deadline)
    }
}

impl DirectoryImage {
    #[cfg(unix)]
    pub(super) fn finish_publication(
        state: &mut DirectoryImageState,
        generation: Vec<u8>,
        outcome: storage::PublicationOutcome,
    ) -> Result<(), CheckpointError> {
        state.current = Some(DirectoryGeneration::Named(state.generation.clone()));
        state.base = Some(generation);
        state.generation = Self::generation();
        match outcome {
            storage::PublicationOutcome::Durable => Ok(()),
            storage::PublicationOutcome::PublishedNotDurable(error) => Err(error),
        }
    }

    fn commit_inner(
        &self,
        transaction: NonZeroU64,
        manifest: &[u8],
        deadline: std::time::Instant,
    ) -> Result<(), CheckpointError> {
        let mut state = self.state_until(deadline)?;
        Self::validate_transaction(&state, transaction, deadline)?;
        #[cfg(unix)]
        Self::replace_at(&self.directory, &format!("{}/MANIFEST", state.generation), manifest)?;
        #[cfg(not(unix))]
        Self::replace(&self.root.join(&state.generation).join("MANIFEST"), manifest)?;
        #[cfg(unix)]
        {
            use std::os::fd::AsFd as _;
            let staging = Self::open_directory(&self.directory, &state.generation)?;
            Self::sync_tree(staging)?;
            nix::unistd::fsync(self.directory.as_fd())
                .map_err(|error| CheckpointError::new(format!("sync checkpoint namespace: {error}")))?;
        }
        #[cfg(unix)]
        let lock = {
            use nix::fcntl::{OFlag, openat};
            use nix::sys::stat::{Mode, SFlag, fstat};
            let descriptor = openat(
                &self.directory,
                ".publication.lock",
                OFlag::O_RDWR | OFlag::O_CREAT | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK,
                Mode::from_bits_truncate(0o600),
            )
            .map_err(|error| CheckpointError::new(format!("open checkpoint publication lock: {error}")))?;
            let kind = SFlag::from_bits_truncate(
                fstat(&descriptor)
                    .map_err(|error| CheckpointError::new(format!("inspect checkpoint publication lock: {error}")))?
                    .st_mode,
            );
            if !kind.contains(SFlag::S_IFREG) {
                return Err(CheckpointError::new(
                    "checkpoint publication lock is not a regular file",
                ));
            }
            std::fs::File::from(descriptor)
        };
        #[cfg(not(unix))]
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(self.root.join(".publication.lock"))
            .map_err(|error| CheckpointError::new(format!("open checkpoint publication lock: {error}")))?;
        Self::lock_publication(&lock, Some(deadline))?;
        #[cfg(unix)]
        let published = Self::read_optional(&self.directory, "current")?;
        #[cfg(not(unix))]
        let published = match std::fs::read(self.root.join("current")) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(CheckpointError::new(format!(
                    "read checkpoint current generation: {error}"
                )));
            }
        };
        if published != state.base {
            return Err(CheckpointError::new(
                "checkpoint generation changed while capture was in progress",
            ));
        }
        if std::time::Instant::now() >= deadline {
            return Err(CheckpointError::deadline());
        }
        let generation = state.generation.as_bytes().to_vec();
        #[cfg(unix)]
        let publication = Self::replace_at_outcome(&self.directory, "current", &generation)?;
        #[cfg(not(unix))]
        {
            Self::replace(&self.root.join("current"), &generation)?;
            state.current = Some(DirectoryGeneration::Named(state.generation.clone()));
            state.base = Some(generation);
            state.generation = Self::generation();
        }
        #[cfg(unix)]
        {
            let result = Self::finish_publication(&mut state, generation, publication);
            state.transaction = None;
            result
        }
        #[cfg(not(unix))]
        {
            state.transaction = None;
            Ok(())
        }
    }
}
