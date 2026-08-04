use std::sync::Arc;

use hl_descriptor::DescriptorTable;
use hl_linux::{GuestArchitecture, GuestMemory};

use crate::{FsContext, RuntimeFilesystemSyscalls, WorkingDirectory};

impl<M: GuestMemory> RuntimeFilesystemSyscalls<M> {
    pub fn new(descriptors: Arc<DescriptorTable>, memory: M, architecture: GuestArchitecture) -> Self {
        Self {
            descriptors,
            memory,
            architecture,
            path_host: None,
            memfds: None,
            pipe_signal: None,
            file_size_limit: None,
            async_signal: None,
            dnotify: None,
            pipe_cancellation: None,
            pipe_registry: None,
            backing_changes: None,
            socket_ioctl: None,
            vector_terminal: None,
            actor: None,
            locks: None,
            working: Arc::new(WorkingDirectory::root()),
            terminals: None,
            terminal_tasks: None,
            fs_context: Arc::new(FsContext::default()),
            unix_socket_paths: None,
        }
    }

    pub fn with_fs_context(mut self, context: Arc<FsContext>) -> Self {
        self.fs_context = context;
        self
    }
}
