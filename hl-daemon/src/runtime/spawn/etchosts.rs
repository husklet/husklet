//! Pure text/value builders for the `/etc` files `spawn_live` writes into a guest's writable layer.
//! The caller gathers the endpoint data under the state lock; these helpers just render it, so the
//! reach-by-name `/etc/hosts` body and the effective UTS hostname are byte-for-byte characterizable.

/// The own-entry name column for `/etc/hosts`: the endpoint's network name, plus the container's
/// `--hostname` when it is set AND differs from the network name (Docker lists both as aliases of the
/// own IP). Returns just the network name otherwise.
pub(super) fn own_hosts_names(own_name: &str, hostname: &str) -> String {
    let mut names = own_name.to_string();
    if !hostname.is_empty() && hostname != own_name {
        names.push(' ');
        names.push_str(hostname);
    }
    names
}

/// Render the reach-by-name `/etc/hosts` body from already-gathered endpoint data: the fixed
/// `127.0.0.1 localhost` line, the container's own `ip\tnames` line (when it has an endpoint), then one
/// `ip\tname` line per same-network peer in the exact order the caller collected them. Byte-identical to
/// the inline builder `spawn_live` used before extraction.
pub(super) fn render_etc_hosts(own: Option<(&str, &str)>, peers: &[(String, String)]) -> String {
    let mut hosts = String::from("127.0.0.1\tlocalhost\n");
    if let Some((ip, names)) = own {
        hosts.push_str(&format!("{ip}\t{names}\n"));
    }
    for (ip, name) in peers {
        hosts.push_str(&format!("{ip}\t{name}\n"));
    }
    hosts
}

/// The effective UTS hostname written to `/etc/hostname` (and handed to the engine as `HL_HOSTNAME`):
/// the user's `--hostname` when set, else Docker's default 12-char short id derived from the full id.
pub(super) fn eff_hostname(id: &str, hostname: &str) -> String {
    if hostname.is_empty() {
        id[..id.len().min(12)].to_string()
    } else {
        hostname.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_hosts_names_appends_distinct_hostname() {
        // --hostname set and different from the network name -> both listed, space-separated.
        assert_eq!(own_hosts_names("web", "myhost"), "web myhost");
    }

    #[test]
    fn own_hosts_names_omits_empty_or_equal_hostname() {
        // empty hostname -> just the network name.
        assert_eq!(own_hosts_names("web", ""), "web");
        // hostname equal to the network name -> not duplicated.
        assert_eq!(own_hosts_names("web", "web"), "web");
    }

    #[test]
    fn render_etc_hosts_localhost_only_when_no_endpoints() {
        // --network host/none: no own endpoint, no peers -> just the localhost line.
        assert_eq!(render_etc_hosts(None, &[]), "127.0.0.1\tlocalhost\n");
    }

    #[test]
    fn render_etc_hosts_own_then_peers_in_order() {
        let peers = vec![
            ("172.18.0.3".to_string(), "db".to_string()),
            ("172.18.0.4".to_string(), "cache".to_string()),
        ];
        let out = render_etc_hosts(Some(("172.18.0.2", "web myhost")), &peers);
        assert_eq!(
            out,
            "127.0.0.1\tlocalhost\n\
             172.18.0.2\tweb myhost\n\
             172.18.0.3\tdb\n\
             172.18.0.4\tcache\n"
        );
    }

    #[test]
    fn render_etc_hosts_own_no_peers() {
        let out = render_etc_hosts(Some(("172.18.0.2", "web")), &[]);
        assert_eq!(out, "127.0.0.1\tlocalhost\n172.18.0.2\tweb\n");
    }

    #[test]
    fn eff_hostname_prefers_user_value_else_short_id() {
        assert_eq!(eff_hostname("0123456789abcdef0000", "myhost"), "myhost");
        // empty hostname -> first 12 chars of the id.
        assert_eq!(eff_hostname("0123456789abcdef0000", ""), "0123456789ab");
        // a short id (< 12 chars) is used whole, never panics on the slice bound.
        assert_eq!(eff_hostname("abc", ""), "abc");
    }
}
