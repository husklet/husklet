//! Image discovery + reference resolution, thinned onto `dd-images`.
//!
//! The pure pipeline — scanning the store, sniffing each image's arch from binary magic, recovering env
//! from an on-disk OCI config, and deduping by tag — lives in `dd_images` (runtime-agnostic). This module
//! maps its plain [`dd_images::DiscoveredImage`] results onto the daemon's [`Image`] model and keeps the
//! over-the-live-store resolver ([`find_image`]) that ranks the daemon's own `Image`s.
use super::*;

// `ref_name` / `ref_repo` are pure reference-string helpers now owned by dd-images; re-export them under
// their old names so `crate::util::ref_name` / `crate::util::ref_repo` call sites keep resolving.
pub(crate) use dd_images::{ref_name, ref_repo};

/// Discover `<images>/<name>/rootfs` dirs and map each onto the daemon's [`Image`]. The scan + arch
/// detection + env recovery + tag-dedup all happen in `dd_images`; here we only translate the plain
/// [`dd_images::DiscoveredImage`] (runtime-agnostic arch, raw-JSON healthcheck) into an `Image`.
pub(crate) fn discover_images(images_dir: &str) -> Vec<Image> {
    dd_images::discover_images(images_dir)
        .into_iter()
        .map(image_from_discovered)
        .collect()
}

/// Map one runtime-agnostic [`dd_images::DiscoveredImage`] onto the daemon's [`Image`]: its [`dd_images::Arch`]
/// becomes a [`Guest`] and its raw-JSON healthcheck is parsed into a [`crate::model::HealthConfig`].
fn image_from_discovered(d: dd_images::DiscoveredImage) -> Image {
    let healthcheck = d
        .healthcheck
        .and_then(|v| serde_json::from_value::<crate::model::HealthConfig>(v).ok());
    Image {
        name: d.name,
        rootfs: d.rootfs.to_string_lossy().into_owned(),
        arch: guest_of(d.arch),
        cmd: d.cmd,
        env: d.env,
        entrypoint: d.entrypoint,
        workdir: d.workdir,
        user: d.user,
        exposed_ports: d.exposed_ports,
        created: d.created,
        stop_signal: d.stop_signal,
        img_volumes: d.img_volumes,
        healthcheck,
        ..Default::default()
    }
}

/// Probe the rootfs and pick the guest target from a binary's magic (dd-images' detector mapped onto the
/// runtime's [`Guest`] personality). Tries well-known paths first, then a bounded whole-rootfs scan.
pub(crate) fn detect_arch(rootfs: &std::path::Path) -> Option<Guest> {
    dd_images::detect_arch(rootfs).map(guest_of)
}

/// A coarse "richness" score for a stored [`Image`], used to break ties in [`find_image`] when several
/// entries resolve to one reference. A non-empty environment is the decisive signal (a real config beats
/// an empty-env duplicate); the remaining run metadata + labels break finer ties.
pub(crate) fn image_score(img: &Image) -> i32 {
    let mut s = 0;
    if !img.env.is_empty() {
        s += 1000;
    }
    if !img.entrypoint.is_empty() {
        s += 10;
    }
    if !img.workdir.is_empty() {
        s += 5;
    }
    s += img.labels.len() as i32;
    // A recorded CMD beats the `/bin/sh` default the discovery fallback substitutes.
    if img.cmd.len() != 1 || img.cmd[0] != "/bin/sh" {
        s += 1;
    }
    s
}

/// Pick the single best image matching a docker reference (`docker inspect ubuntu`, `docker run alpine`),
/// deterministically. dd's lookup is lenient — a bare name matches any stored image with that repository
/// regardless of tag (see [`ref_repo`]) — so several images can match one query. Ranks by:
///   1. an exact `repository:tag` match for the requested reference, then `<name>:latest`,
///   2. then the richest metadata (a real environment beats an empty one — see [`image_score`]),
///   3. then the name string (reversed so the lexicographically smallest wins) to settle any remainder.
pub(crate) fn find_image<'a>(images: &'a [Image], reference: &str) -> Option<&'a Image> {
    let want_repo = ref_repo(reference);
    let want = ref_name(reference);
    let want_rt = repo_tag(reference);
    let want_latest = format!("{want}:latest");
    images
        .iter()
        // Match on the fully-qualified repository (registry+namespace+name), NOT the bare basename, so a
        // bare official `nginx` never resolves to a third-party `linuxserver/nginx`.
        .filter(|i| ref_repo(&i.name) == want_repo)
        .max_by_key(|i| {
            (
                repo_tag(&i.name) == want_rt,
                repo_tag(&i.name) == want_latest,
                image_score(i),
                std::cmp::Reverse(i.name.clone()),
            )
        })
}

#[cfg(test)]
mod id_resolution_tests {
    use super::*;

    fn img(name: &str) -> Image {
        Image {
            name: name.into(),
            ..Default::default()
        }
    }

    // a container id must be 64 hex of REAL entropy, not a 16-hex value tiled 4x.
    #[test]
    fn new_id_is_full_entropy_64_hex() {
        let a = new_id("alpine");
        assert_eq!(a.len(), 64, "docker container ids are 64 hex chars");
        assert!(
            a.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "id must be lowercase hex: {a}"
        );
        // The old bug tiled one 16-hex value 4x: the first three quarters were identical. Reject that.
        let (q0, q1, q2) = (&a[0..16], &a[16..32], &a[32..48]);
        assert!(!(q0 == q1 && q1 == q2), "id looks tiled (low entropy): {a}");
        // Consecutive ids differ (no counter/clock collision).
        assert_ne!(a, new_id("alpine"));
        // The 12-char short id is also unique across many draws (entropy reaches the leading bytes).
        let mut shorts = std::collections::HashSet::new();
        for _ in 0..1000 {
            assert!(shorts.insert(new_id("x")[..12].to_string()));
        }
    }

    // the fully qualified repository distinguishes cross-repo basename collisions.
    #[test]
    fn ref_repo_distinguishes_cross_repo_basenames() {
        assert_eq!(ref_repo("nginx"), ref_repo("library/nginx"));
        assert_eq!(ref_repo("nginx"), ref_repo("docker.io/library/nginx:1.25"));
        assert_eq!(ref_repo("nginx"), ref_repo("nginx:latest"));
        assert_ne!(ref_repo("nginx"), ref_repo("linuxserver/nginx"));
        assert_ne!(ref_repo("nginx"), ref_repo("ghcr.io/o/nginx"));
    }

    // `docker run nginx` must NOT resolve to a locally-present `linuxserver/nginx`.
    #[test]
    fn find_image_does_not_cross_repo_collide() {
        let only_third_party = vec![img("linuxserver/nginx:latest")];
        assert!(
            find_image(&only_third_party, "nginx").is_none(),
            "bare official nginx must not resolve to linuxserver/nginx"
        );
        let both = vec![img("linuxserver/nginx:latest"), img("nginx:latest")];
        assert_eq!(find_image(&both, "nginx").unwrap().name, "nginx:latest");
        // The third-party image still resolves to itself when its full repo is named.
        assert_eq!(
            find_image(&both, "linuxserver/nginx").unwrap().name,
            "linuxserver/nginx:latest"
        );
        // library/ prefix and docker.io/ registry are equivalent to the bare name.
        assert_eq!(
            find_image(&both, "library/nginx").unwrap().name,
            "nginx:latest"
        );
    }
}
