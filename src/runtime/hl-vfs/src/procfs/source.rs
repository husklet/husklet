use super::{
    AddressSpaceView, CgroupView, CpuView, DescriptorView, Error, MemoryView, MountView, NetworkView, ProcessView,
    StatView, SystemView, UtsView,
};

/// Consumer-owned boundary for obtaining coherent process snapshots.
pub trait Source: Send + Sync {
    fn processes(&self) -> Result<Vec<u32>, Error> {
        Err(Error::NotFound)
    }
    fn thread(&self, _process: u32, _thread: u32) -> Result<(), Error> {
        Err(Error::NotFound)
    }
    fn threads(&self, _process: u32) -> Result<Vec<u32>, Error> {
        Err(Error::NotFound)
    }
    fn root(&self, _process: u32) -> Result<Vec<u8>, Error> {
        Err(Error::NotFound)
    }
    fn cwd(&self, _process: u32) -> Result<Vec<u8>, Error> {
        Err(Error::NotFound)
    }
    fn process(&self, process: u32) -> Result<ProcessView, Error>;
    fn cmdline(&self, _process: u32) -> Result<Vec<u8>, Error> {
        Err(Error::NotFound)
    }
    fn environment(&self, _process: u32) -> Result<Vec<u8>, Error> {
        Err(Error::NotFound)
    }
    fn oom_score_adj(&self, _process: u32) -> Result<i16, Error> {
        Err(Error::NotFound)
    }
    fn write_oom_score_adj(
        &self,
        _process: u32,
        _actor: hl_descriptor::OperationActor,
        _value: i16,
    ) -> Result<(), hl_descriptor::ObjectError> {
        Err(hl_descriptor::ObjectError::NotSupported)
    }
    fn stat(&self, _process: u32) -> Result<StatView, Error> {
        Err(Error::NotFound)
    }
    fn memory(&self, _process: u32) -> Result<MemoryView, Error> {
        Err(Error::NotFound)
    }
    fn address_space(&self, _process: u32) -> Result<AddressSpaceView, Error> {
        Err(Error::NotFound)
    }
    fn comm(&self, process: u32, thread: Option<u32>) -> Result<Vec<u8>, Error> {
        let _ = thread;
        self.process(process).map(|process| process.comm())
    }
    fn write_comm(
        &self,
        _process: u32,
        _thread: Option<u32>,
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
    fn uts(&self, _process: u32) -> Result<UtsView, Error> {
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
    fn descriptors(&self, _process: u32) -> Result<Vec<DescriptorView>, Error> {
        Err(Error::NotFound)
    }
    fn cgroup(&self) -> Result<CgroupView, Error> {
        Err(Error::NotFound)
    }
    fn mounts(&self, _process: u32) -> Result<MountView, Error> {
        Err(Error::NotFound)
    }
    fn network(&self, _process: u32) -> Result<NetworkView, Error> {
        Err(Error::NotFound)
    }
}
