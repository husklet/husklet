//! Launch-scoped engine options.
//!
//! This retains the C registry contract without retaining its thread/process
//! globals. A caller explicitly owns and passes one store per launch.

/// Maximum aggregate storage, including one trailing NUL per value.
const STORE_LIMIT: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Shape {
    Text,
    Path,
    Integer,
    Flag,
    Records,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Definition {
    name: &'static str,
    shape: Shape,
}

macro_rules! launch {
    ($name:literal, $_purpose:literal, $shape:ident) => {
        Definition {
            name: $name,
            shape: Shape::$shape,
        }
    };
}
macro_rules! debug {
    ($name:literal, $_purpose:literal, $shape:ident) => {
        Definition {
            name: $name,
            shape: Shape::$shape,
        }
    };
}
macro_rules! internal {
    ($name:literal, $_purpose:literal, $shape:ident) => {
        Definition {
            name: $name,
            shape: Shape::$shape,
        }
    };
}

const DEFINITIONS: &[Definition] = &[
    launch!("HL_CHECKPOINT", "arm checkpoint capture over the store channel", Flag),
    internal!(
        "HL_CHECKPOINT_COORDINATOR",
        "this launch owns the domain freeze: exactly one engine per checkpoint broker",
        Flag
    ),
    internal!(
        "HL_CHECKPOINT_PHASE_LEDGER",
        "emit checkpoint phase timing records for performance gates",
        Flag
    ),
    internal!(
        "HL_CHECKPOINT_FD_SCAN_PROFILE",
        "emit checkpoint descriptor scan complexity records for performance gates",
        Flag
    ),
    internal!(
        "HL_DIAGNOSTIC_PORT",
        "private engine diagnostic writer descriptor",
        Integer
    ),
    internal!(
        "HL_CHECKPOINT_PHASE_CLOCK_FAIL",
        "inject an unavailable checkpoint phase clock",
        Flag
    ),
    internal!("HL_CHECKPOINT_PHASE_ISA", "checkpoint phase ledger guest ISA", Text),
    internal!(
        "HL_CHECKPOINT_PHASE_GENERATION",
        "checkpoint restore phase ledger generation",
        Integer
    ),
    launch!(
        "HL_CHECKPOINT_POLICY",
        "checkpoint incompatible-resource recovery policy",
        Integer
    ),
    launch!("HL_CPUS", "guest-visible CPU quota", Integer),
    launch!("HL_CWD", "initial guest working directory", Path),
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
        "HL_C_DIAGNOSTICS",
        "report retained C translation and dispatch phase counters at launch exit",
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
    launch!(
        "HL_TRANSLIT",
        "select the same-ISA transliterating backend for an x86-64 guest",
        Flag
    ),
    internal!(
        "HL_TRANSLIT_PROVENANCE_FALLBACK",
        "test-only same-ISA fault recovery without exact instruction provenance",
        Flag
    ),
    internal!(
        "HL_TRANSLIT_BODY_OWNER_EXHAUST",
        "test-only exhaust immutable same-ISA body-owner capacity",
        Flag
    ),
    internal!(
        "HL_TRANSLIT_BODY_OWNER_ROTATE_TEST",
        "test-only force single-thread same-ISA body-owner cache rotation",
        Flag
    ),
    internal!(
        "HL_TRANSLIT_PERF_FRESH_ROLLOVER_TEST",
        "test-only force a threaded same-ISA fresh-arena rollover",
        Flag
    ),
    internal!(
        "HL_TRANSLIT_FS_AUTHORITY_TEST",
        "test-only force same-ISA FS direct-data authority refusal",
        Flag
    ),
    internal!(
        "HL_TRANSLIT_PCACHE_DROP_RELOCATION_TEST",
        "test-only omit one same-ISA pcache external relocation",
        Flag
    ),
    internal!(
        "HL_TRANSLIT_PCACHE_WARM_FAIL_STAGE",
        "test-only fail one same-ISA warm-cache reconstruction stage",
        Text
    ),
    internal!(
        "HL_TRANSLIT_PCACHE_SINGLE_MAP_TEST",
        "test-only exercise warm-cache rollback through the single-map W^X arena",
        Flag
    ),
    internal!(
        "HL_PCACHE_OBSERVE",
        "emit structured persistent-cache diagnostics",
        Flag
    ),
    launch!(
        "HL_TRANSLIT_MIXED_SSE_DISABLE",
        "disable mixed normal/SSE same-ISA descriptor admission",
        Flag
    ),
    internal!(
        "HL_TRANSLIT_JCC_LINK_DISABLE",
        "test-only disable already-published same-page JCC links",
        Flag
    ),
    launch!(
        "HL_TRANSLIT_JCC_IBTC_DISABLE",
        "disable unresolved constant-JCC late linking through the same-ISA IBTC",
        Flag
    ),
    launch!(
        "HL_TRANSLIT_DIRECT_JMP_IBTC_DISABLE",
        "disable direct-JMP late linking through the same-ISA IBTC",
        Flag
    ),
    internal!(
        "HL_TRANSLIT_PROFILE_WIDE_TEST",
        "test-only render every same-ISA profile counter at UINT64 width",
        Flag
    ),
    internal!(
        "HL_TRANSLIT_PERF_MAP",
        "test-only directory for same-ISA perf map publication",
        Path
    ),
    internal!(
        "HL_TRANSLIT_SYMBOLIZE",
        "publish same-ISA perf symbols without enabling execution diagnostics",
        Flag
    ),
    internal!(
        "HL_TRANSLIT_SYMBOL_RECEIPT",
        "test-only sampling symbol enqueue and teardown receipts",
        Flag
    ),
    internal!(
        "HL_CKPT_TEST_FAIL_AFTER_FORK",
        "test-only restore failure after rebuilding descendants",
        Flag
    ),
    internal!(
        "HL_CKPT_TEST_FAIL_TRIGGER_REATTACH",
        "test-only restored checkpoint trigger reattachment failure",
        Flag
    ),
    internal!(
        "HL_CKPT_TEST_PEER_EXIT_BEFORE_JOIN",
        "test-only capture peer that exits before proving membership",
        Flag
    ),
    internal!(
        "HL_CKPT_TEST_PEER_EXIT_AFTER_JOIN",
        "test-only capture peer that exits after proving membership",
        Flag
    ),
    internal!(
        "HL_CKPT_TEST_PEER_SLOW_SAFEPOINT",
        "test-only capture peer that works far longer than the rendezvous stall window before committing",
        Flag
    ),
    internal!(
        "HL_CKPT_TEST_PEER_STALLS_AT_SAFEPOINT",
        "test-only capture peer that never commits and consumes no host CPU time",
        Flag
    ),
    internal!(
        "HL_CKPT_TEST_PEER_FORGOTTEN_AFTER_KICK",
        "test-only capture peer dropped from the coordinator's enumeration after it is kicked",
        Flag
    ),
    internal!(
        "HL_CKPT_TEST_REAPED_UNNAMEABLE",
        "test-only capture whose reap destroys an unnameable child exit status",
        Flag
    ),
    internal!(
        "HL_CKPT_TEST_PEER_HIDDEN_FROM_ENUMERATION",
        "test-only capture peer withheld from the coordinator's first enumeration",
        Flag
    ),
    internal!(
        "HL_CKPT_TEST_FAIL_TTY_MASK",
        "test-only terminal-claim mask failure",
        Flag
    ),
    internal!(
        "HL_CKPT_TEST_FAIL_PIDMAP_AT",
        "test-only restore identity-publication failure at this guest pid",
        Integer
    ),
    launch!("HL_ROOTFS_RO", "mount the guest root filesystem read-only", Flag),
    launch!("HL_SANDBOX", "apply host confinement to the untrusted worker", Flag),
    launch!("HL_SECCOMP_BASELINE", "guest-visible launch seccomp baseline", Text),
    launch!(
        "HL_NATIVE_SUPERVISED",
        "run a Linux x86-64 guest natively under syscall supervision",
        Flag
    ),
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
    internal!(
        "HL_NATIVE_SUPERVISED_REFUSE",
        "test-only supervised syscall refusal as number:errno",
        Text
    ),
    internal!(
        "HL_NATIVE_NOTIFY_TEST_RECEIPT",
        "test-only supervised notification census receipt",
        Path
    ),
    internal!(
        "HL_NATIVE_CKPT_TEST_RECEIPT",
        "test-only native checkpoint phase receipt",
        Path
    ),
    internal!(
        "HL_NATIVE_CKPT_TEST_IDLE_RECEIPT",
        "test-only native checkpoint idle-wakeup receipt",
        Path
    ),
    internal!(
        "HL_NATIVE_CKPT_TEST_SKIP_REGISTER",
        "test-only native checkpoint registration mutation",
        Flag
    ),
    debug!("HL_LOG", "debug-build logging tag selector", Text),
    debug!("HL_FATAL_DIAGNOSTICS", "fatal guest register publication", Flag),
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
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&'static str, &[u8])> {
        DEFINITIONS
            .iter()
            .zip(&self.values)
            .filter_map(|(definition, value)| value.as_deref().map(|value| (definition.name, value)))
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.get_bytes(name).and_then(|value| std::str::from_utf8(value).ok())
    }

    #[must_use]
    pub(crate) fn get_bytes(&self, name: &str) -> Option<&[u8]> {
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
        let tail = &DEFINITIONS[DEFINITIONS.len() - 2..];
        assert_eq!(tail[0].name, "HL_LOG");
        assert_eq!(tail[1].name, "HL_FATAL_DIAGNOSTICS");
    }

    /// The C registry in `src/runtime/hl-native/src/native/engine/options.c` is the
    /// authoritative one: `hl_options_set` rejects a name it does not define, so a worker
    /// launched with a Rust-only option fails to start. A Rust-only name therefore promises
    /// a launch effect no layer implements, and a C-only name is unreachable through the
    /// product surface. Three failure-injection options
    /// (`HL_CKPT_TEST_FAIL_AFTER_FORK`, `HL_CKPT_TEST_FAIL_TRIGGER_REATTACH`,
    /// `HL_CKPT_TEST_FAIL_TTY_MASK`) drifted this way and their tests passed because the
    /// engine never launched, not because the injected failure was handled.
    const C_REGISTRY: &str = include_str!("../../../runtime/hl-native/src/native/engine/options.c");

    fn c_registry_definitions() -> Vec<(String, Shape)> {
        let body = C_REGISTRY
            .split_once("hl_option_definitions[] = {")
            .expect("C registry table")
            .1
            .split_once("\n};")
            .expect("C registry table end")
            .0;
        let mut definitions = Vec::new();
        for entry in body.split("_OPTION(").skip(1) {
            let name = entry
                .split_once('"')
                .and_then(|(_, rest)| rest.split_once('"'))
                .expect("option name")
                .0;
            let shape = entry
                .split_once("HL_OPTION_")
                .expect("option shape")
                .1
                .split_once(')')
                .expect("option shape end")
                .0
                .trim();
            let shape = match shape {
                "TEXT" => Shape::Text,
                "PATH" => Shape::Path,
                "INTEGER" => Shape::Integer,
                "FLAG" => Shape::Flag,
                "RECORDS" => Shape::Records,
                other => panic!("unknown C option shape {other} for {name}"),
            };
            definitions.push((name.to_owned(), shape));
        }
        definitions
    }

    #[test]
    fn c_registry_parse_is_not_vacuous() {
        let c = c_registry_definitions();
        assert!(c.len() > 40, "parsed only {} C options", c.len());
        assert!(c.contains(&("HL_UID".to_owned(), Shape::Integer)));
        assert!(c.contains(&("HL_CHECKPOINT".to_owned(), Shape::Flag)));
        assert!(c.contains(&("HL_CWD".to_owned(), Shape::Path)));
    }

    #[test]
    fn every_option_is_registered_on_both_sides_with_the_same_shape() {
        let mut c = c_registry_definitions();
        c.sort_by(|left, right| left.0.cmp(&right.0));
        let mut rust = DEFINITIONS
            .iter()
            .map(|definition| (definition.name.to_owned(), definition.shape))
            .collect::<Vec<_>>();
        rust.sort_by(|left, right| left.0.cmp(&right.0));
        let rust_only = rust.iter().filter(|entry| !c.contains(entry)).collect::<Vec<_>>();
        let c_only = c.iter().filter(|entry| !rust.contains(entry)).collect::<Vec<_>>();
        assert!(
            rust_only.is_empty() && c_only.is_empty(),
            "option registries drifted; a Rust-only name makes the worker refuse to start.\n             registered only in hl-engine/src/options.rs: {rust_only:?}\n             registered only in native/engine/options.c: {c_only:?}"
        );
    }

    /// The native tree, for scanning what the engine actually *reads*.
    fn native_sources() -> Vec<String> {
        fn walk(directory: &std::path::Path, sources: &mut Vec<String>) {
            for entry in std::fs::read_dir(directory).expect("native directory").flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, sources);
                } else if path.extension().is_some_and(|kind| kind == "c" || kind == "h") {
                    sources.push(std::fs::read_to_string(&path).unwrap_or_default());
                }
            }
        }
        let native = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../runtime/hl-native/src/native");
        let mut sources = Vec::new();
        walk(&native, &mut sources);
        assert!(sources.len() > 50, "scanned only {} native sources", sources.len());
        sources
    }

    /// Every injection name the engine reads, taken from the engine rather than from a list a lane
    /// has to remember to update.
    fn injection_names_read_by_the_engine() -> Vec<String> {
        let mut names = Vec::new();
        for source in native_sources() {
            for fragment in source.split("hl_option_get(\"").skip(1) {
                let Some((name, _)) = fragment.split_once('"') else {
                    continue;
                };
                if name.starts_with("HL_CKPT_TEST_") && !names.iter().any(|held| held == name) {
                    names.push(name.to_owned());
                }
            }
        }
        names.sort_unstable();
        names
    }

    /// An unregistered name is not a typo, it is an injection that can never fire.
    ///
    /// `hl_option_get` resolves a name against the registry and answers NULL for anything it does
    /// not define, so a read of an unregistered option is dead code that silently reports "not
    /// armed" forever -- and a test written against it passes without ever injecting. Three
    /// injections had already drifted the other way (registered in Rust only, so the worker refused
    /// to start); `HL_CKPT_TEST_FAIL_PIDMAP_AT` had drifted this way, read by
    /// `ckpt_restore_identity_hydrate` and defined nowhere.
    #[test]
    fn every_injection_the_engine_reads_is_registered() {
        let read = injection_names_read_by_the_engine();
        assert!(read.len() >= 6, "found only {read:?}");
        let unregistered = read
            .iter()
            .filter(|name| !DEFINITIONS.iter().any(|definition| definition.name == name.as_str()))
            .collect::<Vec<_>>();
        assert!(
            unregistered.is_empty(),
            "read by the engine but registered nowhere, so the injection can never fire: {unregistered:?}"
        );
    }

    /// The scoping guarantee, pinned where it is expressed.
    ///
    /// `HL_OPTION_TEST_INJECTION` is what makes `hl_options_clone` drop an armed injection, so an
    /// injection cannot be inherited by a nested engine built with no explicit options, nor carried
    /// across an exec by the guest environment update. Classifying a new injection as
    /// `HL_INTERNAL_OPTION` would silently restore that inheritance, so the class is asserted here
    /// against the same authoritative C source the shape cross-check reads.
    #[test]
    fn every_injection_is_classified_as_one_in_the_c_registry() {
        let body = C_REGISTRY
            .split_once("hl_option_definitions[] = {")
            .expect("C registry table")
            .1
            .split_once("\n};")
            .expect("C registry table end")
            .0;
        for name in injection_names_read_by_the_engine() {
            let entry = body
                .split("_OPTION(")
                .find(|entry| entry.starts_with(&format!("\"{name}\"")))
                .unwrap_or_else(|| panic!("{name} is not in the C registry"));
            let macro_name = body
                .split_once(&format!("_OPTION(\"{name}\""))
                .expect("registry entry")
                .0;
            assert!(
                macro_name.ends_with("HL_INJECTION"),
                "{name} is registered as {}_OPTION, so hl_options_clone would copy it into every \
                 derived option set instead of scoping it to the launch that armed it",
                macro_name
                    .rsplit(|character: char| character.is_whitespace())
                    .next()
                    .unwrap_or("")
            );
            let _ = entry;
        }
    }

    #[test]
    fn perf_map_directory_is_exec_cloned_internal_authority() {
        assert!(
            C_REGISTRY.contains("HL_INTERNAL_OPTION(\"HL_TRANSLIT_PERF_MAP\""),
            "guest exec must retain the caller-owned profiler directory"
        );
        assert!(!C_REGISTRY.contains("HL_INJECTION_OPTION(\"HL_TRANSLIT_PERF_MAP\""));
        assert!(C_REGISTRY.contains("HL_INTERNAL_OPTION(\"HL_TRANSLIT_SYMBOLIZE\""));
        assert!(!C_REGISTRY.contains("HL_INJECTION_OPTION(\"HL_TRANSLIT_SYMBOLIZE\""));
    }

    #[test]
    fn retired_native_executor_options_are_not_registered() {
        // The Rust native executor these named was deleted; the C engine's authoritative registry
        // (src/runtime/hl-native/src/native/engine/options.c) never defined them, so accepting them
        // would promise a launch effect no layer implements.
        for name in [
            "HL_NATIVE_EXECUTION",
            "HL_NATIVE_DIAGNOSTICS",
            "HL_NATIVE_ADMISSION_CACHE",
            "HL_NATIVE_DIRECT_STICKY",
            "HL_NATIVE_DIRECT_STICKY_LIMIT",
            "HL_NATIVE_DIRECT_HOLD_RUNS",
            "HL_NATIVE_DIRECT_STICKY_PERMANENT",
            "HL_NATIVE_SPLIT_MODE_EXECUTORS",
            // Same class, found by diffing the two registries: registered in Rust only,
            // with no consumer in C or Rust anywhere in the tree.
            "HL_A64_DIRTY_OVERFLOW_CONTINUE",
            "HL_A64_DIRTY_OVERFLOW_EXIT",
            "HL_A64_NO_WRITE_COMMIT",
            "HL_A64_NO_WRITE_RESERVE",
            "HL_A64_RUNTIME_WRITE_RESERVE",
            "HL_C_EXECUTION_ATTESTATION",
            "HL_C_NO_RUNTIME_EXIT",
            "HL_C_NO_RUNTIME_IDENTITY",
        ] {
            assert!(
                !DEFINITIONS.iter().any(|definition| definition.name == name),
                "{name} is registered but nothing consumes it"
            );
            assert!(Options::default().set(name, "1", true).is_err(), "{name} was accepted");
        }
    }

    #[test]
    fn set_overwrite_unset() {
        let mut options = Options::default();
        options.set("HL_UID", "1000", true).unwrap();
        assert_eq!(options.get("HL_UID"), Some("1000"));
        options.set("HL_UID", "7", false).unwrap();
        assert_eq!(options.get("HL_UID"), Some("1000"));
        options.set("HL_UID", "7", true).unwrap();
        options.unset("HL_UID").unwrap();
        assert_eq!(options.get("HL_UID"), None);
    }

    #[test]
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
        }
        options.set("HL_UID", "4294967295", true).unwrap();
        assert_eq!(options.get("HL_UID"), Some("4294967295"));
    }
}
