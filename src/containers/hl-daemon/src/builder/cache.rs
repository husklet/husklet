use super::{BuildError, RemoteSources};
use hl_images::build::{Base, CacheSharing, Recipe};
use hl_images::{Platform, Reference};
use sha2::{Digest as _, Sha256};
use std::sync::Arc;

pub(super) fn cache_name(
    key: &str,
    recipe: &Recipe,
    images: &hl_images::Images,
    platform: &Platform,
    remotes: &RemoteSources,
) -> Result<Option<Reference>, BuildError> {
    let mut digest = Sha256::new();
    digest.update(key.as_bytes());
    digest.update(platform.os.as_bytes());
    digest.update(platform.architecture.as_bytes());
    digest.update(platform.variant.as_deref().unwrap_or_default().as_bytes());
    for stage in &recipe.stages {
        if let Base::Image(reference) = &stage.base {
            let Some(image) = images.resolve(reference)? else {
                return Ok(None);
            };
            digest.update(image.target.digest().to_string().as_bytes());
        }
    }
    for (url, remote_digest) in remotes.entries() {
        digest.update(url.as_bytes());
        digest.update(remote_digest);
    }
    let digest = hl_images::Digest::from(<[u8; 32]>::from(digest.finalize()));
    Ok(Some(
        format!("hl-build-cache/{}:cache", digest.encoded()).parse()?,
    ))
}

pub(super) fn build_cache_key(
    dockerfile: &str,
    arguments: &std::collections::BTreeMap<String, String>,
    target: Option<&str>,
    context: [u8; 32],
) -> String {
    let mut digest = Sha256::new();
    digest.update(dockerfile.as_bytes());
    for (name, value) in arguments {
        digest.update(name.as_bytes());
        digest.update([0]);
        digest.update(value.as_bytes());
        digest.update([0]);
    }
    digest.update(target.unwrap_or_default().as_bytes());
    digest.update(context);
    hl_images::Digest::from(<[u8; 32]>::from(digest.finalize()))
        .encoded()
        .to_owned()
}

pub(super) fn cache_volume_name(scope: &str, target: &str, sharing: CacheSharing) -> String {
    let mut digest = Sha256::new();
    digest.update(scope.as_bytes());
    digest.update([0]);
    digest.update(target.as_bytes());
    digest.update([sharing as u8]);
    let digest = hl_images::Digest::from(<[u8; 32]>::from(digest.finalize()));
    format!("hl-build-cache-{}", &digest.encoded()[..32])
}

pub(super) struct Caches;

impl Caches {
    pub(super) async fn lock(name: &str) -> tokio::sync::OwnedMutexGuard<()> {
        static LOCKS: std::sync::OnceLock<
            std::sync::Mutex<std::collections::BTreeMap<String, Arc<tokio::sync::Mutex<()>>>>,
        > = std::sync::OnceLock::new();
        let lock = LOCKS
            .get_or_init(Default::default)
            .lock()
            .expect("build cache lock registry poisoned")
            .entry(name.into())
            .or_default()
            .clone();
        lock.lock_owned().await
    }
}
