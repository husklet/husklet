#![cfg(feature = "native-test-hooks")]
#![allow(unsafe_code)]

//! The process- and peer-identity capabilities these cases exercise belong to the macOS host
//! adapter: a kqueue `EVFILT_PROC` registration bound to a pid's birth time and generation, and the
//! `SCM_RIGHTS` peer capability minted from the creator's identity. Linux has neither mechanism and
//! none of the `checkpoint_*_identity_open_test` hooks resolve there, so there is nothing here for
//! this host to run.
//!
//! The gate was `#![cfg(all(..., target_os = "macos"))]` at FILE scope, so on a Linux host the
//! whole target compiled to zero tests and reported `test result: ok`. 0bb190fa4 aligned this
//! file's `SCM_RIGHTS` control region from a Linux box with no host on which any of it ran. The
//! gate is now per item so the file can still say what it left uncovered -- and per item rather
//! than around a `mod`, because two of these cases re-exec the test binary with `--exact
//! <test name>` and a module would silently rename the filter's target.

#[cfg(target_os = "macos")]
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

/// Storage for one `SCM_RIGHTS` control message, aligned for `libc::cmsghdr`.
///
/// `CMSG_FIRSTHDR` reinterprets `msg_control` as a `cmsghdr` and `recvmsg` writes a
/// header through that pointer, so the region must satisfy the header's alignment.
/// `Vec<u8>` only promises alignment 1 — whatever the allocator happens to return is
/// not a guarantee the type carries — so the region is backed by `u64`, whose
/// alignment is at least `cmsghdr`'s on every supported target. The returned length
/// is the exact `CMSG_SPACE` byte count, not the rounded-up word count.
#[cfg(target_os = "macos")]
fn control_storage() -> (Vec<u64>, usize) {
    // SAFETY: CMSG_SPACE is pure arithmetic for this fixed payload size.
    let bytes = unsafe { libc::CMSG_SPACE(mem::size_of::<RawFd>() as u32) } as usize;
    (vec![0_u64; bytes.div_ceil(mem::size_of::<u64>())], bytes)
}

#[cfg(target_os = "macos")]
fn send_descriptor(socket: RawFd, descriptor: RawFd) {
    let mut byte = 1_u8;
    let mut iov = libc::iovec {
        iov_base: (&raw mut byte).cast(),
        iov_len: 1,
    };
    let (mut control, control_len) = control_storage();
    // SAFETY: msghdr is a plain C aggregate for which an all-zero value is valid.
    let mut message: libc::msghdr = unsafe { mem::zeroed() };
    message.msg_iov = &raw mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = control_len.try_into().expect("control length fits socklen_t");
    // SAFETY: message owns a control region sized by CMSG_SPACE and aligned for cmsghdr,
    // so CMSG_FIRSTHDR yields one writable header. The descriptor is copied byte-wise
    // rather than through a `*mut RawFd`, because CMSG_DATA's offset carries no
    // alignment guarantee of its own.
    unsafe {
        let header = libc::CMSG_FIRSTHDR(&raw const message);
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(mem::size_of::<RawFd>() as u32);
        libc::CMSG_DATA(header).copy_from_nonoverlapping(descriptor.to_ne_bytes().as_ptr(), mem::size_of::<RawFd>());
        assert_eq!(libc::sendmsg(socket, &raw const message, 0), 1);
    }
}

#[cfg(target_os = "macos")]
fn receive_descriptor(socket: RawFd) -> RawFd {
    let mut byte = 0_u8;
    let mut iov = libc::iovec {
        iov_base: (&raw mut byte).cast(),
        iov_len: 1,
    };
    let (mut control, control_len) = control_storage();
    // SAFETY: msghdr is a plain C aggregate for which an all-zero value is valid.
    let mut message: libc::msghdr = unsafe { mem::zeroed() };
    message.msg_iov = &raw mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = control_len.try_into().expect("control length fits socklen_t");
    // SAFETY: all message buffers are writable for their declared lengths, and the
    // control region is aligned for the cmsghdr the kernel writes into it. The
    // descriptor is copied byte-wise rather than loaded through a `*mut RawFd`,
    // because CMSG_DATA's offset carries no alignment guarantee of its own.
    unsafe {
        assert_eq!(libc::recvmsg(socket, &raw mut message, 0), 1);
        let header = libc::CMSG_FIRSTHDR(&raw const message);
        assert!(!header.is_null());
        assert_eq!((*header).cmsg_level, libc::SOL_SOCKET);
        assert_eq!((*header).cmsg_type, libc::SCM_RIGHTS);
        let mut descriptor = [0_u8; mem::size_of::<RawFd>()];
        libc::CMSG_DATA(header).copy_to_nonoverlapping(descriptor.as_mut_ptr(), descriptor.len());
        RawFd::from_ne_bytes(descriptor)
    }
}

#[cfg(target_os = "macos")]
fn sleeping_child() -> Child {
    Command::new("/bin/sleep")
        .arg("30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn child")
}

#[cfg(target_os = "macos")]
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

#[cfg(target_os = "macos")]
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

#[cfg(target_os = "macos")]
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

#[cfg(target_os = "macos")]
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

#[cfg(target_os = "macos")]
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

/// A file gated out at file scope is indistinguishable in the harness output from one whose tests
/// all passed, so say which coverage this host does not have. The notice goes to the real stderr
/// descriptor rather than through `eprintln!`, because libtest captures Rust-level output and
/// prints it only for a FAILING test -- the same reason `hl-native`'s `guest_compiler_present`
/// skip notice writes to descriptor 2.
#[cfg(not(target_os = "macos"))]
#[test]
fn macos_process_and_peer_identity_are_uncovered_on_this_host() {
    let notice = "SKIP macos_process_identity: 3 cases left UNCOVERED -- kqueue EVFILT_PROC birth/\
                  generation binding and the SCM_RIGHTS peer capability exist only on Darwin.\n";
    // SAFETY: a write of a `'static` initialized buffer to the process's stderr descriptor. It
    // borrows nothing beyond the call, and a short or failed write is not an error worth acting on.
    unsafe {
        libc::write(2, notice.as_ptr().cast(), notice.len());
    }
}
