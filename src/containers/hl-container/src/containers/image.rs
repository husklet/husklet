use super::Containers;
use crate::{Container, ContainerSpec, Result};

#[derive(Clone, Debug, Default)]
pub struct CommitMetadata {
    pub author: Option<String>,
    pub comment: Option<String>,
    pub changes: Vec<String>,
}

impl Containers {
    /// Validate commit configuration changes against the inherited image metadata without mutation.
    ///
    /// # Errors
    /// Returns lookup, image metadata, or change validation failures.
    pub async fn validate_commit(&self, reference: &str, changes: &[String]) -> Result<()> {
        if changes.is_empty() {
            return Ok(());
        }
        let container = self.inspect(reference).await?;
        let parent_name = container.spec.image.as_ref().ok_or_else(|| {
            crate::Error::InvalidSpec("commit changes require an image-backed container".into())
        })?;
        let images = self.images()?;
        let parent = images.resolve(parent_name)?.ok_or_else(|| {
            crate::Error::Corrupt(format!("parent image {parent_name} is missing"))
        })?;
        let platform = match container.spec.guest {
            crate::Guest::Aarch64 => hl_images::Platform::linux_arm64(),
            crate::Guest::X86_64 => hl_images::Platform::linux_amd64(),
        };
        let mut metadata = images.details(&parent, &platform)?;
        hl_images::build::Changes::new(changes)
            .apply(&mut metadata)
            .map_err(Into::into)
    }
    /// Snapshot a stopped container's merged filesystem as a named OCI image.
    ///
    /// # Errors
    /// Returns lookup, lifecycle, filesystem, archive, or image persistence failures.
    pub async fn commit(
        &self,
        reference: &str,
        name: hl_images::Reference,
        commit: CommitMetadata,
    ) -> Result<hl_images::Image> {
        let container = self.inspect(reference).await?;
        if container.state.is_active() && !container.state.is_paused() {
            return Err(crate::Error::InvalidState {
                id: container.id,
                actual: container.state,
                expected: "stopped for a coherent image commit",
            });
        }
        let process = &container.spec.process;
        let runtime = hl_images::RuntimeConfig {
            entrypoint: vec![process.program.clone()],
            command: process.args.clone(),
            environment: process.env.text()?,
            working_directory: process.working_dir.to_string_lossy().into_owned(),
            user: process.uid.map_or_else(String::new, |uid| {
                process
                    .gid
                    .map_or_else(|| uid.to_string(), |gid| format!("{uid}:{gid}"))
            }),
        };
        let platform = match container.spec.guest {
            crate::Guest::Aarch64 => hl_images::Platform::linux_arm64(),
            crate::Guest::X86_64 => hl_images::Platform::linux_amd64(),
        };
        let images = self.images()?;
        if let (crate::Rootfs::Image(rootfs), Some(parent_name)) =
            (&container.spec.rootfs, &container.spec.image)
        {
            if let Ok(overlay) = images.roots().open_overlay(rootfs) {
                let parent = images.resolve(parent_name)?.ok_or_else(|| {
                    crate::Error::Corrupt(format!("parent image {parent_name} is missing"))
                })?;
                let mut metadata = images.details(&parent, &platform)?;
                metadata.runtime = runtime;
                metadata.author = commit.author;
                hl_images::build::Changes::new(&commit.changes).apply(&mut metadata)?;
                metadata.history.push(hl_images::History {
                    created_by: Some("hl commit".into()),
                    comment: commit.comment,
                    ..hl_images::History::default()
                });
                let mut layer = Vec::new();
                overlay.archive_upper(&mut layer)?;
                return images
                    .commit_child(&parent, std::io::Cursor::new(layer), &name, &metadata)
                    .map_err(Into::into);
            }
        }
        let mut layer = Vec::new();
        self.filesystem(reference).await?.archive("/", &mut layer)?;
        images
            .commit(&layer, &runtime, &platform, &name)
            .map_err(Into::into)
    }

    /// Returns the runtime-neutral OCI image service used by this container service.
    ///
    /// Pull, unpack, inspect, import, and rootfs pinning remain `hl-images` operations rather than
    /// Docker protocol behavior.
    ///
    /// # Errors
    /// Returns an internal composition error if an injected test service omitted image storage.
    pub fn images(&self) -> Result<hl_images::Images> {
        self.service
            .images()
            .ok_or_else(|| crate::Error::Corrupt("image service is not configured".into()))
    }

    /// Forks an unpacked OCI image and atomically transfers the private rootfs into a container.
    ///
    /// `configure` may set the name, guest, mounts, resources, and isolation without exposing a
    /// snapshot path. A failed create releases the newly acquired lease.
    ///
    /// # Errors
    /// Returns image pinning, validation, uniqueness, or persistence failures.
    pub async fn create_image(
        &self,
        image: &hl_images::UnpackedImage,
        overrides: hl_images::RuntimeOverrides,
        configure: impl FnOnce(ContainerSpec) -> ContainerSpec,
    ) -> Result<Container> {
        let images = self.images()?;
        let runtime = image.runtime().merge(overrides)?;
        let mut arguments = runtime.entrypoint;
        arguments.extend(runtime.command);
        let Some(program) = arguments.first().cloned() else {
            return Err(crate::Error::InvalidSpec(
                "image and container configuration provide no command".into(),
            ));
        };
        let mut process = crate::Process::new(program)
            .args(arguments.into_iter().skip(1))
            .working_dir(runtime.working_directory);
        for (name, value) in runtime.environment {
            process = process.env(name, value);
        }
        let root_store = images.roots();
        let candidate = root_store.fork_overlay(image.snapshot())?;
        let mut rootfs = match root_store.open_overlay(&candidate) {
            Ok(view)
                if self
                    .service
                    .validate_overlay(&crate::service::OverlayConfig {
                        lower: view.lower().to_owned(),
                        upper: view.upper().to_owned(),
                        work: view.work().to_owned(),
                    }) =>
            {
                candidate
            }
            _ => {
                root_store.release(&candidate)?;
                images.rootfs(image)?
            }
        };
        if !runtime.user.is_empty() {
            let path = match root_store.open_overlay(&rootfs) {
                Ok(root) => root.lower().to_owned(),
                Err(_) => root_store.open(&rootfs)?.path().to_owned(),
            };
            match crate::Process::resolve_user(&runtime.user, &path) {
                Ok((uid, gid)) => process = process.user(uid, gid),
                Err(error) => {
                    images.roots().release(&rootfs)?;
                    return Err(error);
                }
            }
        }
        let mut spec = configure(
            ContainerSpec::new(rootfs.clone(), process)
                .image(image.image().name.clone())
                .guest(crate::Guest::for_platform(image.platform())?),
        );
        if spec.rootfs != crate::Rootfs::Image(rootfs.clone()) {
            images.roots().release(&rootfs)?;
            return Err(crate::Error::InvalidSpec(
                "image configuration must not replace its owned rootfs".into(),
            ));
        }
        if rootfs.overlay().is_some() && !spec.mounts.is_empty() {
            root_store.release(&rootfs)?;
            rootfs = images.rootfs(image)?;
            spec.rootfs = crate::Rootfs::Image(rootfs.clone());
        }
        if let Err(error) = spec.validate() {
            images.roots().release(&rootfs)?;
            return Err(error);
        }
        let root = images.roots().open(&rootfs)?;
        if let Err(error) = self.volumes.populate(&spec.mounts, root.path()).await {
            images.roots().release(&rootfs)?;
            return Err(error);
        }
        match self.create(spec).await {
            Ok(container) => Ok(container),
            Err(error) => {
                if let Err(cleanup) = images.roots().release(&rootfs) {
                    return Err(crate::Error::Corrupt(format!(
                        "container creation failed ({error}); rootfs cleanup also failed ({cleanup})"
                    )));
                }
                Err(error)
            }
        }
    }
}
