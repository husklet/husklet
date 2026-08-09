use super::{LinuxHost, abi};
use crate::native_host::{
    ChildExit, ClockKind, ForkFrame, HostClock, HostError, HostSyscalls, NativeSignal, NativeSignalMask, OwnedMapping,
    ProcessGroup, ProcessHandle, ProcessId, ProcessSignal, ProcessSyscalls, Protection, SignalSyscalls, SpawnRequest,
};
use std::ffi::CString;
use std::path::PathBuf;
use std::sync::Arc;

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "hl-engine-native-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn file_offset_metadata() {
    let temporary = TemporaryDirectory::new();
    let host = Arc::new(LinuxHost);
    let path = CString::new(temporary.0.join("value").as_os_str().as_encoded_bytes()).unwrap();
    let file = host
        .open(&path, abi::O_RDWR | abi::O_CREAT | abi::O_TRUNC, 0o600)
        .unwrap();
    assert_eq!(file.write(b"abcd"), Ok(4));
    assert_eq!(file.write_at(1, b"XY"), Ok(2));
    let mut bytes = [0; 4];
    assert_eq!(file.read_at(0, &mut bytes), Ok(4));
    assert_eq!(&bytes, b"aXYd");
    assert_eq!(file.metadata().unwrap().size, 4);

    let directory_path = CString::new(temporary.0.as_os_str().as_encoded_bytes()).unwrap();
    let directory = host.open(&directory_path, abi::O_RDONLY, 0).unwrap();
    let mut cookie = 0;
    let mut found = false;
    for _ in 0..8 {
        let mut name = [0; 64];
        let Some((entry, length)) = directory.next(cookie, &mut name).unwrap() else {
            break;
        };
        cookie = entry.cookie;
        found |= &name[..length] == b"value";
    }
    assert!(found);
}

#[test]
fn clock_page_geometry() {
    let host = Arc::new(LinuxHost);
    assert!(HostClock::new(Arc::clone(&host)).now(ClockKind::Monotonic).unwrap() > 0);
    let page = host.page_size().unwrap();
    let mapping = OwnedMapping::allocate(Arc::clone(&host), page, Protection::READ).unwrap();
    mapping.protect(Protection::READ.union(Protection::WRITE)).unwrap();
    assert!(matches!(
        mapping.protect(Protection::WRITE.union(Protection::EXECUTE)),
        Err(crate::native_host::HostError::Denied)
    ));
}

#[test]
fn abi_layouts_match() {
    assert_eq!(std::mem::size_of::<abi::timespec>(), 16);
    assert_eq!(std::mem::size_of::<abi::Statx>(), 256);
    assert_eq!(std::mem::align_of::<abi::Statx>(), 8);
}

#[test]
fn spawn_reports_exec() {
    let host = Arc::new(LinuxHost);
    let missing = SpawnRequest {
        program: CString::new("/definitely/missing/hl-engine-test").unwrap(),
        arguments: Vec::new(),
        environment: Vec::new(),
        process_group: ProcessGroup::New,
        file_actions: Vec::new(),
    };
    assert!(matches!(
        ProcessHandle::spawn(Arc::clone(&host), &missing),
        Err(crate::native_host::HostError::NotFound)
    ));

    let sleep = SpawnRequest {
        program: CString::new("/bin/sleep").unwrap(),
        arguments: vec![CString::new("30").unwrap()],
        environment: Vec::new(),
        process_group: ProcessGroup::New,
        file_actions: Vec::new(),
    };
    let process = ProcessHandle::spawn(host, &sleep).unwrap();
    process.signal_group(ProcessSignal::Terminate).unwrap();
    let mut exit = None;
    for _ in 0..10_000 {
        exit = process.wait().unwrap();
        if exit.is_some() {
            break;
        }
        std::thread::yield_now();
    }
    assert_eq!(exit, Some(ChildExit::Signal(15)));
}

/// A Linux host numbers signals as the guest ABI does, so an arbitrary number — including a
/// real-time one — reaches the child unchanged. Measured against this box's kernel.
///
/// The caller's signal mask is deliberately full: a blocked mask, like an ignored disposition,
/// survives execve, so a child that inherited either would never see these signals at all.
#[test]
fn arbitrary_signal_numbers_reach_a_host_child() {
    // 28 (WINCH) and 19 (STOP) are omitted: their default actions do not exit the child.
    let numbers = [1_u8, 6, 10, 34, 64];
    let blocked = numbers.iter().fold(NativeSignalMask::default(), |mask, number| {
        mask.with(NativeSignal::new(*number).unwrap())
    });
    let previous = SignalSyscalls::signal_block(&LinuxHost, blocked).unwrap();
    for number in numbers {
        let host = Arc::new(LinuxHost);
        let sleep = SpawnRequest {
            program: CString::new("/bin/sleep").unwrap(),
            arguments: vec![CString::new("30").unwrap()],
            environment: Vec::new(),
            process_group: ProcessGroup::New,
            file_actions: Vec::new(),
        };
        let process = ProcessHandle::spawn(host, &sleep).unwrap();
        process.signal_group(ProcessSignal::Number(number)).unwrap();
        let mut exit = None;
        for _ in 0..1_000_000 {
            exit = process.wait().unwrap();
            if exit.is_some() {
                break;
            }
            std::thread::yield_now();
        }
        assert_eq!(exit, Some(ChildExit::Signal(number)), "signal {number}");
    }
    SignalSyscalls::signal_restore(&LinuxHost, previous).unwrap();
}

#[test]
fn missing_signal_target() {
    let host = LinuxHost;
    let missing = ProcessId::new(i32::MAX as u32).unwrap();
    assert_eq!(
        ProcessSyscalls::signal(&host, missing, ProcessSignal::Kill),
        Err(HostError::NotFound),
    );
    assert_eq!(
        ProcessSyscalls::signal_group(&host, missing, ProcessSignal::Kill),
        Err(HostError::NotFound),
    );
}

#[test]
fn child_channel_is() {
    let host = Arc::new(LinuxHost);
    let (mut sender, mut receiver) = host.channel_pair().unwrap();
    let frame = ForkFrame::new(vec![7; 8192]).unwrap();
    sender.begin_send(&frame).unwrap();
    let mut received = None;
    for _ in 0..10_000 {
        let _ = sender.flush();
        match receiver.receive() {
            Ok(frame) => {
                received = frame;
                break;
            }
            Err(crate::native_host::ForkWireError::Host(crate::native_host::HostError::WouldBlock)) => {
                std::thread::yield_now();
            }
            Err(error) => panic!("wire failure: {error:?}"),
        }
    }
    assert_eq!(received, Some(frame));
}

#[test]
fn concurrent_children_reap() {
    let host = Arc::new(LinuxHost);
    let request = SpawnRequest {
        program: CString::new("/bin/true").unwrap(),
        arguments: Vec::new(),
        environment: Vec::new(),
        process_group: ProcessGroup::Inherit,
        file_actions: Vec::new(),
    };
    let children: Vec<_> = (0..8)
        .map(|_| ProcessHandle::spawn(Arc::clone(&host), &request).unwrap())
        .collect();
    for child in children {
        assert_eq!(child.wait_blocking(), Ok(ChildExit::Code(0)));
    }
}
