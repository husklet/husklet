//! Image discovery + reference resolution, thinned onto `hl-images`.
//!
//! The pure pipeline — scanning the store, sniffing each image's arch from binary magic, recovering env
//! from an on-disk OCI config, and deduping by tag — lives in `hl_images` (runtime-agnostic). This module
//! maps its plain [`hl_images::DiscoveredImage`] results onto the daemon's [`Image`] model and keeps the
//! over-the-live-store resolver ([`find_image`]) that ranks the daemon's own `Image`s.
use super::*;

// `ref_name` / `ref_repo` are pure reference-string helpers now owned by hl-images; re-export them under
// their old names so `crate::util::ref_name` / `crate::util::ref_repo` call sites keep resolving.
pub(crate) use hl_images::{ref_name, ref_repo};

/// Discover `<images>/<name>/rootfs` dirs and map each onto the daemon's [`Image`]. The scan + arch
/// detection + env recovery + tag-dedup all happen in `hl_images`; here we only translate the plain
/// [`hl_images::DiscoveredImage`] (runtime-agnostic arch, raw-JSON healthcheck) into an `Image`.
pub(crate) fn discover_images(images_dir: &str) -> Vec<Image> {
    let mut imgs: Vec<Image> = hl_images::discover_images(images_dir)
        .into_iter()
        .map(image_from_discovered)
        .collect();
    // Re-apply persisted `docker tag` aliases: each records `alias -> rootfs`, so we clone the discovered
    // base image (matched by rootfs) under the alias name. This makes tags survive a daemon restart —
    // previously they lived only in memory and vanished on rediscovery.
    for (alias, rootfs) in read_tag_aliases(images_dir) {
        if imgs.iter().any(|i| i.name == alias) {
            continue;
        }
        if let Some(base) = imgs.iter().find(|i| i.rootfs == rootfs).cloned() {
            imgs.push(Image { name: alias, ..base });
        }
    }
    imgs
}

/// The dir holding persisted `docker tag` alias records (`<images_dir>/hl-aliases/*.json`).
fn tag_aliases_dir(images_dir: &str) -> std::path::PathBuf {
    std::path::Path::new(images_dir).join("hl-aliases")
}

/// Persist one `docker tag` alias (`alias` reference -> the shared image `rootfs`) so it survives a daemon
/// restart. Best-effort — a write failure just means the alias won't survive, matching prior in-memory-only
/// behavior for the failure case.
pub(crate) fn persist_tag_alias(images_dir: &str, alias: &str, rootfs: &str) {
    let dir = tag_aliases_dir(images_dir);
    let _ = std::fs::create_dir_all(&dir);
    let file = dir.join(format!("{}.json", crate::util::fake_id(alias)));
    let _ = std::fs::write(
        file,
        serde_json::json!({ "name": alias, "rootfs": rootfs }).to_string(),
    );
}

/// Read every persisted tag alias as `(alias, rootfs)` pairs. Missing dir / bad files are skipped.
fn read_tag_aliases(images_dir: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(tag_aliases_dir(images_dir)) else {
        return out;
    };
    for e in rd.flatten() {
        if let Some(v) = std::fs::read_to_string(e.path())
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        {
            if let (Some(name), Some(rootfs)) = (v["name"].as_str(), v["rootfs"].as_str()) {
                out.push((name.to_string(), rootfs.to_string()));
            }
        }
    }
    out
}

/// Map one runtime-agnostic [`hl_images::DiscoveredImage`] onto the daemon's [`Image`]: its [`hl_images::Arch`]
/// becomes a [`Guest`] and its raw-JSON healthcheck is parsed into a [`crate::model::HealthConfig`].
fn image_from_discovered(d: hl_images::DiscoveredImage) -> Image {
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
        labels: d.labels,
        created: d.created,
        stop_signal: d.stop_signal,
        img_volumes: d.img_volumes,
        healthcheck,
        ..Default::default()
    }
}

/// Probe the rootfs and pick the guest target from a binary's magic (hl-images' detector mapped onto the
/// runtime's [`Guest`] personality). Tries well-known paths first, then a bounded whole-rootfs scan.
pub(crate) fn detect_arch(rootfs: &std::path::Path) -> Option<Guest> {
    hl_images::detect_arch(rootfs).map(guest_of)
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
/// deterministically. hl's lookup is lenient — a bare name matches any stored image with that repository
/// regardless of tag (see [`ref_repo`]) — so several images can match one query. Ranks by:
///   1. an exact `repository:tag` match for the requested reference, then `<name>:latest`,
///   2. then the richest metadata (a real environment beats an empty one — see [`image_score`]),
///   3. then the name string (reversed so the lexicographically smallest wins) to settle any remainder.
pub(crate) fn find_image<'a>(images: &'a [Image], reference: &str) -> Option<&'a Image> {
    let want_repo = ref_repo(reference);
    let want = ref_name(reference);
    let want_rt = repo_tag(reference);
    let want_latest = format!("{want}:latest");
    // Whether the reference pinned an EXPLICIT tag (a ':' in the last path segment, excluding a `@digest`
    // and a registry `host:port`). `app:2` should resolve ONLY to `app:2` — never fall back to a sibling
    // tag like `app:1`. A bare `app` keeps the lenient best-local-tag resolution other flows depend on.
    let has_explicit_tag = reference
        .rsplit('/')
        .next()
        .is_some_and(|last| last.contains(':') && !last.contains('@'));
    let best = images
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
        });
    match best {
        // An explicit tag that isn't present locally is a not-found — do NOT hand back a different tag.
        Some(i) if has_explicit_tag && repo_tag(&i.name) != want_rt => None,
        other => other,
    }
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

    // "Explicit Tag Lookup Falls Back To Another Tag" (P1): `app:2` must resolve only to `app:2`, never
    // to a sibling `app:1`; a bare `app` keeps resolving to the best local tag.
    #[test]
    fn find_image_rejects_missing_explicit_tag() {
        let only_v1 = vec![img("app:1")];
        assert!(
            find_image(&only_v1, "app:2").is_none(),
            "explicit app:2 must not fall back to app:1"
        );
        // The explicit tag resolves when it IS present.
        let both = vec![img("app:1"), img("app:2")];
        assert_eq!(find_image(&both, "app:2").unwrap().name, "app:2");
        // A bare reference still resolves leniently to a local tag.
        assert_eq!(find_image(&only_v1, "app").unwrap().name, "app:1");
    }
}

#[cfg(test)]
mod image_score_tests {
    use super::*;

    // A default (empty-config) image: env/entrypoint/workdir/labels all empty, and cmd empty (len != 1),
    // so ONLY the "cmd isn't the plain /bin/sh default" branch fires -> score 1.
    #[test]
    fn image_score_default_is_one() {
        assert_eq!(image_score(&Image::default()), 1);
    }

    // An image whose cmd is EXACTLY ["/bin/sh"] (the discovery fallback default) scores the cmd branch as
    // 0, so a bare such image is the unique score-0 case.
    #[test]
    fn image_score_bin_sh_cmd_scores_zero() {
        let img = Image {
            cmd: vec!["/bin/sh".to_string()],
            ..Default::default()
        };
        assert_eq!(image_score(&img), 0);
    }

    // A cmd of length 1 that is NOT "/bin/sh", or any multi-element cmd, still earns the +1 cmd bonus.
    #[test]
    fn image_score_non_default_cmd_scores_one() {
        let bash = Image {
            cmd: vec!["/bin/bash".to_string()],
            ..Default::default()
        };
        assert_eq!(image_score(&bash), 1);
        let multi = Image {
            cmd: vec!["/bin/sh".to_string(), "-c".to_string()],
            ..Default::default()
        };
        assert_eq!(image_score(&multi), 1);
    }

    // A non-empty environment is the decisive +1000 signal; combined with the empty-cmd +1 it is 1001.
    #[test]
    fn image_score_env_dominates() {
        let img = Image {
            env: vec!["A=1".to_string()],
            ..Default::default()
        };
        assert_eq!(image_score(&img), 1001);
    }

    // Each contributing field adds its documented weight; they sum. env 1000 + entrypoint 10 + workdir 5
    // + 2 labels + cmd bonus 1 = 1018.
    #[test]
    fn image_score_weights_sum() {
        let mut labels = std::collections::HashMap::new();
        labels.insert("a".to_string(), "b".to_string());
        labels.insert("c".to_string(), "d".to_string());
        let img = Image {
            env: vec!["A=1".to_string()],
            entrypoint: vec!["/e".to_string()],
            workdir: "/w".to_string(),
            labels,
            cmd: vec!["run".to_string()],
            ..Default::default()
        };
        assert_eq!(image_score(&img), 1000 + 10 + 5 + 2 + 1);
    }

    // Entrypoint (+10) and workdir (+5) each contribute independently of env/labels.
    #[test]
    fn image_score_entrypoint_and_workdir_only() {
        let ep = Image {
            entrypoint: vec!["/entry".to_string()],
            cmd: vec!["/bin/sh".to_string()], // suppress the cmd bonus to isolate the +10
            ..Default::default()
        };
        assert_eq!(image_score(&ep), 10);
        let wd = Image {
            workdir: "/srv".to_string(),
            cmd: vec!["/bin/sh".to_string()],
            ..Default::default()
        };
        assert_eq!(image_score(&wd), 5);
    }
}
