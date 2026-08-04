use std::sync::Arc;

use hl_runtime::RuntimeNetworkSyscalls;

use super::process_memory::ProcessMemory;

mod checkpoint;
mod native;
mod reactor;
mod runtime;
mod socket_option;
mod transfer;
pub(super) use native::Native;
pub use runtime::CheckpointRuntime;

#[cfg(test)]
mod test;

pub(super) type Runtime = RuntimeNetworkSyscalls<Native, ProcessMemory>;

struct Credentials {
    tasks: Arc<hl_task::TaskRegistry>,
    process: hl_task::ProcessId,
}

impl hl_runtime::SocketCredentials for Credentials {
    fn current(&self) -> Option<hl_network::SenderCredentials> {
        let credentials = self.tasks.credentials(self.process).ok()?;
        Some(hl_network::SenderCredentials {
            process: self.process.number(),
            user: credentials.real_user,
            group: credentials.real_group,
        })
    }
}

pub(super) fn runtime(
    descriptors: Arc<hl_descriptor::DescriptorTable>,
    tasks: Arc<hl_task::TaskRegistry>,
    process: hl_task::ProcessId,
    memory: ProcessMemory,
    architecture: hl_linux::GuestArchitecture,
    checkpoint: &CheckpointRuntime,
    enabled: bool,
    wait: Arc<dyn hl_runtime::SocketWait>,
    unix_socket_paths: Option<Arc<dyn hl_runtime::UnixSocketPathPort>>,
) -> Runtime {
    let catalog = checkpoint.catalog();
    let mut runtime = RuntimeNetworkSyscalls::new(descriptors, catalog.current(), memory, architecture)
        .with_checkpoint_catalog(catalog)
        .with_registry(checkpoint.sockets())
        .with_descriptor_transfer(checkpoint.transfer())
        .with_network_policy(checkpoint.policy())
        .with_host_projection(enabled)
        .with_credential_source(Arc::new(Credentials { tasks, process }))
        .with_wait_port(wait);
    if let Some(paths) = unix_socket_paths {
        runtime = runtime.with_unix_socket_paths(paths);
    }
    runtime.with_host(checkpoint.host())
}
