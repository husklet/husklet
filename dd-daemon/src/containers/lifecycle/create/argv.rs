//! Docker argv resolution for `POST /containers/create`: fold the create-body `--entrypoint`/CMD
//! overrides over the image's ENTRYPOINT/CMD into the final launch argv. Pure — no state/IO; the caller
//! supplies the four resolved config values.

/// Compute the final `argv = entrypoint ++ cmd` per docker semantics. The entrypoint is the user's
/// `--entrypoint` when given, else the image's ENTRYPOINT. A user `--entrypoint` RESETS CMD (an empty
/// cmd), but the image's own ENTRYPOINT still keeps the image CMD. An empty user Cmd falls back to the
/// image default; if the whole argv still ends up empty it falls back to the image CMD.
pub(crate) fn resolve_argv(
    user_entrypoint: Option<Vec<String>>,
    user_cmd: Option<Vec<String>>,
    img_entrypoint: &[String],
    img_cmd: &[String],
) -> Vec<String> {
    let user_ep = user_entrypoint.is_some();
    let mut argv = user_entrypoint.unwrap_or_else(|| img_entrypoint.to_vec());
    let cmd = user_cmd.filter(|c| !c.is_empty()).unwrap_or_else(|| {
        if user_ep {
            vec![]
        } else {
            img_cmd.to_vec()
        }
    });
    argv.extend(cmd);
    if argv.is_empty() {
        argv = img_cmd.to_vec();
    }
    argv
}

/// Merge `K=V` env lines with docker last-wins semantics: a duplicate key collapses to its LAST value
/// (an `-e KEY=` override replaces the image's), the surviving entry keeps the last occurrence's position,
/// and forward order is otherwise preserved. This is the *config* dedup so inspect/state don't expose a
/// stale image value that the guest launch env (`dd_jit::guest_env`) already collapses the same way.
pub(crate) fn dedup_env_last_wins(env: impl IntoIterator<Item = String>) -> Vec<String> {
    let key = |kv: &str| kv.split('=').next().unwrap_or(kv).to_string();
    let all: Vec<String> = env.into_iter().collect();
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::with_capacity(all.len());
    for kv in all.iter().rev() {
        if seen.insert(key(kv)) {
            out.push(kv.clone());
        }
    }
    out.reverse();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn dedup_env_collapses_duplicate_keys_last_wins() {
        assert_eq!(
            dedup_env_last_wins(v(&["FOO=image", "BAR=base", "FOO=run"])),
            v(&["BAR=base", "FOO=run"])
        );
    }

    #[test]
    fn defaults_to_image_entrypoint_plus_cmd() {
        // Bare `docker run img`: image ENTRYPOINT ++ image CMD.
        assert_eq!(
            resolve_argv(None, None, &v(&["/entry"]), &v(&["serve", "--foo"])),
            v(&["/entry", "serve", "--foo"])
        );
    }

    #[test]
    fn user_cmd_overrides_image_cmd_but_keeps_entrypoint() {
        // `docker run img arg1`: image ENTRYPOINT ++ user CMD.
        assert_eq!(
            resolve_argv(None, Some(v(&["arg1"])), &v(&["/entry"]), &v(&["serve"])),
            v(&["/entry", "arg1"])
        );
    }

    #[test]
    fn user_entrypoint_resets_cmd() {
        // `docker run --entrypoint /bin/sh img`: user entrypoint, and CMD is reset to empty.
        assert_eq!(
            resolve_argv(Some(v(&["/bin/sh"])), None, &v(&["/entry"]), &v(&["serve"])),
            v(&["/bin/sh"])
        );
    }

    #[test]
    fn user_entrypoint_with_user_cmd() {
        assert_eq!(
            resolve_argv(Some(v(&["/bin/sh"])), Some(v(&["-c", "echo hi"])), &[], &v(&["serve"])),
            v(&["/bin/sh", "-c", "echo hi"])
        );
    }

    #[test]
    fn empty_user_cmd_falls_back_to_image_cmd() {
        // An explicit empty Cmd ([]) is filtered out and falls back to the image default.
        assert_eq!(
            resolve_argv(None, Some(vec![]), &v(&["/entry"]), &v(&["serve"])),
            v(&["/entry", "serve"])
        );
    }

    #[test]
    fn empty_everything_falls_back_to_image_cmd() {
        // No entrypoint, no cmd, empty image entrypoint -> the whole argv would be empty, so it falls
        // back to the image CMD.
        assert_eq!(resolve_argv(None, None, &[], &v(&["run"])), v(&["run"]));
    }
}
