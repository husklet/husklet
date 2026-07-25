use hl_container::Rootfs;
use std::collections::BTreeMap;
use std::fmt;

pub(super) struct PortKey(hl_container::Port);

impl From<hl_container::Port> for PortKey {
    fn from(port: hl_container::Port) -> Self {
        Self(port)
    }
}

impl fmt::Display for PortKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let protocol = match self.0.protocol {
            hl_container::Protocol::Tcp => "tcp",
        };
        write!(formatter, "{}/{protocol}", self.0.guest)
    }
}

pub(super) struct Signal(hl_container::Signal);

impl From<hl_container::Signal> for Signal {
    fn from(signal: hl_container::Signal) -> Self {
        Self(signal)
    }
}

impl fmt::Display for Signal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self.0 {
            hl_container::Signal::Terminate => "SIGTERM",
            hl_container::Signal::Kill => "SIGKILL",
            hl_container::Signal::Interrupt => "SIGINT",
            hl_container::Signal::Quit => "SIGQUIT",
            hl_container::Signal::Hangup => "SIGHUP",
            hl_container::Signal::User1 => "SIGUSR1",
            hl_container::Signal::User2 => "SIGUSR2",
        })
    }
}

pub(super) struct Ports<'a>(&'a hl_container::ContainerSpec);

impl<'a> From<&'a hl_container::ContainerSpec> for Ports<'a> {
    fn from(spec: &'a hl_container::ContainerSpec) -> Self {
        Self(spec)
    }
}

impl Ports<'_> {
    pub(super) fn bindings(&self) -> BTreeMap<String, Option<Vec<crate::api::PortBinding>>> {
        let mut ports = self
            .0
            .ports
            .iter()
            .map(|port| (PortKey::from(*port).to_string(), None))
            .collect::<BTreeMap<_, _>>();
        for publication in &self.0.publish {
            ports
                .entry(PortKey::from(publication.port).to_string())
                .or_insert_with(|| Some(Vec::new()))
                .get_or_insert_with(Vec::new)
                .push(crate::api::PortBinding {
                    host_ip: publication.host_ip.to_string(),
                    host_port: publication.host.to_string(),
                });
        }
        ports
    }

    pub(super) fn summaries(&self) -> Vec<crate::api::PortSummary> {
        let published = self
            .0
            .publish
            .iter()
            .map(|publication| crate::api::PortSummary {
                ip: Some(publication.host_ip.to_string()),
                private_port: publication.port.guest,
                public_port: Some(publication.host),
                protocol: "tcp".into(),
            });
        let unbound = self
            .0
            .ports
            .iter()
            .filter(|port| {
                !self
                    .0
                    .publish
                    .iter()
                    .any(|publication| publication.port == **port)
            })
            .map(|port| crate::api::PortSummary {
                ip: None,
                private_port: port.guest,
                public_port: None,
                protocol: "tcp".into(),
            });
        published.chain(unbound).collect()
    }
}

struct RootfsName<'a>(&'a Rootfs);

impl fmt::Display for RootfsName<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Rootfs::Image(reference) => {
                write!(formatter, "snapshot:{}", reference.snapshot().as_str())
            }
            Rootfs::Directory(path) => path.display().fmt(formatter),
        }
    }
}

pub(super) struct ImageName<'a>(&'a hl_container::ContainerSpec);

impl<'a> From<&'a hl_container::ContainerSpec> for ImageName<'a> {
    fn from(spec: &'a hl_container::ContainerSpec) -> Self {
        Self(spec)
    }
}

impl fmt::Display for ImageName<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0.image {
            Some(image) => image.fmt(formatter),
            None => RootfsName(&self.0.rootfs).fmt(formatter),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ImageName, PortKey, Ports, Signal};

    #[test]
    fn docker_port_projection_preserves_keys_bindings_and_unbound_ports() {
        let mut spec = hl_container::ContainerSpec::from_directory(
            "/rootfs",
            hl_container::Process::new("/bin/true"),
        );
        spec.ports.insert(hl_container::Port::tcp(443).unwrap());
        spec = spec.publish(
            hl_container::Publication::tcp(std::net::Ipv4Addr::LOCALHOST, 8_080, 80).unwrap(),
        );

        let ports = Ports::from(&spec);
        assert_eq!(
            PortKey::from(hl_container::Port::tcp(80).unwrap()).to_string(),
            "80/tcp"
        );
        assert_eq!(
            ports.bindings(),
            [
                (
                    "80/tcp".into(),
                    Some(vec![crate::api::PortBinding {
                        host_ip: "127.0.0.1".into(),
                        host_port: "8080".into(),
                    }]),
                ),
                ("443/tcp".into(), None),
            ]
            .into_iter()
            .collect()
        );
        assert_eq!(
            ports.summaries(),
            vec![
                crate::api::PortSummary {
                    ip: Some("127.0.0.1".into()),
                    private_port: 80,
                    public_port: Some(8_080),
                    protocol: "tcp".into(),
                },
                crate::api::PortSummary {
                    ip: None,
                    private_port: 443,
                    public_port: None,
                    protocol: "tcp".into(),
                },
            ]
        );
    }

    #[test]
    fn docker_signal_and_image_names_preserve_wire_text_and_rootfs_fallback() {
        assert_eq!(
            Signal::from(hl_container::Signal::Terminate).to_string(),
            "SIGTERM"
        );
        assert_eq!(
            Signal::from(hl_container::Signal::User2).to_string(),
            "SIGUSR2"
        );

        let spec = hl_container::ContainerSpec::from_directory(
            "/var/lib/husklet/rootfs",
            hl_container::Process::new("/bin/true"),
        );
        assert_eq!(
            ImageName::from(&spec).to_string(),
            "/var/lib/husklet/rootfs"
        );

        let tagged = spec.image("registry.test/team/tool:7".parse().unwrap());
        assert_eq!(
            ImageName::from(&tagged).to_string(),
            "registry.test/team/tool:7"
        );
    }
}
