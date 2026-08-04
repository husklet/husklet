use super::*;

#[derive(Debug)]
struct ResidencyHost {
    calls: Mutex<usize>,
    result: Result<Vec<bool>, RuntimeMemoryError>,
}

impl ResidencyHost {
    fn calls(&self) -> usize {
        *self.calls.lock().unwrap()
    }
}

impl RuntimeMemoryHost for ResidencyHost {
    fn advise(&self, _: hl_linux::AdvicePlan) -> Result<(), RuntimeMemoryError> {
        Err(RuntimeMemoryError::Unsupported)
    }

    fn residency(&self, _: hl_linux::MemoryRangePlan) -> Result<Vec<bool>, RuntimeMemoryError> {
        *self.calls.lock().unwrap() += 1;
        self.result.clone()
    }

    fn lock(&self, _: Option<hl_linux::MemoryRangePlan>, _: bool) -> Result<(), RuntimeMemoryError> {
        Err(RuntimeMemoryError::Unsupported)
    }

    fn unlock(&self, _: Option<hl_linux::MemoryRangePlan>) -> Result<(), RuntimeMemoryError> {
        Err(RuntimeMemoryError::Unsupported)
    }

    fn lock_all(&self, _: hl_linux::LockAllPlan) -> Result<(), RuntimeMemoryError> {
        Err(RuntimeMemoryError::Unsupported)
    }

    fn unlock_all(&self) -> Result<(), RuntimeMemoryError> {
        Err(RuntimeMemoryError::Unsupported)
    }

    fn sync(&self, _: hl_linux::MsyncPlan) -> Result<(), RuntimeMemoryError> {
        Err(RuntimeMemoryError::Unsupported)
    }
}

fn runtime_with_residency(
    fixture: &Fixture,
    architecture: GuestArchitecture,
    host: Arc<ResidencyHost>,
) -> RuntimeMemorySyscalls<Mapping, Memory> {
    RuntimeMemorySyscalls::new(
        fixture.coordinator.clone(),
        fixture.descriptors.clone(),
        fixture.memory.clone(),
        architecture,
    )
    .with_address_minimum(4096)
    .with_host(host)
}

#[test]
fn isas_linux_order() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let host = Arc::new(ResidencyHost {
            calls: Mutex::new(0),
            result: Ok(vec![true, false]),
        });
        let mut runtime = runtime_with_residency(&fixture, architecture, host.clone());

        assert_eq!(
            runtime.handle(Fixture::operation("mincore"), [1, 0, u64::MAX, 0, 0, 0],),
            LinuxResult::Error(Errno::EINVAL),
        );
        assert_eq!(
            runtime.handle(Fixture::operation("mincore"), [0x4001, 4096, u64::MAX, 0, 0, 0],),
            LinuxResult::Error(Errno::EINVAL),
        );
        assert_eq!(
            runtime.handle(Fixture::operation("mincore"), [0x4000, 8192, 512, 0, 0, 0],),
            LinuxResult::Error(Errno::ENOMEM),
        );
        assert_eq!(host.calls(), 0);

        for address in [0x4000, 0x5000] {
            assert_eq!(
                runtime.handle(Fixture::operation("mmap"), [address, 4096, 1, 0x22, u64::MAX, 0],),
                LinuxResult::Value(address),
            );
        }
        assert_eq!(
            runtime.handle(Fixture::operation("mincore"), [0x4000, 8192, 512, 0, 0, 0],),
            LinuxResult::Error(Errno::EFAULT),
        );
        assert_eq!(host.calls(), 0);
        assert_eq!(
            runtime.handle(Fixture::operation("mincore"), [0x4000, 8192, 32, 0, 0, 0],),
            LinuxResult::Value(0),
        );
        assert_eq!(host.calls(), 1);
        assert_eq!(&fixture.memory.inner.bytes.lock().unwrap()[32..34], &[1, 0],);
    }
}

#[test]
fn mincore_copies_out() {
    for result in [Err(RuntimeMemoryError::Failed), Ok(vec![true])] {
        let fixture = Fixture::new();
        let host = Arc::new(ResidencyHost {
            calls: Mutex::new(0),
            result,
        });
        let mut runtime = runtime_with_residency(&fixture, GuestArchitecture::X86_64, host);
        runtime.handle(Fixture::operation("mmap"), [0x4000, 8192, 1, 0x22, u64::MAX, 0]);
        assert_eq!(
            runtime.handle(Fixture::operation("mincore"), [0x4000, 8192, 32, 0, 0, 0],),
            LinuxResult::Error(Errno::EIO),
        );
        assert_eq!(&fixture.memory.inner.bytes.lock().unwrap()[32..34], &[0, 0],);
    }
}
