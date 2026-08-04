use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use hl_ipc::{Credentials, MessageQueueId};
use hl_isa::GuestArchitecture;
use hl_linux::{Errno, IPC_CREAT, IPC_RMID, IpcSyscalls, LinuxResult, MSG_NOWAIT};
use hl_sync::Interruption;

use crate::BlockingWait;

use super::Fixture;

struct WaitPort(Arc<Interruption>);

impl BlockingWait for WaitPort {
    fn interruption(&self) -> Arc<Interruption> {
        self.0.clone()
    }
}

fn architectures() -> [GuestArchitecture; 2] {
    [GuestArchitecture::Aarch64, GuestArchitecture::X86_64]
}

impl Fixture {
    fn wait_message_waiter(&self) {
        self.wait_ipc_waiter(|| self.messages.active_waiters());
    }

    fn wait_semaphore_waiter(&self) {
        self.wait_ipc_waiter(|| self.semaphores.active_waiters());
    }

    fn wait_ipc_waiter(&self, count: impl Fn() -> usize) {
        let timeout = Instant::now() + Duration::from_secs(2);
        while Instant::now() < timeout {
            if count() != 0 {
                return;
            }
            thread::sleep(Duration::from_millis(1));
        }
        panic!("IPC operation did not register a blocking waiter");
    }
}

#[test]
fn blocking_preserves_copyout() {
    for architecture in architectures() {
        let fixture = Fixture::new();
        let LinuxResult::Value(id) =
            fixture.call(architecture, "msgget", [0, u64::from(IPC_CREAT | 0o600), 0, 0, 0, 0])
        else {
            panic!("queue creation failed");
        };
        let wait = Arc::new(WaitPort(Arc::new(Interruption::new())));
        let mut receiver = fixture.runtime(architecture).with_wait_port(wait);
        let operation = Fixture::operation(architecture, "msgrcv");
        let handle = thread::spawn(move || receiver.handle(operation, [id, 96, 4, 0, 0, 0]));
        fixture.runtime.3.put(32, &7_i64.to_le_bytes());
        fixture.runtime.3.put(40, b"rust");
        assert_eq!(
            fixture.call(architecture, "msgsnd", [id, 32, 4, u64::from(MSG_NOWAIT), 0, 0]),
            LinuxResult::Value(0),
        );
        assert_eq!(handle.join().unwrap(), LinuxResult::Value(4));
    }
}

#[test]
fn blocking_remains_retryable() {
    for architecture in architectures() {
        let fixture = Fixture::new();
        let LinuxResult::Value(id) =
            fixture.call(architecture, "msgget", [0, u64::from(IPC_CREAT | 0o600), 0, 0, 0, 0])
        else {
            panic!("queue creation failed");
        };
        let interruption = Arc::new(Interruption::new());
        let wait = Arc::new(WaitPort(interruption.clone()));
        let mut receiver = fixture.runtime(architecture).with_wait_port(wait);
        let operation = Fixture::operation(architecture, "msgrcv");
        let handle = thread::spawn(move || receiver.handle(operation, [id, 96, 4, 0, 0, 0]));
        interruption.interrupt();
        assert_eq!(handle.join().unwrap(), LinuxResult::Error(Errno::EINTR));
    }
}

#[test]
fn zero_eagain_mutation() {
    for architecture in architectures() {
        let fixture = Fixture::new();
        let LinuxResult::Value(id) =
            fixture.call(architecture, "semget", [0, 1, u64::from(IPC_CREAT | 0o600), 0, 0, 0])
        else {
            panic!("semaphore creation failed");
        };
        fixture.runtime.3.put(32, &[0, 0, 0xff, 0xff, 0, 0]);
        fixture.runtime.3.put(64, &[0; 16]);
        let mut runtime = fixture
            .runtime(architecture)
            .with_wait_port(Arc::new(WaitPort(Arc::new(Interruption::new()))));
        assert_eq!(
            runtime.handle(Fixture::operation(architecture, "semtimedop"), [id, 32, 1, 64, 0, 0],),
            LinuxResult::Error(Errno::EAGAIN),
        );
    }
}

#[test]
fn blocked_queue_capacity() {
    for architecture in architectures() {
        let fixture = Fixture::new();
        let LinuxResult::Value(id) =
            fixture.call(architecture, "msgget", [0, u64::from(IPC_CREAT | 0o600), 0, 0, 0, 0])
        else {
            panic!("queue creation failed");
        };
        fixture
            .messages
            .set_control(
                MessageQueueId::from_linux_id(id as i32).unwrap(),
                Credentials { uid: 1000, gid: 1000 },
                Credentials { uid: 1000, gid: 1000 },
                0o600,
                1,
                2,
            )
            .unwrap();
        fixture.runtime.3.put(32, &1_i64.to_le_bytes());
        fixture.runtime.3.put(40, b"a");
        assert_eq!(
            fixture.call(architecture, "msgsnd", [id, 32, 1, u64::from(MSG_NOWAIT), 0, 0],),
            LinuxResult::Value(0),
        );
        let mut sender = fixture
            .runtime(architecture)
            .with_wait_port(Arc::new(WaitPort(Arc::new(Interruption::new()))));
        let operation = Fixture::operation(architecture, "msgsnd");
        let handle = thread::spawn(move || sender.handle(operation, [id, 32, 1, 0, 0, 0]));
        assert_eq!(
            fixture.call(architecture, "msgrcv", [id, 96, 1, 0, u64::from(MSG_NOWAIT), 0],),
            LinuxResult::Value(1),
        );
        assert_eq!(handle.join().unwrap(), LinuxResult::Value(0));
    }
}

#[test]
fn removed_receive_eidrm() {
    for architecture in architectures() {
        let fixture = Fixture::new();
        let LinuxResult::Value(id) =
            fixture.call(architecture, "msgget", [0, u64::from(IPC_CREAT | 0o600), 0, 0, 0, 0])
        else {
            panic!("queue creation failed");
        };
        let mut receiver = fixture
            .runtime(architecture)
            .with_wait_port(Arc::new(WaitPort(Arc::new(Interruption::new()))));
        let operation = Fixture::operation(architecture, "msgrcv");
        let handle = thread::spawn(move || receiver.handle(operation, [id, 96, 1, 0, 0, 0]));
        fixture.wait_message_waiter();
        assert_eq!(
            fixture.call(architecture, "msgctl", [id, u64::from(IPC_RMID), 0, 0, 0, 0],),
            LinuxResult::Value(0),
        );
        assert_eq!(handle.join().unwrap(), LinuxResult::Error(Errno::EIDRM));
    }
}

#[test]
fn removed_send_eidrm() {
    for architecture in architectures() {
        let fixture = Fixture::new();
        let LinuxResult::Value(id) =
            fixture.call(architecture, "msgget", [0, u64::from(IPC_CREAT | 0o600), 0, 0, 0, 0])
        else {
            panic!("queue creation failed");
        };
        fixture
            .messages
            .set_control(
                MessageQueueId::from_linux_id(id as i32).unwrap(),
                Credentials { uid: 1000, gid: 1000 },
                Credentials { uid: 1000, gid: 1000 },
                0o600,
                1,
                2,
            )
            .unwrap();
        fixture.runtime.3.put(32, &1_i64.to_le_bytes());
        fixture.runtime.3.put(40, b"a");
        assert_eq!(
            fixture.call(architecture, "msgsnd", [id, 32, 1, u64::from(MSG_NOWAIT), 0, 0],),
            LinuxResult::Value(0),
        );
        let mut sender = fixture
            .runtime(architecture)
            .with_wait_port(Arc::new(WaitPort(Arc::new(Interruption::new()))));
        let operation = Fixture::operation(architecture, "msgsnd");
        let handle = thread::spawn(move || sender.handle(operation, [id, 32, 1, 0, 0, 0]));
        fixture.wait_message_waiter();
        assert_eq!(
            fixture.call(architecture, "msgctl", [id, u64::from(IPC_RMID), 0, 0, 0, 0],),
            LinuxResult::Value(0),
        );
        assert_eq!(handle.join().unwrap(), LinuxResult::Error(Errno::EIDRM));
    }
}

#[test]
fn removed_operation_eidrm() {
    for architecture in architectures() {
        let fixture = Fixture::new();
        let LinuxResult::Value(id) =
            fixture.call(architecture, "semget", [0, 1, u64::from(IPC_CREAT | 0o600), 0, 0, 0])
        else {
            panic!("semaphore creation failed");
        };
        fixture.runtime.3.put(32, &[0, 0, 0xff, 0xff, 0, 0]);
        let mut semaphore = fixture
            .runtime(architecture)
            .with_wait_port(Arc::new(WaitPort(Arc::new(Interruption::new()))));
        let operation = Fixture::operation(architecture, "semop");
        let handle = thread::spawn(move || semaphore.handle(operation, [id, 32, 1, 0, 0, 0]));
        fixture.wait_semaphore_waiter();
        assert_eq!(
            fixture.call(architecture, "semctl", [id, 0, u64::from(IPC_RMID), 0, 0, 0],),
            LinuxResult::Value(0),
        );
        assert_eq!(handle.join().unwrap(), LinuxResult::Error(Errno::EIDRM));
    }
}
