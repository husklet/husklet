//! The docker-parity guest environment helper ([`guest_env`]) and the default guest PATH.

/// The PATH a guest gets when its image sets none — matches the docker daemon's default.
pub const DEFAULT_GUEST_PATH: &str =
    "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// Resolve a merged container environment (image `Config.Env` then `-e` overrides, as `K=V` lines) into
/// the exact lines a guest should see: duplicate keys collapse to their LAST value (an explicit `-e KEY=`
/// overrides the image), forward order is preserved, a default PATH is injected when the image set none,
/// and — when a pseudo-terminal is allocated (`tty`) and the image set no TERM — `TERM=xterm` is added
/// (docker parity, so readline/ncurses/debconf get a real terminal). A non-tty container gets no TERM.
pub fn guest_env(env: &[String], tty: bool) -> Vec<String> {
    let key = |kv: &str| kv.split('=').next().unwrap_or(kv).to_string();
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::with_capacity(env.len());
    // Walk back-to-front so the LAST assignment of each key wins, then restore forward order.
    for kv in env.iter().rev() {
        if seen.insert(key(kv)) {
            out.push(kv.clone());
        }
    }
    out.reverse();
    if !seen.contains("PATH") {
        out.push(DEFAULT_GUEST_PATH.to_string());
    }
    if tty && !seen.contains("TERM") {
        out.push("TERM=xterm".to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guest_env_dedup_last_wins_and_default_path() {
        // Last assignment of a key wins; forward order preserved; PATH injected when absent.
        let merged = vec!["A=1".to_string(), "B=x".to_string(), "A=2".to_string()];
        let out = guest_env(&merged, false);
        assert_eq!(out, vec!["B=x", "A=2", DEFAULT_GUEST_PATH]);
        // No TERM without a tty.
        assert!(!out.iter().any(|l| l.starts_with("TERM=")));
    }

    #[test]
    fn guest_env_tty_injects_term_and_respects_image_path() {
        let merged = vec!["PATH=/opt/bin".to_string()];
        let out = guest_env(&merged, true);
        assert_eq!(out, vec!["PATH=/opt/bin", "TERM=xterm"]);
    }

    #[test]
    fn guest_env_empty_gets_default_path_and_no_term() {
        // Empty env with no tty: only the default PATH is injected.
        let out = guest_env(&[], false);
        assert_eq!(out, vec![DEFAULT_GUEST_PATH]);
        // With a tty, an empty env gets both PATH and TERM (PATH first, in injection order).
        let out = guest_env(&[], true);
        assert_eq!(out, vec![DEFAULT_GUEST_PATH, "TERM=xterm"]);
    }

    #[test]
    fn guest_env_existing_term_not_overwritten() {
        // A caller-supplied TERM survives; no second TERM is appended.
        let merged = vec!["TERM=screen-256color".to_string()];
        let out = guest_env(&merged, true);
        assert_eq!(out, vec!["TERM=screen-256color", DEFAULT_GUEST_PATH]);
    }
}
