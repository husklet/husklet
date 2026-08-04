use std::sync::Mutex;

use hl_event::{EpollEvent, TimerSetting};
use hl_isa::GuestArchitecture;
use hl_time::Duration;

use crate::{EpollOperation, EventAbi, EventMarshalError, GuestAccess, GuestFault, GuestMarshaller, GuestMemory};

const BASE: u64 = 0x1000;

struct Memory(Mutex<Vec<u8>>);

impl Memory {
    fn new() -> Self {
        Self(Mutex::new(vec![0; 0x3000]))
    }

    fn offset(address: u64, access: GuestAccess) -> Result<usize, GuestFault> {
        usize::try_from(address.checked_sub(BASE).ok_or(GuestFault { address, access })?)
            .map_err(|_| GuestFault { address, access })
    }

    fn put(&self, address: u64, bytes: &[u8]) {
        let offset = Self::offset(address, GuestAccess::Write).unwrap();
        self.0.lock().unwrap()[offset..offset + bytes.len()].copy_from_slice(bytes);
    }

    fn get(&self, address: u64, length: usize) -> Vec<u8> {
        let offset = Self::offset(address, GuestAccess::Read).unwrap();
        self.0.lock().unwrap()[offset..offset + length].to_vec()
    }
}

impl GuestMemory for Memory {
    fn probe(&self, address: u64, length: usize, access: GuestAccess) -> Result<usize, GuestFault> {
        let offset = Self::offset(address, access)?;
        let available = self.0.lock().unwrap().len().saturating_sub(offset);
        if length != 0 && available == 0 {
            return Err(GuestFault { address, access });
        }
        Ok(length.min(available))
    }

    fn read(&self, address: u64, output: &mut [u8]) -> Result<usize, GuestFault> {
        let count = self.probe(address, output.len(), GuestAccess::Read)?;
        let offset = Self::offset(address, GuestAccess::Read)?;
        output[..count].copy_from_slice(&self.0.lock().unwrap()[offset..offset + count]);
        Ok(count)
    }

    fn write(&self, address: u64, input: &[u8]) -> Result<usize, GuestFault> {
        let count = self.probe(address, input.len(), GuestAccess::Write)?;
        let offset = Self::offset(address, GuestAccess::Write)?;
        self.0.lock().unwrap()[offset..offset + count].copy_from_slice(&input[..count]);
        Ok(count)
    }
}

#[test]
fn epoll_event_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let memory = Memory::new();
        let abi = EventAbi::new(&memory, architecture);
        let mut fixture = Vec::new();
        fixture.extend_from_slice(&0x8000_0001_u32.to_le_bytes());
        if architecture == GuestArchitecture::Aarch64 {
            fixture.extend_from_slice(&[0; 4]);
        }
        fixture.extend_from_slice(&0x1122_3344_5566_7788_u64.to_le_bytes());
        memory.put(BASE, &fixture);
        let plan = abi.epoll_control(1, 9, BASE).unwrap();
        assert_eq!(plan.operation, EpollOperation::Add);
        assert_eq!(plan.interests.unwrap().bits(), 0x8000_0001);
        assert_eq!(plan.data, Some(0x1122_3344_5566_7788));

        let wait = abi.epoll_wait(BASE + 0x100, 1, 0, 0, 99).unwrap();
        let staged = abi
            .stage_epoll_events(
                &wait,
                &[EpollEvent {
                    readiness: hl_descriptor::Readiness::from_bits(
                        hl_descriptor::Readiness::READ | hl_descriptor::Readiness::WRITE,
                    ),
                    data: 0xaabb_ccdd_eeff_0011,
                }],
            )
            .unwrap();
        staged.commit(&GuestMarshaller::new(&memory, architecture)).unwrap();
        let mut expected = 5_u32.to_le_bytes().to_vec();
        if architecture == GuestArchitecture::Aarch64 {
            expected.extend_from_slice(&[0; 4]);
        }
        expected.extend_from_slice(&0xaabb_ccdd_eeff_0011_u64.to_le_bytes());
        assert_eq!(memory.get(BASE + 0x100, expected.len()), expected);
    }
}

#[test]
fn wait_signalfd_pointers() {
    let memory = Memory::new();
    let abi = EventAbi::new(&memory, GuestArchitecture::X86_64);
    assert_eq!(
        abi.epoll_wait(u64::MAX, 0, 0, u64::MAX, 7),
        Err(EventMarshalError::Invalid),
    );
    assert_eq!(abi.signalfd4(-1, u64::MAX, 7, 0), Err(EventMarshalError::Invalid),);
    assert!(matches!(
        abi.signalfd4(-1, u64::MAX, 8, 0),
        Err(EventMarshalError::Marshal(_)),
    ));
    assert_eq!(abi.epoll_control(2, 4, u64::MAX).unwrap().data, None);
}

#[test]
fn creation_flags_local() {
    let (_, neutral, split) = EventAbi::<Memory>::eventfd2(3, 1 | 0x800 | 0x8_0000).unwrap();
    assert!(split.close_on_exec);
    assert!(split.nonblocking);
    assert_eq!(
        neutral.bits(),
        hl_event::EventFdFlags::SEMAPHORE | hl_event::EventFdFlags::NONBLOCKING,
    );
    assert!(EventAbi::<Memory>::epoll_create1(0x800).is_err());
    assert!(EventAbi::<Memory>::timerfd_create(6, 0).is_err());
    assert!(EventAbi::<Memory>::inotify_init1(1).is_err());
}

#[test]
fn timer_layout_linux() {
    let memory = Memory::new();
    let abi = EventAbi::new(&memory, GuestArchitecture::Aarch64);
    assert!(matches!(
        abi.timerfd_settime(4, u64::MAX, 0),
        Err(EventMarshalError::Marshal(_)),
    ));
    let mut timer = Vec::new();
    for value in [2_i64, 3, 4, 5] {
        timer.extend_from_slice(&value.to_le_bytes());
    }
    memory.put(BASE, &timer);
    assert_eq!(abi.timerfd_settime(4, BASE, 0), Err(EventMarshalError::Invalid),);
    let setting = TimerSetting {
        interval: Duration::from_nanoseconds(2_000_000_003),
        value: Duration::from_nanoseconds(4_000_000_005),
    };
    let staged = abi.timerfd_gettime_copyout(BASE + 0x100, setting).unwrap();
    assert_eq!(memory.get(BASE + 0x100, 32), vec![0; 32]);
    staged
        .commit(&GuestMarshaller::new(&memory, GuestArchitecture::Aarch64))
        .unwrap();
    assert_eq!(memory.get(BASE + 0x100, 32), timer);
}

#[test]
fn inotify_path_bounded() {
    let memory = Memory::new();
    memory.put(BASE, b"/tmp/watch\0");
    let abi = EventAbi::new(&memory, GuestArchitecture::X86_64);
    let plan = abi.inotify_add_watch(BASE, hl_event::InotifyMask::CREATE).unwrap();
    assert_eq!(plan.path, b"/tmp/watch");
    assert!(abi.inotify_add_watch(BASE, 0).is_err());
    assert!(abi.inotify_add_watch(u64::MAX, hl_event::InotifyMask::CREATE).is_err());
}
