use std::io::Read;
use std::sync::Arc;

use hl_container::{ContainerSpec, Containers, ExitStatus};
use hl_images::build::{Base, CopySource, Recipe, Step};
use hl_images::{snapshot::Ownerships, Image, Platform, Reference};
mod cache;
mod context;
mod copy;
mod execution;
mod remote;
mod stage;

use cache::{build_cache_key, cache_name};
use context::Context;
use remote::RemoteSources;

#[derive(Debug, thiserror::Error)]
pub(crate) enum BuildError {
    #[error("invalid build request: {0}")]
    Image(#[from] hl_images::Error),
    #[error("container build failed: {0}")]
    Container(#[from] hl_container::Error),
    #[error("build context failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("RUN exited with {0:?}")]
    Run(ExitStatus),
    #[error("Dockerfile is not valid UTF-8")]
    Dockerfile,
    #[error("COPY source {0:?} is outside the build context")]
    Copy(String),
    #[error("remote ADD request failed: {0}")]
    Remote(#[from] reqwest::Error),
}

/// Classic Dockerfile build service over the container and OCI primitives.
#[derive(Clone)]
pub(crate) struct Builder {
    containers: Containers,
    platform: Platform,
    source: Arc<dyn hl_images::remote::Source>,
    network: BuildNetwork,
}

/// Policy for resolving image-backed Dockerfile stages and COPY sources.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum BaseImages {
    #[default]
    Local,
    Pull,
}

impl BaseImages {
    fn requires_pull(self, present: bool) -> bool {
        self == Self::Pull || !present
    }
}

/// Network policy applied to transient Dockerfile `RUN` containers.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) enum BuildNetwork {
    #[default]
    Default,
    None,
    Host,
    Named(String),
}

pub(crate) struct BuildRequest<'a> {
    pub(crate) dockerfile: &'a str,
    pub(crate) name: Reference,
    pub(crate) arguments: &'a std::collections::BTreeMap<String, String>,
    pub(crate) target: Option<&'a str>,
    pub(crate) cache: Option<String>,
    pub(crate) base_images: BaseImages,
}

impl BuildNetwork {
    fn container(&self, mut spec: ContainerSpec) -> ContainerSpec {
        if *self != Self::None {
            let isolation = spec.isolation;
            spec = spec.isolation(hl_container::Isolation {
                network_isolated: false,
                ..isolation
            });
        }
        if *self == Self::Host {
            spec = spec.network_mode(hl_container::NetworkMode::Host);
        }
        spec
    }

    fn cache_key(&self) -> &str {
        match self {
            Self::Default => "default",
            Self::None => "none",
            Self::Host => "host",
            Self::Named(name) => name,
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct Builds {
    locks: Arc<tokio::sync::Mutex<std::collections::BTreeMap<String, Arc<tokio::sync::Mutex<()>>>>>,
}

impl Builds {
    pub(crate) async fn lock(&self, key: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = self
            .locks
            .lock()
            .await
            .entry(key.to_owned())
            .or_default()
            .clone();
        lock.lock_owned().await
    }
}

impl Builder {
    pub(crate) fn new(
        containers: Containers,
        platform: Platform,
        source: Arc<dyn hl_images::remote::Source>,
    ) -> Self {
        Self {
            containers,
            platform,
            source,
            network: BuildNetwork::default(),
        }
    }

    #[must_use]
    pub(crate) fn network(mut self, value: BuildNetwork) -> Self {
        self.network = value;
        self
    }

    async fn resolve_bases(&self, recipe: &Recipe, policy: BaseImages) -> Result<(), BuildError> {
        let images = self.containers.images()?;
        for stage in &recipe.stages {
            if let Base::Image(reference) = &stage.base {
                if policy.requires_pull(images.resolve(reference)?.is_some()) {
                    images
                        .pull(self.source.as_ref(), reference.clone(), &self.platform)
                        .await?;
                }
            }
            for reference in stage.steps.iter().filter_map(|step| match step {
                Step::Copy {
                    from: Some(CopySource::Image(reference)),
                    ..
                } => Some(reference),
                _ => None,
            }) {
                if policy.requires_pull(images.resolve(reference)?.is_some()) {
                    images
                        .pull(self.source.as_ref(), reference.clone(), &self.platform)
                        .await?;
                }
            }
        }
        Ok(())
    }

    pub(crate) async fn build(
        &self,
        context: impl Read,
        request: BuildRequest<'_>,
    ) -> Result<Image, BuildError> {
        let BuildRequest {
            dockerfile,
            name,
            arguments,
            target,
            cache,
            base_images,
        } = request;
        let _span = hl_log::hl_span!(hl_log::tag::DAEMON, "image.build");
        let directory = tempfile::tempdir()?;
        hl_images::layer::Layer::new(context).apply(directory.path())?;
        let context = Context::new(directory.path());
        let source = context.read(dockerfile)?;
        context.ignore(dockerfile)?;
        let context_digest = context.digest()?;
        let recipe = Recipe::parse_with_platforms(
            &source,
            arguments,
            target,
            Some(&self.platform),
            Some(&self.platform),
        )?;
        if let BuildNetwork::Named(network) = &self.network {
            self.containers.networks().inspect(network).await?;
        }
        if let Some(requested) = recipe
            .stages
            .iter()
            .filter_map(|stage| stage.platform.as_ref())
            .find(|platform| *platform != &self.platform)
        {
            return Err(hl_images::Error::UnsupportedPlatform {
                os: requested.os.clone(),
                architecture: requested.architecture.clone(),
                variant: requested.variant.clone().unwrap_or_default(),
            }
            .into());
        }
        self.resolve_bases(&recipe, base_images).await?;
        let images = self.containers.images()?;
        let remotes = RemoteSources::fetch(&recipe).await?;
        let cache_source = format!("{source}\n# hl-build-network={}", self.network.cache_key());
        let cache_key = build_cache_key(&cache_source, arguments, target, context_digest);
        let cache = cache
            .is_some()
            .then(|| cache_name(&cache_key, &recipe, &images, &self.platform, &remotes))
            .transpose()?
            .flatten();
        if let Some(cache) = &cache {
            if let Some(hit) = images.resolve(cache)? {
                return Ok(images.tag(&hit, name)?);
            }
        }
        let mut built = Vec::new();
        for stage in recipe.stages.iter().take(recipe.selected + 1) {
            built.push(
                self.stage(&context, stage, &built, &images, &remotes)
                    .await?,
            );
        }
        let selected = &built[recipe.selected];
        let mut layer = Vec::new();
        selected
            .ownerships
            .archive(selected.root.path(), &mut layer)?;
        let metadata = hl_images::Metadata {
            author: None,
            platform: self.platform.clone(),
            created: None,
            labels: selected.labels.clone(),
            history: selected.history.clone(),
            runtime: selected.runtime.clone(),
            onbuild: selected.onbuild.clone(),
            exposed_ports: selected.exposed_ports.clone(),
            volumes: selected.volumes.clone(),
            healthcheck: selected.healthcheck.clone(),
            stop_signal: selected.stop_signal.clone(),
        };
        let image = images.build(&layer, &name, &metadata)?;
        if let Some(cache) = cache {
            images.tag(&image, cache)?;
        }
        Ok(image)
    }
}

struct Build {
    root: tempfile::TempDir,
    runtime: hl_images::RuntimeConfig,
    labels: std::collections::BTreeMap<String, String>,
    history: Vec<hl_images::History>,
    onbuild: Vec<String>,
    exposed_ports: std::collections::BTreeSet<String>,
    volumes: std::collections::BTreeSet<String>,
    healthcheck: Option<serde_json::Value>,
    stop_signal: Option<String>,
    ownerships: Ownerships,
}

struct BaseState {
    base: hl_images::RuntimeConfig,
    labels: std::collections::BTreeMap<String, String>,
    history: Vec<hl_images::History>,
    triggers: Vec<String>,
    exposed_ports: std::collections::BTreeSet<String>,
    volumes: std::collections::BTreeSet<String>,
    healthcheck: Option<serde_json::Value>,
    stop_signal: Option<String>,
    ownerships: Ownerships,
}

#[cfg(test)]
mod tests;
