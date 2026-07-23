use super::*;

#[test]
fn handshake_writes_and_reads_over_a_socketpair() {
    let (a, b) = UnixStream::pair().unwrap();
    let caps = Capabilities::full("adapter-host");
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
