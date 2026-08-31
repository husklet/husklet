#![allow(unsafe_code)]

#[cfg(unix)]
use crate::composition::{CheckpointSink, CheckpointSource};
use crate::composition::{CompositionError, GuestMachine, RuntimeConstruction, RuntimeFactory};
use crate::engine::{EngineError, EngineExit, ExitKind, StopRequest};
use std::ffi::CString;
use std::sync::{Arc, Mutex};

struct BoxProjection {
    raw: hl_native::EngineBoxConfig,
    _strings: [Option<CString>; 13],
    _publish: Vec<hl_native::EnginePublishRule>,
    _network_names: Vec<CString>,
    _network_interfaces: Vec<hl_native::EngineNetworkInterface>,
}

impl BoxProjection {
    fn new(policy: &crate::launcher::plan::RuntimeBoxPolicy) -> Result<Self, EngineError> {
        let strings: [Option<CString>; 13] = [
            &policy.working_directory,
            &policy.hostname,
            &policy.environment,
            &policy.lower_layers,
            &policy.volumes,
            &policy.limits,
            &policy.network_namespace,
            &policy.translation_cache,
            &policy.network_bridge,
            &policy.ip,
            &policy.filesystem_generation,
            &policy.egress_proxy,
            &policy.file_owners,
        ]
        .map(|value| value.as_deref().map(CString::new).transpose())
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| EngineError::LaunchFailed)?
        .try_into()
        .map_err(|_| EngineError::LaunchFailed)?;
        let publish = policy
            .publish
            .iter()
            .map(|rule| hl_native::EnginePublishRule {
                host_ipv4_be: rule.host_ipv4_be,
                host_port: rule.host_port,
                guest_port: rule.guest_port,
            })
            .collect::<Vec<_>>();
        let network_names = policy
            .network_interfaces
            .iter()
            .map(|interface| CString::new(interface.bridge.clone()).map_err(|_| EngineError::LaunchFailed))
            .collect::<Result<Vec<_>, _>>()?;
        let network_interfaces = policy
            .network_interfaces
            .iter()
            .zip(&network_names)
            .map(|(interface, bridge)| hl_native::EngineNetworkInterface {
                bridge: bridge.as_ptr(),
                address_ipv4_be: interface.address_ipv4_be,
                gateway_ipv4_be: interface.gateway_ipv4_be,
                prefix: interface.prefix,
                reserved: [0; 3],
            })
            .collect::<Vec<_>>();
        let pointer = |index: usize| strings[index].as_ref().map_or(std::ptr::null(), |value| value.as_ptr());
        let raw = hl_native::EngineBoxConfig {
            abi: 2,
            size: std::mem::size_of::<hl_native::EngineBoxConfig>() as u32,
            flags: policy.flags,
            uid: policy.uid,
            gid: policy.gid,
            reserved: 0,
            working_directory: pointer(0),
            hostname: pointer(1),
            environment: pointer(2),
            lower_layers: pointer(3),
            publish: if publish.is_empty() {
                std::ptr::null()
            } else {
                publish.as_ptr()
            },
            publish_count: publish.len().try_into().map_err(|_| EngineError::LaunchFailed)?,
            volumes: pointer(4),
            limits: pointer(5),
            network_namespace: pointer(6),
            translation_cache: pointer(7),
            network_bridge: pointer(8),
            ip: pointer(9),
            filesystem_generation: pointer(10),
            egress_proxy: pointer(11),
            file_owners: pointer(12),
            checkpoint_mode: policy.checkpoint_mode,
            checkpoint_policy: policy.checkpoint_policy,
            network_mode: policy.network_mode,
            network_interface_count: network_interfaces
                .len()
                .try_into()
                .map_err(|_| EngineError::LaunchFailed)?,
            network_interfaces: if network_interfaces.is_empty() {
                std::ptr::null()
            } else {
                network_interfaces.as_ptr()
            },
        };
        Ok(Self {
            raw,
            _strings: strings,
            _publish: publish,
            _network_names: network_names,
            _network_interfaces: network_interfaces,
        })
    }
}

#[cfg(unix)]
#[path = "execution_checkpoint.rs"]
mod checkpoint;
#[cfg(all(unix, test))]
pub(crate) use checkpoint::await_capture_completion;
#[cfg(unix)]
use checkpoint::{CheckpointControl, run_with_recovery};

#[cfg(unix)]
use super::checkpoint::Server;

#[cfg(unix)]
use super::terminal::NativeOutputBridge;
#[cfg(unix)]
use super::terminal::{InputDiscipline, NativeTerminalBridge};

const REQUEST_INTERRUPT: u32 = 1;
const REQUEST_FORCE_STOP: u32 = 2;
const REQUEST_SIGNAL: u32 = 3;
#[cfg(unix)]
const REQUEST_CHECKPOINT: u32 = 4;

fn checkpoint_sandbox_refusal(options: &crate::options::Options) -> Option<EngineError> {
    options
        .get_bytes("HL_UNTRUSTED")
        .map(|_| EngineError::CheckpointUnsupportedUnderSandbox)
}

fn native_run_failure(status: i32) -> EngineError {
    EngineError::NativeRunFailed(status)
}

pub(crate) struct ProductionMachine {
    isa: crate::activation::GuestIsa,
    plan: crate::launcher::plan::RuntimePlan,
    native_supervised: bool,
    #[cfg(unix)]
    terminal: Option<NativeTerminalBridge>,
    #[cfg(unix)]
    output: Option<NativeOutputBridge>,
    #[cfg(unix)]
    checkpoint: Option<CheckpointControl>,
    /// Set instead of `checkpoint` when this machine joins a domain freeze it does
    /// not coordinate. A member has no `Server`, so it has no channel to publish an
    /// image of its own on; its guest processes commit into the coordinator's store.
    #[cfg(unix)]
    member: Option<crate::composition::CheckpointChannel>,
    state: Mutex<StartupState<hl_native::Engine>>,
}

struct StartupState<T> {
    engine: Option<Arc<T>>,
    pending_stop: Option<StopRequest>,
}

impl<T> Default for StartupState<T> {
    fn default() -> Self {
        Self {
            engine: None,
            pending_stop: None,
        }
    }
}

impl<T> StartupState<T> {
    fn publish(&mut self, engine: Arc<T>) -> Option<StopRequest> {
        self.engine = Some(engine);
        self.pending_stop.take()
    }

    fn request(&mut self, request: StopRequest) -> Option<Arc<T>> {
        let Some(engine) = self.engine.as_ref() else {
            self.pending_stop = Some(match (self.pending_stop, request) {
                (Some(StopRequest::Force), _) | (_, StopRequest::Force) => StopRequest::Force,
                (_, request) => request,
            });
            return None;
        };
        Some(Arc::clone(engine))
    }

    fn retained(&self) -> Option<Arc<T>> {
        self.engine.as_ref().map(Arc::clone)
    }

    fn discard_if(&mut self, engine: &Arc<T>) {
        if self.engine.as_ref().is_some_and(|current| Arc::ptr_eq(current, engine)) {
            self.engine = None;
        }
    }
}

#[cfg(test)]
mod startup_state_tests {
    use super::*;

    #[test]
    fn a_stop_during_native_construction_is_delivered_when_the_engine_publishes() {
        let mut state = StartupState::<()>::default();
        assert!(state.request(StopRequest::Interrupt).is_none());
        assert!(state.request(StopRequest::Force).is_none());
        assert!(state.request(StopRequest::Signal(12)).is_none());

        let engine = Arc::new(());
        assert_eq!(state.publish(Arc::clone(&engine)), Some(StopRequest::Force));
        assert!(Arc::ptr_eq(&state.request(StopRequest::Signal(10)).unwrap(), &engine));
    }
}

pub(crate) struct ProductionFactory;

const BOX_ROOTFS_READ_ONLY: u32 = 1;
const BOX_NETWORK_ISOLATED: u32 = 1 << 2;
const BOX_TRANSLATION_CACHE_DISABLED: u32 = 1 << 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeSupervisedRefusal {
    Host,
    Kernel,
    GuestIsa,
    Executable,
    Root,
    Identity,
    Cgroup,
    Overlay,
    Ownership,
    Volumes,
    Network,
    Checkpoint,
    Sandbox,
    Seccomp,
    BackendControl,
    BoxFlags,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NativeSupervisedRequest {
    Auto,
    On,
    Off,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NativeCheckpointIntent {
    None,
    FreshCoordinator,
    DomainMember,
    Restore,
    Partial,
}

fn native_checkpoint_intent(
    has_sink: bool,
    has_source: bool,
    has_channel: bool,
    restore: bool,
) -> NativeCheckpointIntent {
    match (has_sink, has_source, has_channel, restore) {
        (false, false, false, false) => NativeCheckpointIntent::None,
        (true, true, false, false) => NativeCheckpointIntent::FreshCoordinator,
        (false, false, true, false) => NativeCheckpointIntent::DomainMember,
        (true, true, false, true) => NativeCheckpointIntent::Restore,
        _ => NativeCheckpointIntent::Partial,
    }
}

#[derive(Clone, Copy, Debug)]
struct NativeHostCapabilities {
    linux_x86_64: bool,
    root: bool,
    clone3: bool,
    pidfd_getfd: bool,
    seccomp_notify: bool,
    executable_x86_64: bool,
    rootfs_directory: bool,
    isolated_hostname_projection: bool,
}

#[cfg(target_os = "linux")]
fn syscall_has_errno(number: libc::c_long, arguments: [libc::c_long; 3], accepted: &[i32]) -> bool {
    // SAFETY: capability probes use invalid scalar arguments and are required to fail without changing state.
    let result = unsafe { libc::syscall(number, arguments[0], arguments[1], arguments[2]) };
    result >= 0 || std::io::Error::last_os_error().raw_os_error().is_some_and(|error| accepted.contains(&error))
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn x86_64_executable_header(regular: bool, header: Option<[u8; 20]>) -> bool {
    let Some(header) = header else { return false };
    regular
        && header[..6] == [0x7f, b'E', b'L', b'F', 2, 1]
        && u16::from_le_bytes([header[18], header[19]]) == 62
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn executable_is_x86_64(path: Option<&[u8]>) -> bool {
    use std::os::unix::fs::{FileExt, OpenOptionsExt};
    use std::os::unix::ffi::OsStrExt;
    let Some(path) = path else { return false };
    let mut header = [0_u8; 20];
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(std::ffi::OsStr::from_bytes(path));
    let Ok(file) = file else { return false };
    let regular = file.metadata().is_ok_and(|metadata| metadata.is_file());
    let header = file.read_exact_at(&mut header, 0).is_ok().then_some(header);
    x86_64_executable_header(regular, header)
}

fn hostname_valid(hostname: &[u8]) -> bool {
    !hostname.is_empty()
        && hostname.len() <= 64
        && hostname.iter().enumerate().all(|(index, byte)| {
            let alphanumeric = byte.is_ascii_alphanumeric();
            alphanumeric
                || (*byte == b'-'
                    && index != 0
                    && index + 1 != hostname.len()
                    && hostname[index - 1] != b'.'
                    && hostname[index + 1] != b'.')
                || (*byte == b'.'
                    && index != 0
                    && index + 1 != hostname.len()
                    && hostname[index - 1] != b'.'
                    && hostname[index - 1] != b'-')
        })
}

#[cfg(unix)]
fn isolated_hostname_projection_ready(plan: &crate::launcher::plan::RuntimePlan) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let (Some(root), Some(hostname)) = (plan.rootfs.as_deref(), plan.box_policy.hostname.as_deref()) else {
        return false;
    };
    if !hostname_valid(hostname) { return false; }
    let root = std::path::PathBuf::from(std::ffi::OsStr::from_bytes(root));
    if !std::fs::symlink_metadata(&root).is_ok_and(|metadata| metadata.file_type().is_dir()) {
        return false;
    }
    let etc = root.join("etc");
    if !std::fs::symlink_metadata(&etc).is_ok_and(|metadata| metadata.file_type().is_dir()) {
        return false;
    }
    let hosts = etc.join("hosts");
    std::fs::symlink_metadata(hosts)
        .is_ok_and(|metadata| metadata.file_type().is_file() && metadata.len() <= 1024 * 1024)
}

#[cfg(not(unix))]
fn isolated_hostname_projection_ready(_plan: &crate::launcher::plan::RuntimePlan) -> bool { false }

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
fn executable_is_x86_64(_path: Option<&[u8]>) -> bool { false }

fn native_host_capabilities(plan: &crate::launcher::plan::RuntimePlan) -> NativeHostCapabilities {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        // GET_NOTIF_SIZES is observational; invalid clone3/pidfd_getfd arguments prove syscall presence.
        let mut sizes = [0_u16; 3];
        // SAFETY: the kernel writes exactly `seccomp_notif_sizes` (three u16 fields) for operation 3.
        let seccomp_notify = unsafe {
            libc::syscall(libc::SYS_seccomp, 3, 0, sizes.as_mut_ptr()) == 0
        };
        NativeHostCapabilities {
            linux_x86_64: true,
            // SAFETY: identity queries have no side effects.
            root: unsafe { libc::geteuid() == 0 && libc::getegid() == 0 },
            clone3: syscall_has_errno(libc::SYS_clone3, [0, 0, 0], &[libc::EFAULT, libc::EINVAL]),
            pidfd_getfd: syscall_has_errno(libc::SYS_pidfd_getfd, [-1, -1, 0], &[libc::EBADF]),
            seccomp_notify,
            executable_x86_64: executable_is_x86_64(plan.executable_host.as_deref()),
            rootfs_directory: plan.rootfs.as_deref().is_some_and(|path| {
                use std::os::unix::ffi::OsStrExt;
                std::fs::metadata(std::ffi::OsStr::from_bytes(path)).is_ok_and(|metadata| metadata.is_dir())
            }),
            isolated_hostname_projection: isolated_hostname_projection_ready(plan),
        }
    }
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    {
        let _ = plan;
        NativeHostCapabilities {
            linux_x86_64: false,
            root: false,
            clone3: false,
            pidfd_getfd: false,
            seccomp_notify: false,
            executable_x86_64: false,
            rootfs_directory: false,
            isolated_hostname_projection: false,
        }
    }
}

fn native_request(options: &crate::options::Options) -> NativeSupervisedRequest {
    match options.get_bytes("HL_NATIVE_SUPERVISED") {
        None => NativeSupervisedRequest::Auto,
        Some(b"0" | b"off") => NativeSupervisedRequest::Off,
        Some(_) => NativeSupervisedRequest::On,
    }
}

fn native_eligibility_for_request(
    requested: NativeSupervisedRequest,
    isa: crate::activation::GuestIsa,
    plan: &crate::launcher::plan::RuntimePlan,
    checkpoint: NativeCheckpointIntent,
    probe: impl FnOnce() -> NativeHostCapabilities,
) -> Result<(), NativeSupervisedRefusal> {
    if requested == NativeSupervisedRequest::Off {
        return Err(NativeSupervisedRefusal::Host);
    }
    let host = probe();
    let eligibility = native_eligibility(isa, plan, checkpoint, host);
    if requested == NativeSupervisedRequest::Auto {
        native_auto_eligibility(plan, host, eligibility)
    } else {
        eligibility
    }
}

fn valid_guest_path(path: &[u8]) -> bool {
    path.starts_with(b"/")
        && path.len() > 1
        && path[1..]
            .split(|byte| *byte == b'/')
            .all(|part| !part.is_empty() && part != b"." && part != b"..")
}

fn volume_spec_supported(spec: Option<&[u8]>) -> bool {
    let Some(spec) = spec else { return true };
    let mut guests: Vec<&[u8]> = Vec::new();
    for raw in spec.split(|byte| *byte == b',') {
        if raw.is_empty() || guests.len() == 32 { return false; }
        let record = raw.strip_prefix(b"ro:").or_else(|| raw.strip_prefix(b"rw:")).unwrap_or(raw);
        let Some(split) = record.iter().position(|byte| *byte == b':') else { return false };
        let (guest, host) = (&record[..split], &record[split + 1..]);
        if !valid_guest_path(guest) || guest == b"/proc" || guest.starts_with(b"/proc/") ||
            !host.starts_with(b"/") || host.contains(&b':') || guest.len() >= 4096 {
            return false;
        }
        let overlaps = guests.iter().any(|prior| {
            (guest.starts_with(prior) && (guest.len() == prior.len() || guest[prior.len()] == b'/')) ||
            (prior.starts_with(guest) && (prior.len() == guest.len() || prior[guest.len()] == b'/'))
        });
        if overlaps { return false; }
        guests.push(guest);
    }
    true
}

fn translated_backend_control(plan: &crate::launcher::plan::RuntimePlan) -> Option<&'static str> {
    if plan.box_policy.translation_cache.is_some() {
        return Some("translation-cache-policy");
    }
    plan.options.iter().find_map(|(name, _)| {
        (name == "HL_PCACHE"
            || name == "HL_PCACHE_DIR"
            || name.starts_with("HL_TRANSLIT"))
        .then_some(name)
    })
}

fn native_eligibility(
    isa: crate::activation::GuestIsa,
    plan: &crate::launcher::plan::RuntimePlan,
    checkpoint: NativeCheckpointIntent,
    host: NativeHostCapabilities,
) -> Result<(), NativeSupervisedRefusal> {
    use NativeSupervisedRefusal as R;
    if !host.linux_x86_64 || !host.root { return Err(R::Host); }
    if !host.clone3 || !host.pidfd_getfd || !host.seccomp_notify { return Err(R::Kernel); }
    if isa != crate::activation::GuestIsa::X86_64 { return Err(R::GuestIsa); }
    if !host.executable_x86_64 { return Err(R::Executable); }
    if plan.rootfs.is_none() || plan.executable_host.is_none() || !host.rootfs_directory { return Err(R::Root); }
    let box_policy = &plan.box_policy;
    if box_policy.uid < -1 || box_policy.gid < -1 { return Err(R::Identity); }
    if plan.options.get_bytes("HL_MEM_MAX").is_some() || plan.options.get_bytes("HL_PIDS_MAX").is_some() ||
        plan.options.get_bytes("HL_CPUS").is_some() {
        return Err(R::Cgroup);
    }
    if box_policy.lower_layers.as_deref().is_some_and(|layers| layers.contains(&b'\n')) {
        return Err(R::Overlay);
    }
    if box_policy.file_owners.is_some() && box_policy.lower_layers.is_none() { return Err(R::Ownership); }
    if !volume_spec_supported(box_policy.volumes.as_deref()) { return Err(R::Volumes); }
    // Keep every configured checkpoint role translated until native late-capture/member lifecycle
    // fixtures prove the shared trigger across a real product plan. The typed split prevents a future
    // proof for FreshCoordinator from accidentally admitting Restore or malformed partial services.
    if checkpoint != NativeCheckpointIntent::None
        || box_policy.checkpoint_mode != 0
        || box_policy.checkpoint_policy != 0
        || plan.options.get_bytes("HL_CHECKPOINT").is_some()
        || plan.options.get_bytes("HL_RESTORE").is_some()
    {
        return Err(R::Checkpoint);
    }
    if plan.options.get_bytes("HL_UNTRUSTED").is_some() { return Err(R::Sandbox); }
    if plan.options.get_bytes("HL_SECCOMP_BASELINE").is_some() { return Err(R::Seccomp); }
    if translated_backend_control(plan).is_some() { return Err(R::BackendControl); }
    let isolated = box_policy.flags & BOX_NETWORK_ISOLATED != 0;
    let supported_network = match box_policy.network_mode {
        // Isolated launches always receive a fresh netns. The typed namespace is its process-domain
        // identity, not authority to join an existing host namespace, so retaining it is harmless.
        0 => isolated,
        2 => !isolated && box_policy.network_namespace.is_none(),
        _ => false,
    };
    if !supported_network || !box_policy.publish.is_empty() || !box_policy.network_interfaces.is_empty() ||
        box_policy.network_bridge.is_some() || box_policy.ip.is_some() || box_policy.egress_proxy.is_some() {
        return Err(R::Network);
    }
    let allowed = BOX_ROOTFS_READ_ONLY | BOX_NETWORK_ISOLATED | BOX_TRANSLATION_CACHE_DISABLED;
    if box_policy.flags & !allowed != 0 { return Err(R::BoxFlags); }
    Ok(())
}

fn native_selection(
    request: NativeSupervisedRequest,
    eligibility: Result<(), NativeSupervisedRefusal>,
) -> Result<bool, CompositionError> {
    match (request, eligibility) {
        (NativeSupervisedRequest::Off, _) => Ok(false),
        (NativeSupervisedRequest::Auto, Ok(())) | (NativeSupervisedRequest::On, Ok(())) => Ok(true),
        (NativeSupervisedRequest::Auto, Err(_)) => Ok(false),
        (NativeSupervisedRequest::On, Err(reason)) => Err(CompositionError::NativeSupervisedRefused(reason)),
    }
}

fn native_auto_eligibility(
    plan: &crate::launcher::plan::RuntimePlan,
    host: NativeHostCapabilities,
    mut eligibility: Result<(), NativeSupervisedRefusal>,
) -> Result<(), NativeSupervisedRefusal> {
    // Diagnostics are implemented by both backends, so explicit native ON keeps them. AUTO must not
    // silently change which diagnostic family an absent backend selection would have produced.
    if plan.options.get_bytes("HL_C_DIAGNOSTICS").is_some() {
        eligibility = Err(NativeSupervisedRefusal::BackendControl);
    }
    if plan.box_policy.volumes.is_some() { eligibility = Err(NativeSupervisedRefusal::Volumes); }
    if plan.box_policy.lower_layers.is_some() || plan.box_policy.file_owners.is_some() {
        eligibility = Err(NativeSupervisedRefusal::Overlay);
    }
    let isolated_ready = plan.box_policy.network_mode == 0
        && plan.box_policy.flags & BOX_NETWORK_ISOLATED != 0
        && host.isolated_hostname_projection;
    if plan.box_policy.network_mode != 2 && !isolated_ready {
        eligibility = Err(NativeSupervisedRefusal::Network);
    }
    eligibility
}

#[cfg(test)]
mod native_eligibility_tests {
    use super::*;

    fn host() -> NativeHostCapabilities {
        NativeHostCapabilities {
            linux_x86_64: true,
            root: true,
            clone3: true,
            pidfd_getfd: true,
            seccomp_notify: true,
            executable_x86_64: true,
            rootfs_directory: true,
            isolated_hostname_projection: true,
        }
    }

    fn plan() -> crate::launcher::plan::RuntimePlan {
        let mut box_policy = crate::launcher::plan::RuntimeBoxPolicy::default();
        box_policy.network_mode = 2;
        crate::launcher::plan::RuntimePlan {
            rootfs: Some(b"/root".to_vec()),
            executable_host: Some(b"/root/bin/true".to_vec()),
            arguments: vec![b"true".to_vec()],
            environment: Vec::new(),
            result_path: None,
            options: crate::options::Options::default(),
            box_policy,
        }
    }

    fn verdict(plan: &crate::launcher::plan::RuntimePlan, host: NativeHostCapabilities) -> Result<(), NativeSupervisedRefusal> {
        native_eligibility(crate::activation::GuestIsa::X86_64, plan, NativeCheckpointIntent::None, host)
    }

    #[test]
    fn pure_eligibility_names_every_refusal_class() {
        assert_eq!(verdict(&plan(), host()), Ok(()));

        let mut changed = host(); changed.root = false;
        assert_eq!(verdict(&plan(), changed), Err(NativeSupervisedRefusal::Host));
        let mut changed = host(); changed.clone3 = false;
        assert_eq!(verdict(&plan(), changed), Err(NativeSupervisedRefusal::Kernel));
        assert_eq!(native_eligibility(crate::activation::GuestIsa::Aarch64, &plan(), NativeCheckpointIntent::None, host()), Err(NativeSupervisedRefusal::GuestIsa));
        let mut changed = host(); changed.executable_x86_64 = false;
        assert_eq!(verdict(&plan(), changed), Err(NativeSupervisedRefusal::Executable));

        let mut changed = plan(); changed.rootfs = None;
        assert_eq!(verdict(&changed, host()), Err(NativeSupervisedRefusal::Root));
        let mut changed = plan(); changed.box_policy.uid = -2;
        assert_eq!(verdict(&changed, host()), Err(NativeSupervisedRefusal::Identity));
        let mut changed = plan(); changed.options.set("HL_MEM_MAX", "1", true).unwrap();
        assert_eq!(verdict(&changed, host()), Err(NativeSupervisedRefusal::Cgroup));
        let mut changed = plan(); changed.box_policy.lower_layers = Some(b"a\nb".to_vec());
        assert_eq!(verdict(&changed, host()), Err(NativeSupervisedRefusal::Overlay));
        let mut changed = plan(); changed.box_policy.file_owners = Some(b"0:0".to_vec());
        assert_eq!(verdict(&changed, host()), Err(NativeSupervisedRefusal::Ownership));
        let mut changed = plan(); changed.box_policy.volumes = Some(b"rw:/proc/x:/tmp".to_vec());
        assert_eq!(verdict(&changed, host()), Err(NativeSupervisedRefusal::Volumes));
        let mut changed = plan(); changed.box_policy.publish.push(crate::config::PortPublication { host_ipv4_be: 0, host_port: 1, guest_port: 1 });
        assert_eq!(verdict(&changed, host()), Err(NativeSupervisedRefusal::Network));
        let changed = plan();
        assert_eq!(native_eligibility(crate::activation::GuestIsa::X86_64, &changed, NativeCheckpointIntent::Restore, host()), Err(NativeSupervisedRefusal::Checkpoint));
        let mut changed = plan(); changed.options.set("HL_UNTRUSTED", "1", true).unwrap();
        assert_eq!(verdict(&changed, host()), Err(NativeSupervisedRefusal::Sandbox));
        let mut changed = plan(); changed.options.set("HL_SECCOMP_BASELINE", "default", true).unwrap();
        assert_eq!(verdict(&changed, host()), Err(NativeSupervisedRefusal::Seccomp));
        let mut changed = plan(); changed.options.set("HL_TRANSLIT", "1", true).unwrap();
        assert_eq!(verdict(&changed, host()), Err(NativeSupervisedRefusal::BackendControl));
        let mut changed = plan(); changed.box_policy.flags = 1 << 1;
        assert_eq!(verdict(&changed, host()), Err(NativeSupervisedRefusal::BoxFlags));
    }

    #[test]
    fn request_is_a_real_tri_state() {
        let mut options = crate::options::Options::default();
        assert_eq!(native_request(&options), NativeSupervisedRequest::Auto);
        options.set("HL_NATIVE_SUPERVISED", "0", true).unwrap();
        assert_eq!(native_request(&options), NativeSupervisedRequest::Off);
        options.set("HL_NATIVE_SUPERVISED", "1", true).unwrap();
        assert_eq!(native_request(&options), NativeSupervisedRequest::On);
        assert_eq!(native_selection(NativeSupervisedRequest::Auto, Ok(())), Ok(true));
        assert_eq!(native_selection(NativeSupervisedRequest::Auto, Err(NativeSupervisedRefusal::Network)), Ok(false));
        assert_eq!(native_selection(NativeSupervisedRequest::Off, Ok(())), Ok(false));
        assert_eq!(
            native_selection(NativeSupervisedRequest::On, Err(NativeSupervisedRefusal::Network)),
            Err(CompositionError::NativeSupervisedRefused(NativeSupervisedRefusal::Network))
        );

        let probes = std::cell::Cell::new(0);
        let eligibility = native_eligibility_for_request(
            NativeSupervisedRequest::Off,
            crate::activation::GuestIsa::X86_64,
            &plan(),
            NativeCheckpointIntent::None,
            || { probes.set(probes.get() + 1); host() },
        );
        assert_eq!(probes.get(), 0, "explicit OFF performed host/path preflight");
        assert_eq!(native_selection(NativeSupervisedRequest::Off, eligibility), Ok(false));
    }

    #[test]
    fn volume_admission_rejects_overlap_traversal_proc_and_excess() {
        assert!(volume_spec_supported(Some(b"ro:/src:/host/src,rw:/out:/host/out")));
        for invalid in [
            b"rw:/a:/x,ro:/a/b:/y".as_slice(),
            b"rw:/../a:/x".as_slice(),
            b"rw:/proc/x:/x".as_slice(),
            b"rw:/a:relative".as_slice(),
        ] {
            assert!(!volume_spec_supported(Some(invalid)), "accepted {invalid:?}");
        }
        let too_many = (0..33).map(|index| format!("rw:/v{index}:/tmp/v{index}")).collect::<Vec<_>>().join(",");
        assert!(!volume_spec_supported(Some(too_many.as_bytes())));
    }

    #[test]
    fn every_translated_backend_control_family_blocks_native_selection() {
        for (name, value) in [
            ("HL_TRANSLIT", "1"),
            ("HL_TRANSLIT_RIPREL_READONLY", "1"),
            ("HL_TRANSLIT_PERF_MAP", "/tmp/map"),
            ("HL_TRANSLIT_SYMBOLIZE", "1"),
            ("HL_PCACHE", "1"),
            ("HL_PCACHE_DIR", "/tmp/cache"),
        ] {
            let mut changed = plan();
            changed.options.set(name, value, true).unwrap();
            assert_eq!(verdict(&changed, host()), Err(NativeSupervisedRefusal::BackendControl), "{name}");
            assert_eq!(
                native_selection(NativeSupervisedRequest::Auto, verdict(&changed, host())),
                Ok(false),
                "AUTO ignored {name}",
            );
            assert_eq!(
                native_selection(NativeSupervisedRequest::On, verdict(&changed, host())),
                Err(CompositionError::NativeSupervisedRefused(NativeSupervisedRefusal::BackendControl)),
                "explicit ON ignored {name}",
            );
            assert_eq!(native_selection(NativeSupervisedRequest::Off, verdict(&changed, host())), Ok(false));
        }
        let mut changed = plan();
        changed.box_policy.translation_cache = Some(b"/tmp/cache".to_vec());
        assert_eq!(verdict(&changed, host()), Err(NativeSupervisedRefusal::BackendControl));

        let mut diagnostic = plan();
        diagnostic.options.set("HL_C_DIAGNOSTICS", "1", true).unwrap();
        let eligible = verdict(&diagnostic, host());
        assert_eq!(eligible, Ok(()), "diagnostics are supported by native supervision");
        assert_eq!(native_selection(NativeSupervisedRequest::On, eligible), Ok(true));
        assert_eq!(
            native_auto_eligibility(&diagnostic, host(), eligible),
            Err(NativeSupervisedRefusal::BackendControl),
        );

        let mut options = crate::options::Options::default();
        assert_eq!(options.set("HL_NATIVE_EXECUTION", "0", true), Err(crate::options::OptionError::UnknownName));
    }

    #[test]
    fn isolated_auto_requires_the_proven_hostname_projection_boundary() {
        let mut isolated = plan();
        isolated.box_policy.network_mode = 0;
        isolated.box_policy.flags = BOX_NETWORK_ISOLATED;
        isolated.box_policy.hostname = Some(b"builder".to_vec());
        assert_eq!(native_auto_eligibility(&isolated, host(), verdict(&isolated, host())), Ok(()));
        let mut unavailable = host();
        unavailable.isolated_hostname_projection = false;
        assert_eq!(
            native_auto_eligibility(&isolated, unavailable, verdict(&isolated, unavailable)),
            Err(NativeSupervisedRefusal::Network),
        );
        for invalid in [b"line\nbreak".as_slice(), b"under_score".as_slice(), b"-edge".as_slice()] {
            assert!(!hostname_valid(invalid));
        }
        assert!(hostname_valid(b"build-agent.example"));

        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("etc")).unwrap();
        let mut actual = isolated.clone();
        actual.rootfs = Some(root.path().as_os_str().as_encoded_bytes().to_vec());
        assert!(!isolated_hostname_projection_ready(&actual), "missing hosts admitted");
        std::fs::write(root.path().join("etc/hosts"), b"127.0.0.1 localhost\n").unwrap();
        assert!(isolated_hostname_projection_ready(&actual));
        actual.box_policy.hostname = Some(b"line\nbreak".to_vec());
        assert!(!isolated_hostname_projection_ready(&actual), "invalid hostname admitted");
        actual.box_policy.hostname = Some(b"builder".to_vec());
        actual.box_policy.hostname = None;
        assert!(!isolated_hostname_projection_ready(&actual), "missing hostname admitted");
        actual.box_policy.hostname = Some(b"builder".to_vec());
        std::fs::remove_file(root.path().join("etc/hosts")).unwrap();
        std::fs::create_dir(root.path().join("etc/hosts")).unwrap();
        assert!(!isolated_hostname_projection_ready(&actual), "directory hosts admitted");
        std::fs::remove_dir(root.path().join("etc/hosts")).unwrap();
        std::fs::write(root.path().join("etc/real-hosts"), b"127.0.0.1 localhost\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("real-hosts", root.path().join("etc/hosts")).unwrap();
        assert!(!isolated_hostname_projection_ready(&actual), "symlink hosts admitted");
        std::fs::remove_file(root.path().join("etc/hosts")).unwrap();
        let large = std::fs::File::create(root.path().join("etc/hosts")).unwrap();
        large.set_len(1024 * 1024 + 1).unwrap();
        assert!(!isolated_hostname_projection_ready(&actual), "oversized hosts admitted");
        drop(large);
        std::fs::remove_file(root.path().join("etc/hosts")).unwrap();
        std::fs::remove_file(root.path().join("etc/real-hosts")).unwrap();
        std::fs::remove_dir(root.path().join("etc")).unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("hosts"), b"127.0.0.1 outside\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), root.path().join("etc")).unwrap();
        assert!(!isolated_hostname_projection_ready(&actual), "intermediate etc symlink escaped root");

        let mut wrong_mode = isolated;
        wrong_mode.box_policy.network_mode = 1;
        assert_eq!(
            native_auto_eligibility(&wrong_mode, host(), Ok(())),
            Err(NativeSupervisedRefusal::Network),
        );
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn executable_probe_refuses_nonregular_and_symlink_inputs_without_blocking() {
        use std::os::unix::fs::symlink;
        let directory = tempfile::tempdir().unwrap();
        let executable = std::env::current_exe().unwrap();
        let link = directory.path().join("executable-link");
        symlink(&executable, &link).unwrap();
        assert!(!executable_is_x86_64(Some(link.as_os_str().as_encoded_bytes())));
        let fifo = directory.path().join("executable-fifo");
        let fifo_name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) }, 0);
        let started = std::time::Instant::now();
        assert!(!executable_is_x86_64(Some(fifo.as_os_str().as_encoded_bytes())));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert!(!executable_is_x86_64(Some(b"/dev/null")));
        let mut valid = [0_u8; 20];
        valid[..6].copy_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1]);
        valid[18..20].copy_from_slice(&62_u16.to_le_bytes());
        assert!(!x86_64_executable_header(false, Some(valid)), "valid bytes over nonregular authority admitted");
        assert!(x86_64_executable_header(true, Some(valid)));
    }

    #[test]
    fn unsupported_kernel_falls_back_only_for_auto() {
        let mut unsupported = host();
        unsupported.clone3 = false;
        let refusal = verdict(&plan(), unsupported);
        assert_eq!(refusal, Err(NativeSupervisedRefusal::Kernel));
        assert_eq!(native_selection(NativeSupervisedRequest::Auto, refusal), Ok(false));
        assert_eq!(
            native_selection(NativeSupervisedRequest::On, refusal),
            Err(CompositionError::NativeSupervisedRefused(NativeSupervisedRefusal::Kernel)),
        );
        assert_eq!(native_selection(NativeSupervisedRequest::Off, refusal), Ok(false));
    }

    #[test]
    fn checkpoint_intent_is_typed_but_only_none_is_admitted_without_lifecycle_proof() {
        assert_eq!(native_eligibility(crate::activation::GuestIsa::X86_64, &plan(), NativeCheckpointIntent::None, host()), Ok(()));
        for intent in [
            NativeCheckpointIntent::FreshCoordinator,
            NativeCheckpointIntent::DomainMember,
            NativeCheckpointIntent::Restore,
            NativeCheckpointIntent::Partial,
        ] {
            let refusal = native_eligibility(crate::activation::GuestIsa::X86_64, &plan(), intent, host());
            assert_eq!(refusal, Err(NativeSupervisedRefusal::Checkpoint), "{intent:?}");
            assert_eq!(native_selection(NativeSupervisedRequest::Auto, refusal), Ok(false));
            assert!(matches!(
                native_selection(NativeSupervisedRequest::On, refusal),
                Err(CompositionError::NativeSupervisedRefused(NativeSupervisedRefusal::Checkpoint)),
            ));
        }
        assert_eq!(native_checkpoint_intent(false, false, false, false), NativeCheckpointIntent::None);
        assert_eq!(native_checkpoint_intent(true, true, false, false), NativeCheckpointIntent::FreshCoordinator);
        assert_eq!(native_checkpoint_intent(false, false, true, false), NativeCheckpointIntent::DomainMember);
        assert_eq!(native_checkpoint_intent(true, true, false, true), NativeCheckpointIntent::Restore);
        assert_eq!(native_checkpoint_intent(true, false, false, false), NativeCheckpointIntent::Partial);
        assert_eq!(native_checkpoint_intent(true, true, true, false), NativeCheckpointIntent::Partial);
        for configure in ["mode", "policy", "option"] {
            let mut configured = plan();
            match configure {
                "mode" => configured.box_policy.checkpoint_mode = 1,
                "policy" => configured.box_policy.checkpoint_policy = 1,
                "option" => configured.options.set("HL_CHECKPOINT", "1", true).unwrap(),
                _ => unreachable!(),
            }
            assert_eq!(
                native_eligibility(crate::activation::GuestIsa::X86_64, &configured, NativeCheckpointIntent::None, host()),
                Err(NativeSupervisedRefusal::Checkpoint),
                "unpaired {configure} checkpoint configuration admitted",
            );
        }
    }
}

impl RuntimeFactory for ProductionFactory {
    type Machine = ProductionMachine;

    fn construct(&self, request: RuntimeConstruction<'_>) -> Result<Self::Machine, CompositionError> {
        let requested = native_request(&request.plan.options);
        let checkpoint = native_checkpoint_intent(
            request.services.checkpoint_sink.is_some(),
            request.services.checkpoint_source.is_some(),
            {
                #[cfg(unix)] { request.services.checkpoint_channel.is_some() }
                #[cfg(not(unix))] { false }
            },
            request.plan.options.get_bytes("HL_RESTORE").is_some()
                || request.plan.box_policy.checkpoint_mode & 2 != 0,
        );
        // OFF is a strict translated path: do not even inspect host paths or probe kernel support.
        let eligibility = native_eligibility_for_request(
            requested,
            request.isa,
            request.plan,
            checkpoint,
            || native_host_capabilities(request.plan),
        );
        // A native volume source is authenticated with openat2/open_tree immediately before namespace
        // projection. AUTO cannot prove that future transaction without opening authority-bearing FDs,
        // so it conservatively stays translated; explicit ON retains the existing secure late validation.
        let native_supervised = native_selection(requested, eligibility)?;
        #[cfg(unix)]
        let terminal = request
            .services
            .streams
            .terminal()
            .map(|terminal| NativeTerminalBridge::attach(terminal, InputDiscipline::Linux))
            .transpose()?;
        #[cfg(unix)]
        let output = if terminal.is_none() {
            request
                .services
                .streams
                .output()
                .map(NativeOutputBridge::attach)
                .transpose()?
        } else {
            None
        };
        #[cfg(unix)]
        let member = request.services.checkpoint_channel.clone();
        #[cfg(unix)]
        let checkpoint = match (
            request.services.checkpoint_sink.clone(),
            request.services.checkpoint_source.clone(),
        ) {
            (Some(sink), Some(source)) => Some(CheckpointControl::start(
                sink,
                source,
                request.isa,
                request
                    .plan
                    .options
                    .get_bytes("HL_CHECKPOINT_PHASE_LEDGER")
                    .and_then(|_| request.plan.options.get("HL_DIAGNOSTIC_PORT"))
                    .and_then(|value| value.parse().ok()),
                request
                    .plan
                    .options
                    .get_bytes("HL_CHECKPOINT_PHASE_CLOCK_FAIL")
                    .is_some(),
            )?),
            (None, None) => None,
            _ => return Err(CompositionError::RuntimeConstruction),
        };
        Ok(ProductionMachine {
            isa: request.isa,
            plan: request.plan.clone(),
            native_supervised,
            #[cfg(unix)]
            terminal,
            #[cfg(unix)]
            output,
            #[cfg(unix)]
            checkpoint,
            #[cfg(unix)]
            member,
            state: Mutex::new(StartupState::default()),
        })
    }
}

impl ProductionMachine {
    fn encode_environment(&self) -> Vec<u8> {
        let mut encoded = Vec::new();
        for (index, record) in self.plan.environment.iter().enumerate() {
            if index != 0 {
                encoded.push(b'\n');
            }
            encode_environment_record(&mut encoded, record);
        }
        encoded
    }

    fn create(&self) -> Result<hl_native::Engine, EngineError> {
        #[cfg(unix)]
        self.plan.refuse_unownable_root()?;
        let rootfs = self
            .plan
            .rootfs
            .as_deref()
            .map(CString::new)
            .transpose()
            .map_err(|_| EngineError::LaunchFailed)?;
        let executable = self
            .plan
            .executable_host
            .as_deref()
            .map(CString::new)
            .transpose()
            .map_err(|_| EngineError::LaunchFailed)?;
        let box_projection = BoxProjection::new(&self.plan.box_policy)?;
        let mut options = self
            .plan
            .options
            .iter()
            .map(|(name, value)| Ok((CString::new(name)?, CString::new(value)?)))
            .collect::<Result<Vec<_>, std::ffi::NulError>>()
            .map_err(|_| EngineError::LaunchFailed)?;
        if self.native_supervised && self.plan.options.get_bytes("HL_NATIVE_SUPERVISED").is_none() {
            options.push((
                CString::new("HL_NATIVE_SUPERVISED").expect("literal"),
                CString::new("1").expect("literal"),
            ));
        }
        // Name the coordinator on the launch boundary. Only a machine holding a CheckpointControl can be
        // sent REQUEST_CHECKPOINT, so this is exactly "the embedder will ask THIS engine to capture"; a
        // domain member carries a channel and no Server. The engine's election reads it instead of asking
        // whether it is the top of a launch, which every exec session also is.
        #[cfg(unix)]
        if self.checkpoint.is_some() {
            options.push((
                CString::new("HL_CHECKPOINT_COORDINATOR").expect("literal"),
                CString::new("1").expect("literal"),
            ));
        }
        options.push((
            CString::new("HL_GUEST_ENV").expect("literal"),
            CString::new(self.encode_environment()).map_err(|_| EngineError::LaunchFailed)?,
        ));
        options.push((
            CString::new("HL_GUEST_ENV_ESC").expect("literal"),
            CString::new("1").expect("literal"),
        ));
        options.push((
            CString::new("HL_GUEST_ENV_EXACT").expect("literal"),
            CString::new("1").expect("literal"),
        ));
        let names = options.iter().map(|(name, _)| name.as_ptr()).collect::<Vec<_>>();
        let values = options.iter().map(|(_, value)| value.as_ptr()).collect::<Vec<_>>();
        #[cfg(unix)]
        let standard_fds = self
            .terminal
            .as_ref()
            .map(NativeTerminalBridge::standard_fds)
            .or_else(|| self.output.as_ref().map(NativeOutputBridge::standard_fds))
            .unwrap_or([0, 1, 2]);
        #[cfg(not(unix))]
        let standard_fds = [0, 1, 2];
        let config = hl_native::EngineConfig {
            isa: match self.isa {
                crate::activation::GuestIsa::Aarch64 => 1,
                crate::activation::GuestIsa::X86_64 => 2,
            },
            rootfs: rootfs.as_deref(),
            executable_host: executable.as_deref(),
            executable_fd: -1,
            option_names: &names,
            option_values: &values,
            box_config: Some(&box_projection.raw),
            standard_fds,
            provider_fd: -1,
        };
        // SAFETY: all pointers in config remain live for this call and there is no callback state.
        let engine = unsafe { hl_native::Engine::create(config) }.map_err(|error| match error {
            hl_native::Error::Load(kind) => EngineError::NativeLoadFailed(kind),
            hl_native::Error::Status(status) => EngineError::NativeCreateFailed(status),
        })?;
        #[cfg(unix)]
        let mut engine = engine;
        #[cfg(unix)]
        if let Some(transport) = self
            .checkpoint
            .as_ref()
            .map(|checkpoint| checkpoint.transport.as_ref())
            .or_else(|| self.member.as_ref().map(|channel| channel.0.as_ref()))
        {
            engine
                .configure_checkpoint(transport)
                .map_err(|_| EngineError::LaunchFailed)?;
        }
        Ok(engine)
    }

    fn native_supervised(&self) -> bool {
        self.native_supervised
    }

    fn current(&self) -> Result<Arc<hl_native::Engine>, EngineError> {
        self.state
            .lock()
            .map_err(|_| EngineError::Synchronization)?
            .engine
            .clone()
            .ok_or(EngineError::NotStarted)
    }

    fn exit(engine: &hl_native::Engine) -> EngineExit {
        let exit = engine.exit();
        EngineExit {
            kind: match exit.kind {
                1 => ExitKind::Code,
                2 => ExitKind::Signal,
                3 => ExitKind::Fault,
                _ => ExitKind::EngineError,
            },
            guest_status: exit.status,
            detail: exit.detail,
            fault: None,
        }
    }
}

fn encode_environment_record(encoded: &mut Vec<u8>, record: &[u8]) {
    for byte in record {
        match byte {
            b'\\' => encoded.extend_from_slice(b"\\\\"),
            b'\n' => encoded.extend_from_slice(b"\\n"),
            byte => encoded.push(*byte),
        }
    }
}

impl GuestMachine for ProductionMachine {
    fn start(&self) -> Result<(), EngineError> {
        #[cfg(unix)]
        if self.native_supervised()
            && let Some(checkpoint) = &self.checkpoint
        {
            checkpoint
                .server
                .reset_native_refusal()
                .map_err(CheckpointControl::capture_failure)?;
        }
        #[cfg(unix)]
        let recovery = if self.plan.options.get_bytes("HL_RESTORE").is_some() {
            let checkpoint = self.checkpoint.as_ref().ok_or(EngineError::LaunchFailed)?;
            // A refused recovery admission must name itself: the restore driver downstream is the only
            // other place that reports, and it never runs when admission refuses.
            Some(
                checkpoint
                    .begin_recovery(std::time::Instant::now() + crate::composition::DEFAULT_CHECKPOINT_TIMEOUT)
                    .inspect_err(|error| eprintln!("[restore] refuse: recovery admission rejected: {error:?}"))?,
            )
        } else {
            None
        };
        // A supervised run returns only after namespace PID1 has reaped every descendant and the
        // result has been published. At that boundary the immutable machine still owns exactly the
        // same root, executable and typed box policy, so retaining its pinned native authority is
        // safe. A different RuntimePlan constructs a different ProductionMachine and cannot enter
        // this cache.
        let engine = if self.native_supervised() {
            self.state
                .lock()
                .map_err(|_| EngineError::Synchronization)?
                .retained()
                .map_or_else(|| self.create().map(Arc::new), Ok)?
        } else {
            Arc::new(self.create()?)
        };
        let pending_stop = self
            .state
            .lock()
            .map_err(|_| EngineError::Synchronization)?
            .publish(Arc::clone(&engine));
        if let Some(request) = pending_stop {
            let (kind, signal) = match request {
                StopRequest::Interrupt => (REQUEST_INTERRUPT, request.signal()),
                StopRequest::Force => (REQUEST_FORCE_STOP, request.signal()),
                StopRequest::Signal(signal) => (REQUEST_SIGNAL, signal),
            };
            engine.request(kind, signal).map_err(EngineError::NativeStopFailed)?;
        }
        let arguments = self
            .plan
            .arguments
            .iter()
            .map(|argument| CString::new(argument.as_slice()).map_err(|_| EngineError::LaunchFailed))
            .collect::<Result<Vec<_>, _>>()?;
        let pointers = arguments.iter().map(|argument| argument.as_ptr()).collect::<Vec<_>>();
        #[cfg(unix)]
        let run = if let Some(recovery) = recovery.as_ref() {
            // Recovery publication is completed by the restored process while
            // `run` is still waiting for that process to exit. Waiting only
            // after `run` returns lets a later checkpoint reuse the server
            // state first; the stale recovery waiter then observes that newer
            // generation and reports `Busy` despite a successful checkpoint.
            run_with_recovery(recovery, || engine.run(&pointers).map_err(native_run_failure))
        } else {
            engine.run(&pointers).map_err(native_run_failure)
        };
        #[cfg(not(unix))]
        let run = engine.run(&pointers).map_err(native_run_failure);
        if let Err(error) = run {
            self.state
                .lock()
                .map_err(|_| EngineError::Synchronization)?
                .discard_if(&engine);
            return Err(error);
        }
        #[cfg(unix)]
        if let Some(terminal) = &self.terminal {
            terminal.flush();
        }
        #[cfg(unix)]
        if let Some(output) = &self.output {
            output.flush();
        }
        Ok(())
    }

    fn wait(&self) -> Result<EngineExit, EngineError> {
        let engine = self.current()?;
        Ok(Self::exit(engine.as_ref()))
    }

    fn stop(&self, request: StopRequest) -> Result<(), EngineError> {
        let (kind, signal) = match request {
            StopRequest::Interrupt => (REQUEST_INTERRUPT, request.signal()),
            StopRequest::Force => (REQUEST_FORCE_STOP, request.signal()),
            StopRequest::Signal(signal) => (REQUEST_SIGNAL, signal),
        };
        let engine = self
            .state
            .lock()
            .map_err(|_| EngineError::Synchronization)?
            .request(request);
        engine.map_or(Ok(()), |engine| {
            engine.request(kind, signal).map_err(EngineError::NativeStopFailed)
        })
    }

    #[cfg(unix)]
    fn checkpoint_channel(&self) -> Option<crate::composition::CheckpointChannel> {
        self.checkpoint
            .as_ref()
            .map(|checkpoint| crate::composition::CheckpointChannel(Arc::clone(&checkpoint.transport)))
    }

    fn guest_pid(&self) -> Option<std::num::NonZeroI32> {
        self.current().ok().and_then(|engine| engine.guest_pid())
    }

    #[cfg(unix)]
    fn restored_member(&self, guest_pid: std::num::NonZeroI32) -> Option<crate::runtime::RestoredMember> {
        self.checkpoint
            .as_ref()
            .and_then(|checkpoint| checkpoint.server.restored_member(guest_pid))
            .map(crate::runtime::RestoredMember::new)
    }

    #[cfg(unix)]
    fn provide_member_terminal(
        &self,
        guest_pid: std::num::NonZeroI32,
        terminal: std::os::fd::OwnedFd,
    ) -> Result<(), EngineError> {
        let checkpoint = self.checkpoint.as_ref().ok_or(EngineError::Unsupported)?;
        checkpoint
            .server
            .register_member_terminal(guest_pid, terminal)
            .map_err(|reason| {
                hl_log::hl_error!(hl_log::tag::CHECKPOINT, "member terminal registration failed: {reason}");
                EngineError::Unsupported
            })
    }

    fn checkpoint_supported(&self) -> Result<(), EngineError> {
        if let Some(refusal) = checkpoint_sandbox_refusal(&self.plan.options) {
            return Err(refusal);
        }
        #[cfg(unix)]
        if self.checkpoint.is_some() {
            return Ok(());
        }
        Err(EngineError::Unsupported)
    }

    fn capture_checkpoint(&self) -> Result<(), EngineError> {
        self.capture_checkpoint_until(std::time::Instant::now() + crate::composition::DEFAULT_CHECKPOINT_TIMEOUT)
    }

    fn capture_checkpoint_until(&self, deadline: std::time::Instant) -> Result<(), EngineError> {
        #[cfg(not(unix))]
        let _ = deadline;
        if let Some(refusal) = checkpoint_sandbox_refusal(&self.plan.options) {
            return Err(refusal);
        }
        #[cfg(unix)]
        if let Some(checkpoint) = &self.checkpoint {
            let engine = self.current()?;
            return checkpoint.capture(engine.as_ref(), self.isa, deadline, !self.native_supervised);
        }
        Err(EngineError::Unsupported)
    }
}

/// Classify a launch that the checkpoint engine cannot capture under.
///
/// `HL_UNTRUSTED` forks the sentry and routes every host-authority syscall through it, so the
/// worker process that would dump itself does not own the descriptors, sockets or pipes the guest
/// sees; `ckpt_dump_self_locked` refuses on that gate. Capturing under the sentry requires the
/// sentry to participate in capture and restore -- exporting its descriptor table, open-file
/// descriptions and connection state across the control ring -- which is not implemented.
///
/// Reporting it here rather than as a bare native failure keeps the refusal on the launch-policy
/// boundary that owns the option, and makes it permanent so a preflight does not poll for it.
#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::checkpoint::{CheckpointPhaseLedger, RECOVERY_OPEN, RecoveryAdmission};
    use super::*;

    #[cfg(unix)]
    struct EmptyCheckpointStore;

    #[cfg(unix)]
    impl crate::composition::CheckpointSink for EmptyCheckpointStore {
        fn replace(&self, _: &[u8]) -> Result<(), CompositionError> {
            Err(CompositionError::RuntimeConstruction)
        }

        fn begin_until(&self, _: std::time::Instant) -> Result<std::num::NonZeroU64, CompositionError> {
            Ok(std::num::NonZeroU64::MIN)
        }

        fn put_until(
            &self,
            _: std::num::NonZeroU64,
            _: &str,
            _: &[u8],
            _: std::time::Instant,
        ) -> Result<(), CompositionError> {
            Err(CompositionError::RuntimeConstruction)
        }

        fn abort_until(&self, _: std::num::NonZeroU64, _: std::time::Instant) -> Result<(), CompositionError> {
            Ok(())
        }

        fn commit_until(
            &self,
            _: std::num::NonZeroU64,
            _: &[u8],
            _: std::time::Instant,
        ) -> Result<(), CompositionError> {
            Err(CompositionError::RuntimeConstruction)
        }
    }

    #[cfg(unix)]
    impl crate::composition::CheckpointSource for EmptyCheckpointStore {
        fn read(&self, _: usize) -> Result<Vec<u8>, CompositionError> {
            Err(CompositionError::RuntimeConstruction)
        }

        fn get_until(&self, _: &str, _: std::time::Instant) -> Result<Vec<u8>, CompositionError> {
            Err(CompositionError::RuntimeConstruction)
        }

        fn list_until(&self, _: std::time::Instant) -> Result<Vec<String>, CompositionError> {
            // Recovery refuses a generation carrying no manifest, so a store that
            // admits recovery must present a finalized one.
            Ok(vec![String::from("MANIFEST")])
        }
    }

    /// A staged generation never becomes finalized while this launch is waiting,
    /// so the checkpoint preflight must surface the refusal instead of polling
    /// until its deadline.
    ///
    /// `#[cfg(unix)]` because both names in the body are: `CheckpointControl` is
    /// declared under one in this file, and `super::super::checkpoint` under one in
    /// `runtime/api.rs`. The gate belongs on the test rather than on the module,
    /// because the module is a checkpoint coordinator that passes descriptors over an
    /// `AF_UNIX` socket with `SCM_RIGHTS` -- widening it to Windows is a port, not a
    /// `cfg` edit. Everything else in this `mod tests` that names either already
    /// carries the same gate; this one item did not, and nothing built the
    /// configuration that would have said so.
    #[cfg(unix)]
    #[test]
    fn an_unfinalized_generation_is_a_permanent_recovery_refusal() {
        assert_eq!(
            CheckpointControl::capture_failure(super::super::checkpoint::CaptureFailure::Unfinalized),
            EngineError::CheckpointGenerationUnfinalized
        );
        assert!(EngineError::CheckpointGenerationUnfinalized.is_permanent_refusal());
    }

    #[test]
    fn native_run_status_survives_the_engine_boundary() {
        assert_eq!(native_run_failure(13), EngineError::NativeRunFailed(13));
    }

    #[test]
    fn typed_network_projection_preserves_order_gateway_and_publications() {
        let policy = crate::launcher::plan::RuntimeBoxPolicy {
            network_mode: 0,
            network_namespace: Some(b"container-key".to_vec()),
            network_interfaces: vec![
                crate::launcher::plan::NetworkInterface {
                    bridge: b"front".to_vec(),
                    address_ipv4_be: u32::from_le_bytes([172, 29, 0, 2]),
                    gateway_ipv4_be: u32::from_le_bytes([172, 29, 0, 1]),
                    prefix: 24,
                },
                crate::launcher::plan::NetworkInterface {
                    bridge: b"back".to_vec(),
                    address_ipv4_be: u32::from_le_bytes([10, 7, 0, 9]),
                    gateway_ipv4_be: u32::from_le_bytes([10, 7, 0, 1]),
                    prefix: 19,
                },
            ],
            publish: vec![crate::config::PortPublication {
                host_ipv4_be: u32::from_le_bytes([127, 0, 0, 1]),
                host_port: 18080,
                guest_port: 8080,
            }],
            ..Default::default()
        };
        let projection = BoxProjection::new(&policy).unwrap();
        assert_eq!(projection.raw.abi, 2);
        assert_eq!(projection.raw.network_interface_count, 2);
        assert_eq!(projection.raw.publish_count, 1);
        assert_eq!(projection._network_interfaces[0].prefix, 24);
        assert_eq!(
            projection._network_interfaces[1].gateway_ipv4_be,
            u32::from_le_bytes([10, 7, 0, 1])
        );
        assert_eq!(projection._network_names[0].to_bytes(), b"front");
        assert_eq!(projection._publish[0].guest_port, 8080);
    }

    #[cfg(unix)]
    #[test]
    fn dropped_recovery_admission_releases_scope_and_rejects_stale_id() {
        let store = Arc::new(EmptyCheckpointStore);
        let server = Arc::new(Server::new(store.clone(), store));
        let first = server
            .begin_recovery(11, std::time::Instant::now() + std::time::Duration::from_secs(1))
            .unwrap();
        {
            let _admission = RecoveryAdmission {
                server: Arc::clone(&server),
                id: first,
                state: std::sync::atomic::AtomicU8::new(RECOVERY_OPEN),
                phases: CheckpointPhaseLedger::new(None, false, crate::activation::GuestIsa::Aarch64),
            };
        }
        let second = server
            .begin_recovery(12, std::time::Instant::now() + std::time::Duration::from_secs(1))
            .unwrap();
        assert_eq!(
            server.abort_recovery(first),
            Err(super::super::checkpoint::CaptureFailure::Busy)
        );
        server.abort_recovery(second).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn pre_channel_run_failure_aborts_recovery_without_waiting_for_deadline() {
        let store = Arc::new(EmptyCheckpointStore);
        let server = Arc::new(Server::new(store.clone(), store));
        let id = server
            .begin_recovery(21, std::time::Instant::now() + std::time::Duration::from_secs(5))
            .unwrap();
        let admission = RecoveryAdmission {
            server: Arc::clone(&server),
            id,
            state: std::sync::atomic::AtomicU8::new(RECOVERY_OPEN),
            phases: CheckpointPhaseLedger::new(None, false, crate::activation::GuestIsa::Aarch64),
        };
        let started = std::time::Instant::now();
        assert_eq!(
            run_with_recovery(&admission, || Err::<(), _>(EngineError::NativeRunFailed(7))),
            Err(EngineError::NativeRunFailed(7))
        );
        assert!(started.elapsed() < std::time::Duration::from_millis(250));
        assert_eq!(admission.wait(), Err(EngineError::Busy));
        let retry = server
            .begin_recovery(22, std::time::Instant::now() + std::time::Duration::from_secs(1))
            .unwrap();
        server.abort_recovery(retry).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn dropping_consumed_admission_cannot_abort_reused_generation() {
        let store = Arc::new(EmptyCheckpointStore);
        let server = Arc::new(Server::new(store.clone(), store));
        let id = server
            .begin_recovery(23, std::time::Instant::now() + std::time::Duration::from_secs(1))
            .unwrap();
        let admission = RecoveryAdmission {
            server: Arc::clone(&server),
            id,
            state: std::sync::atomic::AtomicU8::new(RECOVERY_OPEN),
            phases: CheckpointPhaseLedger::new(None, false, crate::activation::GuestIsa::Aarch64),
        };
        admission.abort().unwrap();
        let _ = admission.wait();

        let reused = server
            .begin_recovery(23, std::time::Instant::now() + std::time::Duration::from_secs(1))
            .unwrap();
        drop(admission);
        server
            .fail_recovery(reused, super::super::checkpoint::CaptureFailure::Deadline)
            .expect("a consumed admission must not abort the reused generation");
        assert_eq!(
            server.wait_recovery(reused),
            Err(super::super::checkpoint::CaptureFailure::Deadline)
        );
    }

    #[cfg(unix)]
    #[test]
    fn poisoned_wait_is_aborted_by_admission_drop_and_allows_retry() {
        let store = Arc::new(EmptyCheckpointStore);
        let server = Arc::new(Server::new(store.clone(), store));
        let id = server
            .begin_recovery(24, std::time::Instant::now() + std::time::Duration::from_secs(1))
            .unwrap();
        let admission = RecoveryAdmission {
            server: Arc::clone(&server),
            id,
            state: std::sync::atomic::AtomicU8::new(RECOVERY_OPEN),
            phases: CheckpointPhaseLedger::new(None, false, crate::activation::GuestIsa::Aarch64),
        };
        let poison = Arc::clone(&server);
        let _ = std::thread::spawn(move || poison.poison_coordination()).join();

        // A poisoned capture ledger is not a launch failure. Naming it as one is what put
        // "LaunchFailed" in front of a desktop user whose workspace had launched fine.
        assert_eq!(admission.wait(), Err(EngineError::CapturePoisoned));
        drop(admission);
        let retry = server
            .begin_recovery(25, std::time::Instant::now() + std::time::Duration::from_secs(1))
            .expect("dropping a poisoned admission must release its recovery transaction");
        server.abort_recovery(retry).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn poisoned_unwaited_admission_drop_allows_retry() {
        let store = Arc::new(EmptyCheckpointStore);
        let server = Arc::new(Server::new(store.clone(), store));
        let id = server
            .begin_recovery(26, std::time::Instant::now() + std::time::Duration::from_secs(1))
            .unwrap();
        let admission = RecoveryAdmission {
            server: Arc::clone(&server),
            id,
            state: std::sync::atomic::AtomicU8::new(RECOVERY_OPEN),
            phases: CheckpointPhaseLedger::new(None, false, crate::activation::GuestIsa::Aarch64),
        };
        let poison = Arc::clone(&server);
        let _ = std::thread::spawn(move || poison.poison_coordination()).join();

        drop(admission);
        let retry = server
            .begin_recovery(27, std::time::Instant::now() + std::time::Duration::from_secs(1))
            .expect("dropping an unwaited poisoned admission must release its recovery transaction");
        server.abort_recovery(retry).unwrap();
    }
}

#[cfg(test)]
mod sandbox_refusal_tests {
    use super::*;

    /// `Sandbox::SentryOnly` is the container default, so this is the ordinary launch. A checkpoint
    /// of it must refuse with a cause the product can show, not with a bare native failure, and the
    /// refusal must be permanent so the checkpoint preflight reports it instead of polling for 30s.
    #[test]
    fn a_sentry_launch_is_refused_permanently_with_its_own_cause() {
        let mut options = crate::options::Options::default();
        options.set("HL_UNTRUSTED", "1", true).unwrap();
        assert_eq!(
            checkpoint_sandbox_refusal(&options),
            Some(EngineError::CheckpointUnsupportedUnderSandbox)
        );
        assert!(EngineError::CheckpointUnsupportedUnderSandbox.is_permanent_refusal());
    }

    #[test]
    fn a_launch_without_the_sentry_is_not_refused_by_policy() {
        assert_eq!(checkpoint_sandbox_refusal(&crate::options::Options::default()), None);
    }
}
