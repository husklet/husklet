//! Exact OCI fixture materialization shared by real-image compatibility groups.

use hl_images::{
    format::docker::{Archive, Limits},
    remote::{Auth, Registry},
    rootfs::{Reference as RootReference, View},
    FsImageStore, Image, ImageStore, Images, Platform, Reference, RuntimeConfig,
};
use std::{
    env,
    fs::File,
    io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use tokio::time::{sleep, Duration};

type Error = Box<dyn std::error::Error>;

pub(crate) struct Fixture {
    images: Images,
    reference: RootReference,
    view: View,
    runtime: RuntimeConfig,
}

impl Fixture {
    pub(crate) async fn materialize(raw: &str) -> Result<Self, Error> {
        Self::materialize_for(raw, &Platform::linux_arm64()).await
    }

    pub(crate) async fn materialize_for(raw: &str, platform: &Platform) -> Result<Self, Error> {
        Self::materialize_inner(raw, platform)
            .await
            .map_err(|error| format!("image materialization failed: {error}").into())
    }

    async fn materialize_inner(raw: &str, platform: &Platform) -> Result<Self, Error> {
        let parsed: Reference = raw.parse()?;
        let absent = env::var_os("HL_SCENARIO_OFFLINE").is_some()
            && FsImageStore::open(cache_root()?.join("metadata"))?
                .get(&parsed)?
                .is_none();
        if absent {
            return Err(format!(
                "image materialization unavailable offline: {raw} is absent from the prefetched cache"
            )
            .into());
        }
        let (images, reference) = cache(raw)?;
        let image = if let Some(image) = images.resolve(&reference)? {
            image
        } else {
            if env::var_os("HL_SCENARIO_DOCKER_IMAGES").is_some() {
                let _ = load_docker_image(&images, raw)?;
            }
            if let Some(image) = images.resolve(&reference)? {
                image
            } else {
                if env::var_os("HL_SCENARIO_OFFLINE").is_some() {
                    return Err(format!(
                        "image materialization unavailable offline: {raw} is absent from the prefetched cache"
                    )
                    .into());
                }
                pull(&images, reference, platform).await?
            }
        };
        Self::from_image(images, &image, platform)
    }

    fn from_image(images: Images, image: &Image, platform: &Platform) -> Result<Self, Error> {
        let unpacked = images
            .unpack(image, platform)
            .map_err(|error| format!("unpack {}: {error}", image.name))?;
        let runtime = unpacked.runtime().clone();
        let reference = images
            .rootfs(&unpacked)
            .map_err(|error| format!("fork rootfs for {}: {error}", image.name))?;
        let view = images
            .roots()
            .open(&reference)
            .map_err(|error| format!("open rootfs for {}: {error}", image.name))?;
        Ok(Self {
            images,
            reference,
            view,
            runtime,
        })
    }

    pub(crate) fn path(&self) -> &Path {
        self.view.path()
    }

    pub(crate) fn runtime(&self) -> &RuntimeConfig {
        &self.runtime
    }

    pub(crate) fn release(self) -> Result<(), Error> {
        self.images.roots().release(&self.reference)?;
        Ok(())
    }
}

async fn pull(images: &Images, reference: Reference, platform: &Platform) -> Result<Image, Error> {
    let registry = Registry::new(registry_auth());
    let mut delay = Duration::from_secs(2);
    for attempt in 1..=3 {
        match images.pull(&registry, reference.clone(), platform).await {
            Ok(image) => return Ok(image),
            Err(error) if attempt < 3 && retryable(&error.to_string()) => {
                eprintln!(
                    "registry prefetch attempt {attempt} for {reference} failed; retrying in {}s: {error}",
                    delay.as_secs()
                );
                sleep(delay).await;
                delay *= 2;
            }
            Err(error) => return Err(error.into()),
        }
    }
    unreachable!()
}

fn registry_auth() -> Auth {
    if let Some(token) = env::var_os("HL_REGISTRY_TOKEN") {
        return Auth::Bearer(token.to_string_lossy().into_owned());
    }
    match (
        env::var_os("HL_REGISTRY_USERNAME"),
        env::var_os("HL_REGISTRY_PASSWORD"),
    ) {
        (Some(username), Some(password)) => Auth::Basic {
            username: username.to_string_lossy().into_owned(),
            password: password.to_string_lossy().into_owned(),
        },
        _ => Auth::Anonymous,
    }
}

fn retryable(error: &str) -> bool {
    [
        "429",
        "rate limit",
        "temporarily unavailable",
        "timeout",
        "connection reset",
    ]
    .iter()
    .any(|needle| error.to_ascii_lowercase().contains(needle))
}

fn load_docker_image(images: &Images, reference: &str) -> Result<bool, Error> {
    let direct = Command::new("docker")
        .args(["image", "inspect"])
        .arg(reference)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?
        .success();
    let mut command = if direct {
        Command::new("docker")
    } else {
        let mut command = Command::new("sudo");
        command.args(["-n", "docker"]);
        command
    };
    let mut archive = tempfile::NamedTempFile::new()?;
    let mut child = command
        .args(["image", "save"])
        .arg(reference)
        .stdout(Stdio::piped())
        .spawn()?;
    io::copy(
        child
            .stdout
            .as_mut()
            .ok_or("Docker save has no output stream")?,
        archive.as_file_mut(),
    )?;
    let status = child.wait()?;
    if !status.success() {
        return Ok(false);
    }
    Archive::load(File::open(archive.path())?, images, Limits::default())?;
    Ok(true)
}

fn cache(raw: &str) -> Result<(Images, Reference), Error> {
    let cache = cache_root()?;
    let images = Images::open(cache)?;
    let reference: Reference = raw.parse()?;
    Ok((images, reference))
}

fn cache_root() -> Result<PathBuf, Error> {
    let cache = env::var_os("HL_SCENARIO_IMAGE_CACHE")
        .map_or_else(|| PathBuf::from("target/scenarios/images"), PathBuf::from);
    let cache = if cache.is_absolute() {
        cache
    } else {
        env::current_dir()?.join(cache)
    };
    Ok(cache)
}

pub(crate) fn quarantine(raw: &str) -> Result<Option<String>, Error> {
    let (images, reference) = cache(raw)?;
    let digest = images
        .resolve(&reference)?
        .map(|image| image.target.digest().to_string());
    if digest.is_some() {
        images.remove(&reference)?;
    }
    Ok(digest)
}
