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
        let cache = ImageCache::for_platform(platform)?;
        let (images, image) = cache.resolve(name).await?;
        images.details(&image, platform)?;
        Ok(image.target.digest().to_string())
    }

    /// Concurrent workers share one on-disk snapshot store, so a lost race there is transient.
    pub async fn materialize(name: &str, platform: &Platform) -> Result<Self, Error> {
        for attempt in 1..=MATERIALIZE_ATTEMPTS {
            let failure = match Self::attempt(name, platform).await {
                Ok(fixture) => return Ok(fixture),
                Err(error) => error.to_string(),
            };
            if attempt == MATERIALIZE_ATTEMPTS || !failure.store_race() {
                return Err(failure.into());
            }
            eprintln!("image materialization attempt {attempt} for {name} lost a store race; retrying: {failure}");
            sleep(Duration::from_millis(200 * u64::from(attempt))).await;
        }
        unreachable!("bounded materialization retry loop always returns")
    }

    /// Opening the shared store races too, so resolution and unpacking retry together.
    async fn attempt(name: &str, platform: &Platform) -> Result<Self, Error> {
        let cache = ImageCache::for_platform(platform)?;
        let (images, image) = cache.resolve(name).await?;
        Self::from_image(images, &image, platform)
    }

    fn from_image(images: Images, image: &Image, platform: &Platform) -> Result<Self, Error> {
        let digest = image.target.digest().to_string();
        // One critical section: a peer must never repair the chain between our unpack and our fork.
        let (unpacked, reference) = images
            .materialize(image, platform)
            .map_err(|error| format!("materialize {digest}: {error}"))?;
        let runtime = unpacked.runtime().clone();
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

/// The per-platform on-disk image store every harness shares.
pub struct ImageCache {
    root: PathBuf,
    platform: Platform,
}

impl ImageCache {
    /// # Errors
    /// Returns a failure when the workspace root cannot be located.
    pub fn for_platform(platform: &Platform) -> Result<Self, Error> {
        let configured = env::var_os("HL_SCENARIO_IMAGE_CACHE").map(PathBuf::from);
        Ok(Self {
            root: Self::locate(configured, platform, &workspace()?),
            platform: platform.clone(),
        })
    }

    fn locate(configured: Option<PathBuf>, platform: &Platform, workspace: &Path) -> PathBuf {
        let path =
            configured.unwrap_or_else(|| PathBuf::from("target/testing/images").join(platform.architecture.as_str()));
        if path.is_absolute() { path } else { workspace.join(path) }
    }

    fn open(&self) -> Result<Images, Error> {
        Images::open(&self.root).map_err(|error| format!("open image cache {}: {error}", self.root.display()).into())
    }

    /// Reports whether the prefetched cache already serves this name on this platform.
    ///
    /// # Errors
    /// Returns cache open, reference parsing, and image lookup failures.
    pub fn preflight(&self, name: &str) -> Result<bool, Error> {
        let images = self.open()?;
        let reference: Reference = name.parse()?;
        let Some(image) = images.resolve(&reference)? else {
            return Ok(false);
        };
        match images.details(&image, &self.platform) {
            Ok(_) => Ok(true),
            Err(hl_images::Error::UnsupportedPlatform { .. }) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    async fn resolve(&self, name: &str) -> Result<(Images, Image), Error> {
        let images = self.open()?;
        let reference: Reference = name.parse()?;
        let image = match images
            .resolve(&reference)
            .map_err(|error| format!("resolve {name}: {error}"))?
        {
            Some(image) if images.details(&image, &self.platform).is_ok() => image,
            _ if env::var_os("HL_SCENARIO_OFFLINE").is_some() => {
                return Err(format!(
                    "image materialization unavailable offline: {name} is absent from the prefetched {} cache",
                    self.platform.architecture
                )
                .into());
            }
            _ => pull(&images, reference, &self.platform).await?,
        };
        Ok((images, image))
    }
}

async fn pull(images: &Images, reference: Reference, platform: &Platform) -> Result<Image, Error> {
    let registry = Registry::new(registry_auth());
    let mut delay = Duration::from_secs(2);
    for attempt in 1..=3 {
        match images.pull(&registry, reference.clone(), platform).await {
            Ok(image) => return Ok(image),
            Err(error) if attempt < 3 && error.to_string().transient_registry_fault() => {
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

/// Whether a failure message describes a lost race or a transient registry fault, so the
/// retry policy reads as a property of the failure rather than a detached predicate.
trait FailureText {
    fn store_race(&self) -> bool;
    fn transient_registry_fault(&self) -> bool;
}

impl FailureText for str {
    fn store_race(&self) -> bool {
        let error = self.to_ascii_lowercase();
        ["os error 2", "os error 17", "no such file", "already exists"]
            .iter()
            .any(|needle| error.contains(needle))
    }

    fn transient_registry_fault(&self) -> bool {
        let error = self.to_ascii_lowercase();
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
}

#[cfg(test)]
mod tests {
    use super::{FailureText as _, ImageCache};

    #[test]
    fn only_shared_store_races_are_retried() {
        assert!("fork rootfs from sha256:a: No such file or directory (os error 2)".store_race());
        assert!("File exists (os error 17)".store_race());
        assert!(!"layer DiffID mismatch".store_race());
    }

    use hl_images::Platform;

    #[test]
    fn cache_is_platform_specific_unless_exact_leaf_is_configured() {
        let workspace = std::path::Path::new("/workspace");
        assert_eq!(
            ImageCache::locate(None, &Platform::linux_arm64(), workspace),
            workspace.join("target/testing/images/arm64")
        );
        assert_eq!(
            ImageCache::locate(Some("persistent/amd64".into()), &Platform::linux_amd64(), workspace),
            workspace.join("persistent/amd64")
        );
        assert_eq!(
            ImageCache::locate(Some("/cache/arm64".into()), &Platform::linux_arm64(), workspace),
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
            assert!(error.transient_registry_fault(), "{error}");
        }
        for error in ["unauthorized", "manifest unknown", "invalid digest"] {
            assert!(!error.transient_registry_fault(), "{error}");
        }
    }
}
