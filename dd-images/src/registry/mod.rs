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
pub use events::{layer_short, PullEvent, Pulled};
pub use reference::ImageRef;
pub(crate) use reference::split_tag;

// Internal flat namespace: re-glob every submodule so a submodule's `use super::*` resolves the whole
// registry module's private surface (free fns, structs and consts defined across the sibling files).
#[allow(unused_imports)]
use client::*;
#[allow(unused_imports)]
use credentials::*;
#[allow(unused_imports)]
use events::*;
#[allow(unused_imports)]
use http::*;
#[allow(unused_imports)]
use layer::*;
#[allow(unused_imports)]
use reference::*;

const DOCKER_HUB: &str = "registry-1.docker.io";
const MANIFEST_ACCEPT: &str = "application/vnd.docker.distribution.manifest.list.v2+json,\
application/vnd.oci.image.index.v1+json,\
application/vnd.docker.distribution.manifest.v2+json,\
application/vnd.oci.image.manifest.v1+json";
const MEDIA_MANIFEST: &str = "application/vnd.docker.distribution.manifest.v2+json";
const MEDIA_CONFIG: &str = "application/vnd.docker.container.image.v1+json";
const MEDIA_LAYER: &str = "application/vnd.docker.image.rootfs.diff.tar.gzip";

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn parse_refs() {
        let h = ImageRef::parse("ubuntu");
        assert_eq!(
            (h.registry.as_str(), h.repository.as_str(), h.tag.as_str()),
            (DOCKER_HUB, "library/ubuntu", "latest")
        );
        assert_eq!(ImageRef::parse("alpine:3.19").tag, "3.19");
        assert_eq!(ImageRef::parse("user/app").repository, "user/app");
        let g = ImageRef::parse("ghcr.io/owner/app:v2");
        assert_eq!(
            (g.registry.as_str(), g.repository.as_str(), g.tag.as_str()),
            ("ghcr.io", "owner/app", "v2")
        );
        let l = ImageRef::parse("localhost:5000/img");
        assert_eq!(
            (l.registry.as_str(), l.repository.as_str()),
            ("localhost:5000", "img")
        );
        assert!(is_local_registry(&l.registry));
    }
    #[test]
    fn whiteouts() {
        // A just-extracted layer: a normal whiteout, an opaque-dir marker, and two degenerate names
        // that the old `find | … rm` shell mishandled (a bare `.wh.` wiped the parent dir; `.wh..`
        // made `rm` error). After apply_whiteouts: targets gone, all markers gone, parents kept.
        let root = std::env::temp_dir().join(format!("dd-wh-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(root.join("keep"), b"x").unwrap();
        std::fs::write(sub.join("gone"), b"x").unwrap(); // hidden by sub/.wh.gone
        std::fs::write(sub.join(".wh.gone"), b"").unwrap();
        std::fs::write(sub.join(".wh..wh..opq"), b"").unwrap();
        std::fs::write(sub.join(".wh."), b"").unwrap(); // malformed: must NOT delete sub/
        std::fs::write(sub.join(".wh.."), b"").unwrap(); // malformed: target "." must be ignored

        apply_whiteouts(&root).unwrap();

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
        // dd flattens all layers into one rootfs. Simulate a rootfs already holding LOWER content in
        // `app/`, then apply a new layer (a tar) that replaces `app/` wholesale via a `.wh..wh..opq`
        // marker. Real overlayfs would hide every lower entry of `app/`; the flattened image must too.
        let base = std::env::temp_dir().join(format!("dd-opq-test-{}", std::process::id()));
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
        assert_eq!(opaque_dirs_in_tar(&tar), vec!["app".to_string()]);

        // Apply the layer exactly as unpack_layer does: clear opaque dirs, extract, apply whiteouts.
        clear_opaque_dirs(&root, &opaque_dirs_in_tar(&tar));
        http::extract_targz(&tar, &root).unwrap();
        apply_whiteouts(&root).unwrap();

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
