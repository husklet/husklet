//! The published-port grammar: `parse_publish` (the internal `publish` string → structured
//! bindings) and the two JSON shapers `docker` clients read (`ports_json`, `ports_map_json`).
use super::*;

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
pub(crate) struct Publish<'a>(&'a str);
impl<'a> Publish<'a> {
    pub(crate) fn new(value: &'a str) -> Self {
        Self(value)
    }
    pub(crate) fn bindings(&self) -> Vec<PubPort> {
        let publish = self.0;
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
    pub(crate) fn summaries(&self) -> Vec<Value> {
        self.bindings()
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
    pub(crate) fn map(&self) -> Value {
        let mut m = serde_json::Map::new();
        for p in self.bindings() {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- parse_publish ------------------------------------------------------
    fn one(publish: &str) -> PubPort {
        let mut v = Publish::new(publish).bindings();
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
        assert!(Publish::new("8080:xx").bindings().is_empty());
        // host port "yy" fails too.
        assert!(Publish::new("1.2.3.4:yy:80").bindings().is_empty());
    }
    #[test]
    fn publish_skips_empty_comma_entries() {
        let v = Publish::new("8080:80,,9090:90").bindings();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].host_port, 8080);
        assert_eq!(v[1].host_port, 9090);
    }

    // ---- ports_json ---------------------------------------------------------
    #[test]
    fn ports_json_shape() {
        let arr = Publish::new("1.2.3.4:8080:80/tcp").summaries();
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
        let m = Publish::new("1.2.3.4:8080:80/tcp").map();
        let bindings = m["80/tcp"].as_array().expect("array under 80/tcp");
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0]["HostIp"], "1.2.3.4");
        assert_eq!(bindings[0]["HostPort"], "8080"); // HostPort is a string
    }
    #[test]
    fn ports_map_json_groups_by_key() {
        // Two bindings for the same container port/proto collect under one key.
        let m = Publish::new("1.1.1.1:8080:80/tcp,2.2.2.2:9090:80/tcp").map();
        let bindings = m["80/tcp"].as_array().unwrap();
        assert_eq!(bindings.len(), 2);
        assert_eq!(bindings[0]["HostPort"], "8080");
        assert_eq!(bindings[1]["HostPort"], "9090");
    }
    #[test]
    fn ports_map_json_empty_input_is_empty_object() {
        // No publish string -> an empty JSON object (not null/array), the shape `docker port` reads.
        let m = Publish::new("").map();
        assert_eq!(m, serde_json::json!({}));
        assert!(m.as_object().unwrap().is_empty());
    }
    #[test]
    fn ports_map_json_legacy_two_field_defaults_host_ip() {
        // A 2-field `hostPort:containerPort` entry keys on the container port and defaults HostIp.
        let m = Publish::new("8080:80").map();
        let bindings = m["80/tcp"].as_array().expect("array under 80/tcp");
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0]["HostIp"], "0.0.0.0");
        assert_eq!(bindings[0]["HostPort"], "8080");
    }
    #[test]
    fn ports_map_json_udp_proto_in_key() {
        // The /udp suffix flows into the map key (distinct from the /tcp bucket).
        let m = Publish::new("0.0.0.0:53:53/udp").map();
        assert!(m.get("53/tcp").is_none());
        let bindings = m["53/udp"].as_array().expect("array under 53/udp");
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0]["HostPort"], "53");
    }
    #[test]
    fn ports_map_json_distinct_ports_are_distinct_keys() {
        // Two different container ports produce two separate keys, each with one binding.
        let m = Publish::new("1.2.3.4:8080:80/tcp,1.2.3.4:8443:443/tcp").map();
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
        assert_eq!(Publish::new("8080-8090:80-90").map(), serde_json::json!({}));
        assert_eq!(Publish::new("80").map(), serde_json::json!({}));
    }
}
