//! The docker `--user`/`Config.User` spec resolver ([`resolve_user`]).

/// Resolve a docker `--user`/`Config.User` spec to a numeric `(uid, gid)` against a container `rootfs`.
/// Accepts every docker form: `uid`, `name`, `uid:gid`, `name:group`, `uid:group`, `name:gid`. A numeric
/// component is taken verbatim (no file access); a NAME is looked up in `<rootfs>/etc/passwd` (user) or
/// `<rootfs>/etc/group` (group). With no group, a NAME uses its passwd primary gid while a numeric uid
/// defaults to gid 0 (docker semantics). Returns `None` if a name component can't be resolved.
pub fn resolve_user(rootfs: &str, spec: &str) -> Option<(u32, u32)> {
    let (us, gs) = spec.split_once(':').map_or((spec, None), |(u, g)| (u, Some(g)));
    // passwd line: name:passwd:uid:gid:gecos:home:shell — return (uid, primary gid) for a name match.
    let lookup_passwd = |name: &str| -> Option<(u32, u32)> {
        let passwd = std::fs::read_to_string(format!("{rootfs}/etc/passwd")).ok()?;
        passwd.lines().find_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            (f.len() >= 4 && f[0] == name).then(|| Some((f[2].parse().ok()?, f[3].parse().ok()?)))?
        })
    };
    // group line: name:passwd:gid:members — return the gid for a name match.
    let lookup_group = |name: &str| -> Option<u32> {
        let group = std::fs::read_to_string(format!("{rootfs}/etc/group")).ok()?;
        group.lines().find_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            (f.len() >= 3 && f[0] == name).then(|| f[2].parse().ok())?
        })
    };
    let (uid, primary_gid) = match us.parse::<u32>() {
        Ok(n) => (n, None),
        Err(_) => {
            let (u, g) = lookup_passwd(us)?;
            (u, Some(g))
        }
    };
    // A trailing-colon empty group (`"name:"` / `"1000:"`) means "no group" — not a parse failure.
    let gid = match gs.filter(|g| !g.is_empty()) {
        None => primary_gid.unwrap_or(0),
        Some(g) => g.parse().ok().or_else(|| lookup_group(g))?,
    };
    Some((uid, gid))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a throwaway rootfs with the given `etc/passwd`/`etc/group` contents; returns its path.
    fn make_rootfs(tag: &str, passwd: Option<&str>, group: Option<&str>) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ddjit-resolve-user-{}-{}-{}",
            std::process::id(),
            tag,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("etc")).unwrap();
        if let Some(p) = passwd {
            std::fs::write(dir.join("etc/passwd"), p).unwrap();
        }
        if let Some(g) = group {
            std::fs::write(dir.join("etc/group"), g).unwrap();
        }
        dir
    }

    const PASSWD: &str = "root:x:0:0:root:/root:/bin/sh\npostgres:x:70:70:postgres:/var/lib/postgresql:/bin/sh\n";
    const GROUP: &str = "root:x:0:\npostgres:x:70:\nstaff:x:50:\n";

    #[test]
    fn resolve_user_numeric_uid_defaults_gid_zero() {
        // A bare numeric uid needs no rootfs and gets gid 0 (docker semantics).
        let dir = make_rootfs("num", None, None);
        assert_eq!(resolve_user(dir.to_str().unwrap(), "1000"), Some((1000, 0)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_user_numeric_uid_and_gid() {
        let dir = make_rootfs("numnum", None, None);
        assert_eq!(resolve_user(dir.to_str().unwrap(), "1000:2000"), Some((1000, 2000)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_user_name_uses_passwd_primary_gid() {
        let dir = make_rootfs("name", Some(PASSWD), Some(GROUP));
        assert_eq!(resolve_user(dir.to_str().unwrap(), "postgres"), Some((70, 70)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_user_name_group_via_group_lookup() {
        // `name:group` resolves the group through /etc/group.
        let dir = make_rootfs("namegrp", Some(PASSWD), Some(GROUP));
        assert_eq!(resolve_user(dir.to_str().unwrap(), "postgres:postgres"), Some((70, 70)));
        // A cross group: user postgres with the numeric-named `staff` group by name.
        assert_eq!(resolve_user(dir.to_str().unwrap(), "postgres:staff"), Some((70, 50)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_user_trailing_colon_empty_group_is_gid_zero() {
        // A numeric uid with a trailing empty group: not a parse failure, gid falls back to 0.
        let dir = make_rootfs("trail", None, None);
        assert_eq!(resolve_user(dir.to_str().unwrap(), "1000:"), Some((1000, 0)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_user_name_trailing_colon_keeps_primary_gid() {
        // A NAME with a trailing empty group keeps its passwd primary gid (not 0).
        let dir = make_rootfs("nametrail", Some(PASSWD), Some(GROUP));
        assert_eq!(resolve_user(dir.to_str().unwrap(), "postgres:"), Some((70, 70)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_user_unresolvable_name_returns_none() {
        // No passwd file at all: a name can't be resolved.
        let dir = make_rootfs("missing", None, None);
        assert_eq!(resolve_user(dir.to_str().unwrap(), "postgres"), None);
        // Present passwd but the name isn't in it.
        let dir2 = make_rootfs("absent", Some(PASSWD), Some(GROUP));
        assert_eq!(resolve_user(dir2.to_str().unwrap(), "nobody"), None);
        // Known user, but the named group is absent from /etc/group.
        assert_eq!(resolve_user(dir2.to_str().unwrap(), "postgres:ghosts"), None);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }
}
