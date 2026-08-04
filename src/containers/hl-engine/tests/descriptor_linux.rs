#![cfg(target_os = "linux")]

use hl_engine::native_host::{
    ChildExit, DescriptorInstall, ForkFrame, HostError, LinuxHost, NativeDescriptor, PrivateDescriptorAllocator,
    ProcessGroup, ProcessHandle, SpawnRequest,
};
use std::ffi::CString;
use std::sync::Arc;

struct Collector {
    expected: usize,
    descriptors: Vec<NativeDescriptor<LinuxHost>>,
}

impl DescriptorInstall<LinuxHost> for Collector {
    fn begin(&mut self, count: usize) -> Result<(), HostError> {
        if count != self.expected {
            return Err(HostError::Exhausted);
        }
        Ok(())
    }

    fn install(&mut self, descriptor: NativeDescriptor<LinuxHost>) -> Result<(), HostError> {
        self.descriptors.push(descriptor);
        Ok(())
    }

    fn commit(&mut self) -> Result<(), HostError> {
        Ok(())
    }

    fn rollback(&mut self) {
        self.descriptors.clear();
    }
}

#[test]
fn socketpair_transfers_multiple() {
    let host = Arc::new(LinuxHost);
    let file_a = host.open(c"/dev/null", 0, 0).unwrap();
    let file_b = host.open(c"/dev/zero", 0, 0).unwrap();
    let descriptor_a = host.descriptor_from_file(&file_a).unwrap();
    let descriptor_b = host.descriptor_from_file(&file_b).unwrap();
    let (mut sender, receiver) = host.channel_pair().unwrap();
    let frame = ForkFrame::new(b"provider".to_vec()).unwrap();
    sender
        .send_with_descriptors(&frame, &[&descriptor_a, &descriptor_b])
        .unwrap();
    drop(descriptor_a);
    drop(descriptor_b);
    drop(file_a);
    drop(file_b);
    let mut collector = Collector {
        expected: 2,
        descriptors: Vec::new(),
    };
    let (received, credentials) = receiver.receive_and_install(2, &mut collector).unwrap();
    assert_eq!(received, frame);
    assert_eq!(collector.descriptors.len(), 2);
    assert_eq!(credentials.unwrap().process, std::process::id());
}

#[test]
fn receiver_capacity_rejects() {
    let host = Arc::new(LinuxHost);
    let file = host.open(c"/dev/null", 0, 0).unwrap();
    let descriptor_a = host.descriptor_from_file(&file).unwrap();
    let descriptor_b = host.descriptor_from_file(&file).unwrap();
    let (mut sender, receiver) = host.channel_pair().unwrap();
    let frame = ForkFrame::new(b"bounded".to_vec()).unwrap();
    sender
        .send_with_descriptors(&frame, &[&descriptor_a, &descriptor_b])
        .unwrap();
    assert!(matches!(
        receiver.receive_with_descriptors(1),
        Err(hl_engine::native_host::ForkWireError::Host(HostError::Exhausted))
    ));
}

#[test]
fn private_high_band() {
    let host = Arc::new(LinuxHost);
    let file = host.open(c"/dev/null", 0, 0).unwrap();
    let source = host.descriptor_from_file(&file).unwrap();
    let allocator = PrivateDescriptorAllocator::new(Arc::clone(&host), 512, 2).unwrap();
    let stale = allocator.adopt(&source).unwrap();
    allocator.release(stale).unwrap();
    let replacement = allocator.adopt(&source).unwrap();
    let request = SpawnRequest {
        program: CString::new("/bin/sh").unwrap(),
        arguments: vec![
            CString::new("-c").unwrap(),
            CString::new("for f in /proc/self/fd/*; do n=${f##*/}; [ \"$n\" -lt 512 ] || exit 91; done").unwrap(),
        ],
        environment: Vec::new(),
        process_group: ProcessGroup::Inherit,
        file_actions: Vec::new(),
    };
    let child = ProcessHandle::spawn(Arc::clone(&host), &request).unwrap();
    assert_eq!(child.wait_blocking(), Ok(ChildExit::Code(0)));
    allocator.release(replacement).unwrap();
    assert_eq!(allocator.release(stale), Err(HostError::Invalid));
}

#[test]
fn nonblocking_pressure_queues() {
    let host = Arc::new(LinuxHost);
    let file = host.open(c"/dev/null", 0, 0).unwrap();
    let descriptor = host.descriptor_from_file(&file).unwrap();
    let (mut sender, receiver) = host.channel_pair().unwrap();
    let frame = ForkFrame::new(vec![7; 1 << 20]).unwrap();
    assert!(matches!(
        sender.send_with_descriptors(&frame, &[&descriptor]),
        Err(hl_engine::native_host::ForkWireError::Host(HostError::WouldBlock))
    ));
    sender.cancel_send();
    drop(sender);
    drop(receiver);
}
