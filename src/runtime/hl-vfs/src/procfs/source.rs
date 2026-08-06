use super::{
    AddressSpaceView, CgroupView, CpuView, DescriptorView, Error, MemoryView, MountView, NetworkView, ProcessIdentity,
    ProcessView, StatView, SystemView, ThreadIdentity, UtsView,
};

/// Consumer-owned boundary for obtaining coherent process snapshots.
pub trait Source: Send + Sync {
    /// Resolves a numeric procfs lookup to the exact live process identity.
    ///
    /// Follow-on operations must migrate to accept this value directly; they
    /// must not silently resolve the numeric PID a second time.
    fn resolve_process(&self, process: u32) -> Result<ProcessIdentity, Error>;

    fn processes(&self) -> Result<Vec<u32>, Error> {
        Err(Error::NotFound)
    }
    /// Resolves a numeric TID within an exact process, or that process's exact
    /// leader when `thread` is `None`. A zombie retains its leader identity
    /// through the process snapshot even after live thread membership ends.
    fn resolve_thread(&self, _process: ProcessIdentity, _thread: Option<u32>) -> Result<ThreadIdentity, Error> {
        Err(Error::NotFound)
    }
    fn threads(&self, _process: ProcessIdentity) -> Result<Vec<u32>, Error> {
        Err(Error::NotFound)
    }
    fn root(&self, _process: ProcessIdentity) -> Result<Vec<u8>, Error> {
        Err(Error::NotFound)
    }
    fn cwd(&self, _process: ProcessIdentity) -> Result<Vec<u8>, Error> {
        Err(Error::NotFound)
    }
    fn process(&self, process: ProcessIdentity) -> Result<ProcessView, Error>;
    fn cmdline(&self, _process: ProcessIdentity) -> Result<Vec<u8>, Error> {
        Err(Error::NotFound)
    }
    fn environment(&self, _process: ProcessIdentity) -> Result<Vec<u8>, Error> {
        Err(Error::NotFound)
    }
    fn oom_score_adj(&self, _process: ProcessIdentity) -> Result<i16, Error> {
        Err(Error::NotFound)
    }
    fn write_oom_score_adj(
        &self,
        _process: ProcessIdentity,
        _actor: hl_descriptor::OperationActor,
        _value: i16,
    ) -> Result<(), hl_descriptor::ObjectError> {
        Err(hl_descriptor::ObjectError::NotSupported)
    }
    fn stat(&self, _process: ProcessIdentity) -> Result<StatView, Error> {
        Err(Error::NotFound)
    }
    fn memory(&self, _process: ProcessIdentity) -> Result<MemoryView, Error> {
        Err(Error::NotFound)
    }
    fn address_space(&self, _process: ProcessIdentity) -> Result<AddressSpaceView, Error> {
        Err(Error::NotFound)
    }
    fn comm(&self, process: ProcessIdentity, thread: ThreadIdentity) -> Result<Vec<u8>, Error> {
        let _ = thread;
        self.process(process).map(|process| process.comm())
    }
    fn write_comm(
        &self,
        _process: ProcessIdentity,
        _thread: ThreadIdentity,
        _actor: hl_descriptor::OperationActor,
        _bytes: &[u8],
    ) -> Result<(), hl_descriptor::ObjectError> {
        Err(hl_descriptor::ObjectError::NotSupported)
    }
    fn cpu(&self) -> Result<CpuView, Error>;
    fn system(&self) -> Result<SystemView, Error> {
        Err(Error::NotFound)
    }
    fn boot_identity(&self) -> Result<[u8; 16], Error> {
        Err(Error::NotFound)
    }
    fn random_identity(&self) -> Result<[u8; 16], Error> {
        Err(Error::NotFound)
    }
    fn uts(&self, _process: ProcessIdentity) -> Result<UtsView, Error> {
        Err(Error::NotFound)
    }
    fn uts_namespace(&self, _namespace: u64) -> Result<UtsView, Error> {
        Err(Error::NotFound)
    }
    fn write_uts(
        &self,
        _namespace: u64,
        _domain: bool,
        _actor: hl_descriptor::OperationActor,
        _bytes: &[u8],
    ) -> Result<(), hl_descriptor::ObjectError> {
        Err(hl_descriptor::ObjectError::NotSupported)
    }
    fn descriptors(&self, _process: ProcessIdentity) -> Result<Vec<DescriptorView>, Error> {
        Err(Error::NotFound)
    }
    fn cgroup(&self) -> Result<CgroupView, Error> {
        Err(Error::NotFound)
    }
    fn mounts(&self, _process: ProcessIdentity) -> Result<MountView, Error> {
        Err(Error::NotFound)
    }
    fn network(&self, _process: ProcessIdentity) -> Result<NetworkView, Error> {
        Err(Error::NotFound)
    }
}
