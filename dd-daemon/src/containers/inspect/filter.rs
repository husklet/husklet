//! Pure `docker ps` helpers: `--size` accounting (`container_sizes`), `--filter` matching
//! (`ps_match`), and the humanized Status column (`human_status`). Side-effect-free container→value
//! transforms (bar the on-disk `du` walk in `container_sizes`) split out from the async list handler
//! in `list.rs`, which pulls them back in via `use super::filter::*`.
use super::super::*;

/// `docker ps --size` -> (SizeRw, SizeRootFs). dd gives each container a private copy-on-write UPPER over
/// the read-only image rootfs, so SizeRw is the `du`-style size of that writable upper layer (matching
/// docker, which measures the container's writable diff) and SizeRootFs is the full image rootfs walk.
/// The host-fs `macos` image (rootfs "/") is skipped -- walking it would be catastrophic, exactly as
/// `image_size` guards against.
pub(super) fn container_sizes(c: &Container) -> (i64, i64) {
    if c.image == "macos" || c.rootfs.is_empty() || c.rootfs == "/" {
        return (0, 0);
    }
    let rw = if c.upper.is_empty() {
        0
    } else {
        dir_size(std::path::Path::new(&c.upper))
    };
    (rw, dir_size(std::path::Path::new(&c.rootfs)))
}

/// Apply `docker ps --filter`. `f` is the decoded `filters` map (`{"status":[..],"name":[..],"label":[..]}`).
/// Within a filter type the values are OR'd; `label` entries are AND'd (each must match). `name` is a
/// substring match against the container's effective name; `label` matches `key` or `key=value`.
/// `before_ts`/`since_ts` are the `created` timestamps of the containers named by `before=`/`since=`
/// (resolved by the caller, which holds the full container map); `None` => that key is absent/unresolved.
pub(super) fn ps_match(
    c: &Container,
    name: &str,
    f: &HashMap<String, Vec<String>>,
    before_ts: Option<i64>,
    since_ts: Option<i64>,
) -> bool {
    if let Some(vals) = f.get("status") {
        if !vals.iter().any(|v| v == &c.status) {
            return false;
        }
    }
    if let Some(vals) = f.get("name") {
        if !vals.iter().any(|v| name.contains(v.as_str())) {
            return false;
        }
    }
    if let Some(vals) = f.get("label") {
        for v in vals {
            let ok = match v.split_once('=') {
                Some((k, val)) => c.labels.get(k).map(|cv| cv == val).unwrap_or(false),
                None => c.labels.contains_key(v),
            };
            if !ok {
                return false;
            }
        }
    }
    // `id=`: full-or-prefix match on the container id (docker accepts a leading prefix).
    if let Some(vals) = f.get("id") {
        if !vals.iter().any(|v| c.id.starts_with(v.as_str())) {
            return false;
        }
    }
    // `ancestor=`: the image the container was created from (repo[:tag] or a raw image ref).
    if let Some(vals) = f.get("ancestor") {
        // Repository-aware: `ancestor=nginx` must not also match a `linuxserver/nginx`
        // container just because the basenames coincide. Compare the fully qualified repository.
        if !vals
            .iter()
            .any(|v| c.image == *v || ref_repo(&c.image) == ref_repo(v))
        {
            return false;
        }
    }
    // `exited=N`: containers that exited with code N (only meaningful for the exited state).
    if let Some(vals) = f.get("exited") {
        if !vals.iter().any(|v| {
            v.parse::<i64>()
                .map_or(false, |n| c.status == "exited" && c.exit_code == n)
        }) {
            return false;
        }
    }
    // `health=`: dd models no healthcheck, so every container is effectively `none`; any other value
    // (starting/healthy/unhealthy) matches nothing.
    if let Some(vals) = f.get("health") {
        if !vals.iter().any(|v| v == "none") {
            return false;
        }
    }
    // `before=`/`since=`: created strictly before / after the referenced container (by create time).
    if let Some(ts) = before_ts {
        if c.created >= ts {
            return false;
        }
    }
    if let Some(ts) = since_ts {
        if c.created <= ts {
            return false;
        }
    }
    true
}

/// Render a container's `docker ps` Status column the way docker does: "Up 3 minutes" while
/// running/paused (measured from `StartedAt`), "Exited (0) 5 minutes ago" otherwise (measured from
/// `FinishedAt`). Elapsed time is humanized coarsely (seconds/minutes/hours/days). Falls back to
/// `created` when the relevant timestamp was never set (legacy/edge state).
pub(super) fn human_status(c: &Container) -> String {
    // Coarse elapsed-since humanizer for a base unix-secs timestamp.
    let humanize = |base: i64| -> String {
        let secs = (now_secs() - base).max(0);
        if secs < 60 {
            format!("{secs} seconds")
        } else if secs < 3600 {
            format!("{} minutes", secs / 60)
        } else if secs < 86400 {
            format!("{} hours", secs / 3600)
        } else {
            format!("{} days", secs / 86400)
        }
    };
    let started = if c.started_at > 0 { c.started_at } else { c.created };
    let finished = if c.finished_at > 0 { c.finished_at } else { c.created };
    if c.status == "restarting" {
        // Docker shows a container in its restart-backoff window as "Restarting (code) …" from the
        // last exit time.
        format!("Restarting ({}) {} ago", c.exit_code, humanize(finished))
    } else if c.status == "running" || c.status == "paused" {
        // "Up X" is measured from StartedAt, NOT creation time.
        format!("Up {}", humanize(started))
    } else if c.status == "created" {
        // A created-but-never-started container shows a bare "Created" (no elapsed time), matching docker.
        "Created".to_string()
    } else {
        // "Exited (code) X ago" is measured from FinishedAt.
        format!("Exited ({}) {} ago", c.exit_code, humanize(finished))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctr() -> Container {
        Container {
            id: "abc123def456".into(),
            image: "nginx".into(),
            status: "running".into(),
            ..Default::default()
        }
    }

    fn filt(pairs: &[(&str, &[&str])]) -> HashMap<String, Vec<String>> {
        pairs
            .iter()
            .map(|(k, vs)| (k.to_string(), vs.iter().map(|s| s.to_string()).collect()))
            .collect()
    }

    #[test]
    fn ps_match_no_filters_matches() {
        assert!(ps_match(&ctr(), "web", &HashMap::new(), None, None));
    }

    #[test]
    fn human_status_up_measured_from_started_not_created() {
        // Regression: a container CREATED long ago but STARTED recently must read its recent uptime.
        let mut c = ctr();
        let now = now_secs();
        c.created = now - 100_000; // ~27h ago
        c.started_at = now - 90; // 90s ago
        assert_eq!(human_status(&c), "Up 1 minutes");
    }

    #[test]
    fn human_status_exited_measured_from_finished() {
        let mut c = ctr();
        let now = now_secs();
        c.status = "exited".into();
        c.exit_code = 0;
        c.created = now - 100_000;
        c.finished_at = now - 5; // 5s ago
        assert_eq!(human_status(&c), "Exited (0) 5 seconds ago");
    }

    #[test]
    fn human_status_falls_back_to_created_when_no_started_at() {
        // Legacy/edge: started_at unset (0) -> fall back to `created` so we still show elapsed.
        let mut c = ctr();
        let now = now_secs();
        c.created = now - 3661; // ~1h ago
        c.started_at = 0;
        assert_eq!(human_status(&c), "Up 1 hours");
    }

    #[test]
    fn ps_match_status_ors_within_type() {
        let c = ctr(); // status "running"
        assert!(ps_match(&c, "web", &filt(&[("status", &["running", "exited"])]), None, None));
        assert!(!ps_match(&c, "web", &filt(&[("status", &["exited"])]), None, None));
    }

    #[test]
    fn ps_match_name_is_substring() {
        let c = ctr();
        assert!(ps_match(&c, "myweb1", &filt(&[("name", &["web"])]), None, None));
        assert!(!ps_match(&c, "myweb1", &filt(&[("name", &["db"])]), None, None));
    }

    #[test]
    fn ps_match_label_key_and_kv_are_anded() {
        let mut c = ctr();
        c.labels.insert("env".into(), "prod".into());
        c.labels.insert("team".into(), "core".into());
        // key-only presence and key=value both match.
        assert!(ps_match(&c, "web", &filt(&[("label", &["env"])]), None, None));
        assert!(ps_match(&c, "web", &filt(&[("label", &["env=prod"])]), None, None));
        // Wrong value fails; multiple label entries are AND'd (a missing one fails the whole match).
        assert!(!ps_match(&c, "web", &filt(&[("label", &["env=dev"])]), None, None));
        assert!(!ps_match(&c, "web", &filt(&[("label", &["env", "missing"])]), None, None));
    }

    #[test]
    fn ps_match_id_prefix() {
        let c = ctr(); // id "abc123def456"
        assert!(ps_match(&c, "web", &filt(&[("id", &["abc123"])]), None, None));
        assert!(!ps_match(&c, "web", &filt(&[("id", &["zzz"])]), None, None));
    }

    #[test]
    fn ps_match_exited_code() {
        let mut c = ctr();
        c.status = "exited".into();
        c.exit_code = 137;
        assert!(ps_match(&c, "web", &filt(&[("exited", &["137"])]), None, None));
        assert!(!ps_match(&c, "web", &filt(&[("exited", &["0"])]), None, None));
    }

    #[test]
    fn ps_match_health_only_none_matches() {
        let c = ctr();
        assert!(ps_match(&c, "web", &filt(&[("health", &["none"])]), None, None));
        assert!(!ps_match(&c, "web", &filt(&[("health", &["healthy"])]), None, None));
    }

    #[test]
    fn ps_match_before_and_since_are_strict() {
        let mut c = ctr();
        c.created = 100;
        // before=ts: created must be strictly < ts.
        assert!(ps_match(&c, "web", &HashMap::new(), Some(101), None));
        assert!(!ps_match(&c, "web", &HashMap::new(), Some(100), None));
        // since=ts: created must be strictly > ts.
        assert!(ps_match(&c, "web", &HashMap::new(), None, Some(99)));
        assert!(!ps_match(&c, "web", &HashMap::new(), None, Some(100)));
    }

    #[test]
    fn human_status_created_is_bare() {
        let mut c = ctr();
        c.status = "created".into();
        // A never-started container is a bare "Created" with no elapsed time (time-independent).
        assert_eq!(human_status(&c), "Created");
    }

    #[test]
    fn human_status_prefix_by_state() {
        let mut c = ctr();
        c.created = now_secs(); // ~0 elapsed; assert only the state-dependent prefix.
        c.status = "running".into();
        assert!(human_status(&c).starts_with("Up "), "{}", human_status(&c));
        c.status = "exited".into();
        c.exit_code = 2;
        assert!(human_status(&c).starts_with("Exited (2) "), "{}", human_status(&c));
        c.status = "restarting".into();
        c.exit_code = 1;
        assert!(human_status(&c).starts_with("Restarting (1) "), "{}", human_status(&c));
    }

    #[test]
    fn container_sizes_guards_return_zero_without_touching_fs() {
        // The catastrophic-walk guards: host-fs `macos`, empty rootfs, and rootfs "/" all short-circuit.
        let mut c = ctr();
        c.image = "macos".into();
        c.rootfs = "/".into();
        assert_eq!(container_sizes(&c), (0, 0));
        c.image = "nginx".into();
        c.rootfs = "".into();
        assert_eq!(container_sizes(&c), (0, 0));
        c.rootfs = "/".into();
        assert_eq!(container_sizes(&c), (0, 0));
    }
}
