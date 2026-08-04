use std::sync::Mutex;

use hl_isa::GuestArchitecture;
use hl_sync::FutexClock;
use hl_task::{AlternateStack, SignalAction, SignalDisposition, SignalMask, SignalNumber};
use hl_time::Timespec;

use crate::{
    ClockIdentity, FutexOperation, GuestAccess, GuestFault, GuestMarshaller, GuestMemory, SignalAbi,
    SignalMarshalError, TimeFutexAbi, TimeFutexMarshalError, TimerPlan,
};

const BASE: u64 = 0x1000;

struct Memory(Mutex<Vec<u8>>);

impl Memory {
    fn new() -> Self {
        Self(Mutex::new(vec![0; 0x8000]))
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
fn action_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let memory = Memory::new();
        let abi = SignalAbi::new(&memory, architecture);
        let action = SignalAction {
            disposition: SignalDisposition::Handler(0x1234),
            flags: 0x0400_0000,
            restorer: 0x5678,
            mask: SignalMask::from_bits(5),
        };
        let staged = abi.stage_action(BASE, action).unwrap();
        assert_eq!(memory.get(BASE, 32), vec![0; 32]);
        staged.commit(&GuestMarshaller::new(&memory, architecture)).unwrap();
        assert_eq!(memory.get(BASE, 8), 0x1234_u64.to_le_bytes());
        assert_eq!(memory.get(BASE + 24, 8), 5_u64.to_le_bytes());
        assert_eq!(abi.action(10, BASE, 8).unwrap().1, Some(action));
    }
}

#[test]
fn sigset_size_access() {
    let memory = Memory::new();
    let abi = SignalAbi::new(&memory, GuestArchitecture::X86_64);
    assert_eq!(abi.action(10, u64::MAX, 16), Err(SignalMarshalError::Invalid));
    assert_eq!(abi.action(65, u64::MAX, 8), Err(SignalMarshalError::Invalid));
    assert_eq!(abi.mask(99, u64::MAX, 8), Err(SignalMarshalError::Invalid));
}

#[test]
fn alternate_stack_staged() {
    let memory = Memory::new();
    let abi = SignalAbi::new(&memory, GuestArchitecture::Aarch64);
    let stack = AlternateStack::Enabled {
        pointer: 0x4000,
        size: 8192,
    };
    let staged = abi.stage_alternate_stack(BASE, stack).unwrap();
    staged
        .commit(&GuestMarshaller::new(&memory, GuestArchitecture::Aarch64))
        .unwrap();
    assert_eq!(abi.alternate_stack(BASE).unwrap(), Some(stack));
    let stack = AlternateStack::Autodisarm {
        pointer: 0x6000,
        size: 8192,
    };
    abi.stage_alternate_stack(BASE, stack)
        .unwrap()
        .commit(&GuestMarshaller::new(&memory, GuestArchitecture::Aarch64))
        .unwrap();
    assert_eq!(abi.alternate_stack(BASE).unwrap(), Some(stack));

    let info = hl_task::SignalInfo::bare(SignalNumber::new(12).unwrap());
    let staged = abi.stage_info(BASE + 0x100, info).unwrap();
    assert_eq!(memory.get(BASE + 0x100, 128), vec![0; 128]);
    staged
        .commit(&GuestMarshaller::new(&memory, GuestArchitecture::Aarch64))
        .unwrap();
    assert_eq!(memory.get(BASE + 0x100, 4), 12_i32.to_le_bytes());
}

#[test]
fn stack_validation() {
    let memory = Memory::new();
    let abi = SignalAbi::new(&memory, GuestArchitecture::X86_64);
    let mut bytes = [0_u8; 24];
    bytes[..8].copy_from_slice(&0x4000_u64.to_le_bytes());
    bytes[16..24].copy_from_slice(&1_u64.to_le_bytes());
    memory.put(BASE, &bytes);
    assert_eq!(abi.alternate_stack(BASE), Err(SignalMarshalError::NoMemory));
    assert_eq!(SignalMarshalError::NoMemory.errno(), crate::Errno::ENOMEM);

    bytes[8..12].copy_from_slice(&0x40_u32.to_le_bytes());
    bytes[16..24].copy_from_slice(&8192_u64.to_le_bytes());
    memory.put(BASE, &bytes);
    assert_eq!(abi.alternate_stack(BASE), Err(SignalMarshalError::Invalid));
}

#[test]
fn queued_copyin() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let memory = Memory::new();
        let abi = SignalAbi::new(&memory, architecture);
        let mut bytes = [0_u8; 128];
        bytes[..4].copy_from_slice(&9_i32.to_le_bytes());
        bytes[4..8].copy_from_slice(&3_i32.to_le_bytes());
        bytes[8..12].copy_from_slice(&(-1_i32).to_le_bytes());
        bytes[16..20].copy_from_slice(&17_u32.to_le_bytes());
        bytes[20..24].copy_from_slice(&19_u32.to_le_bytes());
        bytes[24..32].copy_from_slice(&23_u64.to_le_bytes());
        memory.put(BASE, &bytes);

        let queued = abi.queued_info(7, 35, BASE).unwrap();
        let info = queued.info.unwrap();
        assert_eq!(queued.target, 7);
        assert_eq!(queued.code, -1);
        assert_eq!(info.signal.get(), 35);
        assert_eq!(
            (info.error, info.sender_process, info.sender_user, info.value),
            (3, 17, 19, 23)
        );
        assert_eq!(abi.queued_info(7, 0, BASE).unwrap().info, None);
        assert_eq!(abi.queued_info(7, 65, u64::MAX), Err(SignalMarshalError::Fault));
    }
}

#[test]
fn time_structures_bytes() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let memory = Memory::new();
        let abi = TimeFutexAbi::new(&memory, architecture);
        memory.put(BASE, &1_i64.to_le_bytes());
        memory.put(BASE + 8, &1_000_000_000_i64.to_le_bytes());
        assert_eq!(abi.nanosleep(BASE, 0), Err(TimeFutexMarshalError::Invalid),);
        let time = Timespec::new(7, 8).unwrap();
        assert_eq!(abi.stage_timespec(u64::MAX, time), Err(TimeFutexMarshalError::Fault));
        let staged = abi.stage_timespec(BASE + 0x100, time).unwrap();
        assert_eq!(memory.get(BASE + 0x100, 16), vec![0; 16]);
        staged.commit(&GuestMarshaller::new(&memory, architecture)).unwrap();
        assert_eq!(memory.get(BASE + 0x100, 8), 7_i64.to_le_bytes());
    }
}

#[test]
fn sleep_clock_admission_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let memory = Memory::new();
        memory.put(BASE, &[0; 16]);
        let abi = TimeFutexAbi::new(&memory, architecture);
        assert_eq!(abi.clock_nanosleep(2, 1, BASE, 0).unwrap().0, ClockIdentity::ProcessCpu);
        for clock in [3, 4, 5, 6] {
            assert_eq!(
                abi.clock_nanosleep(clock, 1, BASE, 0),
                Err(TimeFutexMarshalError::Unsupported)
            );
        }
    }
}

#[test]
fn futex_decodes_precedence() {
    let memory = Memory::new();
    let abi = TimeFutexAbi::new(&memory, GuestArchitecture::X86_64);
    assert_eq!(
        abi.futex(BASE + 1, 511, 0, u64::MAX, 0, 0),
        Err(TimeFutexMarshalError::Invalid),
    );
    assert_eq!(
        abi.futex(BASE, 1 | 256, 0, u64::MAX, 0, 0),
        Err(TimeFutexMarshalError::Invalid),
    );
    memory.put(BASE + 0x100, &1_i64.to_le_bytes());
    memory.put(BASE + 0x108, &2_i64.to_le_bytes());
    let plan = abi.futex(BASE, 9 | 128, 3, BASE + 0x100, 0, 0xff).unwrap();
    assert_eq!(plan.operation, FutexOperation::WaitBitset);
    assert!(plan.private);
    assert_eq!(plan.bitset, 0xff);
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let abi = TimeFutexAbi::new(&memory, architecture);
        let lock_pi = abi.futex(BASE, 6, 0, BASE + 0x100, 0, 0).unwrap();
        assert_eq!(lock_pi.deadline.unwrap().clock, FutexClock::Realtime);
        let lock_pi2 = abi.futex(BASE, 13 | 256, 0, BASE + 0x100, 0, 0).unwrap();
        assert_eq!(lock_pi2.deadline.unwrap().clock, FutexClock::Realtime);
        let lock_pi2 = abi.futex(BASE, 13, 0, BASE + 0x100, 0, 0).unwrap();
        assert_eq!(lock_pi2.deadline.unwrap().clock, FutexClock::Monotonic);
    }
}

#[test]
fn futex_waitv_bytes() {
    let memory = Memory::new();
    let abi = TimeFutexAbi::new(&memory, GuestArchitecture::Aarch64);
    assert_eq!(
        abi.wait_vectors(u64::MAX, 129, 0, 0, 0),
        Err(TimeFutexMarshalError::Invalid),
    );
    let mut vector = [0_u8; 24];
    vector[..8].copy_from_slice(&9_u64.to_le_bytes());
    vector[8..16].copy_from_slice(&BASE.to_le_bytes());
    vector[16..20].copy_from_slice(&2_u32.to_le_bytes());
    vector[20..24].copy_from_slice(&1_u32.to_le_bytes());
    memory.put(BASE + 0x200, &vector);
    assert_eq!(
        abi.wait_vectors(BASE + 0x200, 1, 0, 0, 0),
        Err(TimeFutexMarshalError::Invalid),
    );
    vector[20..24].fill(0);
    memory.put(BASE + 0x200, &vector);
    assert_eq!(abi.wait_vectors(BASE + 0x200, 1, 0, 0, 0).unwrap().0[0].value, 9,);
}

#[test]
fn robust_list_copyouts() {
    let memory = Memory::new();
    let abi = TimeFutexAbi::new(&memory, GuestArchitecture::X86_64);
    assert_eq!(abi.robust_list(BASE + 1, 24).unwrap().head, BASE + 1);
    assert_eq!(abi.robust_list(BASE, 23), Err(TimeFutexMarshalError::Invalid),);
    let (head, length) = abi.stage_robust_list(BASE, BASE + 8, 0x8000).unwrap();
    assert_eq!(memory.get(BASE, 16), vec![0; 16]);
    head.commit(&GuestMarshaller::new(&memory, GuestArchitecture::X86_64))
        .unwrap();
    length
        .commit(&GuestMarshaller::new(&memory, GuestArchitecture::X86_64))
        .unwrap();
    assert_eq!(memory.get(BASE, 8), 0x8000_u64.to_le_bytes());
    assert_eq!(memory.get(BASE + 8, 8), 24_u64.to_le_bytes());
}

#[test]
fn interval_timer_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let memory = Memory::new();
        let mut bytes = [0_u8; 32];
        bytes[0..8].copy_from_slice(&2_i64.to_le_bytes());
        bytes[8..16].copy_from_slice(&3_i64.to_le_bytes());
        bytes[16..24].copy_from_slice(&4_i64.to_le_bytes());
        bytes[24..32].copy_from_slice(&5_i64.to_le_bytes());
        memory.put(BASE, &bytes);
        let abi = TimeFutexAbi::new(&memory, architecture);
        let timer = abi.interval_timer(BASE).unwrap();
        assert_eq!(timer.interval, Timespec::new(2, 3_000).unwrap());
        assert_eq!(timer.value, Timespec::new(4, 5_000).unwrap());
        abi.stage_interval(BASE + 0x100, timer)
            .unwrap()
            .commit(&GuestMarshaller::new(&memory, architecture))
            .unwrap();
        assert_eq!(memory.get(BASE + 0x100, 32), bytes);
    }
}

#[test]
fn rejects_usec() {
    let memory = Memory::new();
    let mut bytes = [0_u8; 32];
    bytes[8..16].copy_from_slice(&1_000_000_i64.to_le_bytes());
    memory.put(BASE, &bytes);
    assert_eq!(
        TimeFutexAbi::new(&memory, GuestArchitecture::Aarch64).interval_timer(BASE),
        Err(TimeFutexMarshalError::Invalid),
    );
}

#[test]
fn posix_sigevent_decodes_only_the_linux_payload() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let memory = Memory::new();
        let none = BASE + 0x7ff0;
        let mut prefix = [0_u8; 16];
        prefix[..8].copy_from_slice(&0x1122_3344_5566_7788_u64.to_le_bytes());
        prefix[8..12].copy_from_slice(&99_i32.to_le_bytes());
        prefix[12..16].copy_from_slice(&1_i32.to_le_bytes());
        memory.put(none, &prefix);
        let TimerPlan::Create { event: Some(event), .. } = TimeFutexAbi::new(&memory, architecture)
            .timer_create(1, none, BASE)
            .unwrap()
        else {
            panic!("create plan");
        };
        assert_eq!(event.value, 0x1122_3344_5566_7788);
        assert_eq!(event.signal, 99); // SIGEV_NONE ignores sigev_signo.
        assert_eq!(event.notification, 1);
        assert_eq!(event.target_thread, None);

        let directed = BASE + 0x7fec;
        prefix[8..12].copy_from_slice(&35_i32.to_le_bytes());
        prefix[12..16].copy_from_slice(&4_i32.to_le_bytes());
        memory.put(directed, &prefix);
        memory.put(directed + 16, &1234_i32.to_le_bytes());
        let TimerPlan::Create { event: Some(event), .. } = TimeFutexAbi::new(&memory, architecture)
            .timer_create(1, directed, BASE)
            .unwrap()
        else {
            panic!("create plan");
        };
        assert_eq!(event.target_thread, Some(1234));
    }
}
