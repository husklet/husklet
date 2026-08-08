use crate::{Digest, Error, Result, layer::DiffSize};

#[derive(serde::Deserialize, serde::Serialize)]
pub(super) struct DraftOwner {
    version: u8,
    key: super::Id,
    target: Option<super::Id>,
}

impl DraftOwner {
    pub(super) fn active(key: super::Id) -> Self {
        Self {
            version: 1,
            key,
            target: None,
        }
    }

    pub(super) fn publishing(key: super::Id, target: super::Id) -> Self {
        Self {
            version: 1,
            key,
            target: Some(target),
        }
    }

    pub(super) fn validate(self, key: &super::Id) -> Result<Option<super::Id>> {
        if self.version != 1 || &self.key != key {
            return Err(Error::InvalidMetadata("invalid snapshot draft owner".into()));
        }
        Ok(self.target)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub(crate) struct LayerRecord {
    pub(crate) diff_id: Digest,
    pub(crate) parent_chain_id: Option<Digest>,
    pub(crate) chain_id: Digest,
    pub(crate) diff_size: DiffSize,
}

impl LayerRecord {
    pub(crate) fn new(
        diff_id: Digest,
        parent_chain_id: Option<Digest>,
        chain_id: Digest,
        diff_size: DiffSize,
    ) -> Result<Self> {
        let expected = parent_chain_id.as_ref().map_or_else(
            || diff_id.clone(),
            |parent| Digest::sha256(format!("{parent} {diff_id}").as_bytes()),
        );
        if chain_id != expected {
            return Err(Error::InvalidMetadata(
                "layer record ChainID does not match its parent and DiffID".into(),
            ));
        }
        Ok(Self {
            diff_id,
            parent_chain_id,
            chain_id,
            diff_size,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub(super) enum Publication {
    Generic { version: u8 },
    LayerChain { version: u8, layers: Vec<LayerRecord> },
}

impl Publication {
    const VERSION: u8 = 1;
    const MAX_LAYERS: usize = 125;

    pub(super) fn generic() -> Self {
        Self::Generic { version: Self::VERSION }
    }

    pub(super) fn layer_chain(layers: Vec<LayerRecord>) -> Result<Self> {
        if layers.len() > Self::MAX_LAYERS {
            return Err(Error::InvalidMetadata("snapshot layer record count exceeds 125".into()));
        }
        let mut parent = None;
        for layer in &layers {
            let validated = LayerRecord::new(
                layer.diff_id.clone(),
                parent.clone(),
                layer.chain_id.clone(),
                layer.diff_size,
            )?;
            parent = Some(validated.chain_id);
        }
        Ok(Self::LayerChain {
            version: Self::VERSION,
            layers,
        })
    }

    pub(super) fn validate(self) -> Result<Self> {
        match self {
            Self::Generic { version: Self::VERSION } => Ok(self),
            Self::LayerChain {
                version: Self::VERSION,
                layers,
            } => Self::layer_chain(layers),
            _ => Err(Error::InvalidMetadata(
                "unsupported snapshot publication version".into(),
            )),
        }
    }

    /// Only immutable layer chains are name-indexed; writable uppers and
    /// forked container snapshots publish generically.
    pub(super) const fn is_layer_chain(&self) -> bool {
        matches!(self, Self::LayerChain { .. })
    }

    pub(super) fn layers(self) -> Option<Vec<LayerRecord>> {
        match self {
            Self::LayerChain { layers, .. } => Some(layers),
            Self::Generic { .. } => None,
        }
    }

    pub(super) fn validate_key(&self, id: &super::Id) -> Result<()> {
        let Self::LayerChain { layers, .. } = self else {
            return Ok(());
        };
        let matches = layers.last().map_or_else(
            || id.as_str() == "chain-empty",
            |layer| id.as_str().strip_prefix("chain-") == Some(layer.chain_id.encoded()),
        );
        if !matches {
            return Err(Error::InvalidMetadata(
                "snapshot publication ChainID does not match its key".into(),
            ));
        }
        Ok(())
    }
}
