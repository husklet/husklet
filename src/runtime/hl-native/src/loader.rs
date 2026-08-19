#![allow(unsafe_code)]

use crate::bindings::{Backend, EngineExit, MainImagePlan, SyscallDispatch};
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
pub(crate) type UnixIdentityTest = unsafe extern "C" fn(c_uint, c_int, u64, *mut u64, *mut u64, *mut c_uint) -> c_int;
#[cfg(feature = "native-test-hooks")]
pub(crate) type UnixIdentityCaptureTest = unsafe extern "C" fn(c_int) -> c_int;

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
}

#[derive(Clone, Copy)]
#[repr(C)]
struct BridgeHeader {
    abi: u32,
    size: u32,
}

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
    pub(crate) aarch64_signal_errno_frame: SignalFrameTest,
    pub(crate) x86_64_signal_errno_frame: SignalFrameTest,
    pub(crate) aarch64_checkpoint_signal_precedence: NoArgumentTest,
    pub(crate) x86_64_checkpoint_signal_precedence: NoArgumentTest,
    pub(crate) aarch64_checkpoint_restart_register: NoArgumentTest,
    pub(crate) x86_64_checkpoint_restart_register: NoArgumentTest,
    pub(crate) aarch64_checkpoint_restore_claim: ScenarioTest,
    pub(crate) x86_64_checkpoint_restore_claim: ScenarioTest,
    pub(crate) aarch64_checkpoint_restore_rollback: NoArgumentTest,
    pub(crate) x86_64_checkpoint_restore_rollback: NoArgumentTest,
    pub(crate) aarch64_unix_identity: UnixIdentityTest,
    pub(crate) x86_64_unix_identity: UnixIdentityTest,
    pub(crate) aarch64_unix_identity_capture: UnixIdentityCaptureTest,
    pub(crate) x86_64_unix_identity_capture: UnixIdentityCaptureTest,
    pub(crate) errno_from_host: unsafe extern "C" fn(c_uint, c_int) -> c_int,
    pub(crate) identity_registry: unsafe extern "C" fn(c_uint, c_uint) -> c_int,
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
    #[cfg(test)]
    pub(crate) activation_ready_pause: unsafe extern "C" fn(c_int),
}

#[derive(Debug)]
pub enum LoadError {
    CurrentExecutable(std::io::Error),
    NotFound(Vec<PathBuf>),
    Open { path: PathBuf, detail: String },
    MissingBridge { path: PathBuf, detail: String },
    AbiMismatch { expected: u32, actual: u32 },
    TableTooSmall { minimum: usize, actual: u32 },
    NullEntry(&'static str),
    EngineAbi { expected: u32, actual: u32 },
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
            Self::AbiMismatch { .. } | Self::EngineAbi { .. } => LoadKind::Abi,
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
        }
    }
}

impl std::error::Error for LoadError {}

struct Loaded {
    _library: DynamicLibrary,
    api: BridgeApi,
    path: PathBuf,
    #[cfg(feature = "native-test-hooks")]
    tests: TestApi,
}

static LOADED: OnceLock<Result<Loaded, LoadError>> = OnceLock::new();

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

fn loaded() -> Result<&'static Loaded, &'static LoadError> {
    match LOADED.get_or_init(load) {
        Ok(value) => Ok(value),
        Err(error) => Err(error),
    }
}

fn load() -> Result<Loaded, LoadError> {
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
    type Getter = unsafe extern "C" fn() -> *const BridgeHeader;
    // SAFETY: the symbol name is the versioned getter whose C declaration has this exact signature.
    let getter: Getter = unsafe { std::mem::transmute(getter_address) };
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
    #[cfg(feature = "native-test-hooks")]
    let tests = load_tests(&library, &path)?;
    Ok(Loaded {
        _library: library,
        api: table,
        path,
        #[cfg(feature = "native-test-hooks")]
        tests,
    })
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
    validate(&table)?;
    Ok(table)
}

#[cfg(feature = "native-test-hooks")]
fn load_tests(library: &DynamicLibrary, path: &Path) -> Result<TestApi, LoadError> {
    macro_rules! symbol {
        ($name:literal, $kind:ty) => {{
            let address =
                library
                    .symbol(concat!($name, "\0").as_bytes())
                    .map_err(|detail| LoadError::MissingBridge {
                        path: path.to_owned(),
                        detail,
                    })?;
            // SAFETY: every named test export is declared with `$kind` in the native test ABI.
            unsafe { std::mem::transmute::<*mut c_void, $kind>(address) }
        }};
    }
    Ok(TestApi {
        aarch64_bound_vector_io: symbol!("hl_aarch64_bound_vector_io_test", VectorIoTest),
        x86_64_bound_vector_io: symbol!("hl_x86_64_bound_vector_io_test", VectorIoTest),
        #[cfg(test)]
        aarch64_fdvis_path_publication: symbol!("hl_aarch64_fdvis_path_publication_test", ScenarioTest),
        #[cfg(test)]
        x86_64_fdvis_path_publication: symbol!("hl_x86_64_fdvis_path_publication_test", ScenarioTest),
        aarch64_namespace_transaction: symbol!("hl_aarch64_namespace_transaction_test", ScenarioTest),
        x86_64_namespace_transaction: symbol!("hl_x86_64_namespace_transaction_test", ScenarioTest),
        x86_64_store_preflight: symbol!("hl_x86_64_store_preflight_test", NoArgumentTest),
        aarch64_signal_errno_frame: symbol!("hl_aarch64_signal_errno_frame_test", SignalFrameTest),
        x86_64_signal_errno_frame: symbol!("hl_x86_64_signal_errno_frame_test", SignalFrameTest),
        aarch64_checkpoint_signal_precedence: symbol!("hl_aarch64_checkpoint_signal_precedence_test", NoArgumentTest),
        x86_64_checkpoint_signal_precedence: symbol!("hl_x86_64_checkpoint_signal_precedence_test", NoArgumentTest),
        aarch64_checkpoint_restart_register: symbol!("hl_aarch64_checkpoint_restart_register_test", NoArgumentTest),
        x86_64_checkpoint_restart_register: symbol!("hl_x86_64_checkpoint_restart_register_test", NoArgumentTest),
        aarch64_checkpoint_restore_claim: symbol!("hl_aarch64_checkpoint_restore_claim_test", ScenarioTest),
        x86_64_checkpoint_restore_claim: symbol!("hl_x86_64_checkpoint_restore_claim_test", ScenarioTest),
        aarch64_checkpoint_restore_rollback: symbol!("hl_aarch64_checkpoint_restore_rollback_test", NoArgumentTest),
        x86_64_checkpoint_restore_rollback: symbol!("hl_x86_64_checkpoint_restore_rollback_test", NoArgumentTest),
        aarch64_unix_identity: symbol!("hl_aarch64_unix_identity_test", UnixIdentityTest),
        x86_64_unix_identity: symbol!("hl_x86_64_unix_identity_test", UnixIdentityTest),
        aarch64_unix_identity_capture: symbol!("hl_aarch64_unix_identity_capture_test", UnixIdentityCaptureTest),
        x86_64_unix_identity_capture: symbol!("hl_x86_64_unix_identity_capture_test", UnixIdentityCaptureTest),
        errno_from_host: symbol!(
            "hl_c_backend_errno_from_host_test",
            unsafe extern "C" fn(c_uint, c_int) -> c_int
        ),
        identity_registry: symbol!(
            "hl_c_backend_identity_registry_test",
            unsafe extern "C" fn(c_uint, c_uint) -> c_int
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
        #[cfg(test)]
        activation_ready_pause: symbol!("hl_c_backend_activation_ready_pause", unsafe extern "C" fn(c_int)),
    })
}

fn validate(table: &BridgeApi) -> Result<(), LoadError> {
    macro_rules! required {
        ($($field:ident),+ $(,)?) => {
            $(if table.$field.is_none() { return Err(LoadError::NullEntry(stringify!($field))); })+
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

#[cfg(debug_assertions)]
fn candidates() -> Result<Vec<PathBuf>, LoadError> {
    Ok(vec![PathBuf::from(env!("HL_NATIVE_LIBRARY_PATH"))])
}

#[cfg(not(debug_assertions))]
fn candidates() -> Result<Vec<PathBuf>, LoadError> {
    let executable = std::env::current_exe().map_err(LoadError::CurrentExecutable)?;
    release_candidates(&executable)
}

#[cfg(any(not(debug_assertions), test))]
fn release_candidates(executable: &Path) -> Result<Vec<PathBuf>, LoadError> {
    let directory = executable.parent().ok_or_else(|| LoadError::NotFound(Vec::new()))?;
    let name = env!("HL_NATIVE_LIBRARY_NAME");
    #[cfg(target_os = "macos")]
    return Ok(vec![
        directory.join("../Frameworks").join(name),
        directory.join("../lib").join(name),
        directory.join(name),
    ]);
    #[cfg(target_os = "windows")]
    return Ok(vec![directory.join(name)]);
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    Ok(vec![directory.join("../lib").join(name), directory.join(name)])
}

struct DynamicLibrary(*mut c_void);

// SAFETY: the handle is immutable after construction, and symbol lookup is serialized by OnceLock.
unsafe impl Send for DynamicLibrary {}
// SAFETY: the platform loaders permit concurrent calls through resolved immutable function pointers.
unsafe impl Sync for DynamicLibrary {}

#[cfg(unix)]
impl Drop for DynamicLibrary {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: this handle was returned by dlopen and is dropped at most once.
            unsafe { dlclose(self.0) };
        }
    }
}

#[cfg(windows)]
impl Drop for DynamicLibrary {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: this handle was returned by LoadLibraryExW and is dropped at most once.
            unsafe { FreeLibrary(self.0) };
        }
    }
}

#[cfg(unix)]
impl DynamicLibrary {
    fn open(path: &Path) -> Result<Self, String> {
        use std::os::unix::ffi::OsStrExt as _;
        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|error| error.to_string())?;
        // SAFETY: path is NUL-terminated and flags request immediate, local binding.
        let handle = unsafe { dlopen(path.as_ptr(), RTLD_NOW | RTLD_LOCAL) };
        if handle.is_null() {
            Err(dynamic_error())
        } else {
            Ok(Self(handle))
        }
    }

    fn symbol(&self, name: &'static [u8]) -> Result<*mut c_void, String> {
        // SAFETY: names are static NUL-terminated byte strings and the handle remains live.
        let address = unsafe { dlsym(self.0, name.as_ptr().cast()) };
        if address.is_null() {
            Err(dynamic_error())
        } else {
            Ok(address)
        }
    }
}

#[cfg(unix)]
fn dynamic_error() -> String {
    // SAFETY: dlerror returns either null or a thread-local NUL-terminated message.
    let message = unsafe { dlerror() };
    if message.is_null() {
        "dynamic loader did not report an error".to_owned()
    } else {
        // SAFETY: a non-null dlerror result is a NUL-terminated string valid until the next loader call.
        unsafe { std::ffi::CStr::from_ptr(message) }
            .to_string_lossy()
            .into_owned()
    }
}

#[cfg(unix)]
const RTLD_LOCAL: c_int = 0;
#[cfg(unix)]
const RTLD_NOW: c_int = 2;

#[cfg(unix)]
unsafe extern "C" {
    fn dlopen(path: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    fn dlerror() -> *const c_char;
    fn dlclose(handle: *mut c_void) -> c_int;
}

#[cfg(windows)]
impl DynamicLibrary {
    fn open(path: &Path) -> Result<Self, String> {
        use std::os::windows::ffi::OsStrExt as _;
        let mut path = path.as_os_str().encode_wide().collect::<Vec<_>>();
        path.push(0);
        // SAFETY: the path is absolute and NUL-terminated; flags restrict dependency lookup to the DLL
        // directory and System32, never the current directory or ambient PATH.
        let handle = unsafe {
            LoadLibraryExW(
                path.as_ptr(),
                std::ptr::null_mut(),
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        };
        if handle.is_null() {
            Err(format!("Windows loader error {}", unsafe { GetLastError() }))
        } else {
            Ok(Self(handle))
        }
    }

    fn symbol(&self, name: &'static [u8]) -> Result<*mut c_void, String> {
        // SAFETY: the symbol name is static and NUL-terminated and the module remains live.
        let address = unsafe { GetProcAddress(self.0, name.as_ptr().cast()) };
        if address.is_null() {
            Err(format!("Windows loader error {}", unsafe { GetLastError() }))
        } else {
            Ok(address)
        }
    }
}

#[cfg(windows)]
const LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR: u32 = 0x0000_0100;
#[cfg(windows)]
const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x0000_0800;

#[cfg(windows)]
unsafe extern "system" {
    fn LoadLibraryExW(path: *const u16, file: *mut c_void, flags: u32) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
    fn GetLastError() -> u32;
    fn FreeLibrary(module: *mut c_void) -> i32;
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
        #[cfg(target_os = "macos")]
        assert_eq!(
            paths,
            [
                PathBuf::from("/opt/husklet/bin/../Frameworks/libhl_native_engine.dylib"),
                PathBuf::from("/opt/husklet/bin/../lib/libhl_native_engine.dylib"),
                PathBuf::from("/opt/husklet/bin/libhl_native_engine.dylib"),
            ]
        );
        #[cfg(target_os = "windows")]
        assert_eq!(paths, [PathBuf::from("/opt/husklet/bin/hl_native_engine.dll")]);
        #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
        assert_eq!(
            paths,
            [
                PathBuf::from("/opt/husklet/bin/../lib/libhl_native_engine.so"),
                PathBuf::from("/opt/husklet/bin/libhl_native_engine.so"),
            ]
        );
    }
}
