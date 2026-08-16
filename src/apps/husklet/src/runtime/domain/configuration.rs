use super::{RuntimeIdentity, CONFIGURATION_SIGNATURE, CONTAINER, RUNTIME_SIGNATURE, SIGNATURE};
use crate::config::WorkspaceConfig;
use hl_container::{ContainerSpec, Guest, Isolation, Mount, Resources, Sandbox};
use hl_ws::Arch;
use std::collections::BTreeMap;

pub(super) struct Configuration<'a>(&'a WorkspaceConfig);

impl<'a> Configuration<'a> {
    pub(super) fn new(workspace: &'a WorkspaceConfig) -> Self {
        Self(workspace)
    }

    pub(super) fn container(
        &self,
        mut spec: ContainerSpec,
        signature: String,
        configuration: String,
        runtime: String,
    ) -> ContainerSpec {
        spec = spec
            .name(CONTAINER)
            .hostname(self.hostname())
            .label(SIGNATURE, signature)
            .label(CONFIGURATION_SIGNATURE, configuration)
            .label(RUNTIME_SIGNATURE, runtime)
            .guest(match self.0.arch {
                Arch::Arm64 => Guest::Aarch64,
                Arch::Amd64 => Guest::X86_64,
            })
            .resources(Resources {
                memory_bytes: self.0.memory_mb.map_or(0, |value| u64::from(value) * 1024 * 1024),
                cpu_count: self.0.cpus.unwrap_or(0),
                ..Resources::default()
            })
            .isolation(Isolation {
                sandbox: Sandbox::Disabled,
                network_isolated: false,
                ..Isolation::default()
            });
        for mount in &self.0.mounts {
            spec = spec.mount(if mount.ro {
                Mount::read_only(&mount.host, &mount.container)
            } else {
                Mount::read_write(&mount.host, &mount.container)
            });
        }
        spec
    }

    pub(super) fn environment(&self) -> BTreeMap<String, String> {
        let mut values = BTreeMap::from([
            ("TERM".into(), "xterm-256color".into()),
            ("COLORTERM".into(), "truecolor".into()),
            ("LANG".into(), "C.UTF-8".into()),
            ("HOME".into(), "/root".into()),
            (
                "PATH".into(),
                "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into(),
            ),
        ]);
        values.extend(self.0.env.iter().cloned());
        values
    }

    pub(super) fn signature(&self) -> std::io::Result<String> {
        let runtime = RuntimeIdentity::current(self.0);
        self.signature_for(runtime.as_str())
    }

    pub(super) fn configuration_signature(&self) -> std::io::Result<String> {
        self.validate()?;
        Ok(Self::digest(&self.identity()))
    }

    pub(super) fn runtime_signature(&self) -> String {
        RuntimeIdentity::current(self.0).as_str().to_owned()
    }

    pub(super) fn signature_for(&self, runtime: &str) -> std::io::Result<String> {
        self.validate()?;
        let mut identity = self.identity();
        Self::field(&mut identity, runtime);
        Ok(Self::digest(&identity))
    }

    pub(super) fn legacy_container_compatible(&self, spec: &ContainerSpec) -> std::io::Result<bool> {
        self.validate()?;
        let image: hl_images::Reference = self.0.image.parse().map_err(std::io::Error::other)?;
        let mounts = self
            .0
            .mounts
            .iter()
            .map(|mount| {
                if mount.ro {
                    Mount::read_only(&mount.host, &mount.container)
                } else {
                    Mount::read_write(&mount.host, &mount.container)
                }
            })
            .collect::<Vec<_>>();
        let resources = Resources {
            memory_bytes: self.0.memory_mb.map_or(0, |value| u64::from(value) * 1024 * 1024),
            cpu_count: self.0.cpus.unwrap_or(0),
            ..Resources::default()
        };
        let isolation = Isolation {
            sandbox: Sandbox::Disabled,
            network_isolated: false,
            ..Isolation::default()
        };
        let guest = match self.0.arch {
            Arch::Arm64 => Guest::Aarch64,
            Arch::Amd64 => Guest::X86_64,
        };
        Ok(spec.name.as_deref() == Some(CONTAINER)
            && spec.hostname.as_deref() == Some(self.hostname().as_str())
            && spec.image.as_ref() == Some(&image)
            && spec.guest == guest
            && spec.mounts == mounts
            && spec.resources == resources
            && spec.isolation == isolation)
    }

    fn digest(identity: &str) -> String {
        use sha2::Digest as _;

        let digest = sha2::Sha256::digest(identity.as_bytes());
        let mut signature = String::with_capacity(digest.len() * 2);
        for byte in digest {
            use std::fmt::Write as _;
            let _ = write!(signature, "{byte:02x}");
        }
        signature
    }

    fn validate(&self) -> std::io::Result<()> {
        let mut targets = std::collections::BTreeSet::new();
        for mount in &self.0.mounts {
            if !hl_container::normalized_mount_target(&mount.container) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("mount target {:?} must be absolute and normalized", mount.container),
                ));
            }
            if !targets.insert(&mount.container) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("duplicate mount target {:?}", mount.container),
                ));
            }
        }
        Ok(())
    }

    fn identity(&self) -> String {
        let mut value = String::new();
        for item in [
            self.0.image.as_str(),
            self.0.arch.as_str(),
            self.0.shell.as_deref().unwrap_or_default(),
        ] {
            Self::field(&mut value, item);
        }
        for (name, item) in &self.0.env {
            Self::field(&mut value, name);
            Self::field(&mut value, item);
        }
        for mount in &self.0.mounts {
            Self::field(&mut value, &mount.host);
            Self::field(&mut value, &mount.container);
            Self::field(&mut value, if mount.ro { "ro" } else { "rw" });
        }
        for item in [
            self.0.cpus.map(|value| value.to_string()).unwrap_or_default(),
            self.0.memory_mb.map(|value| value.to_string()).unwrap_or_default(),
            self.0.docker_sock.to_string(),
            format!("{:?}", self.0.vpn),
        ] {
            Self::field(&mut value, &item);
        }
        value
    }

    fn field(output: &mut String, value: &str) {
        use std::fmt::Write as _;
        let _ = write!(output, "{}:{value}", value.len());
    }

    fn hostname(&self) -> String {
        let value: String = self
            .0
            .name
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || character == '-' {
                    character
                } else {
                    '-'
                }
            })
            .collect();
        match value.trim_matches('-') {
            "" => "workspace".to_owned(),
            value => value.to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Configuration;
    use crate::config::WorkspaceConfig;
    use hl_ws::Arch;

    #[test]
    fn terminal_environment_defaults_to_utf8() {
        let workspace = WorkspaceConfig::new("test", "ubuntu:22.04", Arch::Arm64);

        assert_eq!(
            Configuration::new(&workspace)
                .environment()
                .get("LANG")
                .map(String::as_str),
            Some("C.UTF-8")
        );
    }

    #[test]
    fn workspace_locale_overrides_the_terminal_default() {
        let mut workspace = WorkspaceConfig::new("test", "ubuntu:22.04", Arch::Arm64);
        workspace.env.push(("LANG".into(), "ja_JP.UTF-8".into()));

        assert_eq!(
            Configuration::new(&workspace)
                .environment()
                .get("LANG")
                .map(String::as_str),
            Some("ja_JP.UTF-8")
        );
    }
}
