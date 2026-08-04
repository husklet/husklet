use super::*;

#[test]
fn scalar_nonblocking_errno() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime(architecture);
        assert_eq!(
            runtime.handle(Fixture::operation("socketpair"), [1, 1, 0, 32, 0, 0]),
            LinuxResult::Value(0),
        );
        fixture.memory.inner.bytes.lock().unwrap()[80..84].copy_from_slice(b"data");
        assert_eq!(
            runtime.handle(Fixture::operation("send"), [0, 80, 4, 0, 0, 0]),
            LinuxResult::Value(4),
        );
        assert_eq!(
            runtime.handle(Fixture::operation("recv"), [1, 96, 2, 0, 0, 0]),
            LinuxResult::Value(2),
        );
        assert_eq!(&fixture.memory.inner.bytes.lock().unwrap()[96..98], b"da");
        assert_eq!(
            runtime.handle(Fixture::operation("recv"), [1, 98, 4, 0x40, 0, 0]),
            LinuxResult::Value(2),
        );
        assert_eq!(&fixture.memory.inner.bytes.lock().unwrap()[98..100], b"ta");
        assert_eq!(
            runtime.handle(Fixture::operation("recv"), [1, 100, 1, 0x40, 0, 0]),
            LinuxResult::Error(Errno::EAGAIN),
        );
    }
}

#[test]
fn addressed_result_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime(architecture);
        let fd = match runtime.handle(Fixture::operation("socket"), [2, 2, 17, 0, 0, 0]) {
            LinuxResult::Value(value) => value,
            other => panic!("socket failed: {other:?}"),
        };
        fixture.memory.put(80, b"dns?");
        fixture
            .memory
            .put(96, &[2, 0, 0, 53, 127, 0, 0, 11, 0, 0, 0, 0, 0, 0, 0, 0]);
        fixture.host.state.lock().unwrap().send_to_result = Some(Ok(3));

        assert_eq!(
            runtime.handle(Fixture::operation("sendto"), [fd, 80, 4, 0x40, 96, 16],),
            LinuxResult::Value(3),
        );
        assert_eq!(
            fixture.host.state.lock().unwrap().sent_to,
            [(
                1,
                b"dns?".to_vec(),
                SocketAddress::Inet4 {
                    address: [127, 0, 0, 11],
                    port: 53,
                },
                true,
            )],
        );
    }
}

#[test]
fn addressed_host_mutation() {
    let fixture = Fixture::new();
    let mut runtime = fixture.runtime(GuestArchitecture::Aarch64);
    let fd = match runtime.handle(Fixture::operation("socket"), [2, 2, 17, 0, 0, 0]) {
        LinuxResult::Value(value) => value,
        other => panic!("socket failed: {other:?}"),
    };
    fixture.memory.put(80, b"dns?");

    assert_eq!(
        runtime.handle(Fixture::operation("sendto"), [fd, 80, 4, 0, 600, 16],),
        LinuxResult::Error(Errno::EFAULT),
    );
    assert!(fixture.host.state.lock().unwrap().sent_to.is_empty());
}

#[test]
fn recvfrom_source_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime(architecture);
        let fd = match runtime.handle(Fixture::operation("socket"), [2, 2, 17, 0, 0, 0]) {
            LinuxResult::Value(value) => value,
            other => panic!("socket failed: {other:?}"),
        };
        {
            let mut state = fixture.host.state.lock().unwrap();
            state.receive_from_data = b"answer".to_vec();
            state.receive_from_result = Some(Ok(crate::ReceivedDatagram {
                count: 3,
                full_length: 3,
                source: SocketAddress::Inet4 {
                    address: [127, 0, 0, 11],
                    port: 53,
                },
            }));
        }
        fixture.memory.put(64, &4_u32.to_le_bytes());

        assert_eq!(
            runtime.handle(Fixture::operation("recvfrom"), [fd, 80, 6, 0x40, 96, 64],),
            LinuxResult::Value(3),
        );
        let bytes = fixture.memory.inner.bytes.lock().unwrap();
        assert_eq!(&bytes[80..83], b"ans");
        assert_eq!(&bytes[96..100], &[2, 0, 0, 53]);
        assert_eq!(&bytes[64..68], &16_u32.to_le_bytes());
        drop(bytes);
        assert_eq!(fixture.host.state.lock().unwrap().received_from, [(1, 6, true)],);
    }
}

#[test]
fn recvfrom_length_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime(architecture);
        let LinuxResult::Value(fd) = runtime.handle(Fixture::operation("socket"), [2, 2, 17, 0, 0, 0]) else {
            panic!("socket failed");
        };
        fixture.host.state.lock().unwrap().receive_from_result = Some(Ok(crate::ReceivedDatagram {
            count: 3,
            full_length: 7,
            source: SocketAddress::Inet4 {
                address: [127, 0, 0, 1],
                port: 53,
            },
        }));
        fixture.host.state.lock().unwrap().receive_from_data = b"payload".to_vec();
        assert_eq!(
            runtime.handle(Fixture::operation("recvfrom"), [fd, 80, 3, 0x20, 0, 0],),
            LinuxResult::Value(7),
        );
        assert_eq!(&fixture.memory.inner.bytes.lock().unwrap()[80..83], b"pay");
    }
}

#[test]
fn recvfrom_guest_output() {
    let fixture = Fixture::new();
    let mut runtime = fixture.runtime(GuestArchitecture::Aarch64);
    let fd = match runtime.handle(Fixture::operation("socket"), [2, 2, 17, 0, 0, 0]) {
        LinuxResult::Value(value) => value,
        other => panic!("socket failed: {other:?}"),
    };
    fixture.memory.put(64, &16_u32.to_le_bytes());
    fixture.memory.put(80, b"payload-sentinel");
    fixture.memory.put(112, b"address-sentinel");
    fixture.host.state.lock().unwrap().receive_from_result = Some(Err(crate::RuntimeNetworkError::WouldBlock));

    assert_eq!(
        runtime.handle(Fixture::operation("recvfrom"), [fd, 80, 8, 0, 112, 64],),
        LinuxResult::Error(Errno::EAGAIN),
    );
    let bytes = fixture.memory.inner.bytes.lock().unwrap();
    assert_eq!(&bytes[80..88], b"payload-");
    assert_eq!(&bytes[112..120], b"address-");
    assert_eq!(&bytes[64..68], &16_u32.to_le_bytes());
}

#[test]
fn recvfrom_host_receive() {
    let fixture = Fixture::new();
    let mut runtime = fixture.runtime(GuestArchitecture::X86_64);
    let fd = match runtime.handle(Fixture::operation("socket"), [2, 2, 17, 0, 0, 0]) {
        LinuxResult::Value(value) => value,
        other => panic!("socket failed: {other:?}"),
    };

    assert_eq!(
        runtime.handle(Fixture::operation("recvfrom"), [fd, 80, 8, 0, 96, 600],),
        LinuxResult::Error(Errno::EFAULT),
    );
    assert!(fixture.host.state.lock().unwrap().received_from.is_empty());
}

#[test]
fn recvfrom_guest_mutation() {
    let fixture = Fixture::new();
    let mut runtime = fixture.runtime(GuestArchitecture::Aarch64);
    let fd = match runtime.handle(Fixture::operation("socket"), [2, 2, 17, 0, 0, 0]) {
        LinuxResult::Value(value) => value,
        other => panic!("socket failed: {other:?}"),
    };
    fixture.memory.put(64, &16_u32.to_le_bytes());
    fixture.memory.put(80, b"payload-sentinel");
    fixture.host.state.lock().unwrap().receive_from_data = b"answer".to_vec();

    assert_eq!(
        runtime.handle(Fixture::operation("recvfrom"), [fd, 80, 6, 0, 600, 64],),
        LinuxResult::Error(Errno::EFAULT),
    );
    assert_eq!(&fixture.memory.inner.bytes.lock().unwrap()[80..86], b"payloa",);
    assert_eq!(fixture.host.state.lock().unwrap().received_from, [(1, 6, true)],);
}
