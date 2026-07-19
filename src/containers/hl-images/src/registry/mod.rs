//! A small OCI / Docker registry client — pull and push images from **any** registry, not just Docker
//! Hub. Auth uses the standard `WWW-Authenticate: Bearer` challenge flow, so Docker Hub, GHCR, Quay, ECR
//! and a plain `localhost:5000` dev registry all work the same way.
//!
//! HTTP goes through `curl` and (de)compression through `tar`/`gzip`/`sha256sum`: the daemon's offline
//! build can't pull in an async HTTP+TLS+tar crate stack, and these tools are universally present. The
//! shelling-out is confined to the small `http` helpers; everything above them is ordinary typed code.
//!
//! The module is split into cohesive files — a [`Credentials`] entity, an [`ImageRef`] reference, the
//! [`Client`] service, plus internal layer/http tools — but shares one flat namespace: each submodule
//! pulls the others in with `use super::*`, and this parent re-globs every submodule below.

mod client;
mod credentials;
mod events;
mod http;
mod layer;
mod reference;

pub use client::Client;
pub use credentials::Credentials;
pub use events::{LayerId, PullEvent, Pulled};
pub use reference::ImageRef;

// Internal flat namespace: siblings reach layer/http free fns (whiteouts, tar, curl helpers) via
// `use super::*`, so re-glob the two modules whose private surface is shared across the module.
use layer::*;
// The tests below reach `BearerChallenge` (client) and `is_local_registry` (reference) through the
// same flat namespace; those private items have no non-test consumer, so gate their re-glob on test.
#[cfg(test)]
use client::*;

const DOCKER_HUB: &str = "registry-1.docker.io";
const MANIFEST_ACCEPT: &str = "application/vnd.docker.distribution.manifest.list.v2+json,\
application/vnd.oci.image.index.v1+json,\
application/vnd.docker.distribution.manifest.v2+json,\
application/vnd.oci.image.manifest.v1+json";
const MEDIA_MANIFEST: &str = "application/vnd.docker.distribution.manifest.v2+json";
const MEDIA_MANIFEST_OCI: &str = "application/vnd.oci.image.manifest.v1+json";
const MEDIA_CONFIG: &str = "application/vnd.docker.container.image.v1+json";
const MEDIA_LAYER: &str = "application/vnd.docker.image.rootfs.diff.tar.gzip";
const MEDIA_LAYER_OCI_GZIP: &str = "application/vnd.oci.image.layer.v1.tar+gzip";

/// A single image manifest's `mediaType` we understand (NOT an index — that's resolved earlier). An
/// absent `mediaType` is tolerated (some registries omit it on the manifest body); a present one must be
/// one of these.
struct MediaType<'a>(&'a str);

impl MediaType<'_> {
    fn new(value: &str) -> MediaType<'_> {
        MediaType(value)
    }

    fn supports_manifest(&self) -> bool {
        self.0 == MEDIA_MANIFEST || self.0 == MEDIA_MANIFEST_OCI
    }

    /// A layer `mediaType` we can actually unpack. Extraction is `tar xzf` (gzip), so only the docker/OCI
    /// GZIP layer types are supported; zstd (`…tar+zstd`), plain uncompressed tar, and foreign/unknown types
    /// are rejected rather than blindly gzip-extracted. An empty/absent type defaults to the docker gzip type
    /// (older registries omit it).
    fn supports_layer(&self) -> bool {
        self.0.is_empty() || self.0 == MEDIA_LAYER || self.0 == MEDIA_LAYER_OCI_GZIP
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn parse_refs() {
        let h = ImageRef::from("ubuntu");
        assert_eq!(
            (h.registry.as_str(), h.repository.as_str(), h.tag.as_str()),
            (DOCKER_HUB, "library/ubuntu", "latest")
        );
        assert_eq!(ImageRef::from("alpine:3.19").tag, "3.19");
        assert_eq!(ImageRef::from("user/app").repository, "user/app");
        let g = ImageRef::from("ghcr.io/owner/app:v2");
        assert_eq!(
            (g.registry.as_str(), g.repository.as_str(), g.tag.as_str()),
            ("ghcr.io", "owner/app", "v2")
        );
        let l = ImageRef::from("localhost:5000/img");
        assert_eq!(
            (l.registry.as_str(), l.repository.as_str()),
            ("localhost:5000", "img")
        );
        assert!(l.local_registry());
    }
    #[test]
    fn whiteouts() {
        // A just-extracted layer: a normal whiteout, an opaque-dir marker, and two degenerate names
        // that the old `find | … rm` shell mishandled (a bare `.wh.` wiped the parent dir; `.wh..`
        // made `rm` error). After apply_whiteouts: targets gone, all markers gone, parents kept.
        let root = std::env::temp_dir().join(format!("hl-wh-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(root.join("keep"), b"x").unwrap();
        std::fs::write(sub.join("gone"), b"x").unwrap(); // hidden by sub/.wh.gone
        std::fs::write(sub.join(".wh.gone"), b"").unwrap();
        std::fs::write(sub.join(".wh..wh..opq"), b"").unwrap();
        std::fs::write(sub.join(".wh."), b"").unwrap(); // malformed: must NOT delete sub/
        std::fs::write(sub.join(".wh.."), b"").unwrap(); // malformed: target "." must be ignored

        LayerRootfs::new(&root).apply_whiteouts().unwrap();

        assert!(root.join("keep").exists(), "unrelated file preserved");
        assert!(sub.exists(), "parent dir must survive a bare .wh. marker");
        assert!(!sub.join("gone").exists(), "whiteout deleted its target");
        for m in [".wh.gone", ".wh..wh..opq", ".wh.", ".wh.."] {
            assert!(!sub.join(m).exists(), "marker {m} removed");
        }
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn opaque_clears_lower_content() {
        // hl flattens all layers into one rootfs. Simulate a rootfs already holding LOWER content in
        // `app/`, then apply a new layer (a tar) that replaces `app/` wholesale via a `.wh..wh..opq`
        // marker. Real overlayfs would hide every lower entry of `app/`; the flattened image must too.
        let base = std::env::temp_dir().join(format!("hl-opq-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("rootfs");
        std::fs::create_dir_all(root.join("app/oldsub")).unwrap();
        std::fs::write(root.join("app/stale.txt"), b"stale").unwrap(); // lower file, must be hidden
        std::fs::write(root.join("app/oldsub/deep"), b"deep").unwrap(); // lower subtree, must be hidden
        std::fs::write(root.join("keep"), b"keep").unwrap(); // unrelated, must survive

        // The new layer's tar: app/.wh..wh..opq (opaque) + app/new.txt (this layer's own entry).
        let layerdir = base.join("layer");
        std::fs::create_dir_all(layerdir.join("app")).unwrap();
        std::fs::write(layerdir.join("app/.wh..wh..opq"), b"").unwrap();
        std::fs::write(layerdir.join("app/new.txt"), b"new").unwrap();
        let tar = base.join("layer.tar.gz");
        let st = Command::new("sh")
            .arg("-c")
            .arg(format!(
                "tar czf '{}' -C '{}' .",
                tar.display(),
                layerdir.display()
            ))
            .status()
            .unwrap();
        assert!(st.success(), "build layer tar");

        // The opaque dir is detected from the tar.
        assert_eq!(
            LayerRootfs::opaque_dirs_in_tar(&tar),
            vec!["app".to_string()]
        );

        // Apply the layer exactly as unpack_layer does: clear opaque dirs, extract, apply whiteouts.
        LayerRootfs::new(&root).clear_opaque(&LayerRootfs::opaque_dirs_in_tar(&tar));
        crate::image::archive::Archive::new(&tar)
            .extract_layer(&root)
            .unwrap();
        LayerRootfs::new(&root).apply_whiteouts().unwrap();

        assert!(
            !root.join("app/stale.txt").exists(),
            "opaque must clear the stale lower file"
        );
        assert!(
            !root.join("app/oldsub").exists(),
            "opaque must clear the stale lower subtree"
        );
        assert!(
            root.join("app/new.txt").exists(),
            "the current layer's file must remain"
        );
        assert!(root.join("keep").exists(), "unrelated content must survive");
        assert!(
            !root.join("app/.wh..wh..opq").exists(),
            "the opaque marker must be removed"
        );
        let _ = std::fs::remove_dir_all(&base);
    }
    #[test]
    fn challenge() {
        let h = "HTTP/1.1 401 Unauthorized\r\nwww-authenticate: Bearer realm=\"https://auth.docker.io/token\",service=\"registry.docker.io\",scope=\"repository:library/ubuntu:pull\"\r\n";
        let c = BearerChallenge::parse(h).unwrap();
        assert_eq!(c.realm, "https://auth.docker.io/token");
        assert_eq!(c.service, "registry.docker.io");
    }
}
