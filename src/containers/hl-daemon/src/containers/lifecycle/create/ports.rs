//! Published-port string assembly for `POST /containers/create`: turn the
//! HostConfig.PortBindings map into the daemon's `ip:hport:cport/proto` publish
//! string (`publish_str`), with an auto-allocating variant (`publish_str_alloc`)
//! that fills empty HostPorts from the ephemeral range. Stateless helpers.
use super::super::super::*;

use super::dto::PortBinding;

/// Split a PortBindings key (`"<cport>/<proto>"`, e.g. `"9000/tcp"`) into (container-port, proto).
pub(crate) struct PublishedPorts<'a>(&'a HashMap<String, Vec<PortBinding>>);
impl<'a> PublishedPorts<'a> {
    pub(crate) fn new(bindings: &'a HashMap<String, Vec<PortBinding>>) -> Self {
        Self(bindings)
    }
    fn split_key(k: &str) -> (&str, &str) {
        k.split_once('/').unwrap_or((k, "tcp"))
    }

    // Only the auto-allocating `publish_str_alloc` is called in production; the plain non-allocating
    // variant exists as the tested foundation of that logic, so it is compiled for tests only.
    #[cfg(test)]
    pub(crate) fn string(&self) -> String {
        let pb = self.0;
        let mut v = Vec::new();
        for (k, binds) in pb {
            let (cport, proto) = PublishedPorts::split_key(k);
            if cport.is_empty() {
                continue;
            }
            for b in binds {
                if let Some(hp) = &b.host_port {
                    if !hp.is_empty() {
                        let ip = b.host_ip.as_deref().unwrap_or("");
                        v.push(format!("{ip}:{hp}:{cport}/{proto}"));
                    }
                }
            }
        }
        v.join(",")
    }

    /// Like [`publish_str`] but AUTO-ASSIGNS a free host port for any binding with an empty `HostPort` —
    /// docker's `-p <container>` / `-p 127.0.0.1::<container>` "publish to an ephemeral host port" form. The
    /// daemon picks the port here (from the IANA dynamic range 49152-65535) so `docker port`/`ps`/inspect
    /// report a concrete host port and the engine's `-p` host forwarder binds it. Ports already published by
    /// existing containers are skipped to avoid intra-daemon collisions. Bindings with an explicit HostPort
    /// are emitted verbatim (byte-identical to `publish_str`).
    pub(crate) fn allocate(&self, g: &Inner) -> String {
        let pb = self.0;
        let mut used: std::collections::HashSet<u16> = g
            .containers
            .values()
            .flat_map(|c| crate::containers::Publish::new(&c.publish).bindings())
            .map(|p| p.host_port)
            .collect();
        let mut next: u16 = 49152;
        let mut alloc = || -> u16 {
            while next < 65535 && used.contains(&next) {
                next += 1;
            }
            let p = next;
            used.insert(p);
            next = next.saturating_add(1);
            p
        };
        // Sort by container port so auto-assignment is deterministic (HashMap iteration order is not).
        let mut keys: Vec<&String> = pb.keys().collect();
        keys.sort();
        let mut v = Vec::new();
        for k in keys {
            let (cport, proto) = Self::split_key(k);
            if cport.is_empty() {
                continue;
            }
            for b in &pb[k] {
                let hp = match &b.host_port {
                    Some(h) if !h.is_empty() => h.clone(),
                    _ => alloc().to_string(),
                };
                let ip = b.host_ip.as_deref().unwrap_or("");
                v.push(format!("{ip}:{hp}:{cport}/{proto}"));
            }
        }
        v.join(",")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bind(host_port: Option<&str>, host_ip: Option<&str>) -> PortBinding {
        PortBinding {
            host_port: host_port.map(|s| s.to_string()),
            host_ip: host_ip.map(|s| s.to_string()),
        }
    }

    fn pb(entries: &[(&str, Vec<PortBinding>)]) -> HashMap<String, Vec<PortBinding>> {
        entries
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn publish_str_basic_no_hostip() {
        // Absent HostIp -> empty ip segment, so the string leads with ':'.
        let m = pb(&[("9000/tcp", vec![bind(Some("8080"), None)])]);
        assert_eq!(PublishedPorts::new(&m).string(), ":8080:9000/tcp");
    }

    #[test]
    fn publish_str_preserves_explicit_host_ip() {
        let m = pb(&[("9000/tcp", vec![bind(Some("8080"), Some("127.0.0.1"))])]);
        assert_eq!(PublishedPorts::new(&m).string(), "127.0.0.1:8080:9000/tcp");
    }

    #[test]
    fn publish_str_skips_empty_and_absent_host_port() {
        // HostPort == Some("") is skipped.
        let empty = pb(&[("9000/tcp", vec![bind(Some(""), None)])]);
        assert_eq!(PublishedPorts::new(&empty).string(), "");
        // HostPort == None is skipped.
        let absent = pb(&[("9000/tcp", vec![bind(None, None)])]);
        assert_eq!(PublishedPorts::new(&absent).string(), "");
    }

    #[test]
    fn publish_str_defaults_proto_to_tcp_when_key_has_no_slash() {
        let m = pb(&[("53", vec![bind(Some("1234"), None)])]);
        assert_eq!(PublishedPorts::new(&m).string(), ":1234:53/tcp");
    }

    #[test]
    fn publish_str_skips_empty_container_port() {
        // "/tcp" splits to an empty container port -> the whole binding is dropped.
        let m = pb(&[("/tcp", vec![bind(Some("8080"), None)])]);
        assert_eq!(PublishedPorts::new(&m).string(), "");
    }

    #[test]
    fn publish_str_alloc_fills_empty_host_port_from_ephemeral_range() {
        // With no existing containers, an empty HostPort auto-allocates the first ephemeral port.
        let m = pb(&[("9000/tcp", vec![bind(None, None)])]);
        let g = Inner::default();
        assert_eq!(PublishedPorts::new(&m).allocate(&g), ":49152:9000/tcp");
    }
}
