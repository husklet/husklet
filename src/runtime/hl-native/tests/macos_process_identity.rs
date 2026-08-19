#![cfg(all(feature = "native-test-hooks", target_os = "macos"))]
#![allow(unsafe_code)]

use std::{
    env,
    io::{Read, Write},
    mem,
    os::{
        fd::{AsRawFd, FromRawFd, RawFd},
        unix::{
            net::{UnixDatagram, UnixStream},
            process::CommandExt,
        },
    },
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

fn send_descriptor(socket: RawFd, descriptor: RawFd) {
    let mut byte = 1_u8;
    let mut iov = libc::iovec {
        iov_base: (&raw mut byte).cast(),
        iov_len: 1,
    };
    // SAFETY: CMSG_SPACE is pure arithmetic for this fixed payload size.
    let mut control = vec![0_u8; unsafe { libc::CMSG_SPACE(mem::size_of::<RawFd>() as u32) } as usize];
    let mut message: libc::msghdr = unsafe { mem::zeroed() };
    message.msg_iov = &raw mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = control.len().try_into().expect("control length fits socklen_t");
    // SAFETY: message owns a correctly sized and aligned control region for one descriptor.
    unsafe {
        let header = libc::CMSG_FIRSTHDR(&message);
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(mem::size_of::<RawFd>() as u32);
        libc::CMSG_DATA(header).cast::<RawFd>().write(descriptor);
        assert_eq!(libc::sendmsg(socket, &message, 0), 1);
    }
}

fn receive_descriptor(socket: RawFd) -> RawFd {
    let mut byte = 0_u8;
    let mut iov = libc::iovec {
        iov_base: (&raw mut byte).cast(),
        iov_len: 1,
    };
    // SAFETY: CMSG_SPACE is pure arithmetic for this fixed payload size.
    let mut control = vec![0_u8; unsafe { libc::CMSG_SPACE(mem::size_of::<RawFd>() as u32) } as usize];
    let mut message: libc::msghdr = unsafe { mem::zeroed() };
    message.msg_iov = &raw mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = control.len().try_into().expect("control length fits socklen_t");
    // SAFETY: all message buffers are writable for their declared lengths.
    unsafe {
        assert_eq!(libc::recvmsg(socket, &raw mut message, 0), 1);
        let header = libc::CMSG_FIRSTHDR(&message);
        assert!(!header.is_null());
        assert_eq!((*header).cmsg_level, libc::SOL_SOCKET);
        assert_eq!((*header).cmsg_type, libc::SCM_RIGHTS);
        libc::CMSG_DATA(header).cast::<RawFd>().read()
    }
}

fn sleeping_child() -> Child {
    Command::new("/bin/sleep")
        .arg("30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn child")
}

#[test]
fn capability_is_bound_to_birth_and_generation_and_signals_exit() {
    let mut child = sleeping_child();
    let pid = i32::try_from(child.id()).expect("pid fits i32");
    let (capability, birth, generation) =
        hl_native::checkpoint_process_identity_open_test(pid, 0, 0).expect("mint process capability");
    assert_ne!(birth, 0);
    assert_ne!(generation, 0);
    assert!(hl_native::checkpoint_process_identity_open_test(pid, birth.wrapping_add(1), generation).is_err());
    assert!(hl_native::checkpoint_process_identity_open_test(pid, birth, generation.wrapping_add(1)).is_err());

    child.kill().expect("kill child");
    child.wait().expect("reap child");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let mut event = libc::pollfd {
            fd: capability.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: event points to one initialized poll record.
        let ready = unsafe { libc::poll(&raw mut event, 1, 0) };
        if ready == 1 {
            assert_ne!(event.revents & libc::POLLIN, 0);
            break;
        }
        assert!(Instant::now() < deadline, "process capability did not report exit");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(hl_native::checkpoint_process_identity_open_test(pid, birth, generation).is_err());
}

#[test]
fn exec_child_process() {
    if env::var_os("HL_MACOS_PEER_EXEC_TEST").is_none() {
        return;
    }
    let mut release = [0_u8; 1];
    std::io::stdin().read_exact(&mut release).expect("exec release");
    let error = Command::new("/bin/sleep").arg("30").exec();
    panic!("exec failed: {error}");
}

#[test]
fn capability_signals_exec_before_pid_can_be_reused() {
    let mut child = Command::new(env::current_exe().expect("current test executable"))
        .args(["--exact", "exec_child_process", "--nocapture"])
        .env("HL_MACOS_PEER_EXEC_TEST", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn exec child");
    let pid = i32::try_from(child.id()).expect("pid fits i32");
    let (capability, _, _) =
        hl_native::checkpoint_process_identity_open_test(pid, 0, 0).expect("mint pre-exec capability");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(b"x")
        .expect("release exec");
    let mut event = libc::pollfd {
        fd: capability.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: event points to one initialized poll record.
    assert_eq!(unsafe { libc::poll(&raw mut event, 1, 5_000) }, 1);
    assert_ne!(event.revents & libc::POLLIN, 0);
    child.kill().expect("kill exec child");
    child.wait().expect("reap exec child");
}

#[test]
fn scm_child_process() {
    let Ok(raw) = env::var("HL_MACOS_PEER_TEST_BROKER") else {
        return;
    };
    let broker: RawFd = raw.parse().expect("broker descriptor");
    let (sent, mut retained) = UnixStream::pair().expect("peer channel pair");
    send_descriptor(broker, sent.as_raw_fd());
    drop(sent);
    let mut release = [0_u8; 1];
    let _ = retained.read(&mut release);
}

#[test]
fn scm_rights_peer_capability_uses_creator_identity_and_survives_disappearance() {
    let (parent_broker, child_broker) = UnixDatagram::pair().expect("broker pair");
    // SAFETY: child_broker is live; clear CLOEXEC solely for the immediately following spawn.
    assert_eq!(unsafe { libc::fcntl(child_broker.as_raw_fd(), libc::F_SETFD, 0) }, 0);
    let mut child = Command::new(env::current_exe().expect("current test executable"))
        .args(["--exact", "scm_child_process", "--nocapture"])
        .env("HL_MACOS_PEER_TEST_BROKER", child_broker.as_raw_fd().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn SCM child");
    drop(child_broker);
    let received = receive_descriptor(parent_broker.as_raw_fd());
    // SAFETY: SCM_RIGHTS transferred one uniquely owned descriptor.
    let channel = unsafe { UnixStream::from_raw_fd(received) };
    let child_pid = u64::from(child.id());
    assert!(hl_native::checkpoint_peer_identity_open_test(channel.as_raw_fd(), child_pid + 1).is_err());
    let (capability, pid, birth, generation) =
        hl_native::checkpoint_peer_identity_open_test(channel.as_raw_fd(), child_pid).expect("peer capability");
    assert_eq!(pid, child_pid);
    assert_ne!(birth, 0);
    assert_ne!(generation, 0);

    child.kill().expect("kill SCM child");
    child.wait().expect("reap SCM child");
    let mut event = libc::pollfd {
        fd: capability.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: event points to one initialized poll record.
    assert_eq!(unsafe { libc::poll(&raw mut event, 1, 5_000) }, 1);
    assert_ne!(event.revents & libc::POLLIN, 0);
    assert!(hl_native::checkpoint_peer_identity_open_test(channel.as_raw_fd(), child_pid).is_err());
}
