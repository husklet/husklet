use super::*;
use hl_descriptor::{DescriptorFlags, StatusFlags};
use hl_linux::{DescriptorIoSyscalls, NetworkSyscalls, SyscallOperation};
use hl_network::SocketType;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use hl_linux::{GuestFault, SyscallFamily};
use hl_network::{
    ControlCodec, ControlMessage, ControlWord, NetworkConfiguration, NetworkResourceKey, SocketConnectStatus,
    SocketHostError, SocketHostReadiness,
};
use hl_task::{ProcessCredentials, ProcessLimits, RegistryConfig, TaskRegistry};

#[derive(Clone, Debug)]
struct Memory {
    inner: Arc<MemoryState>,
}

#[derive(Debug)]
struct MemoryState {
    bytes: Mutex<Vec<u8>>,
    fail_write: AtomicBool,
}

impl Memory {
    fn new() -> Self {
        Self {
            inner: Arc::new(MemoryState {
                bytes: Mutex::new(vec![0; 512]),
                fail_write: AtomicBool::new(false),
            }),
        }
    }

    fn put(&self, address: usize, bytes: &[u8]) {
        self.inner.bytes.lock().unwrap()[address..address + bytes.len()].copy_from_slice(bytes);
    }
}

impl GuestMemory for Memory {
    fn probe(&self, address: u64, length: usize, access: GuestAccess) -> Result<usize, GuestFault> {
        let available = self.inner.bytes.lock().unwrap().len().saturating_sub(address as usize);
        if available < length {
            return Err(GuestFault { address, access });
        }
        Ok(length)
    }

    fn read(&self, address: u64, output: &mut [u8]) -> Result<usize, GuestFault> {
        let start = address as usize;
        let end = start.checked_add(output.len()).ok_or(GuestFault {
            address,
            access: GuestAccess::Read,
        })?;
        let bytes = self.inner.bytes.lock().unwrap();
        let source = bytes.get(start..end).ok_or(GuestFault {
            address,
            access: GuestAccess::Read,
        })?;
        output.copy_from_slice(source);
        Ok(output.len())
    }

    fn write(&self, address: u64, input: &[u8]) -> Result<usize, GuestFault> {
        if self.inner.fail_write.load(Ordering::Acquire) {
            return Err(GuestFault {
                address,
                access: GuestAccess::Write,
            });
        }
        self.put(address as usize, input);
        Ok(input.len())
    }
}

#[derive(Debug, Default)]
struct HostState {
    next: u64,
    local: Option<SocketAddress>,
    peer: Option<SocketAddress>,
    routes: Vec<(&'static str, hl_network::EgressRoute)>,
    bind_routes: Vec<hl_network::BindRoute>,
    message_routes: Vec<Option<hl_network::EgressRoute>>,
    closed: Vec<u64>,
    sent_to: Vec<(u64, Vec<u8>, SocketAddress, bool)>,
    send_to_result: Option<Result<usize, crate::RuntimeNetworkError>>,
    received_from: Vec<(u64, usize, bool)>,
    receive_from_data: Vec<u8>,
    receive_from_result: Option<Result<crate::ReceivedDatagram, crate::RuntimeNetworkError>>,
    connect_start: VecDeque<SocketConnectStatus>,
    connect_poll: VecDeque<SocketConnectStatus>,
}

#[derive(Debug, Default)]
struct Host {
    state: Mutex<HostState>,
}

impl hl_network::SocketHostIo for Host {
    type Token = u64;

    fn read(&self, _: u64, _: &mut [u8], _: bool) -> Result<usize, SocketHostError> {
        Err(SocketHostError::WouldBlock)
    }
    fn write(&self, _: u64, input: &[u8], _: bool) -> Result<usize, SocketHostError> {
        Ok(input.len())
    }
    fn readiness(&self, _: u64) -> SocketHostReadiness {
        SocketHostReadiness {
            writable: true,
            ..Default::default()
        }
    }
    fn start_connect(&self, _: u64, _: bool) -> SocketConnectStatus {
        self.state
            .lock()
            .unwrap()
            .connect_start
            .pop_front()
            .unwrap_or(SocketConnectStatus::Connected)
    }
    fn poll_connect(&self, _: u64) -> SocketConnectStatus {
        self.state
            .lock()
            .unwrap()
            .connect_poll
            .pop_front()
            .unwrap_or(SocketConnectStatus::Connected)
    }
    fn cancel(&self, _: u64) {}
    fn close(&self, token: u64) {
        self.state.lock().unwrap().closed.push(token);
    }
}

impl RuntimeNetworkHost for Host {
    type Attachment = ();

    fn input_queue(&self, _: u64) -> Result<u64, crate::RuntimeNetworkError> {
        Ok(7)
    }

    fn output_queue(&self, _: u64) -> Result<u64, crate::RuntimeNetworkError> {
        Ok(9)
    }

    fn create(
        &self,
        _: AddressFamily,
        _: SocketType,
        _: SocketProtocol,
    ) -> Result<crate::CreatedSocket<u64>, crate::RuntimeNetworkError> {
        let mut state = self.state.lock().unwrap();
        state.next += 1;
        Ok(crate::CreatedSocket {
            token: state.next,
            resource: NetworkResourceKey::new(state.next).unwrap(),
            binding: Arc::new(()),
        })
    }

    fn bind(&self, _: u64, address: SocketAddress) -> Result<SocketAddress, crate::RuntimeNetworkError> {
        self.state.lock().unwrap().local = Some(address.clone());
        Ok(address)
    }

    fn bind_route(
        &self,
        token: u64,
        route: hl_network::BindRoute,
    ) -> Result<SocketAddress, crate::RuntimeNetworkError> {
        self.state.lock().unwrap().bind_routes.push(route.clone());
        self.bind(token, route.address)
    }

    fn prepare_connect(&self, _: u64, address: SocketAddress) -> Result<(), crate::RuntimeNetworkError> {
        self.state.lock().unwrap().peer = Some(address);
        Ok(())
    }

    fn prepare_connect_route(
        &self,
        token: u64,
        route: hl_network::EgressRoute,
    ) -> Result<(), crate::RuntimeNetworkError> {
        self.state.lock().unwrap().routes.push(("connect", route.clone()));
        self.prepare_connect(token, route.address)
    }

    fn listen(&self, _: u64, _: u32) -> Result<(), crate::RuntimeNetworkError> {
        Ok(())
    }
    fn accept(&self, _: u64) -> Result<crate::AcceptedSocket<u64>, crate::RuntimeNetworkError> {
        let mut state = self.state.lock().unwrap();
        state.next += 1;
        Ok(crate::AcceptedSocket {
            token: state.next,
            resource: NetworkResourceKey::new(state.next).unwrap(),
            binding: Arc::new(()),
            local: SocketAddress::Inet4 {
                address: [127, 0, 0, 1],
                port: 8080,
            },
            peer: SocketAddress::Inet4 {
                address: [10, 0, 0, 2],
                port: 5000,
            },
        })
    }
    fn local_address(&self, _: u64) -> Result<SocketAddress, crate::RuntimeNetworkError> {
        self.state
            .lock()
            .unwrap()
            .local
            .clone()
            .ok_or(crate::RuntimeNetworkError::NotConnected)
    }
    fn peer_address(&self, _: u64) -> Result<SocketAddress, crate::RuntimeNetworkError> {
        self.state
            .lock()
            .unwrap()
            .peer
            .clone()
            .ok_or(crate::RuntimeNetworkError::NotConnected)
    }
    fn send_to(
        &self,
        token: u64,
        input: &[u8],
        address: SocketAddress,
        nonblocking: bool,
    ) -> Result<usize, crate::RuntimeNetworkError> {
        let mut state = self.state.lock().unwrap();
        state.sent_to.push((token, input.to_vec(), address, nonblocking));
        state.send_to_result.take().unwrap_or(Ok(input.len()))
    }
    fn send_to_route(
        &self,
        token: u64,
        input: &[u8],
        route: hl_network::EgressRoute,
        nonblocking: bool,
    ) -> Result<usize, crate::RuntimeNetworkError> {
        self.state.lock().unwrap().routes.push(("send", route.clone()));
        self.send_to(token, input, route.address, nonblocking)
    }
    fn receive_from(
        &self,
        token: u64,
        output: &mut [u8],
        nonblocking: bool,
        _: bool,
    ) -> Result<crate::ReceivedDatagram, crate::RuntimeNetworkError> {
        let mut state = self.state.lock().unwrap();
        state.received_from.push((token, output.len(), nonblocking));
        let count = output.len().min(state.receive_from_data.len());
        output[..count].copy_from_slice(&state.receive_from_data[..count]);
        state.receive_from_result.take().unwrap_or(Ok(crate::ReceivedDatagram {
            count,
            full_length: count,
            source: SocketAddress::Inet4 {
                address: [127, 0, 0, 11],
                port: 53,
            },
        }))
    }

    fn send_message(
        &self,
        _: u64,
        message: crate::HostSend<Self::Attachment>,
    ) -> Result<crate::HostSendResult, crate::RuntimeNetworkError> {
        let count = message.payload.len();
        self.state.lock().unwrap().message_routes.push(message.route);
        Ok(crate::HostSendResult {
            count,
            rights_consumed: false,
        })
    }
    fn receive_message(
        &self,
        token: u64,
        payload_limit: usize,
        _: usize,
        nonblocking: bool,
        _: bool,
    ) -> Result<crate::HostReceive<Self::Attachment>, crate::RuntimeNetworkError> {
        let mut state = self.state.lock().unwrap();
        state.received_from.push((token, payload_limit, nonblocking));
        let count = payload_limit.min(state.receive_from_data.len());
        Ok(crate::HostReceive {
            payload: state.receive_from_data[..count].to_vec(),
            full_length: state.receive_from_data.len(),
            source: Some(SocketAddress::Inet4 {
                address: [127, 0, 0, 11],
                port: 53,
            }),
            controls: Vec::new(),
            payload_truncated: count < state.receive_from_data.len(),
            control_truncated: false,
        })
    }
    fn shutdown(&self, _: u64, _: bool, _: bool) -> Result<(), crate::RuntimeNetworkError> {
        Ok(())
    }
    fn set_option(
        &self,
        _: u64,
        _: i32,
        _: i32,
        _: hl_linux::GuestSocketOption,
    ) -> Result<(), crate::RuntimeNetworkError> {
        Ok(())
    }
    fn get_option(&self, _: u64, _: i32, _: i32) -> Result<hl_linux::GuestSocketOption, crate::RuntimeNetworkError> {
        Ok(hl_linux::GuestSocketOption::Scalar(1))
    }
}

struct Fixture {
    descriptors: Arc<DescriptorTable>,
    catalog: Arc<NetworkCatalog>,
    memory: Memory,
    host: Arc<Host>,
    sockets: Arc<crate::RuntimeSocketRegistry<Host>>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            descriptors: Arc::new(DescriptorTable::new(8).unwrap()),
            catalog: Arc::new(NetworkCatalog::new(
                NetworkConfiguration::new(Vec::new(), Vec::new(), Vec::new()).unwrap(),
            )),
            memory: Memory::new(),
            host: Arc::new(Host::default()),
            sockets: Arc::new(crate::RuntimeSocketRegistry::default()),
        }
    }

    fn runtime(&self, architecture: GuestArchitecture) -> RuntimeNetworkSyscalls<Host, Memory> {
        RuntimeNetworkSyscalls::new(
            self.descriptors.clone(),
            self.catalog.clone(),
            self.memory.clone(),
            architecture,
        )
        .with_host(self.host.clone())
        .with_registry(self.sockets.clone())
    }

    fn operation(name: &'static str) -> SyscallOperation {
        SyscallOperation {
            canonical_number: 0,
            name,
            family: SyscallFamily::Network,
        }
    }
}

impl Fixture {
    fn message_header(iovecs: u64, control: u64, control_length: usize) -> [u8; 56] {
        let mut header = [0; 56];
        header[16..24].copy_from_slice(&iovecs.to_le_bytes());
        header[24..32].copy_from_slice(&1_u64.to_le_bytes());
        header[32..40].copy_from_slice(&control.to_le_bytes());
        header[40..48].copy_from_slice(&(control_length as u64).to_le_bytes());
        header
    }

    fn iovec(base: u64, length: u64) -> [u8; 16] {
        let mut vector = [0; 16];
        vector[..8].copy_from_slice(&base.to_le_bytes());
        vector[8..].copy_from_slice(&length.to_le_bytes());
        vector
    }
}

#[test]
fn host_work_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime(architecture);
        let LinuxResult::Value(fd) =
            runtime.handle(Fixture::operation("socket"), [2, 1 | 0x800 | 0x8_0000, 6, 0, 0, 0])
        else {
            panic!();
        };
        assert!(fixture.descriptors.flags(fd as i32).unwrap().closes_on_exec());
        fixture
            .memory
            .put(16, &[2, 0, 0x1f, 0x90, 127, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(
            runtime.handle(Fixture::operation("bind"), [fd, 16, 16, 0, 0, 0]),
            LinuxResult::Value(0),
        );
        fixture
            .memory
            .put(64, &[2, 0, 0, 80, 127, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(
            runtime.handle(Fixture::operation("connect"), [fd, 64, 16, 0, 0, 0]),
            LinuxResult::Value(0),
        );
        fixture.memory.put(120, &16_u32.to_le_bytes());
        assert_eq!(
            runtime.handle(Fixture::operation("getpeername"), [fd, 128, 120, 0, 0, 0]),
            LinuxResult::Value(0),
        );
        assert_eq!(&fixture.memory.inner.bytes.lock().unwrap()[128..130], &[2, 0]);
        fixture.descriptors.close(fd as i32).unwrap();
        assert_eq!(fixture.host.state.lock().unwrap().closed, [1]);
    }
}

#[test]
fn interface_routes_reach_host_bind_connect_and_datagram_send() {
    let fixture = Fixture::new();
    let policy =
        hl_network::NetworkPolicy::from_launch(false, b"", b"", b"wide=10.0.0.2/8\nnarrow=10.4.0.2/16").unwrap();
    let mut runtime = fixture
        .runtime(GuestArchitecture::X86_64)
        .with_network_policy(policy)
        .with_host_projection(true)
        .with_credentials(hl_network::SenderCredentials {
            process: 41,
            user: 42,
            group: 43,
        });
    let sockaddr = |address: [u8; 4], port: u16| {
        let mut value = [0_u8; 16];
        value[..2].copy_from_slice(&2_u16.to_le_bytes());
        value[2..4].copy_from_slice(&port.to_be_bytes());
        value[4..8].copy_from_slice(&address);
        value
    };

    let LinuxResult::Value(listener) = runtime.handle(Fixture::operation("socket"), [2, 1, 6, 0, 0, 0]) else {
        panic!("stream socket creation failed")
    };
    fixture.memory.put(16, &sockaddr([10, 4, 0, 2], 8080));
    assert_eq!(
        runtime.handle(Fixture::operation("bind"), [listener, 16, 16, 0, 0, 0]),
        LinuxResult::Value(0)
    );

    let LinuxResult::Value(client) = runtime.handle(Fixture::operation("socket"), [2, 1, 6, 0, 0, 0]) else {
        panic!("stream socket creation failed")
    };
    fixture.memory.put(64, &sockaddr([10, 4, 9, 8], 8080));
    assert_eq!(
        runtime.handle(Fixture::operation("connect"), [client, 64, 16, 0, 0, 0]),
        LinuxResult::Value(0)
    );

    let LinuxResult::Value(datagram) = runtime.handle(Fixture::operation("socket"), [2, 2, 17, 0, 0, 0]) else {
        panic!("datagram socket creation failed")
    };
    fixture.memory.put(96, b"ping");
    assert_eq!(
        runtime.handle(Fixture::operation("sendto"), [datagram, 96, 4, 0, 64, 16]),
        LinuxResult::Value(4)
    );
    fixture.memory.put(112, &Fixture::iovec(96, 4));
    let control = ControlCodec::encode(
        &[ControlMessage::Credentials {
            process: 41,
            user: 42,
            group: 43,
        }],
        ControlWord::Eight,
        32,
    )
    .unwrap()
    .bytes;
    fixture.memory.put(200, &control);
    let mut header = Fixture::message_header(112, 200, control.len());
    header[..8].copy_from_slice(&64_u64.to_le_bytes());
    header[8..12].copy_from_slice(&16_u32.to_le_bytes());
    fixture.memory.put(128, &header);
    assert_eq!(
        runtime.handle(Fixture::operation("sendmsg"), [datagram, 128, 0, 0, 0, 0]),
        LinuxResult::Value(4)
    );

    let state = fixture.host.state.lock().unwrap();
    assert_eq!(state.bind_routes.len(), 1);
    assert_eq!(state.bind_routes[0].interface.as_ref().unwrap().bridge, b"narrow");
    assert_eq!(state.routes.len(), 2);
    assert_eq!(state.routes[0].0, "connect");
    assert_eq!(state.routes[0].1.interface.as_ref().unwrap().bridge, b"wide");
    assert_eq!(state.routes[1].0, "send");
    assert_eq!(state.routes[1].1.interface.as_ref().unwrap().bridge, b"wide");
    assert_eq!(state.message_routes.len(), 1);
    assert_eq!(
        state.message_routes[0]
            .as_ref()
            .unwrap()
            .interface
            .as_ref()
            .unwrap()
            .bridge,
        b"wide"
    );
}

#[test]
fn ipv4_policy_rejects_global_ipv6_before_host_io() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let policy = hl_network::NetworkPolicy::from_launch(false, b"", b"", b"").unwrap();
        let mut runtime = fixture.runtime(architecture).with_network_policy(policy);
        let mut address = [0_u8; 28];
        address[..2].copy_from_slice(&10_u16.to_le_bytes());
        address[2..4].copy_from_slice(&80_u16.to_be_bytes());
        address[8..24].copy_from_slice(&[0x20, 0x01, 0x48, 0x60, 0x48, 0x60, 0, 0, 0, 0, 0, 0, 0, 0, 0x88, 0x88]);
        fixture.memory.put(64, &address);
        fixture.memory.put(128, b"ping");
        let LinuxResult::Value(stream) = runtime.handle(Fixture::operation("socket"), [10, 1, 6, 0, 0, 0]) else {
            panic!("IPv6 stream socket creation failed")
        };
        assert_eq!(
            runtime.handle(Fixture::operation("connect"), [stream, 64, 28, 0, 0, 0]),
            LinuxResult::Error(Errno::ENETUNREACH),
        );
        let LinuxResult::Value(datagram) = runtime.handle(Fixture::operation("socket"), [10, 2, 17, 0, 0, 0]) else {
            panic!("IPv6 datagram socket creation failed")
        };
        assert_eq!(
            runtime.handle(Fixture::operation("sendto"), [datagram, 128, 4, 0, 64, 28]),
            LinuxResult::Error(Errno::ENETUNREACH),
        );
        let state = fixture.host.state.lock().unwrap();
        assert!(state.peer.is_none());
        assert!(state.sent_to.is_empty());
    }
}

#[test]
fn isolated_policy_creates_inet_sockets_and_rejects_only_external_routes() {
    let fixture = Fixture::new();
    let policy = hl_network::NetworkPolicy::from_launch(true, b"", b"", b"").unwrap();
    let mut runtime = fixture.runtime(GuestArchitecture::X86_64).with_network_policy(policy);
    let LinuxResult::Value(stream) = runtime.handle(Fixture::operation("socket"), [2, 1, 6, 0, 0, 0]) else {
        panic!("isolated namespace must still provide an AF_INET stream socket")
    };
    let mut external = [0_u8; 16];
    external[..2].copy_from_slice(&2_u16.to_le_bytes());
    external[2..4].copy_from_slice(&80_u16.to_be_bytes());
    external[4..8].copy_from_slice(&[8, 8, 8, 8]);
    fixture.memory.put(64, &external);
    assert_eq!(
        runtime.handle(Fixture::operation("connect"), [stream, 64, 16, 0, 0, 0]),
        LinuxResult::Error(Errno::ENETUNREACH),
    );
    let mut loopback = external;
    loopback[4..8].copy_from_slice(&[127, 0, 0, 1]);
    fixture.memory.put(96, &loopback);
    assert_eq!(
        runtime.handle(Fixture::operation("connect"), [stream, 96, 16, 0, 0, 0]),
        LinuxResult::Value(0),
    );
}

#[test]
fn bind_rejects_an_address_no_namespace_interface_owns() {
    let sockaddr = |address: [u8; 4]| {
        let mut value = [0_u8; 16];
        value[..2].copy_from_slice(&2_u16.to_le_bytes());
        value[2..4].copy_from_slice(&23_458_u16.to_be_bytes());
        value[4..8].copy_from_slice(&address);
        value
    };

    let fixture = Fixture::new();
    let policy = hl_network::NetworkPolicy::from_launch(false, b"", b"", b"eth0=172.17.0.2/16").unwrap();
    let mut runtime = fixture.runtime(GuestArchitecture::X86_64).with_network_policy(policy);
    let LinuxResult::Value(fd) = runtime.handle(Fixture::operation("socket"), [2, 1, 6, 0, 0, 0]) else {
        panic!("stream socket creation failed")
    };
    fixture.memory.put(64, &sockaddr([1, 2, 3, 4]));
    assert_eq!(
        runtime.handle(Fixture::operation("bind"), [fd, 64, 16, 0, 0, 0]),
        LinuxResult::Error(Errno::EADDRNOTAVAIL),
    );
    fixture.memory.put(96, &sockaddr([172, 17, 0, 2]));
    assert_eq!(
        runtime.handle(Fixture::operation("bind"), [fd, 96, 16, 0, 0, 0]),
        LinuxResult::Value(0),
    );

    let isolated = Fixture::new();
    let mut runtime = isolated
        .runtime(GuestArchitecture::X86_64)
        .with_network_policy(hl_network::NetworkPolicy::from_launch(true, b"", b"", b"").unwrap());
    let LinuxResult::Value(fd) = runtime.handle(Fixture::operation("socket"), [2, 1, 6, 0, 0, 0]) else {
        panic!("stream socket creation failed")
    };
    isolated.memory.put(64, &sockaddr([172, 17, 0, 2]));
    assert_eq!(
        runtime.handle(Fixture::operation("bind"), [fd, 64, 16, 0, 0, 0]),
        LinuxResult::Error(Errno::EADDRNOTAVAIL),
    );
}

#[test]
fn bind_rejects_a_unix_address_on_an_internet_socket() {
    let fixture = Fixture::new();
    let mut runtime = fixture
        .runtime(GuestArchitecture::X86_64)
        .with_network_policy(hl_network::NetworkPolicy::from_launch(false, b"", b"", b"").unwrap());
    let LinuxResult::Value(fd) = runtime.handle(Fixture::operation("socket"), [2, 1, 6, 0, 0, 0]) else {
        panic!("stream socket creation failed")
    };
    let mut sockaddr = [0_u8; 110];
    sockaddr[..2].copy_from_slice(&1_u16.to_le_bytes());
    sockaddr[2..22].copy_from_slice(b"/tmp/ltp_neterr.sock");
    fixture.memory.put(64, &sockaddr);
    assert_eq!(
        runtime.handle(Fixture::operation("bind"), [fd, 64, 110, 0, 0, 0]),
        LinuxResult::Error(Errno::EAFNOSUPPORT),
    );
}

#[test]
fn exit_catalog_ownership() {
    let catalog = Arc::new(NetworkCatalog::new(
        NetworkConfiguration::new(Vec::new(), Vec::new(), Vec::new()).unwrap(),
    ));
    let host = Arc::new(Host::default());
    let first = Arc::new(DescriptorTable::new(8).unwrap());
    let second = Arc::new(DescriptorTable::new(8).unwrap());
    let mut first_runtime = RuntimeNetworkSyscalls::new(
        Arc::clone(&first),
        Arc::clone(&catalog),
        Memory::new(),
        GuestArchitecture::X86_64,
    )
    .with_host(Arc::clone(&host));
    let mut second_runtime = RuntimeNetworkSyscalls::new(
        Arc::clone(&second),
        Arc::clone(&catalog),
        Memory::new(),
        GuestArchitecture::X86_64,
    )
    .with_host(Arc::clone(&host));
    let LinuxResult::Value(first_fd) = first_runtime.handle(Fixture::operation("socket"), [2, 1, 6, 0, 0, 0]) else {
        panic!()
    };
    let LinuxResult::Value(second_fd) = second_runtime.handle(Fixture::operation("socket"), [2, 1, 6, 0, 0, 0]) else {
        panic!()
    };
    let first_id = first_runtime.lookup(first_fd as i32).unwrap().id;
    let second_id = second_runtime.lookup(second_fd as i32).unwrap().id;

    let (control, _table) = crate::Control::attach(Arc::clone(&first), 8, 8).unwrap();
    let participant = crate::DescriptorExit::new(
        Arc::new(crate::DescriptorImageSlot::from_shared(first)),
        Arc::new(control),
    );
    let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let credentials = ProcessCredentials::new(1, 1, &[], 4).unwrap();
    let (process, _) = tasks.create_init(credentials, ProcessLimits::empty()).unwrap();
    let mut exit = crate::ExitParticipant::prepare(&participant, process, &[]).unwrap();
    exit.publish().unwrap();
    assert!(catalog.snapshot(first_id).is_ok());
    exit.rollback();
    assert!(catalog.snapshot(first_id).is_ok());

    let mut exit = crate::ExitParticipant::prepare(&participant, process, &[]).unwrap();
    exit.publish().unwrap();
    exit.finish();

    assert_eq!(catalog.snapshot(first_id), Err(hl_network::NetworkCatalogError::Stale),);
    assert!(catalog.snapshot(second_id).is_ok());
    assert_eq!(host.state.lock().unwrap().closed, [1]);
}

#[test]
fn unix_transports_bytes() {
    let fixture = Fixture::new();
    let mut runtime = fixture.runtime(GuestArchitecture::Aarch64);
    fixture.memory.inner.fail_write.store(true, Ordering::Release);
    assert_eq!(
        runtime.handle(Fixture::operation("socketpair"), [1, 1, 0, 32, 0, 0]),
        LinuxResult::Error(Errno::EFAULT),
    );
    assert_eq!(
        fixture.descriptors.pin(0).unwrap_err(),
        hl_descriptor::DescriptorError::BadDescriptor,
    );
    fixture.memory.inner.fail_write.store(false, Ordering::Release);
    assert_eq!(
        runtime.handle(Fixture::operation("socketpair"), [1, 1, 0, 32, 0, 0]),
        LinuxResult::Value(0),
    );
    let bytes = fixture.memory.inner.bytes.lock().unwrap();
    let first = i32::from_le_bytes(bytes[32..36].try_into().unwrap());
    let second = i32::from_le_bytes(bytes[36..40].try_into().unwrap());
    drop(bytes);
    assert_eq!(fixture.descriptors.pin(first).unwrap().write(b"rust"), Ok(4));
    let mut output = [0; 4];
    assert_eq!(fixture.descriptors.pin(second).unwrap().read(&mut output), Ok(4));
    assert_eq!(&output, b"rust");
    fixture.descriptors.close(first).unwrap();
    assert_eq!(fixture.descriptors.pin(second).unwrap().read(&mut [0]), Ok(0));
}

#[test]
fn shared_router_registry() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut creator = fixture.runtime(architecture);
        let mut peer = fixture.runtime(architecture);
        assert_eq!(
            creator.handle(Fixture::operation("socketpair"), [1, 1, 0, 32, 0, 0]),
            LinuxResult::Value(0),
        );
        fixture.memory.put(64, b"shared");
        assert_eq!(
            creator.handle(Fixture::operation("send"), [0, 64, 6, 0, 0, 0]),
            LinuxResult::Value(6),
        );
        assert_eq!(
            peer.handle(Fixture::operation("recv"), [1, 80, 6, 0, 0, 0]),
            LinuxResult::Value(6),
        );
        assert_eq!(&fixture.memory.inner.bytes.lock().unwrap()[80..86], b"shared");
    }
}

#[test]
fn accept4_flags_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime(architecture);
        let listener = match runtime.handle(Fixture::operation("socket"), [2, 1, 6, 0, 0, 0]) {
            LinuxResult::Value(value) => value as i32,
            other => panic!("socket failed: {other:?}"),
        };
        fixture.memory.inner.bytes.lock().unwrap()[48..52].copy_from_slice(&16_u32.to_le_bytes());
        fixture.memory.inner.fail_write.store(true, Ordering::Release);
        assert_eq!(
            runtime.handle(Fixture::operation("accept4"), [listener as u64, 64, 48, 0x80800, 0, 0],),
            LinuxResult::Error(Errno::EFAULT),
        );
        assert_eq!(
            fixture.descriptors.pin(1).unwrap_err(),
            hl_descriptor::DescriptorError::BadDescriptor
        );
        fixture.memory.inner.fail_write.store(false, Ordering::Release);
        assert_eq!(
            runtime.handle(Fixture::operation("accept4"), [listener as u64, 64, 48, 0x80800, 0, 0],),
            LinuxResult::Value(1),
        );
        let accepted = fixture.descriptors.pin(1).unwrap();
        assert_ne!(accepted.status().bits() & StatusFlags::NONBLOCKING, 0);
        assert_ne!(
            fixture.descriptors.flags(1).unwrap().bits() & DescriptorFlags::CLOSE_ON_EXEC,
            0,
        );
        let bytes = fixture.memory.inner.bytes.lock().unwrap();
        assert_eq!(&bytes[64..66], &[2, 0]);
        assert_eq!(&bytes[66..68], &5000_u16.to_be_bytes());
        drop(bytes);
        assert_eq!(fixture.host.state.lock().unwrap().closed, [2]);
    }
}

#[test]
fn isolated_nonblocking_listener_accepts_as_would_block() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime(architecture);
        let listener = match runtime.handle(Fixture::operation("socket"), [2, 1 | 0x800, 6, 0, 0, 0]) {
            LinuxResult::Value(value) => value,
            other => panic!("socket failed: {other:?}"),
        };
        fixture
            .memory
            .put(64, &[2, 0, 0, 0, 127, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(
            runtime.handle(Fixture::operation("bind"), [listener, 64, 16, 0, 0, 0]),
            LinuxResult::Value(0),
        );
        assert_eq!(
            runtime.handle(Fixture::operation("listen"), [listener, 1, 0, 0, 0, 0]),
            LinuxResult::Value(0),
        );
        let view = fixture.catalog.namespace_view();
        assert_eq!(view.internet.len(), 1);
        assert_eq!(view.internet[0].state, SocketState::Listening { backlog: 1 });
        assert_eq!(
            runtime.handle(Fixture::operation("accept4"), [listener, 0, 0, 0x800, 0, 0]),
            LinuxResult::Error(Errno::EAGAIN),
        );
    }
}

#[test]
fn accept_alias_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime(architecture);
        let listener = match runtime.handle(Fixture::operation("socket"), [2, 1, 6, 0, 0, 0]) {
            LinuxResult::Value(value) => value,
            other => panic!("socket failed: {other:?}"),
        };
        assert_eq!(
            runtime.handle(Fixture::operation("accept"), [listener, 0, 0, 0, 0, 0],),
            LinuxResult::Value(1),
        );
    }
}

#[path = "datagram_test.rs"]
mod datagram_tests;

#[test]
fn sendmsg_rights_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime(architecture);
        assert_eq!(
            runtime.handle(Fixture::operation("socketpair"), [1, 1, 0, 32, 0, 0]),
            LinuxResult::Value(0),
        );
        fixture.memory.put(200, &Fixture::iovec(250, 3));
        fixture.memory.put(250, b"msg");
        let control = ControlCodec::encode(&[ControlMessage::Rights(vec![0])], ControlWord::Eight, 64)
            .unwrap()
            .bytes;
        fixture
            .memory
            .put(128, &Fixture::message_header(200, 272, control.len()));
        fixture.memory.put(272, &control);
        assert_eq!(
            runtime.handle(Fixture::operation("sendmsg"), [0, 128, 0, 0, 0, 0]),
            LinuxResult::Value(3),
        );
        fixture.memory.put(360, &Fixture::iovec(400, 3));
        fixture.memory.put(300, &Fixture::message_header(360, 440, 64));
        assert_eq!(
            runtime.handle(Fixture::operation("recvmsg"), [1, 300, 0, 0, 0, 0]),
            LinuxResult::Value(3),
        );
        assert_eq!(&fixture.memory.inner.bytes.lock().unwrap()[400..403], b"msg");
        assert!(fixture.descriptors.pin(2).is_ok());
        let bytes = fixture.memory.inner.bytes.lock().unwrap();
        let decoded = ControlCodec::decode(&bytes[440..464], ControlWord::Eight).unwrap();
        assert_eq!(decoded, [ControlMessage::Rights(vec![2])]);
    }
}

#[test]
fn socket_neutral_identity() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime(architecture);
        assert_eq!(
            runtime.handle(Fixture::operation("socketpair"), [1, 1, 0, 32, 0, 0]),
            LinuxResult::Value(0),
        );
        let timeout = [2_i64.to_le_bytes(), 30_i64.to_le_bytes()].concat();
        fixture.memory.put(80, &timeout);
        assert_eq!(
            runtime.handle(Fixture::operation("setsockopt"), [0, 1, 20, 80, 16, 0],),
            LinuxResult::Value(0),
        );
        fixture.memory.put(120, &16_u32.to_le_bytes());
        assert_eq!(
            runtime.handle(Fixture::operation("getsockopt"), [0, 1, 20, 128, 120, 0],),
            LinuxResult::Value(0),
        );
        assert_eq!(&fixture.memory.inner.bytes.lock().unwrap()[128..144], &timeout);
        fixture.memory.put(152, &4_u32.to_le_bytes());
        assert_eq!(
            runtime.handle(Fixture::operation("getsockopt"), [0, 1, 3, 160, 152, 0],),
            LinuxResult::Value(0),
        );
        assert_eq!(
            i32::from_le_bytes(fixture.memory.inner.bytes.lock().unwrap()[160..164].try_into().unwrap(),),
            1,
        );
        let linger = [1_i32.to_le_bytes(), 5_i32.to_le_bytes()].concat();
        fixture.memory.put(176, &linger);
        assert_eq!(
            runtime.handle(Fixture::operation("setsockopt"), [0, 1, 13, 176, 8, 0],),
            LinuxResult::Value(0),
        );
        fixture.memory.put(184, &8_u32.to_le_bytes());
        assert_eq!(
            runtime.handle(Fixture::operation("getsockopt"), [0, 1, 13, 192, 184, 0],),
            LinuxResult::Value(0),
        );
        assert_eq!(&fixture.memory.inner.bytes.lock().unwrap()[192..200], &linger);
        fixture.memory.put(208, &1_i32.to_le_bytes());
        assert_eq!(
            runtime.handle(Fixture::operation("setsockopt"), [0, 1, 15, 208, 4, 0],),
            LinuxResult::Value(0),
        );
        fixture.memory.put(212, &4_u32.to_le_bytes());
        assert_eq!(
            runtime.handle(Fixture::operation("getsockopt"), [0, 1, 15, 216, 212, 0],),
            LinuxResult::Value(0),
        );
        assert_eq!(
            i32::from_le_bytes(fixture.memory.inner.bytes.lock().unwrap()[216..220].try_into().unwrap(),),
            1,
        );
        assert_eq!(
            runtime.handle(Fixture::operation("getsockopt"), [0, 999, 1, 160, 152, 0],),
            LinuxResult::Error(Errno::ENOPROTOOPT),
        );
    }
}

#[test]
fn unix_creation() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime(architecture);
        assert_eq!(
            runtime.handle(Fixture::operation("socket"), [1, 1, 0, 0, 0, 0]),
            LinuxResult::Value(0),
        );
        assert_eq!(
            fixture.descriptors.pin(0).unwrap().status().bits() & StatusFlags::ACCESS_MODE_MASK,
            2,
        );
        fixture.catalog.freeze_checkpoint();
        let image = fixture.catalog.checkpoint_image().unwrap();
        fixture.catalog.thaw_checkpoint();
        let hl_network::NetworkSocketState::Unix { snapshot, .. } = &image.sockets[0] else {
            panic!("standalone Unix socket must be checkpoint-visible");
        };
        assert_eq!(snapshot.family, AddressFamily::Unix);
        assert_eq!(snapshot.state, SocketState::Created);
        let id = snapshot.id;
        let child = fixture.descriptors.fork();
        fixture.descriptors.close(0).unwrap();
        assert!(fixture.catalog.snapshot(id).is_ok());
        child.close(0).unwrap();
        assert!(fixture.catalog.snapshot(id).is_err());
    }
}

#[test]
fn unix_binding() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime(architecture);
        assert_eq!(
            runtime.handle(Fixture::operation("socket"), [1, 1, 0, 0, 0, 0]),
            LinuxResult::Value(0)
        );
        fixture.memory.put(16, &[1, 0]);
        assert_eq!(
            runtime.handle(Fixture::operation("bind"), [0, 16, 2, 0, 0, 0]),
            LinuxResult::Value(0)
        );
        fixture.memory.put(32, &110_u32.to_le_bytes());
        assert_eq!(
            runtime.handle(Fixture::operation("getsockname"), [0, 40, 32, 0, 0, 0]),
            LinuxResult::Value(0),
        );
        let bytes = fixture.memory.inner.bytes.lock().unwrap();
        assert_eq!(&bytes[40..43], &[1, 0, 0]);
        assert_eq!(&bytes[43..48], b"00000");
        drop(bytes);

        assert_eq!(
            runtime.handle(Fixture::operation("socket"), [1, 1, 0, 0, 0, 0]),
            LinuxResult::Value(1)
        );
        fixture.memory.put(64, &[1, 0, 0, b'n', b'a', b'm', b'e']);
        assert_eq!(
            runtime.handle(Fixture::operation("bind"), [1, 64, 7, 0, 0, 0]),
            LinuxResult::Value(0)
        );
        assert_eq!(
            runtime.handle(Fixture::operation("socket"), [1, 1, 0, 0, 0, 0]),
            LinuxResult::Value(2)
        );
        assert_eq!(
            runtime.handle(Fixture::operation("bind"), [2, 64, 7, 0, 0, 0]),
            LinuxResult::Error(Errno::EADDRINUSE),
        );
    }
}

#[derive(Debug)]
struct PathPort {
    fail: AtomicBool,
    state: Arc<PathPortState>,
}

#[derive(Debug, Default)]
struct PathPortState {
    commits: AtomicUsize,
    rollbacks: AtomicUsize,
}

#[derive(Debug)]
struct PathBind {
    state: Arc<PathPortState>,
}

impl crate::PreparedUnixSocketPathBind for PathBind {
    fn commit(self: Box<Self>) {
        self.state.commits.fetch_add(1, Ordering::Relaxed);
    }

    fn rollback(self: Box<Self>) {
        self.state.rollbacks.fetch_add(1, Ordering::Relaxed);
    }
}

impl crate::UnixSocketPathPort for PathPort {
    fn prepare_bind(
        &self,
        _pathname: &hl_vfs::GuestPathBytes,
    ) -> Result<Box<dyn crate::PreparedUnixSocketPathBind>, Errno> {
        if self.fail.load(Ordering::Relaxed) {
            return Err(Errno::EACCES);
        }
        Ok(Box::new(PathBind {
            state: self.state.clone(),
        }))
    }

    fn prepare_unlink(
        &self,
        _pathname: &hl_vfs::GuestPathBytes,
    ) -> Option<Box<dyn crate::PreparedUnixSocketPathUnlink>> {
        None
    }
}

#[test]
fn pathname_port_failure_and_collision_are_transactional() {
    let fixture = Fixture::new();
    let state = Arc::new(PathPortState::default());
    let port = Arc::new(PathPort {
        fail: AtomicBool::new(true),
        state: state.clone(),
    });
    let mut runtime = fixture
        .runtime(GuestArchitecture::Aarch64)
        .with_unix_socket_paths(port.clone());
    assert_eq!(
        runtime.handle(Fixture::operation("socket"), [1, 1, 0, 0, 0, 0]),
        LinuxResult::Value(0)
    );
    fixture.memory.put(64, &[1, 0, b'/', b's']);
    assert_eq!(
        runtime.handle(Fixture::operation("bind"), [0, 64, 4, 0, 0, 0]),
        LinuxResult::Error(Errno::EACCES),
    );
    assert_eq!(
        fixture.sockets.unix_namespace().resolve_pathname(b"/s"),
        hl_network::UnixPathnameResolution::Missing
    );

    port.fail.store(false, Ordering::Relaxed);
    assert_eq!(
        runtime.handle(Fixture::operation("bind"), [0, 64, 4, 0, 0, 0]),
        LinuxResult::Value(0)
    );
    assert_eq!(state.commits.load(Ordering::Relaxed), 1);
    assert_eq!(
        runtime.handle(Fixture::operation("socket"), [1, 1, 0, 0, 0, 0]),
        LinuxResult::Value(1)
    );
    assert_eq!(
        runtime.handle(Fixture::operation("bind"), [1, 64, 4, 0, 0, 0]),
        LinuxResult::Error(Errno::EADDRINUSE),
    );
    assert_eq!(state.rollbacks.load(Ordering::Relaxed), 1);
    assert!(matches!(
        fixture.sockets.unix_namespace().resolve_pathname(b"/s"),
        hl_network::UnixPathnameResolution::Live(_)
    ));
}

#[test]
fn unix_named_accepts_fifo_and_closes_independently() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime(architecture);
        let address = [1, 0, 0, b'q'];
        fixture.memory.put(64, &address);

        assert_eq!(
            runtime.handle(Fixture::operation("socket"), [1, 1, 0, 0, 0, 0]),
            LinuxResult::Value(0)
        );
        assert_eq!(
            runtime.handle(Fixture::operation("bind"), [0, 64, 4, 0, 0, 0]),
            LinuxResult::Value(0)
        );
        assert_eq!(
            runtime.handle(Fixture::operation("listen"), [0, 2, 0, 0, 0, 0]),
            LinuxResult::Value(0)
        );

        for expected in [1, 2] {
            assert_eq!(
                runtime.handle(Fixture::operation("socket"), [1, 1, 0, 0, 0, 0]),
                LinuxResult::Value(expected),
            );
            assert_eq!(
                runtime.handle(Fixture::operation("connect"), [expected, 64, 4, 0, 0, 0]),
                LinuxResult::Value(0),
            );
        }
        assert_eq!(fixture.descriptors.pin(1).unwrap().write(b"first"), Ok(5));
        assert_eq!(fixture.descriptors.pin(2).unwrap().write(b"second"), Ok(6));

        assert_eq!(
            runtime.handle(Fixture::operation("accept"), [0, 0, 0, 0, 0, 0]),
            LinuxResult::Value(3)
        );
        assert_eq!(
            runtime.handle(Fixture::operation("accept"), [0, 0, 0, 0, 0, 0]),
            LinuxResult::Value(4)
        );
        let mut first = [0; 5];
        let mut second = [0; 6];
        assert_eq!(fixture.descriptors.pin(3).unwrap().read(&mut first), Ok(5));
        assert_eq!(fixture.descriptors.pin(4).unwrap().read(&mut second), Ok(6));
        assert_eq!(&first, b"first");
        assert_eq!(&second, b"second");

        fixture.descriptors.close(1).unwrap();
        assert_eq!(fixture.descriptors.pin(3).unwrap().read(&mut [0]), Ok(0));
        assert_eq!(fixture.descriptors.pin(2).unwrap().write(b"alive"), Ok(5));
        let mut alive = [0; 5];
        assert_eq!(fixture.descriptors.pin(4).unwrap().read(&mut alive), Ok(5));
        assert_eq!(&alive, b"alive");
    }
}

#[test]
fn default_protocol() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime(architecture);
        assert_eq!(
            runtime.handle(Fixture::operation("socket"), [2, 1, 0, 0, 0, 0]),
            LinuxResult::Value(0),
        );
        fixture.memory.put(32, &4_u32.to_le_bytes());
        assert_eq!(
            runtime.handle(Fixture::operation("getsockopt"), [0, 1, 38, 40, 32, 0]),
            LinuxResult::Value(0),
        );
        assert_eq!(
            i32::from_le_bytes(fixture.memory.inner.bytes.lock().unwrap()[40..44].try_into().unwrap()),
            6,
        );
    }
}

#[test]
fn tcp_options_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime(architecture);
        assert_eq!(
            runtime.handle(Fixture::operation("socket"), [2, 1, 6, 0, 0, 0]),
            LinuxResult::Value(0),
        );
        fixture.memory.put(32, &4_u32.to_le_bytes());
        assert_eq!(
            runtime.handle(Fixture::operation("getsockopt"), [0, 6, 2, 40, 32, 0]),
            LinuxResult::Value(0),
        );
        assert_eq!(
            i32::from_le_bytes(fixture.memory.inner.bytes.lock().unwrap()[40..44].try_into().unwrap()),
            1,
        );
        fixture.memory.put(48, &1_i32.to_le_bytes());
        assert_eq!(
            runtime.handle(Fixture::operation("setsockopt"), [0, 6, 3, 48, 4, 0]),
            LinuxResult::Value(0),
        );
        fixture.memory.put(52, &4_u32.to_le_bytes());
        assert_eq!(
            runtime.handle(Fixture::operation("getsockopt"), [0, 6, 3, 56, 52, 0]),
            LinuxResult::Value(0),
        );
        assert_eq!(
            i32::from_le_bytes(fixture.memory.inner.bytes.lock().unwrap()[56..60].try_into().unwrap()),
            1,
        );
    }
}

#[test]
fn output_queue_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut network = fixture.runtime(architecture);
        assert_eq!(
            network.handle(Fixture::operation("socket"), [2, 1, 6, 0, 0, 0]),
            LinuxResult::Value(0),
        );
        let mut filesystem = crate::RuntimeFilesystemSyscalls::new(
            Arc::clone(&fixture.descriptors),
            fixture.memory.clone(),
            architecture,
        )
        .with_socket_ioctl(Arc::new(crate::SocketIoctl::new(
            Arc::clone(&fixture.host),
            Arc::clone(&fixture.sockets),
        )));
        assert_eq!(
            filesystem.handle(Fixture::operation("ioctl"), [0, 0x5411, 64, 0, 0, 0]),
            LinuxResult::Value(0),
        );
        assert_eq!(
            i32::from_le_bytes(fixture.memory.inner.bytes.lock().unwrap()[64..68].try_into().unwrap()),
            9,
        );
        assert_eq!(
            filesystem.handle(Fixture::operation("ioctl"), [0, 0x541b, 72, 0, 0, 0]),
            LinuxResult::Value(0),
        );
        assert_eq!(
            i32::from_le_bytes(fixture.memory.inner.bytes.lock().unwrap()[72..76].try_into().unwrap()),
            7,
        );
        assert_eq!(
            filesystem.handle(Fixture::operation("ioctl"), [0, 0x5411, u64::MAX, 0, 0, 0]),
            LinuxResult::Error(Errno::EFAULT),
        );
    }
}

#[test]
fn peer_identity_stable() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let credentials = hl_network::SenderCredentials {
            process: 43,
            user: 47,
            group: 53,
        };
        let mut runtime = fixture.runtime(architecture).with_credentials(credentials);
        assert_eq!(
            runtime.handle(Fixture::operation("socketpair"), [1, 1, 0, 32, 0, 0]),
            LinuxResult::Value(0),
        );
        fixture.memory.put(120, &12_u32.to_le_bytes());
        assert_eq!(
            runtime.handle(Fixture::operation("getsockopt"), [0, 1, 17, 128, 120, 0]),
            LinuxResult::Value(0),
        );
        let expected = [
            credentials.process.to_le_bytes(),
            credentials.user.to_le_bytes(),
            credentials.group.to_le_bytes(),
        ]
        .concat();
        assert_eq!(&fixture.memory.inner.bytes.lock().unwrap()[128..140], &expected);
    }
}

mod fork {
    use super::*;

    #[test]
    fn table_closes() {
        for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
            let fixture = Fixture::new();
            let mut runtime = fixture.runtime(architecture);
            assert_eq!(
                runtime.handle(Fixture::operation("socketpair"), [1, 1, 0, 32, 0, 0]),
                LinuxResult::Value(0),
            );
            let child = fixture.descriptors.fork();
            fixture.descriptors.close(1).unwrap();
            child.close(0).unwrap();

            assert_eq!(fixture.descriptors.pin(0).unwrap().write(b"ABCDE"), Ok(5));
            let mut request = [0_u8; 5];
            assert_eq!(child.pin(1).unwrap().read(&mut request), Ok(5));
            assert_eq!(&request, b"ABCDE");

            assert_eq!(child.pin(1).unwrap().write(b"sum=335"), Ok(7));
            let mut reply = [0_u8; 7];
            assert_eq!(fixture.descriptors.pin(0).unwrap().read(&mut reply), Ok(7));
            assert_eq!(&reply, b"sum=335");
        }
    }
}
#[path = "connect_test.rs"]
mod connect_tests;
#[path = "message_test.rs"]
mod message_tests;
