//! Pull results and per-layer progress events.

use super::*;
use serde_json::Value;

/// What `pull` resolved and unpacked.
pub struct Pulled {
    /// The reference that was resolved and pulled.
    pub image: ImageRef,
    /// The image's OCI run-config blob (`Cmd`/`Entrypoint`/`Env`/`Architecture`/…) for the resolved platform.
    pub config: Value,
}

/// A live progress event for a single image pull, surfaced per-layer as the download/unpack proceeds.
/// `images_create` formats these into docker's newline-delimited JSON status lines and streams them to
/// the client, so the user sees moving download/extract bars instead of one post-hoc dump. `id` is the
/// docker-style short layer id (first 12 hex of the blob digest). Byte counts are the compressed blob
/// size from the manifest (the same units docker's registry pull reports).
#[derive(Clone, Debug)]
pub enum PullEvent {
    /// A layer was discovered in the manifest (docker's "Pulling fs layer").
    Layer {
        /// Short layer id (first 12 hex of the blob digest).
        id: String,
    },
    /// Live download progress for a layer (`current`/`total` compressed bytes).
    Downloading {
        /// Short layer id (first 12 hex of the blob digest).
        id: String,
        /// Compressed bytes downloaded so far.
        current: u64,
        /// Total compressed size of the layer blob, from the manifest.
        total: u64,
    },
    /// A layer finished downloading.
    DownloadComplete {
        /// Short layer id (first 12 hex of the blob digest).
        id: String,
    },
    /// A layer's contents are being unpacked into the rootfs.
    Extracting {
        /// Short layer id (first 12 hex of the blob digest).
        id: String,
        /// Compressed bytes unpacked so far.
        current: u64,
        /// Total compressed size of the layer blob, from the manifest.
        total: u64,
    },
    /// A layer is fully pulled + unpacked.
    PullComplete {
        /// Short layer id (first 12 hex of the blob digest).
        id: String,
    },
}

/// docker's short layer id: the first 12 hex chars after the `sha256:` prefix.
pub fn layer_short(digest: &str) -> String {
    digest
        .trim_start_matches("sha256:")
        .chars()
        .take(12)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::layer_short;

    #[test]
    fn layer_short_strips_prefix_and_takes_12() {
        // Docker's short id: first 12 hex chars after the `sha256:` prefix.
        assert_eq!(layer_short("sha256:deadbeefcafe0000"), "deadbeefcafe");
    }

    #[test]
    fn layer_short_without_prefix() {
        // No `sha256:` prefix: still just the first 12 chars.
        assert_eq!(layer_short("deadbeefcafe0000"), "deadbeefcafe");
    }

    #[test]
    fn layer_short_shorter_than_12() {
        // Fewer than 12 chars available: return the whole (prefix-stripped) string.
        assert_eq!(layer_short("sha256:abc123"), "abc123");
    }
}
