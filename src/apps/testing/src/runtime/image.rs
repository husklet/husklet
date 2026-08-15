use super::Error;
use hl_images::{
    Image, Images, Platform, Reference, RuntimeConfig, UnpackedImage,
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

/// How a case's writable root is derived from the immutable image chain.
///
/// `Overlay` is what the product takes: an empty upper over the unpacked lower.
/// `Copy` is the full recursive fork the product falls back to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Materialization {
    Copy,
    Overlay,
}

impl Materialization {
    /// Honours `HL_HARNESS_ROOTFS=copy|overlay`, defaulting to the product's overlay.
    #[must_use]
    pub fn from_environment() -> Self {
        match env::var("HL_HARNESS_ROOTFS").ok().as_deref() {
            Some("copy") => Self::Copy,
            _ => Self::Overlay,
        }
    }
}

/// The writable root a case runs against, in whichever shape it was materialized.
enum Root {
    Copy {
        reference: RootReference,
        view: View,
    },
    Overlay {
        reference: RootReference,
        lower: PathBuf,
        upper: PathBuf,
    },
}

pub struct TestImage {
    images: Images,
    identity: String,
    /// Retained in overlay mode so the lower chain stays pinned across re-forks.
    unpacked: Option<UnpackedImage>,
    root: Root,
    runtime: RuntimeConfig,
    /// A scratch case owns its isolated image catalog for the full container lifetime.
    _scratch_store: Option<tempfile::TempDir>,
}

impl TestImage {
    /// Concurrent workers share one on-disk snapshot store, so a lost race there is transient.
    pub async fn materialize(name: &str, platform: &Platform) -> Result<Self, Error> {
        Self::materialize_with(name, platform, Materialization::Copy).await
    }

    /// Materializes a writable root in the requested shape, retrying store races.
    ///
    /// # Errors
    /// Returns image lookup, pull, unpack, snapshot, or lease failures.
    pub async fn materialize_with(name: &str, platform: &Platform, mode: Materialization) -> Result<Self, Error> {
        for attempt in 1..=MATERIALIZE_ATTEMPTS {
            let failure = match Self::attempt(name, platform, mode).await {
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
    async fn attempt(name: &str, platform: &Platform, mode: Materialization) -> Result<Self, Error> {
        let cache = ImageCache::for_platform(platform)?;
        let (images, image) = cache.resolve(name).await?;
        Self::from_image(images, &image, platform, mode)
    }

    fn from_image(images: Images, image: &Image, platform: &Platform, mode: Materialization) -> Result<Self, Error> {
        let digest = image.target.digest().to_string();
        // One critical section: a peer must never repair the chain between our unpack and our fork.
        let (unpacked, reference) = match mode {
            Materialization::Copy => images.materialize(image, platform),
            Materialization::Overlay => images.materialize_overlay(image, platform),
        }
        .map_err(|error| format!("materialize {digest}: {error}"))?;
        let runtime = unpacked.runtime().clone();
        let root = Self::open_root(&images, reference, mode)?;
        Ok(Self {
            images,
            identity: digest,
            unpacked: matches!(mode, Materialization::Overlay).then_some(unpacked),
            root,
            runtime,
            _scratch_store: None,
        })
    }

    /// Creates a private OCI image whose single layer contains no entries.
    ///
    /// The catalog is per case, so neither image layers nor concurrent scratch cases can leak into
    /// this root. The regular materialization path still supplies durable reference ownership and
    /// exercises the same copy/overlay container launch contract as a registry image.
    pub fn materialize_scratch(platform: &Platform, mode: Materialization) -> Result<Self, Error> {
        let root = super::work_root::WorkRoot::open()?.scratch_images();
        std::fs::create_dir_all(&root)?;
        let store = tempfile::tempdir_in(root).map_err(|error| format!("create scratch image store: {error}"))?;
        let images = Images::open(store.path())?;
        let mut archive = tar::Builder::new(Vec::new());
        archive.finish()?;
        let layer = archive.into_inner()?;
        let runtime = RuntimeConfig {
            entrypoint: Vec::new(),
            command: Vec::new(),
            environment: std::collections::BTreeMap::new(),
            working_directory: "/".into(),
            user: String::new(),
        };
        let name: Reference = "husklet.invalid/testing-scratch:empty".parse()?;
        let image = images.commit(&layer, &runtime, platform, &name)?;
        let mut fixture = Self::from_image(images, &image, platform, mode)?;
        fixture._scratch_store = Some(store);
        Ok(fixture)
    }

    fn open_root(images: &Images, reference: RootReference, mode: Materialization) -> Result<Root, Error> {
        let lease = reference.lease_id().to_owned();
        match mode {
            Materialization::Copy => {
                let view = images
                    .roots()
                    .open(&reference)
                    .map_err(|error| format!("open rootfs {lease}: {error}"))?;
                Ok(Root::Copy { reference, view })
            }
            Materialization::Overlay => {
                let view = images
                    .roots()
                    .open_overlay(&reference)
                    .map_err(|error| format!("open overlay rootfs {lease}: {error}"))?;
                Ok(Root::Overlay {
                    lower: view.lower().to_owned(),
                    upper: view.upper().to_owned(),
                    reference,
                })
            }
        }
    }

    /// Replaces a consumed overlay root with a fresh empty upper over the same lower.
    ///
    /// Container removal releases an image-backed rootfs, so a repeated attempt
    /// needs its own upper rather than inheriting the previous attempt's writes.
    ///
    /// # Errors
    /// Returns snapshot or lease failures from the fork.
    pub fn refork(&mut self) -> Result<(), Error> {
        let Some(unpacked) = self.unpacked.clone() else {
            return Ok(());
        };
        let reference = self
            .images
            .roots()
            .fork_overlay(unpacked.snapshot())
            .map_err(|error| format!("re-fork overlay for {}: {error}", self.identity))?;
        self.root = Self::open_root(&self.images, reference, Materialization::Overlay)?;
        Ok(())
    }

    /// Where the harness stages guest files: the upper in overlay mode, the tree itself in copy mode.
    pub fn path(&self) -> &Path {
        match &self.root {
            Root::Copy { view, .. } => view.path(),
            Root::Overlay { upper, .. } => upper,
        }
    }

    /// The immutable lower tree, present only when this root is an overlay.
    pub fn lower(&self) -> Option<&Path> {
        match &self.root {
            Root::Copy { .. } => None,
            Root::Overlay { lower, .. } => Some(lower),
        }
    }

    /// The durable rootfs reference, for a spec that must carry the lower/upper split.
    pub const fn reference(&self) -> &RootReference {
        match &self.root {
            Root::Copy { reference, .. } | Root::Overlay { reference, .. } => reference,
        }
    }

    /// The shared image store, so a container service resolves the same snapshots.
    pub fn images(&self) -> Images {
        self.images.clone()
    }

    pub fn runtime(&self) -> &RuntimeConfig {
        &self.runtime
    }

    /// Immutable image identity recorded beside a retained failed overlay.
    pub fn identity(&self) -> &str {
        &self.identity
    }

    /// Archives the exact writable layer while its lease is still live.
    pub fn archive_upper(&self, writer: impl std::io::Write) -> Result<(), Error> {
        let Root::Overlay { reference, .. } = &self.root else {
            return Err("failed-overlay retention requires an overlay root".into());
        };
        self.images.roots().open_overlay(reference)?.archive_upper(writer)?;
        Ok(())
    }

    /// An image-backed spec transfers ownership, so container removal may already
    /// have released this lease; that is success, not a leak.
    pub fn release(self) -> Result<(), Error> {
        match self.images.roots().release(self.reference()) {
            Ok(()) | Err(hl_images::Error::NotOwned { .. }) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

/// Warns once when the image cache sits on a filesystem without reflink, where
/// every copy-materialized root is a full byte copy rather than a share.
fn report_reflink_support(root: &Path) {
    static REPORTED: std::sync::Once = std::sync::Once::new();
    REPORTED.call_once(|| {
        if std::fs::create_dir_all(root).is_err() {
            return;
        }
        let source = root.join(".reflink-probe-source");
        let destination = root.join(".reflink-probe-clone");
        let _ = std::fs::remove_file(&destination);
        if std::fs::write(&source, b"reflink probe").is_err() {
            return;
        }
        if reflink_copy::reflink(&source, &destination).is_err() {
            eprintln!(
                "image cache {} is on a filesystem without reflink support; every copy-materialized \
                 rootfs is a full byte copy. Set HL_RUNTIME_WORK_ROOT to a reflink-capable local \
                 filesystem before taking materialization measurements.",
                root.display()
            );
        }
        let _ = std::fs::remove_file(&source);
        let _ = std::fs::remove_file(&destination);
    });
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
        let root = super::work_root::WorkRoot::open()?.images(platform.architecture.as_str());
        report_reflink_support(&root);
        Ok(Self {
            root,
            platform: platform.clone(),
        })
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
            Err(error) => return Err(provisioning_failure(&reference, &error.to_string())),
        }
    }
    unreachable!("bounded registry retry loop always returns")
}

/// Names a pull failure as image provisioning rather than engine behaviour, so an empty cache on a
/// rate-limited or unauthenticated registry is not read as a defect in the engine under test.
fn provisioning_failure(reference: &Reference, error: &str) -> Error {
    if error.registry_unavailable() {
        return format!(
            "image provisioning failed, not an engine failure: the registry refused to serve \
             {reference} ({error}). Prefetch the image into the shared cache, or set \
             HL_SCENARIO_OFFLINE=1 to fail fast on a cache miss."
        )
        .into();
    }
    format!("pull {reference}: {error}").into()
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
    fn registry_unavailable(&self) -> bool;
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

    /// The registry, not the engine, denied service: rate limiting, authentication, or reachability.
    fn registry_unavailable(&self) -> bool {
        let error = self.to_ascii_lowercase();
        self.transient_registry_fault()
            || [
                "toomanyrequests",
                "too many requests",
                "unauthorized",
                "authentication required",
                "denied",
                "forbidden",
                "dns error",
                "connection refused",
                "network is unreachable",
            ]
            .iter()
            .any(|needle| error.contains(needle))
    }
}

#[cfg(test)]
mod tests {
    use super::{FailureText as _, Materialization, Platform, TestImage};

    #[test]
    fn only_shared_store_races_are_retried() {
        assert!("fork rootfs from sha256:a: No such file or directory (os error 2)".store_race());
        assert!("File exists (os error 17)".store_race());
        assert!(!"layer DiffID mismatch".store_race());
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

    #[tokio::test]
    async fn scratch_materialization_has_no_image_entries_in_either_shape() {
        for mode in [Materialization::Copy, Materialization::Overlay] {
            let fixture = TestImage::materialize_scratch(&Platform::linux_arm64(), mode).unwrap();
            assert_eq!(std::fs::read_dir(fixture.path()).unwrap().count(), 0);
            if let Some(lower) = fixture.lower() {
                assert_eq!(std::fs::read_dir(lower).unwrap().count(), 0);
            }
            fixture.release().unwrap();
        }
    }

    /// An empty cache behind a rate-limited or unauthenticated registry must not read as an engine
    /// defect, which is the diagnosis this failure has repeatedly cost.
    #[test]
    fn registry_denial_is_reported_as_provisioning_rather_than_engine_failure() {
        let reference: hl_images::Reference = "docker.io/library/alpine:3.20".parse().unwrap();
        for error in [
            "HTTP 429 toomanyrequests: You have reached your pull rate limit",
            "unauthorized: authentication required",
            "dns error: failed to lookup registry-1.docker.io",
        ] {
            let text = super::provisioning_failure(&reference, error).to_string();
            assert!(text.contains("not an engine failure"), "{text}");
            assert!(text.contains("HL_SCENARIO_OFFLINE"), "{text}");
        }
        let text = super::provisioning_failure(&reference, "layer DiffID mismatch").to_string();
        assert!(!text.contains("not an engine failure"), "{text}");
    }
}
