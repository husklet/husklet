//! Ranking + dedup: score each [`DiscoveredImage`] and collapse duplicate tags to a single best entry.

use super::*;

/// A coarse "richness" score for a [`DiscoveredImage`], used to pick the best entry when several
/// directories resolve to the same tag (see `dedup_images`). A non-empty environment is the decisive
/// signal: `poc/images` ships some images twice — a single-underscore dd-format dir whose sidecar
/// recorded an empty `env`, AND a umoci bundle dir carrying the full OCI config — and the bundle one
/// (real env) must win. The remaining run metadata break finer ties.
pub fn image_score(img: &DiscoveredImage) -> i32 {
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
    // A recorded CMD beats the `/bin/sh` default the discovery fallback substitutes.
    if img.cmd.len() != 1 || img.cmd[0] != "/bin/sh" {
        s += 1;
    }
    s
}

/// Collapse images that resolve to the same `repository:tag` down to a single best entry so lookup is
/// deterministic regardless of `read_dir` order. Ranks by [`image_score`] (richest wins) and breaks
/// exact ties on the name string so the survivor is stable across runs and machines.
pub(super) fn dedup_images(mut imgs: Vec<DiscoveredImage>) -> Vec<DiscoveredImage> {
    imgs.sort_by(|a, b| {
        image_score(b)
            .cmp(&image_score(a))
            .then_with(|| a.name.cmp(&b.name))
    });
    let mut seen = std::collections::HashSet::new();
    imgs.retain(|i| seen.insert(repo_tag(&i.name)));
    imgs
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal, "poor" DiscoveredImage (empty env, default `/bin/sh` cmd) to perturb per test.
    fn base(name: &str) -> DiscoveredImage {
        DiscoveredImage {
            name: name.to_string(),
            rootfs: PathBuf::from("/nonexistent"),
            arch: Arch::LinuxAarch64,
            cmd: vec!["/bin/sh".to_string()],
            env: vec![],
            entrypoint: vec![],
            workdir: String::new(),
            user: String::new(),
            exposed_ports: vec![],
            created: 0,
            stop_signal: String::new(),
            img_volumes: vec![],
            healthcheck: None,
        }
    }

    #[test]
    fn image_score_env_is_decisive() {
        // A poor bundle scores 0; env alone is worth more than every finer signal combined.
        assert_eq!(image_score(&base("x")), 0);

        let mut with_env = base("x");
        with_env.env = vec!["PATH=/usr/bin".to_string()];
        assert_eq!(image_score(&with_env), 1000);

        // The finer tie-breakers: entrypoint (+10), workdir (+5), non-default cmd (+1).
        let mut rich_but_no_env = base("x");
        rich_but_no_env.entrypoint = vec!["/entry".to_string()];
        rich_but_no_env.workdir = "/app".to_string();
        rich_but_no_env.cmd = vec!["/run".to_string()];
        assert_eq!(image_score(&rich_but_no_env), 16);
        // env still wins outright over all finer metadata combined.
        assert!(image_score(&with_env) > image_score(&rich_but_no_env));
    }

    #[test]
    fn dedup_prefers_the_bundle_with_env() {
        // Two dirs resolve to the same tag (`busybox:latest`): one poor (no env), one with env.
        let poor = base("busybox");
        let mut rich = base("busybox");
        rich.env = vec!["HOME=/root".to_string()];

        // Order must not matter: the env-carrying entry always survives.
        for imgs in [vec![poor.clone(), rich.clone()], vec![rich.clone(), poor.clone()]] {
            let out = dedup_images(imgs);
            assert_eq!(out.len(), 1, "same tag collapses to one entry");
            assert_eq!(out[0].env, vec!["HOME=/root".to_string()], "env bundle wins");
        }
    }

    #[test]
    fn dedup_keeps_distinct_tags() {
        let out = dedup_images(vec![base("busybox"), base("alpine")]);
        assert_eq!(out.len(), 2);
    }
}
