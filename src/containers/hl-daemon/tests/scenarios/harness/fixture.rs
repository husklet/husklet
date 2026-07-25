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
            && FsImageStore::open(cache_root(platform)?.join("metadata"))?
                .get(&parsed)?
                .is_none();
        if absent {
            return Err(format!(
                "image materialization unavailable offline: {raw} is absent from the prefetched cache"
            )
            .into());
        }
        let (images, reference) = cache(raw, platform)?;
        let image = if let Some(image) = resolve_for_platform(&images, &reference, platform)? {
            image
        } else {
            if env::var_os("HL_SCENARIO_DOCKER_IMAGES").is_some() {
                let _ = load_docker_image(&images, raw, platform)?;
            }
            if let Some(image) = resolve_for_platform(&images, &reference, platform)? {
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

fn load_docker_image(images: &Images, reference: &str, platform: &Platform) -> Result<bool, Error> {
    let direct = docker_inspect(reference, false)?;
    let (inspect, sudo) = if direct.status.success() {
        (direct, false)
    } else {
        (docker_inspect(reference, true)?, true)
    };
    if !inspect.status.success()
        || String::from_utf8_lossy(&inspect.stdout).trim()
            != format!("{}/{}", platform.os, platform.architecture)
    {
        return Ok(false);
    }
    let mut command = if sudo {
        let mut command = Command::new("sudo");
        command.args(["-n", "docker"]);
        command
    } else {
        Command::new("docker")
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

fn docker_inspect(reference: &str, sudo: bool) -> Result<std::process::Output, Error> {
    let mut command = if sudo {
        let mut command = Command::new("sudo");
        command.args(["-n", "docker"]);
        command
    } else {
        Command::new("docker")
    };
    Ok(command
        .args(["image", "inspect", "--format", "{{.Os}}/{{.Architecture}}"])
        .arg(reference)
        .output()?)
}

fn resolve_for_platform(
    images: &Images,
    reference: &Reference,
    platform: &Platform,
) -> Result<Option<Image>, Error> {
    let Some(image) = images.resolve(reference)? else {
        return Ok(None);
    };
    match images.details(&image, platform) {
        Ok(_) => Ok(Some(image)),
        Err(hl_images::Error::UnsupportedPlatform { .. }) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn test_platform_aware_cache_resolution() -> Result<(), Error> {
    let root = tempfile::tempdir()?;
    assert_eq!(
        cache_path(None, &Platform::linux_arm64(), root.path()),
        root.path().join("target/scenarios/images/arm64")
    );
    assert_eq!(
        cache_path(None, &Platform::linux_amd64(), root.path()),
        root.path().join("target/scenarios/images/amd64")
    );
    let configured = root.path().join("persistent/amd64");
    assert_eq!(
        cache_path(
            Some(configured.clone()),
            &Platform::linux_amd64(),
            root.path()
        ),
        configured,
        "an explicitly selected per-platform cache remains the exact leaf path"
    );
    let arm64 = Images::open(root.path().join("arm64"))?;
    let amd64 = Images::open(root.path().join("amd64"))?;
    let reference: Reference = "example.test/cache:latest".parse()?;
    let mut layer = Vec::new();
    tar::Builder::new(&mut layer).finish()?;
    let runtime = RuntimeConfig {
        entrypoint: Vec::new(),
        command: vec!["/bin/true".into()],
        environment: std::collections::BTreeMap::new(),
        working_directory: "/".into(),
        user: String::new(),
    };
    arm64.import(
        layer.as_slice(),
        &runtime,
        &Platform::linux_arm64(),
        &reference,
    )?;
    amd64.import(
        layer.as_slice(),
        &runtime,
        &Platform::linux_amd64(),
        &reference,
    )?;
    assert!(resolve_for_platform(&arm64, &reference, &Platform::linux_arm64())?.is_some());
    assert!(resolve_for_platform(&amd64, &reference, &Platform::linux_amd64())?.is_some());
    assert!(
        resolve_for_platform(&arm64, &reference, &Platform::linux_amd64())?.is_none(),
        "the arm64 catalog must not resolve its canonical name as amd64"
    );
    assert!(
        resolve_for_platform(&amd64, &reference, &Platform::linux_arm64())?.is_none(),
        "the amd64 catalog must not resolve its canonical name as arm64"
    );
    Ok(())
}

fn cache(raw: &str, platform: &Platform) -> Result<(Images, Reference), Error> {
    let cache = cache_root(platform)?;
    let images = Images::open(cache)?;
    let reference: Reference = raw.parse()?;
    Ok((images, reference))
}

pub(crate) fn cache_root(platform: &Platform) -> io::Result<PathBuf> {
    Ok(cache_path(
        env::var_os("HL_SCENARIO_IMAGE_CACHE").map(PathBuf::from),
        platform,
        &env::current_dir()?,
    ))
}

fn cache_path(configured: Option<PathBuf>, platform: &Platform, current: &Path) -> PathBuf {
    let path = configured.unwrap_or_else(|| {
        PathBuf::from("target/scenarios/images").join(platform.architecture.as_str())
    });
    if path.is_absolute() {
        path
    } else {
        current.join(path)
    }
}

pub(crate) fn quarantine(raw: &str) -> Result<Option<String>, Error> {
    let platform = crate::contract::Target::from_env()?.platform();
    let (images, reference) = cache(raw, &platform)?;
    let digest = images
        .resolve(&reference)?
        .map(|image| image.target.digest().to_string());
    if digest.is_some() {
        images.remove(&reference)?;
    }
    Ok(digest)
}

pub(crate) fn preflight(raw: &str, platform: &Platform) -> Result<bool, Error> {
    let (images, reference) = cache(raw, platform)?;
    Ok(resolve_for_platform(&images, &reference, platform)?.is_some())
}
