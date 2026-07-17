//! Pure value transforms behind `spawn_container`: mount-source resolution, `--cpus` whole-CPU
//! rounding, and sandbox detection. No state/IO — the caller gathers the inputs and drives the
//! `JitContainer` builder; these helpers only compute the values it feeds in.
use super::*;

/// Resolve a `-v`/`--mount` source to a host path: an absolute path is a bind (verbatim); any other
/// token is a named volume — its registered mountpoint when known, else a dir under `volumes_dir`.
pub(super) fn resolve_mount_src(src: &str, volumes_dir: &str, vols: &[Vol]) -> String {
    if src.starts_with('/') {
        src.to_string()
    } else if let Some(v) = vols.iter().find(|v| v.name == src) {
        v.mountpoint.clone()
    } else {
        // A named-volume-looking source: resolve under volumes_dir, but NEVER let a crafted name like
        // `../../etc` escape the volumes root via path-join (a host-path traversal). Lexically normalize
        // the join and, if it would land outside the root, clamp to a single sanitized segment inside it.
        let root = PathBuf::from(volumes_dir);
        let normalized = lexical_normalize(&root.join(src));
        if normalized.starts_with(&root) {
            normalized.to_string_lossy().into_owned()
        } else {
            root.join(src.replace(['/', '\\'], "_"))
                .to_string_lossy()
                .into_owned()
        }
    }
}

/// Resolve `.`/`..` path components LEXICALLY (no filesystem access, so it works for not-yet-created
/// volume dirs): `.` is dropped, `..` pops the previous normal component (never past the root prefix).
/// Used to detect a volume source that would path-join outside the volumes root.
fn lexical_normalize(p: &std::path::Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                // Pop only a normal segment; never ascend past a root/prefix (keeps escapes detectable).
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                } else {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `--cpus`: NanoCpus → ceil to whole online CPUs (0/unset = unlimited). Mirrors moby's whole-CPU
/// rounding so a fractional `--cpus 1.5` still reserves 2 whole CPUs.
pub(super) fn nano_cpus_to_cpus(nano_cpus: i64) -> u32 {
    if nano_cpus > 0 {
        ((nano_cpus + 999_999_999) / 1_000_000_000) as u32
    } else {
        0
    }
}

/// `--security-opt sandbox`/`untrusted`: run under the untrusted-guest sentry. Case-insensitive
/// substring match over the security-opt list (any entry mentioning `sandbox`/`untrusted` opts in).
pub(super) fn wants_sandbox(security_opt: &[String]) -> bool {
    security_opt.iter().any(|o| {
        let o = o.to_ascii_lowercase();
        o.contains("sandbox") || o.contains("untrusted")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vol(name: &str, mountpoint: &str) -> Vol {
        Vol {
            name: name.to_string(),
            mountpoint: mountpoint.to_string(),
            created_at: 0,
            driver: String::new(),
            options: std::collections::HashMap::new(),
            labels: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn resolve_mount_src_absolute_is_verbatim_bind() {
        assert_eq!(resolve_mount_src("/host/path", "/vols", &[]), "/host/path");
    }

    #[test]
    fn resolve_mount_src_named_volume_uses_registered_mountpoint() {
        let vols = vec![vol("data", "/registered/data")];
        assert_eq!(
            resolve_mount_src("data", "/vols", &vols),
            "/registered/data"
        );
    }

    #[test]
    fn resolve_mount_src_unknown_name_falls_under_volumes_dir() {
        assert_eq!(resolve_mount_src("data", "/vols", &[]), "/vols/data");
    }

    // "Inline Volume Sources Can Escape volumes_dir": a `../..`-laced source must NOT path-join outside
    // the volumes root — it is clamped to a sanitized single segment inside it.
    #[test]
    fn resolve_mount_src_dotdot_source_stays_inside_volumes_dir() {
        for bad in ["../escaped", "../../etc", "a/../../b"] {
            let got = resolve_mount_src(bad, "/vols", &[]);
            assert!(
                got.starts_with("/vols/")
                    && !lexical_normalize(std::path::Path::new(&got)).starts_with("/etc"),
                "{bad} must resolve inside /vols, got {got}"
            );
        }
        // A normal name is unaffected.
        assert_eq!(resolve_mount_src("data", "/vols", &[]), "/vols/data");
    }

    #[test]
    fn nano_cpus_to_cpus_ceils_and_zeroes() {
        assert_eq!(nano_cpus_to_cpus(0), 0); // unset -> unlimited
        assert_eq!(nano_cpus_to_cpus(-1), 0); // negative -> unlimited
        assert_eq!(nano_cpus_to_cpus(1_000_000_000), 1); // exactly 1 CPU
        assert_eq!(nano_cpus_to_cpus(1_500_000_000), 2); // 1.5 -> 2 whole CPUs
        assert_eq!(nano_cpus_to_cpus(1), 1); // any positive fraction -> at least 1
    }

    #[test]
    fn wants_sandbox_matches_case_insensitive_substring() {
        assert!(wants_sandbox(&["seccomp=x".into(), "SANDBOX".into()]));
        assert!(wants_sandbox(&["label=Untrusted".into()]));
        assert!(!wants_sandbox(&["seccomp=unconfined".into()]));
        assert!(!wants_sandbox(&[]));
    }
}
