use super::*;

#[derive(serde::Deserialize)]
pub(super) struct IndexDocument {
    #[serde(rename = "schemaVersion")]
    pub(super) schema_version: u32,
    pub(super) manifests: Vec<PlatformDescriptor>,
}
#[derive(serde::Deserialize)]
pub(super) struct PlatformDescriptor {
    #[serde(flatten)]
    pub(super) descriptor: Descriptor,
    pub(super) platform: Option<Platform>,
}
#[derive(serde::Deserialize)]
pub(super) struct ManifestDocument {
    #[serde(rename = "schemaVersion")]
    pub(super) schema_version: u32,
    pub(super) config: Descriptor,
    pub(super) layers: Vec<Descriptor>,
}

pub(super) struct Blob<'a> {
    bytes: &'a [u8],
    media_type: MediaType,
}

impl<'a> Blob<'a> {
    pub(super) fn new(bytes: &'a [u8], media_type: MediaType) -> Self {
        Self { bytes, media_type }
    }

    pub(super) fn descriptor(&self) -> Result<Descriptor> {
        let digest = Digest::sha256(self.bytes)
            .as_str()
            .parse::<oci_spec::image::Digest>()
            .map_err(|error| Error::MalformedOci(error.to_string()))?;
        DescriptorBuilder::default()
            .media_type(self.media_type.clone())
            .size(u64::try_from(self.bytes.len()).map_err(|_| Error::MalformedOci("blob too large".into()))?)
            .digest(digest)
            .build()
            .map_err(|error| Error::MalformedOci(error.to_string()))
    }
}
#[derive(serde::Deserialize)]
pub(super) struct ConfigDocument {
    #[serde(default)]
    pub(super) architecture: String,
    #[serde(default)]
    pub(super) os: String,
    #[serde(default)]
    pub(super) created: Option<String>,
    #[serde(default)]
    pub(super) author: Option<String>,
    #[serde(default, deserialize_with = "ConfigDocument::history")]
    pub(super) history: Vec<History>,
    pub(super) rootfs: ConfigRootfs,
    #[serde(default, deserialize_with = "ConfigDocument::config")]
    pub(super) config: OciRuntimeConfig,
}

impl ConfigDocument {
    fn history<'de, D>(deserializer: D) -> std::result::Result<Vec<History>, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(<Option<Vec<History>> as serde::Deserialize>::deserialize(deserializer)?.unwrap_or_default())
    }

    fn config<'de, D>(deserializer: D) -> std::result::Result<OciRuntimeConfig, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(<Option<OciRuntimeConfig> as serde::Deserialize>::deserialize(deserializer)?.unwrap_or_default())
    }

    pub(super) fn require_platform(&self, wanted: &Platform) -> Result<()> {
        if self.os == wanted.os && self.architecture == wanted.architecture {
            return Ok(());
        }
        Err(Error::UnsupportedPlatform {
            os: self.os.clone(),
            architecture: self.architecture.clone(),
            variant: String::new(),
        })
    }
}

#[derive(serde::Deserialize)]
pub(super) struct ConfigRootfs {
    #[serde(rename = "type")]
    pub(super) kind: String,
    pub(super) diff_ids: Vec<String>,
}

#[derive(Clone, Default, serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct OciRuntimeConfig {
    #[serde(default)]
    entrypoint: Option<Vec<String>>,
    #[serde(default)]
    cmd: Option<Vec<String>>,
    #[serde(default)]
    env: Option<Vec<String>>,
    #[serde(default)]
    working_dir: String,
    #[serde(default)]
    user: String,
    #[serde(default)]
    pub(super) labels: Option<BTreeMap<String, String>>,
    #[serde(default, deserialize_with = "OciRuntimeConfig::on_build", rename = "OnBuild")]
    pub(super) on_build: Vec<String>,
    #[serde(default, deserialize_with = "OciRuntimeConfig::exposed_ports")]
    pub(super) exposed_ports: BTreeMap<String, serde_json::Value>,
    #[serde(default, deserialize_with = "OciRuntimeConfig::volumes")]
    pub(super) volumes: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub(super) healthcheck: Option<serde_json::Value>,
    #[serde(default)]
    pub(super) stop_signal: Option<String>,
}

impl OciRuntimeConfig {
    fn on_build<'de, D>(deserializer: D) -> std::result::Result<Vec<String>, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(<Option<Vec<String>> as serde::Deserialize>::deserialize(deserializer)?.unwrap_or_default())
    }

    fn exposed_ports<'de, D>(deserializer: D) -> std::result::Result<BTreeMap<String, serde_json::Value>, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(
            <Option<BTreeMap<String, serde_json::Value>> as serde::Deserialize>::deserialize(deserializer)?
                .unwrap_or_default(),
        )
    }

    fn volumes<'de, D>(deserializer: D) -> std::result::Result<BTreeMap<String, serde_json::Value>, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(
            <Option<BTreeMap<String, serde_json::Value>> as serde::Deserialize>::deserialize(deserializer)?
                .unwrap_or_default(),
        )
    }
}

impl TryFrom<OciRuntimeConfig> for RuntimeConfig {
    type Error = Error;

    fn try_from(config: OciRuntimeConfig) -> Result<Self> {
        let mut environment = BTreeMap::new();
        for entry in config.env.unwrap_or_default() {
            let (name, value) = entry
                .split_once('=')
                .ok_or_else(|| Error::MalformedOci(format!("environment entry {entry:?} has no '='")))?;
            environment.insert(name.to_owned(), value.to_owned());
        }
        let runtime = Self {
            entrypoint: config.entrypoint.unwrap_or_default(),
            command: config.cmd.unwrap_or_default(),
            environment,
            working_directory: if config.working_dir.is_empty() {
                "/".into()
            } else {
                config.working_dir
            },
            user: config.user,
        };
        runtime.validate()?;
        Ok(runtime)
    }
}

impl IndexDocument {
    pub(super) fn select_platform(&self, wanted: &Platform) -> Result<Descriptor> {
        if self.schema_version != 2 {
            return Err(Error::MalformedOci(format!(
                "unsupported OCI index schema version {}",
                self.schema_version
            )));
        }
        let matches_os_arch = |candidate: &&PlatformDescriptor| {
            candidate
                .platform
                .as_ref()
                .is_some_and(|platform| platform.os == wanted.os && platform.architecture == wanted.architecture)
        };
        let candidates: Vec<&PlatformDescriptor> = self.manifests.iter().filter(matches_os_arch).collect();
        candidates
            .iter()
            .copied()
            .find(|candidate| {
                candidate
                    .platform
                    .as_ref()
                    .is_some_and(|platform| platform.variant == wanted.variant)
            })
            .or_else(|| wanted.variant.is_none().then(|| candidates.first().copied()).flatten())
            .map(|entry| entry.descriptor.clone())
            .ok_or_else(|| Error::UnsupportedPlatform {
                os: wanted.os.clone(),
                architecture: wanted.architecture.clone(),
                variant: wanted
                    .variant
                    .as_ref()
                    .map_or(String::new(), |value| format!("/{value}")),
            })
    }
}
impl ManifestDocument {
    pub(super) fn validate(&self) -> Result<()> {
        if self.schema_version != 2 {
            return Err(Error::MalformedOci(format!(
                "unsupported OCI manifest schema version {}",
                self.schema_version
            )));
        }
        let mut all = Vec::with_capacity(self.layers.len() + 1);
        all.push(&self.config);
        all.extend(&self.layers);
        for descriptor in all {
            let _: Digest = descriptor.digest().to_string().parse()?;
            if descriptor.size() == 0 {
                return Err(Error::MalformedOci("zero-size descriptor".into()));
            }
        }
        Ok(())
    }
}
