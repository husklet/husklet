#![allow(unsafe_code)]

use crate::{
    bindings::{Backend, EngineExit, MainImagePlan, SyscallDispatch},
    dynamic_library::DynamicLibrary,
};
use std::{
    ffi::{c_char, c_int, c_uint, c_void},
    path::{Path, PathBuf},
    sync::OnceLock,
};

const BRIDGE_ABI: u32 = 1;
const ENGINE_ABI: u32 = 5;

pub(crate) type EngineAbi = unsafe extern "C" fn() -> c_uint;
pub(crate) type EngineVersion = unsafe extern "C" fn() -> *const c_char;
pub(crate) type LeakCheck = unsafe extern "C" fn() -> c_int;
pub(crate) type BrokerPair = unsafe extern "C" fn(*mut c_int, *mut c_int) -> c_int;
pub(crate) type BrokerAccept = unsafe extern "C" fn(c_int, c_int, *mut u64) -> c_int;
pub(crate) type AuthenticatedBrokerAccept =
    unsafe extern "C" fn(c_int, c_int, *mut u64, *mut u64, *mut u64, *mut c_int) -> c_int;
pub(crate) type TriggerCreate = unsafe extern "C" fn(*mut c_int, *mut *mut c_void) -> c_int;
pub(crate) type TriggerBump = unsafe extern "C" fn(*mut c_void) -> c_uint;
pub(crate) type TriggerDestroy = unsafe extern "C" fn(*mut c_void, c_int);
pub(crate) type CheckpointAdopt = unsafe extern "C" fn(c_uint, c_int, c_int) -> c_int;
pub(crate) type InterruptSignal = unsafe extern "C" fn(c_uint) -> c_int;
pub(crate) type CheckpointConfigure = unsafe extern "C" fn(*mut Backend, c_int, c_int) -> c_int;
pub(crate) type ExecutableOpen = unsafe extern "C" fn(*const c_void, *const c_char, *mut c_void) -> c_int;
pub(crate) type ExecutableDiscard = unsafe extern "C" fn(*const c_void, *mut c_void);
pub(crate) type Create = unsafe extern "C" fn(
    c_uint,
    *const c_char,
    *const c_char,
    c_int,
    *const MainImagePlan,
    *const c_void,
    usize,
    c_uint,
    *const *const c_char,
    *const *const c_char,
    *const c_int,
    c_int,
    *mut c_void,
    Option<SyscallDispatch>,
    *mut *mut Backend,
) -> c_int;
pub(crate) type Run = unsafe extern "C" fn(*mut Backend, c_int, *const *const c_char) -> c_int;
pub(crate) type Request = unsafe extern "C" fn(*mut Backend, c_uint, c_int) -> c_int;
pub(crate) type Exit = unsafe extern "C" fn(*mut Backend, *mut EngineExit) -> c_int;
pub(crate) type Destroy = unsafe extern "C" fn(*mut Backend);

#[cfg(feature = "native-test-hooks")]
pub(crate) type VectorIoTest = unsafe extern "C" fn(c_uint, *mut i64, *mut c_uint, *mut u64) -> c_int;
#[cfg(feature = "native-test-hooks")]
pub(crate) type ScenarioTest = unsafe extern "C" fn(c_uint) -> c_int;
#[cfg(feature = "native-test-hooks")]
pub(crate) type SignalFrameTest = unsafe extern "C" fn(c_uint, c_uint, u64, i64, *mut i64, *mut i64) -> c_int;
#[cfg(feature = "native-test-hooks")]
pub(crate) type NoArgumentTest = unsafe extern "C" fn() -> c_int;
#[cfg(feature = "native-test-hooks")]
pub(crate) type TermiosInstallTest = unsafe extern "C" fn(c_int, *const u8);
#[cfg(feature = "native-test-hooks")]
pub(crate) type UnixIdentityTest = unsafe extern "C" fn(c_uint, c_int, u64, *mut u64, *mut u64, *mut c_uint) -> c_int;
#[cfg(feature = "native-test-hooks")]
pub(crate) type UnixIdentityCaptureTest = unsafe extern "C" fn(c_int) -> c_int;
#[cfg(feature = "native-test-hooks")]
pub(crate) type SocketShapeTest = unsafe extern "C" fn(c_uint, c_int, c_uint, *mut u8, *mut c_uint) -> c_int;

pub(crate) type GuestPid = unsafe extern "C" fn(*const Backend) -> c_int;
pub(crate) type ProcessIdentitySignal = unsafe extern "C" fn(c_int, u64, c_int) -> c_int;
pub(crate) type TerminalTermiosGeneration = unsafe extern "C" fn() -> u64;
pub(crate) type TerminalTermios = unsafe extern "C" fn(c_int, *mut u8) -> c_int;
pub(crate) type TerminalTermiosAdopt = unsafe extern "C" fn(c_int, *const u8) -> c_int;

#[derive(Clone, Copy)]
#[repr(C)]
pub(crate) struct BridgeApi {
    abi: u32,
    size: u32,
    pub(crate) engine_abi: Option<EngineAbi>,
    pub(crate) engine_version: Option<EngineVersion>,
    pub(crate) leak_check_nonvacuity: Option<LeakCheck>,
    pub(crate) checkpoint_broker_pair: Option<BrokerPair>,
    pub(crate) checkpoint_broker_accept: Option<BrokerAccept>,
    pub(crate) checkpoint_trigger_create: Option<TriggerCreate>,
    pub(crate) checkpoint_trigger_bump: Option<TriggerBump>,
    pub(crate) checkpoint_trigger_destroy: Option<TriggerDestroy>,
    pub(crate) checkpoint_adopt: Option<CheckpointAdopt>,
    pub(crate) checkpoint_interrupt_signal: Option<InterruptSignal>,
    pub(crate) checkpoint_configure: Option<CheckpointConfigure>,
    pub(crate) executable_open: Option<ExecutableOpen>,
    pub(crate) executable_discard: Option<ExecutableDiscard>,
    pub(crate) create: Option<Create>,
    pub(crate) run: Option<Run>,
    pub(crate) request: Option<Request>,
    pub(crate) exit: Option<Exit>,
    pub(crate) destroy: Option<Destroy>,
    pub(crate) checkpoint_broker_accept_authenticated: Option<AuthenticatedBrokerAccept>,
    pub(crate) guest_pid: Option<GuestPid>,
    pub(crate) process_identity_signal: Option<ProcessIdentitySignal>,
    pub(crate) terminal_termios_generation: Option<TerminalTermiosGeneration>,
    pub(crate) terminal_termios: Option<TerminalTermios>,
    pub(crate) terminal_termios_capture: Option<TerminalTermios>,
    pub(crate) terminal_termios_adopt: Option<TerminalTermiosAdopt>,
}

#[derive(Clone, Copy)]
// The header shares its address with the `BridgeApi` function-pointer table, whose alignment is 8.
#[repr(C, align(8))]
struct BridgeHeader {
    abi: u32,
    size: u32,
}

type BridgeGetter = unsafe extern "C" fn() -> *const BridgeHeader;

#[cfg(feature = "native-test-hooks")]
pub(crate) struct TestApi {
    pub(crate) aarch64_bound_vector_io: VectorIoTest,
    pub(crate) x86_64_bound_vector_io: VectorIoTest,
    #[cfg(test)]
    pub(crate) aarch64_fdvis_path_publication: ScenarioTest,
    #[cfg(test)]
    pub(crate) x86_64_fdvis_path_publication: ScenarioTest,
    pub(crate) aarch64_namespace_transaction: ScenarioTest,
    pub(crate) x86_64_namespace_transaction: ScenarioTest,
    pub(crate) x86_64_store_preflight: NoArgumentTest,
    pub(crate) aarch64_reserved_register: NoArgumentTest,
    pub(crate) x86_64_reserved_register: NoArgumentTest,
    pub(crate) aarch64_imported_path_guard: NoArgumentTest,
    pub(crate) x86_64_imported_path_guard: NoArgumentTest,
    pub(crate) aarch64_signal_errno_frame: SignalFrameTest,
    pub(crate) x86_64_signal_errno_frame: SignalFrameTest,
    pub(crate) aarch64_checkpoint_signal_precedence: NoArgumentTest,
    pub(crate) x86_64_checkpoint_signal_precedence: NoArgumentTest,
    pub(crate) aarch64_checkpoint_restart_register: NoArgumentTest,
    pub(crate) x86_64_checkpoint_restart_register: NoArgumentTest,
    pub(crate) aarch64_checkpoint_restore_claim: ScenarioTest,
    pub(crate) x86_64_checkpoint_restore_claim: ScenarioTest,
    pub(crate) aarch64_checkpoint_restore_slice: ScenarioTest,
    pub(crate) x86_64_checkpoint_restore_slice: ScenarioTest,
    pub(crate) aarch64_checkpoint_gmap_release: ScenarioTest,
    pub(crate) x86_64_checkpoint_gmap_release: ScenarioTest,
    pub(crate) aarch64_checkpoint_anon_shared: ScenarioTest,
    pub(crate) x86_64_checkpoint_anon_shared: ScenarioTest,
    pub(crate) aarch64_checkpoint_rendezvous: ScenarioTest,
    pub(crate) x86_64_checkpoint_rendezvous: ScenarioTest,
    pub(crate) aarch64_checkpoint_membership: ScenarioTest,
    pub(crate) x86_64_checkpoint_membership: ScenarioTest,
    pub(crate) aarch64_checkpoint_election: ScenarioTest,
    pub(crate) x86_64_checkpoint_election: ScenarioTest,
    pub(crate) aarch64_pid_namespace: ScenarioTest,
    pub(crate) x86_64_pid_namespace: ScenarioTest,
    pub(crate) aarch64_checkpoint_identity: ScenarioTest,
    pub(crate) x86_64_checkpoint_identity: ScenarioTest,
    pub(crate) aarch64_checkpoint_launch_identity: ScenarioTest,
    pub(crate) x86_64_checkpoint_launch_identity: ScenarioTest,
    pub(crate) aarch64_checkpoint_pipe_capture: ScenarioTest,
    pub(crate) x86_64_checkpoint_pipe_capture: ScenarioTest,
    pub(crate) aarch64_checkpoint_stdio_alias_capture: ScenarioTest,
    pub(crate) x86_64_checkpoint_stdio_alias_capture: ScenarioTest,
    pub(crate) aarch64_checkpoint_socket_halfclose: ScenarioTest,
    pub(crate) x86_64_checkpoint_socket_halfclose: ScenarioTest,
    pub(crate) aarch64_checkpoint_ipc_admission: ScenarioTest,
    pub(crate) x86_64_checkpoint_ipc_admission: ScenarioTest,
    pub(crate) aarch64_checkpoint_restore_rollback: NoArgumentTest,
    pub(crate) x86_64_checkpoint_restore_rollback: NoArgumentTest,
    pub(crate) aarch64_terminal_termios_store: NoArgumentTest,
    pub(crate) x86_64_terminal_termios_store: NoArgumentTest,
    pub(crate) aarch64_terminal_termios_install: TermiosInstallTest,
    pub(crate) aarch64_unix_identity: UnixIdentityTest,
    pub(crate) x86_64_unix_identity: UnixIdentityTest,
    pub(crate) aarch64_unix_identity_capture: UnixIdentityCaptureTest,
    pub(crate) x86_64_unix_identity_capture: UnixIdentityCaptureTest,
    pub(crate) aarch64_socket_shape: SocketShapeTest,
    pub(crate) x86_64_socket_shape: SocketShapeTest,
    pub(crate) errno_from_host: unsafe extern "C" fn(c_uint, c_int) -> c_int,
    #[cfg(target_os = "macos")]
    pub(crate) directory_stream_private: unsafe extern "C" fn(c_uint) -> c_int,
    pub(crate) identity_registry: unsafe extern "C" fn(c_uint, c_uint) -> c_int,
    pub(crate) private_fork_lock: unsafe extern "C" fn(c_uint) -> c_int,
    pub(crate) aarch64_checkpoint_channel_notify: ScenarioTest,
    pub(crate) x86_64_checkpoint_channel_notify: ScenarioTest,
    pub(crate) aarch64_checkpoint_refusal_order: NoArgumentTest,
    pub(crate) x86_64_checkpoint_refusal_order: NoArgumentTest,
    pub(crate) process_identity_token: unsafe extern "C" fn(c_uint) -> c_int,
    pub(crate) setfl_append_write: unsafe extern "C" fn(c_uint) -> c_int,
    #[allow(dead_code)]
    pub(crate) checkpoint_peer_authenticate: unsafe extern "C" fn(c_int, u64, *mut u64, *mut u64) -> c_int,
    pub(crate) checkpoint_channel_connect: unsafe extern "C" fn(c_int) -> c_int,
    #[cfg(target_os = "macos")]
    pub(crate) checkpoint_process_identity_open: unsafe extern "C" fn(c_int, u64, u64, *mut u64, *mut u64) -> c_int,
    #[cfg(target_os = "macos")]
    pub(crate) checkpoint_peer_identity_open: unsafe extern "C" fn(c_int, u64, *mut u64, *mut u64, *mut u64) -> c_int,
    #[cfg(test)]
    pub(crate) checkpoint_test_prune_foreign_descriptors: unsafe extern "C" fn() -> c_uint,
    #[cfg(test)]
    pub(crate) checkpoint_test_fail_registry_allocation: unsafe extern "C" fn(),
    #[cfg(test)]
    pub(crate) checkpoint_test_fail_private_adopt: unsafe extern "C" fn(c_uint),
    #[cfg(test)]
    pub(crate) checkpoint_test_private_descriptor_count: unsafe extern "C" fn() -> u64,
    #[cfg(all(test, unix))]
    pub(crate) host_process_force: unsafe extern "C" fn(c_int) -> c_int,
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) host_process_peer_enumerated: unsafe extern "C" fn(c_int) -> c_int,
    #[cfg(test)]
    pub(crate) activation_ready_pause: unsafe extern "C" fn(c_int),
}

#[derive(Debug)]
pub enum LoadError {
    CurrentExecutable(std::io::Error),
    NotFound(Vec<PathBuf>),
    Open {
        path: PathBuf,
        detail: String,
    },
    MissingBridge {
        path: PathBuf,
        detail: String,
    },
    AbiMismatch {
        expected: u32,
        actual: u32,
    },
    TableTooSmall {
        minimum: usize,
        actual: u32,
    },
    NullEntry(&'static str),
    EngineAbi {
        expected: u32,
        actual: u32,
    },
    BuildFingerprint {
        path: PathBuf,
        expected: &'static str,
        actual: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoadKind {
    Location,
    Missing,
    Open,
    Contract,
    Abi,
}

impl LoadError {
    #[must_use]
    pub const fn kind(&self) -> LoadKind {
        match self {
            Self::CurrentExecutable(_) => LoadKind::Location,
            Self::NotFound(_) => LoadKind::Missing,
            Self::Open { .. } => LoadKind::Open,
            Self::MissingBridge { .. } | Self::TableTooSmall { .. } | Self::NullEntry(_) => LoadKind::Contract,
            Self::AbiMismatch { .. } | Self::EngineAbi { .. } | Self::BuildFingerprint { .. } => LoadKind::Abi,
        }
    }
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CurrentExecutable(error) => write!(formatter, "resolve current executable: {error}"),
            Self::NotFound(paths) => write!(formatter, "native engine not found at {paths:?}"),
            Self::Open { path, detail } => write!(formatter, "open native engine {}: {detail}", path.display()),
            Self::MissingBridge { path, detail } => {
                write!(formatter, "resolve native bridge in {}: {detail}", path.display())
            }
            Self::AbiMismatch { expected, actual } => {
                write!(
                    formatter,
                    "native bridge ABI mismatch: expected {expected}, found {actual}"
                )
            }
            Self::TableTooSmall { minimum, actual } => {
                write!(
                    formatter,
                    "native bridge table is too small: need {minimum}, found {actual}"
                )
            }
            Self::NullEntry(name) => write!(formatter, "native bridge table entry {name} is null"),
            Self::EngineAbi { expected, actual } => {
                write!(
                    formatter,
                    "native engine ABI mismatch: expected {expected}, found {actual}"
                )
            }
            Self::BuildFingerprint { path, expected, actual } => {
                write!(
                    formatter,
                    "stale native engine {}: it was built from C sources fingerprinted {actual}, \
                     but this executable was built against {expected}. Rebuild, and do not trust any \
                     measurement taken from this process",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for LoadError {}

struct NativeLibrary {
    _library: DynamicLibrary,
    api: BridgeApi,
    path: PathBuf,
    #[cfg(feature = "native-test-hooks")]
    tests: TestApi,
}

static LOADED: OnceLock<Result<NativeLibrary, LoadError>> = OnceLock::new();

pub(crate) fn api() -> Result<&'static BridgeApi, &'static LoadError> {
    loaded().map(|value| &value.api)
}

pub(crate) fn path() -> Result<&'static Path, &'static LoadError> {
    loaded().map(|value| value.path.as_path())
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn tests() -> Result<&'static TestApi, &'static LoadError> {
    loaded().map(|value| &value.tests)
}

fn loaded() -> Result<&'static NativeLibrary, &'static LoadError> {
    match LOADED.get_or_init(NativeLibrary::load) {
        Ok(value) => Ok(value),
        Err(error) => Err(error),
    }
}

impl NativeLibrary {
    fn load() -> Result<Self, LoadError> {
        let candidates = candidates()?;
        let mut existing = Vec::new();
        for candidate in &candidates {
            if candidate.is_file() {
                existing.push(candidate.clone());
            }
        }
        if existing.is_empty() {
            return Err(LoadError::NotFound(candidates));
        }
        let path = existing[0].canonicalize().map_err(|error| LoadError::Open {
            path: existing[0].clone(),
            detail: error.to_string(),
        })?;
        let library = DynamicLibrary::open(&path).map_err(|detail| LoadError::Open {
            path: path.clone(),
            detail,
        })?;
        let getter_address = library
            .symbol(b"hl_c_bridge_api_v1\0")
            .map_err(|detail| LoadError::MissingBridge {
                path: path.clone(),
                detail,
            })?;
        // SAFETY: the symbol name is the versioned getter whose C declaration has this exact signature.
        let getter: BridgeGetter = unsafe { std::mem::transmute(getter_address) };
        // SAFETY: the versioned getter promises either null or a readable stable bridge header.
        let table = unsafe { read_table(getter()) }?;
        // SAFETY: validation proved the metadata entry is non-null.
        let actual = unsafe { table.engine_abi.expect("validated engine_abi")() };
        if actual != ENGINE_ABI {
            return Err(LoadError::EngineAbi {
                expected: ENGINE_ABI,
                actual,
            });
        }
        check_fingerprint(&library, &path)?;
        #[cfg(feature = "native-test-hooks")]
        let tests = TestApi::load(&library, &path)?;
        Ok(Self {
            _library: library,
            api: table,
            path,
            #[cfg(feature = "native-test-hooks")]
            tests,
        })
    }
}

/// Refuses a shared object compiled from C sources other than the ones this executable was
/// built against.
///
/// The engine is dlopened, never linked, so a Rust artifact's bytes are unchanged by most C
/// edits and "the binary did not change" proves nothing. Without this check a stale artifact
/// runs the previous engine silently, and an experiment measures the tree it was meant to
/// replace. This turns that into a load failure naming both fingerprints.
fn check_fingerprint(library: &DynamicLibrary, path: &Path) -> Result<(), LoadError> {
    const EXPECTED: &str = env!("HL_NATIVE_BUILD_FINGERPRINT");
    let address = library
        .symbol(b"hl_c_backend_build_fingerprint\0")
        .map_err(|detail| LoadError::MissingBridge {
            path: path.to_owned(),
            detail,
        })?;
    // SAFETY: the named export is declared in the bridge header with exactly this signature.
    let reader: unsafe extern "C" fn() -> *const c_char = unsafe { std::mem::transmute(address) };
    // SAFETY: the export returns a static NUL-terminated string literal from the artifact's own image.
    let actual = unsafe { std::ffi::CStr::from_ptr(reader()) };
    let actual = String::from_utf8_lossy(actual.to_bytes()).into_owned();
    if actual == EXPECTED {
        Ok(())
    } else {
        Err(LoadError::BuildFingerprint {
            path: path.to_owned(),
            expected: EXPECTED,
            actual,
        })
    }
}

unsafe fn read_table(address: *const BridgeHeader) -> Result<BridgeApi, LoadError> {
    if address.is_null() {
        return Err(LoadError::NullEntry("bridge table"));
    }
    // SAFETY: the getter contract guarantees the fixed header is readable before size is consulted.
    let header = unsafe { address.read() };
    if header.abi != BRIDGE_ABI {
        return Err(LoadError::AbiMismatch {
            expected: BRIDGE_ABI,
            actual: header.abi,
        });
    }
    if usize::try_from(header.size).unwrap_or(0) < size_of::<BridgeApi>() {
        return Err(LoadError::TableTooSmall {
            minimum: size_of::<BridgeApi>(),
            actual: header.size,
        });
    }
    // SAFETY: the validated size proves every byte of the v1 table is readable. Copying avoids
    // creating a typed reference into foreign storage before all pointer entries are validated.
    let table = unsafe { address.cast::<BridgeApi>().read() };
    table.validate()?;
    Ok(table)
}

#[cfg(feature = "native-test-hooks")]
impl TestApi {
    fn load(library: &DynamicLibrary, path: &Path) -> Result<Self, LoadError> {
        macro_rules! symbol {
            ($name:literal, $kind:ty) => {{
                let address = library
                    .symbol(concat!($name, "\0").as_bytes())
                    .map_err(|detail| LoadError::MissingBridge {
                        path: path.to_owned(),
                        detail,
                    })?;
                // SAFETY: every named test export is declared with `$kind` in the native test ABI.
                unsafe { std::mem::transmute::<*mut c_void, $kind>(address) }
            }};
        }
        Ok(Self {
            aarch64_bound_vector_io: symbol!("hl_aarch64_bound_vector_io_test", VectorIoTest),
            x86_64_bound_vector_io: symbol!("hl_x86_64_bound_vector_io_test", VectorIoTest),
            #[cfg(test)]
            aarch64_fdvis_path_publication: symbol!("hl_aarch64_fdvis_path_publication_test", ScenarioTest),
            #[cfg(test)]
            x86_64_fdvis_path_publication: symbol!("hl_x86_64_fdvis_path_publication_test", ScenarioTest),
            aarch64_namespace_transaction: symbol!("hl_aarch64_namespace_transaction_test", ScenarioTest),
            x86_64_namespace_transaction: symbol!("hl_x86_64_namespace_transaction_test", ScenarioTest),
            x86_64_store_preflight: symbol!("hl_x86_64_store_preflight_test", NoArgumentTest),
            aarch64_reserved_register: symbol!("hl_aarch64_reserved_register_test", NoArgumentTest),
            x86_64_reserved_register: symbol!("hl_x86_64_reserved_register_test", NoArgumentTest),
            aarch64_imported_path_guard: symbol!("hl_aarch64_imported_path_guard_test", NoArgumentTest),
            x86_64_imported_path_guard: symbol!("hl_x86_64_imported_path_guard_test", NoArgumentTest),
            aarch64_signal_errno_frame: symbol!("hl_aarch64_signal_errno_frame_test", SignalFrameTest),
            x86_64_signal_errno_frame: symbol!("hl_x86_64_signal_errno_frame_test", SignalFrameTest),
            aarch64_checkpoint_signal_precedence: symbol!(
                "hl_aarch64_checkpoint_signal_precedence_test",
                NoArgumentTest
            ),
            x86_64_checkpoint_signal_precedence: symbol!("hl_x86_64_checkpoint_signal_precedence_test", NoArgumentTest),
            aarch64_checkpoint_restart_register: symbol!("hl_aarch64_checkpoint_restart_register_test", NoArgumentTest),
            x86_64_checkpoint_restart_register: symbol!("hl_x86_64_checkpoint_restart_register_test", NoArgumentTest),
            aarch64_checkpoint_restore_claim: symbol!("hl_aarch64_checkpoint_restore_claim_test", ScenarioTest),
            aarch64_checkpoint_restore_slice: symbol!("hl_aarch64_checkpoint_restore_slice_test", ScenarioTest),
            x86_64_checkpoint_restore_slice: symbol!("hl_x86_64_checkpoint_restore_slice_test", ScenarioTest),
            aarch64_checkpoint_gmap_release: symbol!("hl_aarch64_checkpoint_gmap_release_test", ScenarioTest),
            x86_64_checkpoint_gmap_release: symbol!("hl_x86_64_checkpoint_gmap_release_test", ScenarioTest),
            x86_64_checkpoint_restore_claim: symbol!("hl_x86_64_checkpoint_restore_claim_test", ScenarioTest),
            aarch64_checkpoint_anon_shared: symbol!("hl_aarch64_checkpoint_anon_shared_test", ScenarioTest),
            x86_64_checkpoint_anon_shared: symbol!("hl_x86_64_checkpoint_anon_shared_test", ScenarioTest),
            aarch64_checkpoint_membership: symbol!("hl_aarch64_checkpoint_membership_test", ScenarioTest),
            x86_64_checkpoint_membership: symbol!("hl_x86_64_checkpoint_membership_test", ScenarioTest),
            aarch64_checkpoint_rendezvous: symbol!("hl_aarch64_checkpoint_rendezvous_test", ScenarioTest),
            x86_64_checkpoint_rendezvous: symbol!("hl_x86_64_checkpoint_rendezvous_test", ScenarioTest),
            aarch64_checkpoint_election: symbol!("hl_aarch64_checkpoint_election_test", ScenarioTest),
            x86_64_checkpoint_election: symbol!("hl_x86_64_checkpoint_election_test", ScenarioTest),
            aarch64_pid_namespace: symbol!("hl_aarch64_pid_namespace_test", ScenarioTest),
            x86_64_pid_namespace: symbol!("hl_x86_64_pid_namespace_test", ScenarioTest),
            aarch64_checkpoint_identity: symbol!("hl_aarch64_checkpoint_identity_test", ScenarioTest),
            x86_64_checkpoint_identity: symbol!("hl_x86_64_checkpoint_identity_test", ScenarioTest),
            aarch64_checkpoint_launch_identity: symbol!("hl_aarch64_checkpoint_launch_identity_test", ScenarioTest),
            x86_64_checkpoint_launch_identity: symbol!("hl_x86_64_checkpoint_launch_identity_test", ScenarioTest),
            aarch64_checkpoint_pipe_capture: symbol!("hl_aarch64_checkpoint_pipe_capture_test", ScenarioTest),
            x86_64_checkpoint_pipe_capture: symbol!("hl_x86_64_checkpoint_pipe_capture_test", ScenarioTest),
            aarch64_checkpoint_stdio_alias_capture: symbol!(
                "hl_aarch64_checkpoint_stdio_alias_capture_test",
                ScenarioTest
            ),
            x86_64_checkpoint_stdio_alias_capture: symbol!(
                "hl_x86_64_checkpoint_stdio_alias_capture_test",
                ScenarioTest
            ),
            aarch64_checkpoint_socket_halfclose: symbol!("hl_aarch64_checkpoint_socket_halfclose_test", ScenarioTest),
            x86_64_checkpoint_socket_halfclose: symbol!("hl_x86_64_checkpoint_socket_halfclose_test", ScenarioTest),
            aarch64_checkpoint_ipc_admission: symbol!("hl_aarch64_checkpoint_ipc_admission_test", ScenarioTest),
            x86_64_checkpoint_ipc_admission: symbol!("hl_x86_64_checkpoint_ipc_admission_test", ScenarioTest),
            aarch64_checkpoint_restore_rollback: symbol!("hl_aarch64_checkpoint_restore_rollback_test", NoArgumentTest),
            x86_64_checkpoint_restore_rollback: symbol!("hl_x86_64_checkpoint_restore_rollback_test", NoArgumentTest),
            aarch64_terminal_termios_store: symbol!("hl_aarch64_terminal_termios_store_test", NoArgumentTest),
            x86_64_terminal_termios_store: symbol!("hl_x86_64_terminal_termios_store_test", NoArgumentTest),
            aarch64_terminal_termios_install: symbol!("hl_aarch64_terminal_termios_install_test", TermiosInstallTest),
            aarch64_unix_identity: symbol!("hl_aarch64_unix_identity_test", UnixIdentityTest),
            x86_64_unix_identity: symbol!("hl_x86_64_unix_identity_test", UnixIdentityTest),
            aarch64_unix_identity_capture: symbol!("hl_aarch64_unix_identity_capture_test", UnixIdentityCaptureTest),
            x86_64_unix_identity_capture: symbol!("hl_x86_64_unix_identity_capture_test", UnixIdentityCaptureTest),
            aarch64_socket_shape: symbol!("hl_aarch64_socket_shape_test", SocketShapeTest),
            x86_64_socket_shape: symbol!("hl_x86_64_socket_shape_test", SocketShapeTest),
            errno_from_host: symbol!(
                "hl_c_backend_errno_from_host_test",
                unsafe extern "C" fn(c_uint, c_int) -> c_int
            ),
            #[cfg(target_os = "macos")]
            directory_stream_private: symbol!(
                "hl_c_backend_directory_stream_private_test",
                unsafe extern "C" fn(c_uint) -> c_int
            ),
            identity_registry: symbol!(
                "hl_c_backend_identity_registry_test",
                unsafe extern "C" fn(c_uint, c_uint) -> c_int
            ),
            private_fork_lock: symbol!(
                "hl_c_backend_private_fork_lock_test",
                unsafe extern "C" fn(c_uint) -> c_int
            ),
            aarch64_checkpoint_channel_notify: symbol!("hl_aarch64_checkpoint_channel_notify_test", ScenarioTest),
            x86_64_checkpoint_channel_notify: symbol!("hl_x86_64_checkpoint_channel_notify_test", ScenarioTest),
            aarch64_checkpoint_refusal_order: symbol!("hl_aarch64_checkpoint_refusal_order_test", NoArgumentTest),
            x86_64_checkpoint_refusal_order: symbol!("hl_x86_64_checkpoint_refusal_order_test", NoArgumentTest),
            process_identity_token: symbol!(
                "hl_c_backend_process_identity_token_test",
                unsafe extern "C" fn(c_uint) -> c_int
            ),
            setfl_append_write: symbol!(
                "hl_c_backend_setfl_append_write_test",
                unsafe extern "C" fn(c_uint) -> c_int
            ),
            checkpoint_peer_authenticate: symbol!(
                "hl_c_backend_checkpoint_peer_authenticate_test",
                unsafe extern "C" fn(c_int, u64, *mut u64, *mut u64) -> c_int
            ),
            checkpoint_channel_connect: symbol!(
                "hl_c_backend_checkpoint_channel_connect_test",
                unsafe extern "C" fn(c_int) -> c_int
            ),
            #[cfg(target_os = "macos")]
            checkpoint_process_identity_open: symbol!(
                "hl_c_backend_checkpoint_process_identity_open_test",
                unsafe extern "C" fn(c_int, u64, u64, *mut u64, *mut u64) -> c_int
            ),
            #[cfg(target_os = "macos")]
            checkpoint_peer_identity_open: symbol!(
                "hl_c_backend_checkpoint_peer_identity_open_test",
                unsafe extern "C" fn(c_int, u64, *mut u64, *mut u64, *mut u64) -> c_int
            ),
            #[cfg(test)]
            checkpoint_test_prune_foreign_descriptors: symbol!(
                "hl_c_backend_checkpoint_test_prune_foreign_descriptors",
                unsafe extern "C" fn() -> c_uint
            ),
            #[cfg(test)]
            checkpoint_test_fail_registry_allocation: symbol!(
                "hl_c_backend_checkpoint_test_fail_registry_allocation",
                unsafe extern "C" fn()
            ),
            #[cfg(test)]
            checkpoint_test_fail_private_adopt: symbol!(
                "hl_c_backend_checkpoint_test_fail_private_adopt",
                unsafe extern "C" fn(c_uint)
            ),
            #[cfg(test)]
            checkpoint_test_private_descriptor_count: symbol!(
                "hl_c_backend_checkpoint_test_private_descriptor_count",
                unsafe extern "C" fn() -> u64
            ),
            #[cfg(all(test, unix))]
            host_process_force: symbol!(
                "hl_c_backend_host_process_force_test",
                unsafe extern "C" fn(c_int) -> c_int
            ),
            #[cfg(all(test, target_os = "linux"))]
            host_process_peer_enumerated: symbol!(
                "hl_c_backend_host_process_peer_enumerated_test",
                unsafe extern "C" fn(c_int) -> c_int
            ),
            #[cfg(test)]
            activation_ready_pause: symbol!("hl_c_backend_activation_ready_pause", unsafe extern "C" fn(c_int)),
        })
    }
}

impl BridgeApi {
    fn validate(&self) -> Result<(), LoadError> {
        macro_rules! required {
        ($($field:ident),+ $(,)?) => {
            $(if self.$field.is_none() { return Err(LoadError::NullEntry(stringify!($field))); })+
        };
    }
        required!(
            engine_abi,
            engine_version,
            leak_check_nonvacuity,
            checkpoint_broker_pair,
            checkpoint_broker_accept,
            checkpoint_trigger_create,
            checkpoint_trigger_bump,
            checkpoint_trigger_destroy,
            checkpoint_adopt,
            checkpoint_interrupt_signal,
            checkpoint_configure,
            executable_open,
            executable_discard,
            create,
            run,
            request,
            exit,
            destroy,
            checkpoint_broker_accept_authenticated,
        );
        Ok(())
    }
}

#[cfg(debug_assertions)]
// The release arm of this pair genuinely fails; both cfg arms share one signature.
#[allow(clippy::unnecessary_wraps)]
fn candidates() -> Result<Vec<PathBuf>, LoadError> {
    Ok(vec![PathBuf::from(env!("HL_NATIVE_LIBRARY_PATH"))])
}

#[cfg(not(debug_assertions))]
fn candidates() -> Result<Vec<PathBuf>, LoadError> {
    let executable = crate::platform::current_executable().map_err(LoadError::CurrentExecutable)?;
    release_candidates(&executable)
}

#[cfg(any(not(debug_assertions), test))]
fn release_candidates(executable: &Path) -> Result<Vec<PathBuf>, LoadError> {
    let directory = executable.parent().ok_or_else(|| LoadError::NotFound(Vec::new()))?;
    let mut candidates = installed_candidates(directory);
    // A `--release` **test** binary lives in the Cargo target directory, which carries none of the
    // installed layouts above: `deps/`, `../lib/` and `../Frameworks/` are all absent, so the engine
    // was simply not found and every test that needs one ran against whatever fallback its subject
    // had. That cost a lane a complete measurement table. The build path is baked in at compile time
    // by `hl-native`'s build script -- it is not an ambient search, and it cannot be redirected by
    // the environment -- and it is consulted last, so an installed engine always wins where one
    // exists. On a user's machine the directory does not exist and this candidate simply misses.
    let build = PathBuf::from(env!("HL_NATIVE_LIBRARY_PATH"));
    // A planned-but-unbuilt target bakes the bare filename here, which would be resolved against the
    // working directory. That *would* be an ambient search, so only an absolute path is admitted.
    if build.is_absolute() {
        candidates.push(build);
    }
    Ok(candidates)
}

/// The installed layouts, in the order the packaged application must resolve them.
#[cfg(any(not(debug_assertions), test))]
fn installed_candidates(directory: &Path) -> Vec<PathBuf> {
    let name = env!("HL_NATIVE_LIBRARY_NAME");
    #[cfg(target_os = "macos")]
    return vec![
        directory.join("../Frameworks").join(name),
        directory.join("../lib").join(name),
        directory.join(name),
    ];
    #[cfg(target_os = "windows")]
    return vec![directory.join(name)];
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    vec![directory.join("../lib").join(name), directory.join(name)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_header_is_rejected_before_truncated_table_is_read() {
        let header = BridgeHeader {
            abi: BRIDGE_ABI,
            size: u32::try_from(size_of::<BridgeHeader>()).unwrap(),
        };
        // SAFETY: the header is readable, and its declared size deliberately forbids a full-table read.
        assert!(matches!(
            unsafe { read_table(&raw const header) },
            Err(LoadError::TableTooSmall { .. })
        ));
    }

    #[test]
    fn bridge_header_rejects_null_and_wrong_abi() {
        // SAFETY: null is an explicitly supported rejection input.
        assert!(matches!(
            unsafe { read_table(std::ptr::null()) },
            Err(LoadError::NullEntry("bridge table"))
        ));
        let header = BridgeHeader {
            abi: BRIDGE_ABI + 1,
            size: u32::try_from(size_of::<BridgeApi>()).unwrap(),
        };
        // SAFETY: ABI rejection reads only the live fixed header.
        assert!(matches!(
            unsafe { read_table(&raw const header) },
            Err(LoadError::AbiMismatch { .. })
        ));
    }

    #[test]
    fn release_locations_are_executable_relative_and_ignore_ambient_search() {
        let executable = Path::new("/opt/husklet/bin/husklet");
        let paths = release_candidates(executable).unwrap();
        let installed = installed_candidates(executable.parent().unwrap());
        #[cfg(target_os = "macos")]
        assert_eq!(
            installed,
            [
                PathBuf::from("/opt/husklet/bin/../Frameworks/libhl_native_engine.dylib"),
                PathBuf::from("/opt/husklet/bin/../lib/libhl_native_engine.dylib"),
                PathBuf::from("/opt/husklet/bin/libhl_native_engine.dylib"),
            ]
        );
        #[cfg(target_os = "windows")]
        assert_eq!(installed, [PathBuf::from("/opt/husklet/bin/hl_native_engine.dll")]);
        #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
        assert_eq!(
            installed,
            [
                PathBuf::from("/opt/husklet/bin/../lib/libhl_native_engine.so"),
                PathBuf::from("/opt/husklet/bin/libhl_native_engine.so"),
            ]
        );
        // The installed layouts still come first and in the same order: a packaged application must
        // never prefer a build directory that happens to survive beside it.
        assert_eq!(paths[..installed.len()], installed[..]);
        // Every candidate is absolute, so none of them is resolved against the working directory.
        assert!(paths.iter().all(|path| path.is_absolute()), "{paths:?}");
    }

    /// A `--release` test binary is the one caller with no installed layout anywhere above it, and
    /// before this it found no engine at all: `NotFound`, and every test that needed the engine ran
    /// against its subject's fallback while reporting nothing.
    #[test]
    fn a_release_test_binary_resolves_the_engine_its_own_build_produced() {
        let built = Path::new(env!("HL_NATIVE_LIBRARY_PATH"));
        assert!(
            built.is_absolute(),
            "this target builds an engine, so its path must be absolute: {built:?}"
        );
        let executable = built
            .parent()
            .and_then(Path::parent)
            .expect("the built engine has a containing directory")
            .join("deps")
            .join("hl_native-0123456789abcdef");
        let paths = release_candidates(&executable).unwrap();
        assert!(
            paths.contains(&built.to_path_buf()),
            "a release test binary must reach the engine this build produced: {paths:?}"
        );
        assert_eq!(
            paths.last().map(PathBuf::as_path),
            Some(built),
            "the build path is a last resort, never a preference: {paths:?}"
        );
    }
}
