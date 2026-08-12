//! `HL_*` names in a case environment select engine behaviour, not guest environment.

use super::super::Error;
use super::environment::EnvironmentEntry;
use hl_container::{
    EndpointSpec, Isolation, Mount, NetworkMode, NetworkSpec, ResourceLimit, Resources, Sandbox, SeccompBaseline,
    Subnet,
};
use std::path::PathBuf;

/// Names hl-container can honour, and the ones it cannot express yet.
const SUPPORTED: [&str; 17] = [
    "HL_NETNS",
    "HL_NETBR",
    "HL_IP",
    "HL_UNTRUSTED",
    "HL_SANDBOX",
    "HL_NET_ISOLATE",
    "HL_NET_HOST",
    "HL_UID",
    "HL_GID",
    "HL_VOLUMES",
    "HL_CPUS",
    "HL_MEM_MAX",
    "HL_PIDS_MAX",
    "HL_PCACHE_DIR",
    "HL_SECCOMP_BASELINE",
    "HL_ULIMITS",
    "HL_HOSTNAME",
];
/// Recognised engine options with no hl-container expression; a case asking for one cannot run.
const UNWIRED: [&str; 0] = [];

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct EngineOptions {
    sandbox: Option<Sandbox>,
    network_isolated: Option<bool>,
    network_host: bool,
    uid: Option<i32>,
    gid: Option<i32>,
    mounts: Vec<Mount>,
    cpu_count: Option<u32>,
    memory_bytes: Option<u64>,
    process_count: Option<u32>,
    translation_cache: Option<PathBuf>,
    seccomp_baseline: Option<SeccompBaseline>,
    limits: Vec<ResourceLimit>,
    hostname: Option<String>,
    bridge: Option<String>,
    address: Option<std::net::Ipv4Addr>,
    /// Recognised but unwired names, retained so the case fails by name rather than silently.
    unwired: Vec<String>,
}

impl EngineOptions {
    /// Splits a case environment into guest variables and engine options.
    pub(crate) fn split(environment: &[EnvironmentEntry]) -> Result<(Vec<EnvironmentEntry>, Self), Error> {
        let mut guest = Vec::new();
        let mut options = Self::default();
        for entry in environment {
            // Guest names may be raw bytes; only an engine option has to be readable text.
            let Some(name) = entry
                .name()
                .starts_with(b"HL_")
                .then(|| String::from_utf8(entry.name().to_vec()).ok())
                .flatten()
            else {
                guest.push(entry.clone());
                continue;
            };
            let value = String::from_utf8(entry.value().to_vec()).map_err(|_| format!("{name} value is not UTF-8"))?;
            options.absorb(&name, &value)?;
        }
        Ok((guest, options))
    }

    fn absorb(&mut self, name: &str, value: &str) -> Result<(), Error> {
        if UNWIRED.contains(&name) {
            self.unwired.push(name.to_owned());
            return Ok(());
        }
        let setting = Setting { name, value };
        match name {
            "HL_UNTRUSTED" => self.sandbox = setting.flag()?.then_some(Sandbox::SentryOnly),
            "HL_SANDBOX" => self.sandbox = setting.flag()?.then_some(Sandbox::Enabled),
            "HL_NET_ISOLATE" => self.network_isolated = Some(setting.flag()?),
            "HL_NET_HOST" => self.network_host = setting.flag()?,
            "HL_UID" => self.uid = Some(setting.number()?),
            "HL_GID" => self.gid = Some(setting.number()?),
            "HL_CPUS" => self.cpu_count = Some(setting.number()?),
            "HL_MEM_MAX" => self.memory_bytes = Some(setting.number()?),
            "HL_PIDS_MAX" => self.process_count = Some(setting.number()?),
            "HL_ULIMITS" => self.limits = setting.limits()?,
            "HL_HOSTNAME" if value.is_empty() => return Err("HL_HOSTNAME is empty".into()),
            "HL_HOSTNAME" => self.hostname = Some(value.to_owned()),
            "HL_PCACHE_DIR" => self.translation_cache = Some(PathBuf::from(value)),
            "HL_SECCOMP_BASELINE" => {
                self.seccomp_baseline = Some(match value {
                    "container" => SeccompBaseline::Container,
                    "disabled" => SeccompBaseline::Disabled,
                    _ => return Err(format!("HL_SECCOMP_BASELINE is not container or disabled: {value:?}").into()),
                });
            }
            // Every non-host container already owns a network namespace, so the legacy name is satisfied on arrival.
            "HL_NETNS" if value.is_empty() => return Err("HL_NETNS is empty".into()),
            "HL_NETNS" => {}
            "HL_NETBR" => self.bridge = Some(value.to_owned()),
            "HL_IP" => {
                self.address = Some(
                    value
                        .parse()
                        .map_err(|_| format!("HL_IP is not an IPv4 address: {value:?}"))?,
                );
            }
            "HL_VOLUMES" => {
                self.mounts = value
                    .split(',')
                    .map(|record| VolumeRecord(record).mount())
                    .collect::<Result<_, _>>()?;
            }
            _ => {
                return Err(format!(
                    "unrecognised engine option {name}; wire it in engine_options.rs or rename it, \
                     because an HL_* name never reaches the guest environment (supported: {}; unwired: {})",
                    SUPPORTED.join(", "),
                    UNWIRED.join(", ")
                )
                .into());
            }
        }
        Ok(())
    }

    /// Named so the case reports what it asked for and did not get.
    pub(crate) fn unwired(&self) -> Option<String> {
        (!self.unwired.is_empty()).then(|| {
            format!(
                "case requests engine option(s) {} that the runtime runner cannot express",
                self.unwired.join(", ")
            )
        })
    }

    pub(crate) fn translation_cache(&self) -> Option<&std::path::Path> {
        self.translation_cache.as_deref()
    }

    pub(crate) const fn user(&self) -> Option<(i32, i32)> {
        match (self.uid, self.gid) {
            (Some(uid), Some(gid)) => Some((uid, gid)),
            (Some(uid), None) => Some((uid, uid)),
            (None, Some(gid)) => Some((0, gid)),
            (None, None) => None,
        }
    }

    pub(crate) fn hostname(&self) -> Option<&str> {
        self.hostname.as_deref()
    }

    pub(crate) fn mounts(&self) -> &[Mount] {
        &self.mounts
    }

    /// Host mode requires isolation off, so the two options are resolved together.
    pub(crate) fn isolation(&self, base: Isolation) -> Isolation {
        Isolation {
            sandbox: self.sandbox.unwrap_or(base.sandbox),
            network_isolated: if self.network_host {
                false
            } else {
                self.network_isolated.unwrap_or(base.network_isolated)
            },
            seccomp_baseline: self.seccomp_baseline.unwrap_or(base.seccomp_baseline),
            ..base
        }
    }

    /// The bridge network this case asked to be attached to, addressed inside a /24 around `HL_IP`.
    pub(crate) fn bridge(&self) -> Result<Option<(NetworkSpec, EndpointSpec)>, Error> {
        let Some(bridge) = self.bridge.as_ref() else {
            return Ok(None);
        };
        let address = self
            .address
            .ok_or_else(|| format!("HL_NETBR {bridge:?} has no HL_IP address"))?;
        let octets = address.octets();
        let subnet = Subnet::new(std::net::Ipv4Addr::new(octets[0], octets[1], octets[2], 0), 24)
            .map_err(|error| format!("HL_IP {address} has no /24 subnet: {error}"))?;
        Ok(Some((
            NetworkSpec::bridge(bridge.clone(), subnet),
            EndpointSpec::default().address(address),
        )))
    }

    pub(crate) const fn network_mode(&self) -> NetworkMode {
        if self.network_host {
            NetworkMode::Host
        } else {
            NetworkMode::Automatic
        }
    }

    /// Zero means "engine default" to hl-container, so an unset option stays zero.
    pub(crate) fn resources(&self) -> Resources {
        Resources {
            cpu_count: self.cpu_count.unwrap_or_default(),
            memory_bytes: self.memory_bytes.unwrap_or_default(),
            process_count: self.process_count.unwrap_or_default(),
            limits: self.limits.clone(),
        }
    }
}

/// One option's raw text, kept beside its name so every parse failure can say which option failed.
struct Setting<'a> {
    name: &'a str,
    value: &'a str,
}

impl Setting<'_> {
    /// Parses the `nofile=1024:2048,core=unlimited` form the engine's `HL_ULIMITS` reader accepts.
    fn limits(&self) -> Result<Vec<ResourceLimit>, Error> {
        self.value
            .split(',')
            .filter(|record| !record.is_empty())
            .map(|record| {
                let (name, values) = record
                    .split_once('=')
                    .ok_or_else(|| format!("{} record {record:?} is not name=value", self.name))?;
                if !ResourceLimit::NAMES.contains(&name) {
                    return Err(Error::from(format!("{} names unknown resource {name:?}", self.name)));
                }
                let (soft, hard) = values.split_once(':').unwrap_or((values, values));
                Ok(ResourceLimit {
                    name: name.to_owned(),
                    soft: Self::limit_value(self.name, soft)?,
                    hard: Self::limit_value(self.name, hard)?,
                })
            })
            .collect()
    }

    fn limit_value(name: &str, value: &str) -> Result<u64, Error> {
        match value {
            "unlimited" | "-1" => Ok(u64::MAX),
            _ => value
                .parse()
                .map_err(|_| format!("{name} value {value:?} is not a number").into()),
        }
    }

    fn flag(&self) -> Result<bool, Error> {
        match self.value {
            "1" => Ok(true),
            "0" => Ok(false),
            _ => Err(format!("{} takes 0 or 1, not {:?}", self.name, self.value).into()),
        }
    }

    fn number<T: std::str::FromStr>(&self) -> Result<T, Error> {
        self.value
            .parse()
            .map_err(|_| format!("{} is not a valid number: {:?}", self.name, self.value).into())
    }
}

/// One `[ro:|rw:]<guest target>:<host source>` record, read exactly as the engine reads it.
struct VolumeRecord<'a>(&'a str);

impl VolumeRecord<'_> {
    fn mount(&self) -> Result<Mount, Error> {
        let (record, read_only) = self.0.strip_prefix("ro:").map_or_else(
            || (self.0.strip_prefix("rw:").unwrap_or(self.0), false),
            |rest| (rest, true),
        );
        let (target, source) = record
            .split_once(':')
            .ok_or_else(|| format!("HL_VOLUMES record is not target:source: {record:?}"))?;
        if target.is_empty() || source.is_empty() {
            return Err(format!("HL_VOLUMES record has an empty path: {record:?}").into());
        }
        Ok(if read_only {
            Mount::read_only(source, target)
        } else {
            Mount::read_write(source, target)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{EngineOptions, EnvironmentEntry, Isolation, NetworkMode, Sandbox};

    fn entries(pairs: &[(&str, &str)]) -> Vec<EnvironmentEntry> {
        pairs
            .iter()
            .map(|(name, value)| EnvironmentEntry::owned(name.as_bytes().to_vec(), value.as_bytes().to_vec()))
            .collect()
    }

    #[test]
    fn engine_options_never_reach_the_guest_environment() {
        let (guest, options) = EngineOptions::split(&entries(&[("PATH", "/bin"), ("HL_UNTRUSTED", "1")])).unwrap();
        assert_eq!(guest.len(), 1);
        assert_eq!(guest[0].name(), b"PATH");
        let base = Isolation {
            sandbox: Sandbox::Disabled,
            ..Isolation::default()
        };
        assert_eq!(options.isolation(base).sandbox, Sandbox::SentryOnly);
    }

    #[test]
    fn host_networking_disables_isolation_and_selects_the_mode() {
        let (_, options) = EngineOptions::split(&entries(&[("HL_NET_HOST", "1")])).unwrap();
        assert!(!options.isolation(Isolation::default()).network_isolated);
        assert_eq!(options.network_mode(), NetworkMode::Host);
    }

    #[test]
    fn network_isolation_is_taken_from_the_case_rather_than_the_default() {
        let (_, options) = EngineOptions::split(&entries(&[("HL_NET_ISOLATE", "1")])).unwrap();
        let base = Isolation {
            network_isolated: false,
            ..Isolation::default()
        };
        assert!(options.isolation(base).network_isolated);
    }

    #[test]
    fn volumes_read_target_then_source_like_the_engine() {
        let (_, options) = EngineOptions::split(&entries(&[("HL_VOLUMES", "/mnt:/tmp,ro:/etc/x:/srv/x")])).unwrap();
        let mounts = options.mounts();
        assert_eq!(mounts.len(), 2);
        assert_eq!(mounts[0].target, std::path::Path::new("/mnt"));
        assert_eq!(mounts[1].target, std::path::Path::new("/etc/x"));
    }

    #[test]
    fn ulimits_reach_the_container_spec_rather_than_being_named_unwired() {
        let (_, options) = EngineOptions::split(&entries(&[("HL_ULIMITS", "nofile=1024:2048")])).unwrap();
        assert!(options.unwired().is_none(), "{options:?}");
    }

    #[test]
    fn a_bridge_address_selects_its_own_slash_24() {
        let (_, options) =
            EngineOptions::split(&entries(&[("HL_NETBR", "hlc228br"), ("HL_IP", "172.28.0.5")])).unwrap();
        let (network, endpoint) = options.bridge().unwrap().unwrap();
        assert_eq!(network.name, "hlc228br");
        let subnet = network.subnet.unwrap();
        assert_eq!((subnet.address, subnet.prefix), ("172.28.0.0".parse().unwrap(), 24));
        assert_eq!(endpoint.address, Some("172.28.0.5".parse().unwrap()));
        assert!(options.unwired().is_none());
    }

    #[test]
    fn an_unknown_engine_option_is_rejected_rather_than_exported() {
        let error = EngineOptions::split(&entries(&[("HL_NOT_A_THING", "1")])).unwrap_err();
        assert!(error.to_string().contains("HL_NOT_A_THING"), "{error}");
    }

    #[test]
    fn a_malformed_flag_is_rejected() {
        assert!(EngineOptions::split(&entries(&[("HL_UNTRUSTED", "yes")])).is_err());
    }

    #[test]
    fn user_options_default_the_missing_half() {
        let (_, options) = EngineOptions::split(&entries(&[("HL_UID", "0"), ("HL_GID", "0")])).unwrap();
        assert_eq!(options.user(), Some((0, 0)));
    }

    #[test]
    fn retired_backend_option_is_rejected() {
        for value in ["c", "rust", "other"] {
            assert!(EngineOptions::split(&entries(&[("HL_EXECUTION_BACKEND", value)])).is_err());
        }
    }
}
