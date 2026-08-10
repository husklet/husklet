//! Launch-scoped engine options.
//!
//! This retains the C registry contract without retaining its thread/process
//! globals. A caller explicitly owns and passes one store per launch.

/// Maximum aggregate storage, including one trailing NUL per value.
pub const STORE_LIMIT: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Ownership {
    LaunchInput,
    InternalState,
    DebugOnly,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Shape {
    Text,
    Path,
    Integer,
    Flag,
    Records,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Definition {
    pub name: &'static str,
    pub purpose: &'static str,
    pub ownership: Ownership,
    pub shape: Shape,
}

macro_rules! launch {
    ($name:literal, $purpose:literal, $shape:ident) => {
        Definition {
            name: $name,
            purpose: $purpose,
            ownership: Ownership::LaunchInput,
            shape: Shape::$shape,
        }
    };
}
macro_rules! internal {
    ($name:literal, $purpose:literal, $shape:ident) => {
        Definition {
            name: $name,
            purpose: $purpose,
            ownership: Ownership::InternalState,
            shape: Shape::$shape,
        }
    };
}

pub const DEFINITIONS: &[Definition] = &[
    launch!("HL_CHECKPOINT", "arm checkpoint capture over the store channel", Flag),
    launch!(
        "HL_CHECKPOINT_POLICY",
        "checkpoint incompatible-resource recovery policy",
        Integer
    ),
    launch!("HL_CPUS", "guest-visible CPU quota", Integer),
    launch!("HL_CWD", "initial guest working directory", Path),
    launch!(
        "HL_EXECUTION_BACKEND",
        "launch-selected execution implementation: rust or c",
        Text
    ),
    launch!("HL_EGRESS_SOCKS", "SOCKS5 endpoint for external TCP egress", Text),
    launch!("HL_FSGEN_FILE", "shared overlay filesystem-generation file", Path),
    launch!("HL_FILE_OWNERS", "initial guest file ownership records", Records),
    launch!("HL_GID", "initial guest group identity", Integer),
    launch!("HL_GUEST_ENV", "serialized Linux guest environment", Records),
    launch!("HL_HOSTNAME", "Linux guest hostname", Text),
    launch!("HL_IP", "guest virtual IPv4 address paired with HL_NETBR", Text),
    launch!("HL_LOWER", "ordered root filesystem lower layers", Records),
    launch!("HL_OVERLAY_UPPER", "writable root filesystem overlay layer", Path),
    launch!(
        "HL_OVERLAY_WORK",
        "launch-private portable overlay work directory",
        Text
    ),
    launch!("HL_MEM_MAX", "guest memory limit", Integer),
    launch!("HL_NETBR", "shared virtual-network bridge identity", Text),
    launch!("HL_NETIFS", "serialized virtual-network interfaces", Records),
    launch!("HL_NETNS", "guest network and IPC namespace identity", Text),
    launch!("HL_NAME_BINDS", "live guest basename projection rules", Records),
    launch!("HL_NET_ISOLATE", "disable guest external networking", Flag),
    launch!("HL_NET_HOST", "use the host network stack directly", Flag),
    launch!(
        "HL_NATIVE_ADMISSION_CACHE",
        "reuse the previous native admission across consecutive inline services",
        Flag
    ),
    launch!(
        "HL_NATIVE_DIRECT_STICKY",
        "use sticky run-mode flip scoring before a bounded direct hold",
        Flag
    ),
    launch!(
        "HL_NATIVE_DIRECT_STICKY_LIMIT",
        "run-mode flip budget the sticky hold is taken at",
        Text
    ),
    launch!(
        "HL_NATIVE_DIRECT_HOLD_RUNS",
        "base-budget resolver-run equivalents a bounded direct hold must serve",
        Integer
    ),
    launch!(
        "HL_NATIVE_DIRECT_STICKY_PERMANENT",
        "never return direct authority to a process that alternated run mode",
        Flag
    ),
    launch!(
        "HL_NATIVE_SPLIT_MODE_EXECUTORS",
        "use separate lazy aarch64 native executors for resolver and direct modes",
        Flag
    ),
    launch!(
        "HL_A64_DIRTY_OVERFLOW_CONTINUE",
        "compatibility request to continue aarch64 native execution after dirty-journal saturation",
        Flag
    ),
    launch!(
        "HL_A64_DIRTY_OVERFLOW_EXIT",
        "use the legacy aarch64 native exit after exact dirty-journal saturation",
        Flag
    ),
    launch!(
        "HL_A64_NO_WRITE_COMMIT",
        "drop the aarch64 post-store dirty-journal commit and publish the whole window per crossing",
        Flag
    ),
    launch!(
        "HL_A64_NO_WRITE_RESERVE",
        "drop the aarch64 pre-store dirty-journal reservation",
        Flag
    ),
    launch!(
        "HL_A64_RUNTIME_WRITE_RESERVE",
        "run the aarch64 pre-store dirty-journal reservation only at store sites observed to saturate the ring",
        Flag
    ),
    launch!(
        "HL_NATIVE_DIAGNOSTICS",
        "report native execution counters at launch exit",
        Flag
    ),
    launch!(
        "HL_C_DIAGNOSTICS",
        "report retained C translation and dispatch phase counters at launch exit",
        Flag
    ),
    launch!(
        "HL_NATIVE_EXECUTION",
        "enable the bounded native execution adapter",
        Flag
    ),
    launch!("HL_PCACHE", "enable persistent translated-code caching", Flag),
    launch!("HL_PCACHE_DIR", "persistent translated-code cache storage", Path),
    launch!("HL_PIDS_MAX", "guest process limit", Integer),
    launch!("HL_PROCESS_DOMAIN", "opaque launch process ownership identity", Text),
    launch!("HL_LAUNCH_DOMAIN", "activation-private process tree identity", Text),
    launch!("HL_PUBLISH", "guest-to-host port publication rules", Records),
    launch!("HL_PUBLISH_DAEMON", "host daemon publishes guest ports", Flag),
    launch!("HL_RESTORE", "restore the image held by the store channel", Flag),
    launch!("HL_ROOTFS_RO", "mount the guest root filesystem read-only", Flag),
    launch!("HL_SANDBOX", "apply host confinement to the untrusted worker", Flag),
    launch!("HL_SECCOMP_BASELINE", "guest-visible launch seccomp baseline", Text),
    launch!("HL_UID", "initial guest user identity", Integer),
    launch!("HL_ULIMITS", "serialized Linux resource limits", Records),
    launch!(
        "HL_UNTRUSTED",
        "route host-authority operations through the sentry",
        Flag
    ),
    launch!("HL_VOLUMES", "guest volume mount specification", Records),
    internal!(
        "HL_GUEST_ENV_ESC",
        "guest environment uses escaped record encoding",
        Flag
    ),
    internal!(
        "HL_GUEST_ENV_EXACT",
        "guest exec environment suppresses engine defaults",
        Flag
    ),
    Definition {
        name: "HL_LOG",
        purpose: "debug-build logging tag selector",
        ownership: Ownership::DebugOnly,
        shape: Shape::Text,
    },
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OptionError {
    UnknownName,
    InvalidValue,
    ValueContainsNul,
    StoreLimit,
}

#[derive(Clone, Debug)]
pub struct Options {
    values: Vec<Option<Vec<u8>>>,
    store_size: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            values: vec![None; DEFINITIONS.len()],
            store_size: 0,
        }
    }
}

impl Options {
    /// Iterates the explicitly configured option records in definition order.
    ///
    /// This is the lossless handoff used by alternate execution backends; absent
    /// options remain absent instead of being confused with empty values.
    #[cfg(all(target_os = "linux", target_arch = "aarch64", feature = "c-execution"))]
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&'static str, &[u8])> {
        DEFINITIONS
            .iter()
            .zip(&self.values)
            .filter_map(|(definition, value)| value.as_deref().map(|value| (definition.name, value)))
    }

    #[must_use]
    pub fn defines(name: &str) -> bool {
        DEFINITIONS.iter().any(|definition| definition.name == name)
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.get_bytes(name).and_then(|value| std::str::from_utf8(value).ok())
    }

    #[must_use]
    pub fn get_bytes(&self, name: &str) -> Option<&[u8]> {
        self.index(name).and_then(|index| self.values[index].as_deref())
    }

    pub fn set(&mut self, name: &str, value: &str, overwrite: bool) -> Result<(), OptionError> {
        self.set_bytes(name, value.as_bytes(), overwrite)
    }

    pub fn set_bytes(&mut self, name: &str, value: &[u8], overwrite: bool) -> Result<(), OptionError> {
        let index = self.index(name).ok_or(OptionError::UnknownName)?;
        if !overwrite && self.values[index].is_some() {
            return Ok(());
        }
        if value.contains(&0) {
            return Err(OptionError::ValueContainsNul);
        }
        if DEFINITIONS[index].shape == Shape::Integer
            && (value.is_empty()
                || !value.iter().all(u8::is_ascii_digit)
                || std::str::from_utf8(value)
                    .ok()
                    .and_then(|text| text.parse::<u64>().ok())
                    .is_none())
        {
            return Err(OptionError::InvalidValue);
        }
        let value_size = value.len().checked_add(1).ok_or(OptionError::StoreLimit)?;
        let old_size = self.values[index].as_ref().map_or(0, |old| old.len() + 1);
        let retained = self.store_size - old_size;
        if value_size > STORE_LIMIT || retained > STORE_LIMIT - value_size {
            return Err(OptionError::StoreLimit);
        }
        self.values[index] = Some(value.to_vec());
        self.store_size = retained + value_size;
        Ok(())
    }

    pub fn unset(&mut self, name: &str) -> Result<(), OptionError> {
        let index = self.index(name).ok_or(OptionError::UnknownName)?;
        if let Some(old) = self.values[index].take() {
            self.store_size -= old.len() + 1;
        }
        Ok(())
    }

    #[must_use]
    pub fn store_size(&self) -> usize {
        self.store_size
    }

    pub fn integer(&self, name: &str) -> Result<Option<u64>, OptionError> {
        let index = self.index(name).ok_or(OptionError::UnknownName)?;
        if DEFINITIONS[index].shape != Shape::Integer {
            return Err(OptionError::InvalidValue);
        }
        self.values[index]
            .as_deref()
            .map(|value| {
                std::str::from_utf8(value)
                    .ok()
                    .and_then(|text| text.parse::<u64>().ok())
                    .ok_or(OptionError::InvalidValue)
            })
            .transpose()
    }

    fn index(&self, name: &str) -> Option<usize> {
        DEFINITIONS.iter().position(|definition| definition.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_unique_names_and_debug_tail() {
        let mut names = DEFINITIONS.iter().map(|definition| definition.name).collect::<Vec<_>>();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), DEFINITIONS.len());
        assert_eq!(DEFINITIONS[0].name, "HL_CHECKPOINT");
        assert_eq!(DEFINITIONS.last().unwrap().ownership, Ownership::DebugOnly);
    }

    #[test]
    fn dirty_overflow_policies_are_explicit_launch_options() {
        let mut options = Options::default();
        for name in ["HL_A64_DIRTY_OVERFLOW_CONTINUE", "HL_A64_DIRTY_OVERFLOW_EXIT"] {
            let definition = DEFINITIONS.iter().find(|definition| definition.name == name).unwrap();
            assert_eq!(definition.ownership, Ownership::LaunchInput);
            assert_eq!(definition.shape, Shape::Flag);
            assert_eq!(options.get(name), None);
            options.set(name, "1", true).unwrap();
            assert_eq!(options.get(name), Some("1"));
        }
    }

    #[test]
    fn split_mode_executors_is_an_explicit_launch_option() {
        let definition = DEFINITIONS
            .iter()
            .find(|definition| definition.name == "HL_NATIVE_SPLIT_MODE_EXECUTORS")
            .unwrap();
        assert_eq!(definition.ownership, Ownership::LaunchInput);
        assert_eq!(definition.shape, Shape::Flag);

        let mut options = Options::default();
        assert_eq!(options.get(definition.name), None);
        options.set(definition.name, "1", true).unwrap();
        assert_eq!(options.get(definition.name), Some("1"));
    }

    #[test]
    fn execution_backend_is_an_explicit_launch_option() {
        let definition = DEFINITIONS
            .iter()
            .find(|definition| definition.name == "HL_EXECUTION_BACKEND")
            .unwrap();
        assert_eq!(definition.ownership, Ownership::LaunchInput);
        assert_eq!(definition.shape, Shape::Text);
        let mut options = Options::default();
        options.set(definition.name, "c", true).unwrap();
        assert_eq!(options.get(definition.name), Some("c"));
    }

    #[test]
    fn set_overwrite_unset() {
        let mut options = Options::default();
        options.set("HL_UID", "1000", true).unwrap();
        assert_eq!(options.get("HL_UID"), Some("1000"));
        assert_eq!(options.store_size(), 5);
        options.set("HL_UID", "7", false).unwrap();
        assert_eq!(options.get("HL_UID"), Some("1000"));
        options.set("HL_UID", "7", true).unwrap();
        assert_eq!(options.store_size(), 2);
        options.unset("HL_UID").unwrap();
        assert_eq!(options.get("HL_UID"), None);
        assert_eq!(options.store_size(), 0);
    }

    #[test]
    #[cfg(all(target_os = "linux", target_arch = "aarch64", feature = "c-execution"))]
    fn iteration_preserves_explicit_empty_and_omits_absent_options() {
        let mut options = Options::default();
        options.set("HL_CWD", "", true).unwrap();
        options.set("HL_UID", "1000", true).unwrap();
        assert_eq!(
            options.iter().collect::<Vec<_>>(),
            [("HL_CWD", b"".as_slice()), ("HL_UID", b"1000".as_slice())]
        );
    }

    #[test]
    fn rejects_unknown_names() {
        let mut options = Options::default();
        assert_eq!(options.set("UNKNOWN", "x", true), Err(OptionError::UnknownName));
        assert_eq!(options.set("HL_LOG", "x\0y", true), Err(OptionError::ValueContainsNul));
    }

    #[test]
    fn rejects_invalid_integer() {
        let mut options = Options::default();
        for value in ["", "-1", "+1", " 1", "1x", "18446744073709551616"] {
            assert_eq!(options.set("HL_UID", value, true), Err(OptionError::InvalidValue),);
            assert_eq!(options.integer("HL_UID"), Ok(None));
        }
        options.set("HL_UID", "4294967295", true).unwrap();
        assert_eq!(options.integer("HL_UID"), Ok(Some(4_294_967_295)));
        assert_eq!(options.integer("HL_CWD"), Err(OptionError::InvalidValue));
    }
}
