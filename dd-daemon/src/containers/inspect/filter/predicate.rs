//! `docker ps --filter` matching: the `ps_match` predicate over the decoded `filters` map.
use super::*;

/// Apply `docker ps --filter`. `f` is the decoded `filters` map (`{"status":[..],"name":[..],"label":[..]}`).
/// Within a filter type the values are OR'd; `label` entries are AND'd (each must match). `name` is a
/// substring match against the container's effective name; `label` matches `key` or `key=value`.
/// `before_ts`/`since_ts` are the `created` timestamps of the containers named by `before=`/`since=`
/// (resolved by the caller, which holds the full container map); `None` => that key is absent/unresolved.
pub(crate) fn ps_match(
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
