use super::{Error, workspace};
use hl_images::{
    Image, Images, Platform, Reference, RuntimeConfig,
    remote::{Auth, Registry},
    rootfs::{Reference as RootReference, View},
};
use std::{
    env,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::time::sleep;

const MATERIALIZE_ATTEMPTS: u32 = 4;

pub struct TestImage {
    images: Images,
    identity: String,
    reference: RootReference,
    view: View,
    runtime: RuntimeConfig,
}

impl TestImage {
    /// Resolve the immutable image identity without creating a writable root filesystem.
    ///
    /// # Errors
    /// Returns image lookup, pull, or platform validation failures.
    pub async fn resolve_identity(name: &str, platform: &Platform) -> Result<String, Error> {
        let (images, image) = resolve(name, platform).await?;
        images.details(&image, platform)?;
        Ok(image.target.digest().to_string())
    }

    /// Concurrent workers share one on-disk snapshot store, so a lost race there is transient.
    pub async fn materialize(name: &str, platform: &Platform) -> Result<Self, Error> {
        for attempt in 1..=MATERIALIZE_ATTEMPTS {
            let (images, image) = resolve(name, platform).await?;
            let failure = match Self::from_image(images, &image, platform) {
                Ok(fixture) => return Ok(fixture),
                Err(error) => error.to_string(),
            };
            if attempt == MATERIALIZE_ATTEMPTS || !contended(&failure) {
                return Err(failure.into());
            }
            eprintln!("image materialization attempt {attempt} for {name} lost a store race; retrying: {failure}");
            sleep(Duration::from_millis(200 * u64::from(attempt))).await;
        }
        unreachable!("bounded materialization retry loop always returns")
    }

    fn from_image(images: Images, image: &Image, platform: &Platform) -> Result<Self, Error> {
        let digest = image.target.digest().to_string();
        let unpacked = images
            .unpack(image, platform)
            .map_err(|error| format!("unpack {digest}: {error}"))?;
        let runtime = unpacked.runtime().clone();
        let reference = images
            .rootfs(&unpacked)
            .map_err(|error| format!("fork rootfs from {digest}: {error}"))?;
        let view = images
            .roots()
            .open(&reference)
            .map_err(|error| format!("open rootfs {}: {error}", reference.lease_id()))?;
        Ok(Self {
            images,
            identity: image.target.digest().to_string(),
            reference,
            view,
            runtime,
        })
    }

    pub fn path(&self) -> &Path {
        self.view.path()
    }

    /// Immutable manifest identity selected for this platform.
    pub fn identity(&self) -> &str {
        &self.identity
    }

    pub fn runtime(&self) -> &RuntimeConfig {
        &self.runtime
    }

    pub fn release(self) -> Result<(), Error> {
        self.images.roots().release(&self.reference)?;
        Ok(())
    }
}

async fn resolve(name: &str, platform: &Platform) -> Result<(Images, Image), Error> {
    let root = cache_root(platform)?;
    let images = Images::open(&root).map_err(|error| format!("open image cache {}: {error}", root.display()))?;
    let reference: Reference = name.parse()?;
    let image = match images.resolve(&reference).map_err(|error| format!("resolve {name}: {error}"))? {
        Some(image) if images.details(&image, platform).is_ok() => image,
        _ if offline() => {
            return Err(format!(
                "image materialization unavailable offline: {name} is absent from the prefetched {} cache",
                platform.architecture
            )
            .into());
        }
        _ => pull(&images, reference, platform).await?,
    };
    Ok((images, image))
}

async fn pull(images: &Images, reference: Reference, platform: &Platform) -> Result<Image, Error> {
    let registry = Registry::new(registry_auth());
    let mut delay = Duration::from_secs(2);
    for attempt in 1..=3 {
        match images.pull(&registry, reference.clone(), platform).await {
            Ok(image) => return Ok(image),
            Err(error) if attempt < 3 && retryable(&error.to_string()) => {
                eprintln!(
                    "registry pull attempt {attempt} for {reference} failed; retrying in {}s: {error}",
                    delay.as_secs()
                );
                sleep(delay).await;
                delay *= 2;
            }
            Err(error) => return Err(error.into()),
        }
    }
    unreachable!("bounded registry retry loop always returns")
}

fn registry_auth() -> Auth {
    if let Some(token) = env::var_os("HL_REGISTRY_TOKEN") {
        return Auth::Bearer(token.to_string_lossy().into_owned());
    }
    match (env::var_os("HL_REGISTRY_USERNAME"), env::var_os("HL_REGISTRY_PASSWORD")) {
        (Some(username), Some(password)) => Auth::Basic {
            username: username.to_string_lossy().into_owned(),
            password: password.to_string_lossy().into_owned(),
        },
        _ => Auth::Anonymous,
    }
}

fn contended(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    ["os error 2", "os error 17", "no such file", "already exists"]
        .iter()
        .any(|needle| error.contains(needle))
}

fn retryable(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    [
        "429",
        "rate limit",
        "temporarily unavailable",
        "timeout",
        "connection reset",
    ]
    .iter()
    .any(|needle| error.contains(needle))
}

fn offline() -> bool {
    env::var_os("HL_SCENARIO_OFFLINE").is_some()
}

fn cache_root(platform: &Platform) -> Result<PathBuf, Error> {
    let configured = env::var_os("HL_SCENARIO_IMAGE_CACHE").map(PathBuf::from);
    Ok(cache_path(configured, platform, &workspace()?))
}

fn cache_path(configured: Option<PathBuf>, platform: &Platform, workspace: &Path) -> PathBuf {
    let path =
        configured.unwrap_or_else(|| PathBuf::from("target/testing/images").join(platform.architecture.as_str()));
    if path.is_absolute() { path } else { workspace.join(path) }
}

pub fn preflight(name: &str, platform: &Platform) -> Result<bool, Error> {
    let images = Images::open(cache_root(platform)?)?;
    let reference: Reference = name.parse()?;
    let Some(image) = images.resolve(&reference)? else {
        return Ok(false);
    };
    match images.details(&image, platform) {
        Ok(_) => Ok(true),
        Err(hl_images::Error::UnsupportedPlatform { .. }) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::{cache_path, contended, retryable};

    #[test]
    fn only_shared_store_races_are_retried() {
        assert!(contended("fork rootfs from sha256:a: No such file or directory (os error 2)"));
        assert!(contended("File exists (os error 17)"));
        assert!(!contended("layer DiffID mismatch"));
    }

    use hl_images::Platform;

    #[test]
    fn cache_is_platform_specific_unless_exact_leaf_is_configured() {
        let workspace = std::path::Path::new("/workspace");
        assert_eq!(
            cache_path(None, &Platform::linux_arm64(), workspace),
            workspace.join("target/testing/images/arm64")
        );
        assert_eq!(
            cache_path(Some("persistent/amd64".into()), &Platform::linux_amd64(), workspace),
            workspace.join("persistent/amd64")
        );
        assert_eq!(
            cache_path(Some("/cache/arm64".into()), &Platform::linux_arm64(), workspace),
            std::path::Path::new("/cache/arm64")
        );
    }

    #[test]
    fn registry_retries_only_transient_failures() {
        for error in [
            "HTTP 429",
            "rate limit",
            "temporarily unavailable",
            "request timeout",
            "connection reset",
        ] {
            assert!(retryable(error), "{error}");
        }
        for error in ["unauthorized", "manifest unknown", "invalid digest"] {
            assert!(!retryable(error), "{error}");
        }
    }
}
