use crate::{Access, Container, NetworkDriver, Result, model::ResolvedMount, service::NetworkConfig};
use std::{
    fmt::Write as _,
    path::{Path, PathBuf},
};

/// Per-container Linux identity files owned below the service state directory.
#[derive(Clone)]
pub(crate) struct Identity {
    root: PathBuf,
}

impl Identity {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub(crate) fn prepare(&self, container: &Container, networks: &[NetworkConfig]) -> Result<Vec<ResolvedMount>> {
        let directory = self.directory(container);
        std::fs::create_dir_all(&directory)?;
        self.generation(container)?;
        let hostname = container.hostname();
        Self::write(&directory, "hostname", format!("{hostname}\n").as_bytes())?;
        Self::write(&directory, "hosts", Self::hosts(container, networks).as_bytes())?;
        Self::write(
            &directory,
            "resolv.conf",
            Self::resolver(container, networks)?.as_bytes(),
        )?;
        Ok(Self::mounts(&directory, Self::access(container)))
    }

    pub(crate) fn open(&self, container: &Container) -> Result<Vec<ResolvedMount>> {
        let directory = self.directory(container);
        self.generation(container)?;
        for name in ["hostname", "hosts", "resolv.conf"] {
            if !directory.join(name).is_file() {
                return Err(crate::Error::Corrupt(format!(
                    "container identity file {} is missing",
                    directory.join(name).display()
                )));
            }
        }
        Ok(Self::mounts(&directory, Self::access(container)))
    }

    pub(crate) fn refresh(&self, container: &Container, networks: &[NetworkConfig]) -> Result<()> {
        let directory = self.directory(container);
        self.generation(container)?;
        Self::write(&directory, "hosts", Self::hosts(container, networks).as_bytes())?;
        Self::write(
            &directory,
            "resolv.conf",
            Self::resolver(container, networks)?.as_bytes(),
        )
    }

    pub(crate) fn remove(&self, container: &Container) -> Result<()> {
        let directory = self.directory(container);
        match std::fs::remove_dir_all(directory) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub(crate) fn generation(&self, container: &Container) -> Result<crate::generation::Generation> {
        let directory = self.directory(container);
        std::fs::create_dir_all(&directory)?;
        crate::generation::Generation::open(directory.join("generation"))
    }

    fn directory(&self, container: &Container) -> PathBuf {
        self.root.join(container.id.as_str())
    }

    fn hosts(container: &Container, networks: &[NetworkConfig]) -> String {
        // Match Docker's standard local host projection, including IPv6.  Omitting these IPv6 names
        // makes software using AF_UNSPEC (Apache APR among others) fall through to DNS for its own local
        // listener address; a loopback-only container then observes ECONNREFUSED instead of localhost.
        let mut contents = String::from(
            "127.0.0.1\tlocalhost\n\
             ::1\tlocalhost ip6-localhost ip6-loopback\n\
             fe00::\tip6-localnet\n\
             ff00::\tip6-mcastprefix\n\
             ff02::1\tip6-allnodes\n\
             ff02::2\tip6-allrouters\n",
        );
        for network in networks {
            if network.driver != NetworkDriver::Bridge {
                continue;
            }
            let custom = network.name != "bridge";
            let mut endpoints = network.endpoints.iter().collect::<Vec<_>>();
            endpoints.sort_by_key(|endpoint| (endpoint.container != container.id, endpoint.address));
            for endpoint in endpoints {
                if !custom && endpoint.container != container.id {
                    continue;
                }
                let Some(address) = endpoint.address else {
                    continue;
                };
                let mut names = vec![endpoint.name.clone()];
                names.extend(endpoint.aliases.clone());
                if endpoint.container == container.id {
                    let hostname = container.hostname();
                    if !names.contains(&hostname) {
                        names.push(hostname);
                    }
                }
                writeln!(contents, "{address}\t{}", names.join(" ")).expect("writing to String cannot fail");
            }
        }
        for (name, address) in &container.spec.hosts {
            writeln!(contents, "{address}\t{name}").expect("writing to String cannot fail");
        }
        contents
    }

    fn resolver(container: &Container, networks: &[NetworkConfig]) -> Result<String> {
        if container.spec.network_mode == crate::NetworkMode::Host {
            let contents = std::fs::read_to_string("/etc/resolv.conf")?;
            if contents.len() > 1024 * 1024 {
                return Err(crate::Error::InvalidSpec(
                    "host resolver configuration exceeds 1 MiB".into(),
                ));
            }
            return Ok(contents);
        }
        if !container.spec.isolation.network_isolated
            || networks.iter().any(|network| network.driver == NetworkDriver::Bridge)
        {
            Ok("nameserver 127.0.0.11\noptions ndots:0\n".into())
        } else {
            Ok("nameserver 127.0.0.1\noptions ndots:0\n".into())
        }
    }

    fn write(directory: &Path, name: &str, contents: &[u8]) -> Result<()> {
        let temporary = directory.join(format!(".{name}.new"));
        std::fs::write(&temporary, contents)?;
        std::fs::rename(temporary, directory.join(name))?;
        Ok(())
    }

    fn access(container: &Container) -> Access {
        if container.spec.isolation.read_only_root {
            Access::ReadOnly
        } else {
            Access::ReadWrite
        }
    }

    fn mounts(directory: &Path, access: Access) -> Vec<ResolvedMount> {
        ["hosts", "resolv.conf", "hostname"]
            .into_iter()
            .map(|name| ResolvedMount {
                source: directory.join(name),
                target: PathBuf::from("/etc").join(name),
                access,
            })
            .collect()
    }
}
