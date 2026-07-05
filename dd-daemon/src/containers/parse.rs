//! Pure request/spec parsers shared across the container handlers: bind mounts, stop signals,
//! query truthiness, and the published-port grammar (`parse_publish` + the two JSON shapers).
//! These are side-effect-free string→value transforms with no async/`App` dependency, split out
//! from `mod.rs` so the async handlers (`do_stop`) and these parsers live apart. Re-exported by
//! `mod.rs` via `pub(crate) use parse::*`, so `crate::containers::<fn>` resolves unchanged.
use super::*;

/// Parse a `-v`/Binds spec `src:dst[:opts]` into `(host_source, container_dest, read_only)`. Docker
/// appends comma-separated options after the destination (e.g. `/h:/c:ro`, `vol:/c:rw,z`); `ro` marks
/// the mount read-only. Returns None for a malformed spec (no destination). Note: the prior code split
/// only on the FIRST colon, so `src:dst:ro` mounted at the literal path "dst:ro" — this fixes that and
/// surfaces the RW flag for inspect.
pub(crate) fn parse_bind(b: &str) -> Option<(&str, &str, bool)> {
    let mut it = b.splitn(3, ':');
    let src = it.next()?;
    let dst = it.next()?;
    let ro = it
        .next()
        .map(|o| o.split(',').any(|p| p == "ro"))
        .unwrap_or(false);
    if dst.is_empty() {
        return None;
    }
    Some((src, dst, ro))
}

/// Map a docker signal token ("SIGTERM"/"TERM"/"15"/"9"/"SIGKILL"/...) to its libc number.
/// Numeric tokens are taken verbatim; names are matched case-insensitively with or without the
/// "SIG" prefix. Anything unrecognised falls back to `default`.
pub(crate) fn parse_signal(s: &str, default: i32) -> i32 {
    let t = s.trim();
    if t.is_empty() {
        return default;
    }
    if let Ok(n) = t.parse::<i32>() {
        return n;
    }
    match t.to_ascii_uppercase().trim_start_matches("SIG") {
        "TERM" => libc::SIGTERM,
        "KILL" => libc::SIGKILL,
        "INT" => libc::SIGINT,
        "QUIT" => libc::SIGQUIT,
        "HUP" => libc::SIGHUP,
        "USR1" => libc::SIGUSR1,
        "USR2" => libc::SIGUSR2,
        "STOP" => libc::SIGSTOP,
        "CONT" => libc::SIGCONT,
        _ => default,
    }
}

pub(crate) fn q_truthy(s: &Option<String>) -> bool {
    matches!(s.as_deref(), Some("1") | Some("true") | Some("True"))
}

/// One parsed published-port binding. The internal `publish` string (stored on the container, threaded to
/// the engine + forwarder) is a comma-list of `[hostIP]:hostPort:containerPort[/proto]` entries — the full
/// docker `-p [[hostIP:]hostPort:]containerPort[/proto]` shape (empty hostIP ⇒ 0.0.0.0, absent proto ⇒ tcp).
pub(crate) struct PubPort {
    pub host_ip: String,
    pub host_port: u16,
    pub container_port: u16,
    pub proto: String,
}

/// Parse the internal `publish` string into structured bindings. Tolerates the legacy 2-field
/// `hostPort:containerPort` form (hostIP defaults to 0.0.0.0) so a state file written by an older daemon
/// still loads. IPv6 host addresses (which themselves contain `:`) are handled: we split the port fields
/// off the RIGHT, leaving the remainder as the host IP.
pub(crate) fn parse_publish(publish: &str) -> Vec<PubPort> {
    publish
        .split(',')
        .filter(|s| !s.is_empty())
        .filter_map(|entry| {
            // proto is an optional `/tcp` | `/udp` suffix on the whole entry.
            let (rest, proto) = entry
                .rsplit_once('/')
                .map(|(r, p)| (r, p.to_string()))
                .unwrap_or((entry, "tcp".into()));
            let (rest, cport) = rest.rsplit_once(':')?; // rightmost field = container port
            let (host_ip, hport) = match rest.rsplit_once(':') {
                // next field = host port; rest = host IP
                Some((ip, hp)) => (ip, hp),
                None => ("", rest), // legacy 2-field: only hostPort:cport
            };
            Some(PubPort {
                host_ip: if host_ip.is_empty() {
                    "0.0.0.0".into()
                } else {
                    host_ip.into()
                },
                host_port: hport.parse().ok()?,
                container_port: cport.parse().ok()?,
                proto,
            })
        })
        .collect()
}

/// Build the `Ports` array Docker clients expect (top-level `docker ps` / list JSON).
pub(crate) fn ports_json(publish: &str) -> Vec<Value> {
    parse_publish(publish)
        .into_iter()
        .map(|p| {
            serde_json::to_value(crate::api::PortSummary {
                public_port: p.host_port,
                private_port: p.container_port,
                type_: p.proto,
                ip: p.host_ip,
            })
            .unwrap_or(Value::Null)
        })
        .collect()
}

/// `NetworkSettings.Ports` map (`{"80/tcp": [{"HostIp","HostPort"}]}`) — the shape `docker port` reads
/// (it panics if `.NetworkSettings` is absent). Distinct from the top-level `Ports` array above.
pub(crate) fn ports_map_json(publish: &str) -> Value {
    let mut m = serde_json::Map::new();
    for p in parse_publish(publish) {
        m.entry(format!("{}/{}", p.container_port, p.proto))
            .or_insert_with(|| Value::Array(vec![]))
            .as_array_mut()
            .unwrap()
            .push(
                serde_json::to_value(crate::api::PortBinding {
                    host_ip: p.host_ip,
                    host_port: p.host_port.to_string(),
                })
                .unwrap_or(Value::Null),
            );
    }
    Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- parse_bind ---------------------------------------------------------
    #[test]
    fn bind_src_dst() {
        assert_eq!(parse_bind("/h:/c"), Some(("/h", "/c", false)));
    }
    #[test]
    fn bind_ro_flag() {
        assert_eq!(parse_bind("/h:/c:ro"), Some(("/h", "/c", true)));
        // `ro` may appear among a comma-list of options.
        assert_eq!(parse_bind("vol:/c:ro,z"), Some(("vol", "/c", true)));
    }
    #[test]
    fn bind_rw_flag() {
        assert_eq!(parse_bind("/h:/c:rw"), Some(("/h", "/c", false)));
        assert_eq!(parse_bind("/h:/c:rw,z"), Some(("/h", "/c", false)));
    }
    #[test]
    fn bind_empty_dst_is_none() {
        assert_eq!(parse_bind("/h:"), None);
    }
    #[test]
    fn bind_splitn3_keeps_extra_colons_in_opts() {
        // splitn(3, ':') means only the FIRST two colons split; the remainder is the opts field.
        // "a:b:c:d" -> src="a", dst="b", opts="c:d" (no "ro" -> false).
        assert_eq!(parse_bind("a:b:c:d"), Some(("a", "b", false)));
    }
    #[test]
    fn bind_no_colon_is_none() {
        assert_eq!(parse_bind("justsrc"), None);
    }

    // ---- parse_signal -------------------------------------------------------
    #[test]
    fn signal_named_with_prefix() {
        assert_eq!(parse_signal("SIGTERM", 0), libc::SIGTERM);
    }
    #[test]
    fn signal_named_without_prefix() {
        assert_eq!(parse_signal("TERM", 0), libc::SIGTERM);
        assert_eq!(parse_signal("kill", 0), libc::SIGKILL); // case-insensitive
    }
    #[test]
    fn signal_numeric_verbatim() {
        assert_eq!(parse_signal("15", 0), 15);
        assert_eq!(parse_signal("9", 0), 9);
    }
    #[test]
    fn signal_junk_falls_back_to_default() {
        assert_eq!(parse_signal("NOPE", 7), 7);
        assert_eq!(parse_signal("", 7), 7);
    }

    // ---- parse_publish ------------------------------------------------------
    fn one(publish: &str) -> PubPort {
        let mut v = parse_publish(publish);
        assert_eq!(v.len(), 1, "expected exactly one PubPort for {publish:?}");
        v.pop().unwrap()
    }
    #[test]
    fn publish_full_ip_port_proto() {
        let p = one("1.2.3.4:8080:80/tcp");
        assert_eq!(p.host_ip, "1.2.3.4");
        assert_eq!(p.host_port, 8080);
        assert_eq!(p.container_port, 80);
        assert_eq!(p.proto, "tcp");
    }
    #[test]
    fn publish_legacy_two_field_defaults_ip() {
        let p = one("8080:80");
        assert_eq!(p.host_ip, "0.0.0.0"); // empty host IP -> 0.0.0.0
        assert_eq!(p.host_port, 8080);
        assert_eq!(p.container_port, 80);
        assert_eq!(p.proto, "tcp"); // absent proto -> tcp
    }
    #[test]
    fn publish_ipv6_host_right_split() {
        // rsplit off the two rightmost `:`-fields (cport, then hport); the remainder is the host IP,
        // so an IPv6 host that itself contains colons is preserved.
        let p = one("::1:8080:80");
        assert_eq!(p.host_ip, "::1");
        assert_eq!(p.host_port, 8080);
        assert_eq!(p.container_port, 80);
    }
    #[test]
    fn publish_unparseable_port_dropped() {
        // container port "xx" fails u16::parse -> the whole entry is filtered out.
        assert!(parse_publish("8080:xx").is_empty());
        // host port "yy" fails too.
        assert!(parse_publish("1.2.3.4:yy:80").is_empty());
    }
    #[test]
    fn publish_skips_empty_comma_entries() {
        let v = parse_publish("8080:80,,9090:90");
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].host_port, 8080);
        assert_eq!(v[1].host_port, 9090);
    }

    // ---- ports_json ---------------------------------------------------------
    #[test]
    fn ports_json_shape() {
        let arr = ports_json("1.2.3.4:8080:80/tcp");
        assert_eq!(arr.len(), 1);
        let e = &arr[0];
        assert_eq!(e["PublicPort"], 8080);
        assert_eq!(e["PrivatePort"], 80);
        assert_eq!(e["Type"], "tcp");
        assert_eq!(e["IP"], "1.2.3.4");
    }

    // ---- ports_map_json -----------------------------------------------------
    #[test]
    fn ports_map_json_shape() {
        let m = ports_map_json("1.2.3.4:8080:80/tcp");
        let bindings = m["80/tcp"].as_array().expect("array under 80/tcp");
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0]["HostIp"], "1.2.3.4");
        assert_eq!(bindings[0]["HostPort"], "8080"); // HostPort is a string
    }
    #[test]
    fn ports_map_json_groups_by_key() {
        // Two bindings for the same container port/proto collect under one key.
        let m = ports_map_json("1.1.1.1:8080:80/tcp,2.2.2.2:9090:80/tcp");
        let bindings = m["80/tcp"].as_array().unwrap();
        assert_eq!(bindings.len(), 2);
        assert_eq!(bindings[0]["HostPort"], "8080");
        assert_eq!(bindings[1]["HostPort"], "9090");
    }
    #[test]
    fn ports_map_json_empty_input_is_empty_object() {
        // No publish string -> an empty JSON object (not null/array), the shape `docker port` reads.
        let m = ports_map_json("");
        assert_eq!(m, serde_json::json!({}));
        assert!(m.as_object().unwrap().is_empty());
    }
    #[test]
    fn ports_map_json_legacy_two_field_defaults_host_ip() {
        // A 2-field `hostPort:containerPort` entry keys on the container port and defaults HostIp.
        let m = ports_map_json("8080:80");
        let bindings = m["80/tcp"].as_array().expect("array under 80/tcp");
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0]["HostIp"], "0.0.0.0");
        assert_eq!(bindings[0]["HostPort"], "8080");
    }
    #[test]
    fn ports_map_json_udp_proto_in_key() {
        // The /udp suffix flows into the map key (distinct from the /tcp bucket).
        let m = ports_map_json("0.0.0.0:53:53/udp");
        assert!(m.get("53/tcp").is_none());
        let bindings = m["53/udp"].as_array().expect("array under 53/udp");
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0]["HostPort"], "53");
    }
    #[test]
    fn ports_map_json_distinct_ports_are_distinct_keys() {
        // Two different container ports produce two separate keys, each with one binding.
        let m = ports_map_json("1.2.3.4:8080:80/tcp,1.2.3.4:8443:443/tcp");
        let obj = m.as_object().unwrap();
        assert_eq!(obj.len(), 2);
        assert_eq!(m["80/tcp"].as_array().unwrap().len(), 1);
        assert_eq!(m["443/tcp"].as_array().unwrap().len(), 1);
        assert_eq!(m["443/tcp"][0]["HostPort"], "8443");
    }
    #[test]
    fn ports_map_json_unparseable_entries_dropped() {
        // Port ranges (`8080-8090`) are NOT expanded — they fail u16 parse and the entry is dropped,
        // as is a bare container port with no host field. Both yield an empty map.
        assert_eq!(ports_map_json("8080-8090:80-90"), serde_json::json!({}));
        assert_eq!(ports_map_json("80"), serde_json::json!({}));
    }
}
