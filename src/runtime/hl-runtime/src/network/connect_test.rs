use super::*;

#[test]
fn readiness_transactional_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        {
            let mut state = fixture.host.state.lock().unwrap();
            state.connect_start.push_back(SocketConnectStatus::Pending);
            state
                .connect_poll
                .push_back(SocketConnectStatus::Failed(hl_network::SocketConnectError::Refused));
        }
        let mut runtime = fixture.runtime(architecture);
        let LinuxResult::Value(fd) = runtime.handle(Fixture::operation("socket"), [2, 1 | 0x800, 6, 0, 0, 0]) else {
            panic!("socket creation failed");
        };
        fixture
            .memory
            .put(64, &[2, 0, 0, 80, 127, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(
            runtime.handle(Fixture::operation("connect"), [fd, 64, 16, 0, 0, 0]),
            LinuxResult::Error(Errno::EINPROGRESS),
        );
        let interest = hl_descriptor::Readiness::from_bits(hl_descriptor::Readiness::WRITE);
        let readiness = runtime.lookup(fd as i32).unwrap().readiness(interest);
        assert!(readiness.contains(hl_descriptor::Readiness::WRITE));
        assert!(readiness.contains(hl_descriptor::Readiness::ERROR));
        let repeated = runtime.lookup(fd as i32).unwrap().readiness(interest);
        assert!(repeated.contains(hl_descriptor::Readiness::ERROR));
        fixture.catalog.freeze_checkpoint();
        let image = fixture.catalog.checkpoint_image().unwrap();
        fixture.catalog.thaw_checkpoint();
        let hl_network::NetworkSocketState::Host { snapshot, .. } = &image.sockets[0] else {
            panic!("host socket expected");
        };
        assert_eq!(snapshot.state, SocketState::Connecting);
        assert_eq!(snapshot.connect_error, Some(hl_network::SocketConnectError::Refused),);

        fixture.memory.put(120, &4_u32.to_le_bytes());
        fixture.memory.inner.fail_write.store(true, Ordering::Release);
        assert_eq!(
            runtime.handle(Fixture::operation("getsockopt"), [fd, 1, 4, 128, 120, 0],),
            LinuxResult::Error(Errno::EFAULT),
        );
        fixture.catalog.freeze_checkpoint();
        let image = fixture.catalog.checkpoint_image().unwrap();
        fixture.catalog.thaw_checkpoint();
        let hl_network::NetworkSocketState::Host { snapshot, .. } = &image.sockets[0] else {
            panic!("host socket expected");
        };
        assert_eq!(snapshot.connect_error, Some(hl_network::SocketConnectError::Refused));
        fixture.memory.inner.fail_write.store(false, Ordering::Release);
        assert_eq!(
            runtime.handle(Fixture::operation("getsockopt"), [fd, 1, 4, 128, 120, 0],),
            LinuxResult::Value(0),
        );
        assert_eq!(
            i32::from_le_bytes(fixture.memory.inner.bytes.lock().unwrap()[128..132].try_into().unwrap(),),
            Errno::ECONNREFUSED.raw(),
        );
        fixture.catalog.freeze_checkpoint();
        let image = fixture.catalog.checkpoint_image().unwrap();
        fixture.catalog.thaw_checkpoint();
        let hl_network::NetworkSocketState::Host { snapshot, .. } = &image.sockets[0] else {
            panic!("host socket expected");
        };
        assert_eq!(snapshot.state, SocketState::Created);
        assert_eq!(snapshot.connect_error, None);
        fixture.memory.put(120, &4_u32.to_le_bytes());
        assert_eq!(
            runtime.handle(Fixture::operation("getsockopt"), [fd, 1, 4, 128, 120, 0],),
            LinuxResult::Value(0),
        );
        assert_eq!(
            i32::from_le_bytes(fixture.memory.inner.bytes.lock().unwrap()[128..132].try_into().unwrap(),),
            0,
        );
    }
}
