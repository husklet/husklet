use std::collections::BTreeSet;

use super::lookup::VisibleProbe;
use crate::{
    DirectoryEntry, GuestName, GuestPathBytes, LayerEntry, MergedDirectory, Overlay, OverlayError, OverlayHost,
    OverlayNodeKind,
};

const DIRECTORY_ENTRY_MAXIMUM: usize = 4096;

impl<H: OverlayHost> Overlay<H> {
    /// Merges upper then lower directory scans, retaining the first occurrence.
    ///
    /// Whiteouts participate in deduplication but are never returned. Ordering
    /// within a layer is the host snapshot order; layer precedence is stable.
    pub fn read_directory(&self, path: &GuestPathBytes) -> Result<MergedDirectory, OverlayError> {
        let mut allowed = self.visibility_limit(path)?;
        let VisibleProbe::Present {
            position, kind, opaque, ..
        } = self.probe_visible(path, allowed)?
        else {
            return Err(OverlayError::NotFound);
        };
        if kind != OverlayNodeKind::Directory {
            return Err(OverlayError::NotDirectory);
        }
        if opaque {
            allowed = allowed.min(position + 1);
        }
        let mut seen = BTreeSet::new();
        let mut output = Vec::new();
        for layer_position in 0..allowed {
            let stop = self.merge_layer(path, layer_position, &mut seen, &mut output)?;
            if stop {
                break;
            }
        }
        Ok(MergedDirectory { entries: output })
    }

    fn merge_layer(
        &self,
        path: &GuestPathBytes,
        position: usize,
        seen: &mut BTreeSet<GuestName>,
        output: &mut Vec<DirectoryEntry>,
    ) -> Result<bool, OverlayError> {
        let layer = self.layer(position);
        let entry = self.host.probe(layer, path).map_err(OverlayError::Host)?;
        let LayerEntry::Node { kind, opaque, .. } = entry else {
            return Ok(matches!(entry, LayerEntry::Whiteout));
        };
        if kind != OverlayNodeKind::Directory {
            return Ok(false);
        }
        let entries = self.host.read_directory(layer, path).map_err(OverlayError::Host)?;
        for child in entries {
            Self::merge_child(child, seen, output)?;
        }
        Ok(opaque)
    }

    fn merge_child(
        child: DirectoryEntry,
        seen: &mut BTreeSet<GuestName>,
        output: &mut Vec<DirectoryEntry>,
    ) -> Result<(), OverlayError> {
        if !seen.insert(child.name.clone()) || child.whiteout {
            return Ok(());
        }
        if output.len() == DIRECTORY_ENTRY_MAXIMUM {
            return Err(OverlayError::ResourceLimit);
        }
        output.push(child);
        Ok(())
    }
}
