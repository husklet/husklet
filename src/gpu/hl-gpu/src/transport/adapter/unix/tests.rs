use super::*;
#[cfg(target_os = "linux")]
use crate::runtime::model::sharing::SessionId;
#[cfg(target_os = "linux")]
use crate::{SharedSync, SyncExports};
#[cfg(target_os = "linux")]
use std::os::fd::{FromRawFd, OwnedFd};
#[cfg(target_os = "linux")]
use std::sync::Arc;

#[test]
fn handshake_writes_and_reads_over_a_socketpair() {
    let (a, b) = UnixStream::pair().unwrap();
    let caps = Capabilities::permissive_fixture("adapter-host");
    Connection::new(&a).write_handshake(&caps).unwrap();
    assert_eq!(Connection::new(&b).read_handshake().unwrap(), caps);
}

#[test]
fn scm_rights_transfers_a_working_fd() {
    // Send the read end of a pipe over the socket; the received fd must read the byte written to the
    // pipe's write end — proving it refers to the same open file description.
    let (a, b) = UnixStream::pair().unwrap();
    let mut fds = [0 as RawFd; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    let (read_end, write_end) = (fds[0], fds[1]);

    Connection::new(&a).send_fd(read_end).unwrap();
    let got = Connection::new(&b).recv_fd().unwrap();
    assert!(got >= 0);

    let payload = *b"Z";
    assert_eq!(
        unsafe { libc::write(write_end, payload.as_ptr().cast(), 1) },
        1
    );
    let mut buf = [0u8; 1];
    assert_eq!(unsafe { libc::read(got, buf.as_mut_ptr().cast(), 1) }, 1);
    assert_eq!(buf, payload);

    unsafe {
        libc::close(read_end);
        libc::close(write_end);
        libc::close(got);
    }
}

#[test]
fn doorbell_opens_and_closes() {
    let d = Doorbell::new().unwrap();
    assert!(d.raw_fd() >= 0);
}

#[cfg(target_os = "linux")]
#[test]
fn opaque_sync_fd_duplicates_and_consumes_owned_references() {
    let exports = SyncExports::new();
    let id = exports
        .export(SessionId(1), Arc::new(7u64) as SharedSync)
        .unwrap();
    let token = OpaqueSyncFd::create(id).unwrap();
    let original = token.as_raw_fd();
    let duplicate = token.try_clone().unwrap();
    let duplicate_raw = duplicate.as_raw_fd();
    assert_ne!(original, duplicate_raw);
    drop(token);
    assert_eq!(unsafe { libc::fcntl(original, libc::F_GETFD) }, -1);
    assert_eq!(duplicate.id().unwrap(), id);
    assert_eq!(duplicate.consume().unwrap(), id);
    assert_eq!(unsafe { libc::fcntl(duplicate_raw, libc::F_GETFD) }, -1);
}

#[cfg(target_os = "linux")]
#[test]
fn opaque_sync_fd_rejects_unsealed_and_forged_identities() {
    let raw = unsafe {
        libc::memfd_create(
            b"forged-sync\0".as_ptr().cast(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    assert!(raw >= 0);
    let forged = unsafe { OwnedFd::from_raw_fd(raw) };
    assert!(OpaqueSyncFd::from_owned(forged).is_err());

    let exports = SyncExports::new();
    let live = exports
        .export(SessionId(1), Arc::new(9u64) as SharedSync)
        .unwrap();
    let forged = crate::SyncExportId::from_parts(live.serial(), live.authenticity() ^ 1);
    let carrier = OpaqueSyncFd::create(forged).unwrap();
    let decoded = carrier.consume().unwrap();
    assert!(exports.import(SessionId(2), decoded).is_err());
    assert!(exports.import(SessionId(2), live).is_ok());
}
