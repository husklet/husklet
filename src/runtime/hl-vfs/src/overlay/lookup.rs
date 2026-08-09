use crate::{GuestPathBytes, Layer, LayerEntry, OverlayError, OverlayHost, OverlayLookup, OverlayNodeKind};

const LOWER_MAXIMUM: u8 = 8;

/// OverlayHost-neutral overlay lookup and mutation coordinator.
pub struct Overlay<H: OverlayHost> {
    pub(crate) host: H,
    lower_count: u8,
}

impl<H: OverlayHost> Overlay<H> {
    pub fn new(host: H, lower_count: u8) -> Result<Self, OverlayError> {
        if lower_count > LOWER_MAXIMUM {
            return Err(OverlayError::TooManyLayers);
        }
        Ok(Self { host, lower_count })
    }

    /// Finds the highest visible entry after whiteout and opaque-dir policy.
    pub fn lookup(&self, path: &GuestPathBytes) -> Result<OverlayLookup, OverlayError> {
        if !path.is_absolute() {
            return Err(OverlayError::InvalidPath);
        }
        let path = path.normalize_absolute().map_err(|_| OverlayError::InvalidPath)?;
        let allowed = self.visibility_limit(&path)?;
        match self.probe_visible(&path, allowed)? {
            VisibleProbe::Absent => Ok(OverlayLookup::Absent { upper_path: path }),
            VisibleProbe::Present {
                position,
                kind,
                metadata,
                ..
            } => Ok(OverlayLookup::Present {
                layer: self.layer(position),
                kind,
                metadata,
            }),
        }
    }

    pub(crate) fn visibility_limit(&self, path: &GuestPathBytes) -> Result<usize, OverlayError> {
        let mut allowed = self.layer_count();
        for ancestor in Ancestors::new(path) {
            let ancestor = GuestPathBytes::new(&ancestor).map_err(|_| OverlayError::InvalidPath)?;
            let VisibleProbe::Present {
                position, kind, opaque, ..
            } = self.probe_visible(&ancestor, allowed)?
            else {
                return Err(OverlayError::NotFound);
            };
            if kind != OverlayNodeKind::Directory {
                return Err(OverlayError::NotDirectory);
            }
            if opaque {
                allowed = allowed.min(position + 1);
                hl_log::hl_debug!(
                    hl_log::tag::FS,
                    "overlay opaque directory clamps lower layers path={} position={position} allowed={allowed}",
                    ancestor.as_bytes().escape_ascii()
                );
            }
        }
        Ok(allowed)
    }

    pub(crate) fn probe_visible(&self, path: &GuestPathBytes, allowed: usize) -> Result<VisibleProbe, OverlayError> {
        for position in 0..allowed {
            let entry = self
                .host
                .probe(self.layer(position), path)
                .map_err(OverlayError::Host)?;
            match entry {
                LayerEntry::Absent => {}
                LayerEntry::Whiteout => return Ok(VisibleProbe::Absent),
                LayerEntry::Node { kind, metadata, opaque } => {
                    return Ok(VisibleProbe::Present {
                        position,
                        kind,
                        metadata,
                        opaque,
                    });
                }
            }
        }
        Ok(VisibleProbe::Absent)
    }

    pub(crate) const fn layer_count(&self) -> usize {
        self.lower_count as usize + 1
    }

    #[allow(clippy::unused_self)]
    pub(crate) const fn layer(&self, position: usize) -> Layer {
        if position == 0 {
            Layer::Upper
        } else {
            Layer::Lower((position - 1) as u8)
        }
    }
}

pub(crate) enum VisibleProbe {
    Absent,
    Present {
        position: usize,
        kind: OverlayNodeKind,
        metadata: crate::NodeMetadata,
        opaque: bool,
    },
}

struct Ancestors<'path> {
    path: &'path [u8],
    cursor: usize,
}

impl<'path> Ancestors<'path> {
    fn new(path: &'path GuestPathBytes) -> Self {
        Self {
            path: path.as_bytes(),
            cursor: 1,
        }
    }
}

impl Iterator for Ancestors<'_> {
    type Item = Vec<u8>;

    fn next(&mut self) -> Option<Self::Item> {
        let suffix = self.path.get(self.cursor..)?;
        let relative = suffix.iter().position(|byte| *byte == b'/')?;
        let end = self.cursor + relative;
        self.cursor = end + 1;
        Some(self.path[..end].to_vec())
    }
}
