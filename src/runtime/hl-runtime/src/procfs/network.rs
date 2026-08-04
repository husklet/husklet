use hl_vfs::ProcfsNetworkView;

/// Producer boundary for a process-visible network namespace snapshot.
pub trait NetworkPort: Send + Sync {
    fn view(&self) -> ProcfsNetworkView;
}
