//! Executable image, signal gateway, and workspace-root routing.

use std::os::unix::ffi::OsStrExt;
use std::sync::{Arc, Mutex};

use hl_isa::AddressRange;
use hl_isa::GuestAddress;
use hl_memory::{Backing, MapRequest, MappingCoordinator, Placement, Protection};

use crate::engine::EngineError;
use crate::launch_plan::RuntimeLaunchPlan;

const SIGRETURN_PAGE: u64 = 0x3ff_0000;

pub(super) struct SignalGateway;

impl SignalGateway {
    pub(super) fn install(
        mappings: &MappingCoordinator<super::super::MappingHostAdapter>,
        memory: &super::super::process_memory::ProcessMemory,
        architecture: hl_linux::GuestArchitecture,
    ) -> Result<u64, EngineError> {
        let request = |placement| MapRequest {
            placement,
            length: 4096,
            alignment: 4096,
            protection: Protection::READ.union(Protection::WRITE),
            backing: Backing::Anonymous {
                identity: 0x5349_4752,
                shared: false,
            },
            backing_offset: 0,
        };
        // A main image wider than the gap from its load hint to this page already owns the preferred
        // address; the gateway then takes the first free page above it instead of failing the launch.
        let page = match mappings.map(request(Placement::FixedNoReplace(GuestAddress::new(SIGRETURN_PAGE)))) {
            Ok(address) => address,
            Err(_) => mappings
                .map(request(Placement::Anywhere {
                    minimum: GuestAddress::new(SIGRETURN_PAGE),
                    maximum: GuestAddress::new(hl_memory::MEMORY_ADDRESS_MAXIMUM),
                    hint: None,
                }))
                .map_err(|_| EngineError::LaunchFailed)?,
        }
        .get();
        let code: &[u8] = match architecture {
            hl_linux::GuestArchitecture::Aarch64 => &[0x68, 0x11, 0x80, 0xd2, 0x01, 0x00, 0x00, 0xd4],
            hl_linux::GuestArchitecture::X86_64 => &[0xb8, 0x0f, 0x00, 0x00, 0x00, 0x0f, 0x05],
        };
        hl_linux::GuestMemory::write(memory, page, code).map_err(|_| EngineError::LaunchFailed)?;
        let range = AddressRange::nonempty(GuestAddress::new(page), 4096).map_err(|_| EngineError::LaunchFailed)?;
        mappings
            .protect(range, Protection::READ.union(Protection::EXECUTE))
            .map_err(|_| EngineError::LaunchFailed)?;
        Ok(page)
    }
}

pub(in crate::ffi::linux::execution) struct WorkspaceRoot;

impl WorkspaceRoot {
    pub(super) fn host(
        plan: &RuntimeLaunchPlan,
        authority: Option<Arc<Mutex<crate::native::AuthorityWorker>>>,
        tasks: Arc<hl_task::TaskRegistry>,
        process: hl_task::ProcessId,
        handles: Arc<hl_runtime::NamespaceHandleRegistry>,
        descriptors: Arc<hl_descriptor::DescriptorTable>,
        transfers: Arc<super::super::path::FileTransferRegistry>,
        entropy: Arc<dyn super::super::ports::random::EntropySource>,
        system: Arc<hl_runtime::SystemAuthority>,
        locks: Arc<hl_runtime::AdvisoryLockCoordinator>,
        architecture: hl_linux::GuestArchitecture,
    ) -> Result<
        (
            Option<Arc<super::super::path::NativePath>>,
            Option<Arc<super::super::watch::Hub>>,
        ),
        EngineError,
    > {
        let projected = authority.is_some();
        let Some(root) = Self::select(plan) else {
            return Ok((None, None));
        };
        let overlay_upper = (!projected)
            .then(|| plan.options.get("HL_OVERLAY_UPPER"))
            .flatten()
            .map(|upper| upper.as_bytes().to_vec());
        let watch_root = overlay_upper.as_deref().unwrap_or(&root);
        let watches = if projected {
            super::super::watch::Hub::projected(&root)
        } else {
            super::super::watch::Hub::new(watch_root)
        }
        .map_err(|_| EngineError::LaunchFailed)?;
        // Every layer root a declared path could sit under, so ownership survives an upper-miss.
        let mut owner_roots = vec![std::path::PathBuf::from(std::ffi::OsStr::from_bytes(&root))];
        let native = if projected {
            super::super::path::NativePath::projected(&root, Arc::clone(&watches))
        } else if let Some(upper) = overlay_upper {
            let lowers = plan
                .options
                .get("HL_LOWER")
                .map(|records| {
                    records
                        .lines()
                        .map(|lower| lower.as_bytes().to_vec())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            owner_roots.push(std::path::PathBuf::from(std::ffi::OsStr::from_bytes(&upper)));
            owner_roots.extend(
                lowers
                    .iter()
                    .map(|lower| std::path::PathBuf::from(std::ffi::OsStr::from_bytes(lower))),
            );
            super::super::path::NativePath::layered(&upper, &lowers, Arc::clone(&watches))
        } else {
            super::super::path::NativePath::new(&root, Arc::clone(&watches))
        }
        .map_err(|_| EngineError::LaunchFailed)?;
        let native = native.with_file_owners(
            plan.options.get_bytes("HL_FILE_OWNERS").unwrap_or_default(),
            owner_roots,
        );
        let native = match authority {
            Some(tree) => native.with_projection(tree).map_err(|_| EngineError::LaunchFailed)?,
            None => native,
        };
        Ok((
            Some(Arc::new(
                native
                    .with_cpu_model(hl_runtime::ProcfsCpuPolicy::model(
                        architecture,
                        super::super::GuestExecutor::guest_features(architecture),
                    ))
                    .with_advisory_locks(locks)
                    .with_entropy(entropy)
                    .with_transfers(transfers)
                    .with_system(system)
                    .with_read_only(plan.options.get("HL_ROOTFS_RO") == Some("1"))
                    .with_process(tasks, process, handles, descriptors),
            )),
            Some(watches),
        ))
    }

    pub(super) fn configure(
        host: &super::super::path::NativePath,
        plan: &RuntimeLaunchPlan,
        projected: bool,
    ) -> Result<(), hl_runtime::RuntimePathError> {
        if !projected {
            if let Some(volumes) = plan.options.get("HL_VOLUMES") {
                Self::mount_volumes(host, volumes)?;
            }
            if let Some(name_binds) = plan.options.get("HL_NAME_BINDS") {
                host.ordinary()?.add_name_binds(name_binds)?;
            }
        }
        let executable = if projected {
            Self::executable(plan)
        } else {
            plan.executable_host.clone()
        };
        let Some(executable) = executable else { return Ok(()) };
        if projected {
            host.set_projected_executable(&executable)
        } else {
            host.set_executable(&executable)
        }
    }

    fn mount_volumes(host: &super::super::path::NativePath, volumes: &str) -> Result<(), hl_runtime::RuntimePathError> {
        let context = host.ordinary()?;
        for record in volumes.split(',') {
            let (record, read_only) = if let Some(record) = record.strip_prefix("ro:") {
                (record, true)
            } else if let Some(record) = record.strip_prefix("rw:") {
                (record, false)
            } else {
                (record, false)
            };
            let (guest, backing) = record.split_once(':').ok_or(hl_runtime::RuntimePathError::Invalid)?;
            context.mount_directory(guest, backing, read_only)?;
        }
        Ok(())
    }

    pub(in crate::ffi::linux::execution) fn select(plan: &RuntimeLaunchPlan) -> Option<Vec<u8>> {
        if let Some(root) = &plan.rootfs {
            return Some(root.clone());
        }
        let executable = plan.executable_host.as_deref()?;
        executable.iter().rposition(|byte| *byte == b'/').map(|index| {
            if index == 0 {
                b"/".to_vec()
            } else {
                executable[..index].to_vec()
            }
        })
    }

    pub(in crate::ffi::linux::execution) fn executable(plan: &RuntimeLaunchPlan) -> Option<Vec<u8>> {
        let host = plan.executable_host.as_deref()?;
        if let Some(argument) = plan.arguments.first()
            && argument.first() == Some(&b'/')
            && argument.as_slice() != host
        {
            return Some(argument.clone());
        }
        let relative = plan
            .rootfs
            .as_deref()
            .and_then(|root| host.strip_prefix(root))
            .map(|path| path.strip_prefix(b"/").unwrap_or(path))
            .or_else(|| host.rsplit(|byte| *byte == b'/').next());
        let relative = relative.filter(|path| !path.is_empty())?;
        let mut guest = Vec::with_capacity(relative.len() + 1);
        guest.push(b'/');
        guest.extend_from_slice(relative);
        Some(guest)
    }
}
