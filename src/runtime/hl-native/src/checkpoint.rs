//! Owned checkpoint transport resources for the native engine.

#![allow(unsafe_code)]

use std::{
    ffi::c_void,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    os::unix::net::UnixStream,
    ptr::NonNull,
    time::Duration,
};

use crate::{bindings, engine::STATUS_OK};

/// Receiving end of the native engine's checkpoint channel broker.
pub struct CheckpointBroker(OwnedFd);

#[derive(Debug)]
pub struct AuthenticatedCheckpointPeer {
    pub host_pid: u64,
    pub host_birth: u64,
    pub host_generation: u64,
    pub(crate) process_handle: OwnedFd,
}

impl AuthenticatedCheckpointPeer {
    /// Reads one complete checkpoint frame segment while treating peer exit or
    /// exec as terminal authority revocation.
    #[doc(hidden)]
    pub fn read_exact(&self, channel: &UnixStream, mut output: &mut [u8]) -> std::io::Result<()> {
        while !output.is_empty() {
            let mut waiting = [
                libc::pollfd {
                    fd: self.process_handle.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: channel.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            // SAFETY: waiting is a writable two-element poll array and both descriptors remain borrowed.
            let ready = unsafe { libc::poll(waiting.as_mut_ptr(), 2, -1) };
            if ready < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if waiting[0].revents != 0 {
                return Err(std::io::ErrorKind::ConnectionAborted.into());
            }
            if waiting[1].revents == 0 {
                continue;
            }
            // SAFETY: output is writable for its length; this object is the sole channel reader.
            let count = unsafe { libc::read(channel.as_raw_fd(), output.as_mut_ptr().cast(), output.len()) };
            if count < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if count == 0 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            output = &mut output[usize::try_from(count).expect("positive read count fits usize")..];
        }
        Ok(())
    }

    /// The authenticated capability on this exact process incarnation.
    ///
    /// Exposed so a holder can retain it past the connection that carried it: the capability, not the
    /// pid, is what makes a later reach at this process safe against pid reuse.
    #[doc(hidden)]
    #[must_use]
    pub fn process_capability(&self) -> &std::os::fd::OwnedFd {
        &self.process_handle
    }

    /// Returns false once this exact process incarnation exited or exec'd.
    #[doc(hidden)]
    pub fn is_live(&self) -> std::io::Result<bool> {
        let mut waiting = libc::pollfd {
            fd: self.process_handle.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        loop {
            // SAFETY: waiting is one writable poll record and the capability remains borrowed.
            let ready = unsafe { libc::poll(&raw mut waiting, 1, 0) };
            if ready >= 0 {
                return Ok(ready == 0 && waiting.revents == 0);
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
}

impl CheckpointBroker {
    /// Waits for one guest process to connect to the checkpoint broker.
    #[must_use]
    pub fn accept(&self, timeout: Duration) -> Option<(UnixStream, AuthenticatedCheckpointPeer)> {
        let timeout_ms = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
        let mut host_pid = 0;
        let mut host_birth = 0;
        let mut host_generation = 0;
        let mut process_handle = -1;
        // SAFETY: the broker descriptor remains owned by self through the call;
        // a nonnegative result transfers one stream descriptor to Rust.
        let channel = unsafe {
            bindings::hl_c_backend_checkpoint_broker_accept_authenticated(
                self.0.as_raw_fd(),
                timeout_ms,
                &raw mut host_pid,
                &raw mut host_birth,
                &raw mut host_generation,
                &raw mut process_handle,
            )
        };
        if channel < 0 || process_handle < 0 {
            for descriptor in [channel, process_handle] {
                if descriptor >= 0 {
                    // SAFETY: an inconsistent successful return still transfers this descriptor.
                    drop(unsafe { OwnedFd::from_raw_fd(descriptor) });
                }
            }
            return None;
        }
        Some({
            // SAFETY: C returned a uniquely owned descriptor.
            (
                unsafe { UnixStream::from_raw_fd(channel) },
                AuthenticatedCheckpointPeer {
                    host_pid,
                    host_birth,
                    host_generation,
                    // SAFETY: authenticated accept uniquely transfers this live process capability.
                    process_handle: unsafe { OwnedFd::from_raw_fd(process_handle) },
                },
            )
        })
    }
}

#[cfg(all(test, feature = "native-test-hooks"))]
mod peer_tests {
    use super::*;
    use std::os::fd::AsRawFd;

    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn checkpoint_peer_identity_comes_from_kernel_and_has_birth() {
        let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (local, _peer) = UnixStream::pair().unwrap();
        let claimed = u64::from(std::process::id());
        let mut pid = u64::MAX;
        let mut birth = 0;
        // SAFETY: the descriptor remains live and both output pointers address initialized writable values.
        let status = unsafe {
            bindings::hl_c_backend_checkpoint_peer_authenticate_test(
                local.as_raw_fd(),
                claimed,
                &raw mut pid,
                &raw mut birth,
            )
        };
        assert_eq!(status, 0);
        assert_eq!(pid, claimed);
        assert_ne!(birth, 0);
    }

    #[test]
    fn checkpoint_peer_rejects_forged_hello_pid() {
        let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (local, _peer) = UnixStream::pair().unwrap();
        let claimed = u64::from(std::process::id()).checked_add(1).unwrap();
        let mut pid = u64::MAX;
        let mut birth = u64::MAX;
        // SAFETY: the descriptor remains live and both output pointers address initialized writable values.
        let status = unsafe {
            bindings::hl_c_backend_checkpoint_peer_authenticate_test(
                local.as_raw_fd(),
                claimed,
                &raw mut pid,
                &raw mut birth,
            )
        };
        assert_ne!(status, 0);
        assert_eq!(pid, 0);
        assert_eq!(birth, 0);
    }
}

/// Broker child and shared generation trigger installed into a native engine.
pub struct CheckpointTransport {
    broker_child: OwnedFd,
    trigger_descriptor: OwnedFd,
    trigger_mapping: NonNull<c_void>,
}

// SAFETY: the mapping contains the native transport's shared generation word;
// C is the sole accessor and its bump operation is the synchronization boundary.
unsafe impl Send for CheckpointTransport {}
// SAFETY: callers may bump the generation concurrently; ownership and teardown
// remain unique to this value.
unsafe impl Sync for CheckpointTransport {}

impl CheckpointTransport {
    /// Opens a real checkpoint channel through this transport's broker child.
    #[cfg(feature = "native-test-hooks")]
    #[doc(hidden)]
    pub fn connect_for_test(&self) -> std::io::Result<UnixStream> {
        // SAFETY: the test hook borrows the live broker-child descriptor and transfers one channel.
        let descriptor =
            unsafe { bindings::hl_c_backend_checkpoint_channel_connect_test(self.broker_child.as_raw_fd()) };
        if descriptor < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: the native helper transfers unique ownership on success.
        Ok(unsafe { UnixStream::from_raw_fd(descriptor) })
    }

    /// Announces one checkpoint channel on the broker using nothing but raw
    /// syscalls, for use inside a `fork()` child of a multi-threaded process.
    ///
    /// `connect_for_test` cannot be called there. It reaches
    /// `hl_host_process_fd_private_add`, which takes a process-wide
    /// `pthread_mutex_t`; `fork()` copies that mutex in whatever state another
    /// thread left it, so a child that arrives while a sibling held it blocks in
    /// `pthread_mutex_lock` forever against a thread that does not exist in the
    /// child. That wedged three lanes for hours before it was named.
    ///
    /// This path allocates nothing, takes no lock, and runs no destructor: a
    /// `socketpair` and one `sendmsg` carrying the peer end. It returns
    /// `(channel, announced)`: the caller's channel, and this process's own
    /// reference to the end the broker will read. Both are the caller's to
    /// close, and `announced` must stay open until the broker has answered a
    /// request on the channel -- `checkpoint_channel_receipt_release` in
    /// `engine/checkpoint_channel.c` records what dropping it early costs. On
    /// failure it returns `(-1, -1)` with `errno` set.
    ///
    /// The 16-byte announcement is `hl_ckpt_hello` from
    /// `include/hl/checkpoint_stream.h`. The duplication is deliberate and
    /// self-checking rather than silent: a broker that stops accepting this
    /// exact magic and ABI rejects the connection, and every caller asserts on
    /// the accept that follows.
    ///
    /// # Safety
    ///
    /// The caller must be a `fork()` child that has not yet run Rust code which
    /// could have inherited a held lock, and must own the returned descriptor.
    #[cfg(feature = "native-test-hooks")]
    #[doc(hidden)]
    #[must_use]
    pub unsafe fn connect_in_forked_child_for_test(&self) -> (i32, i32) {
        const MAGIC_HELLO: u32 = 0x484b_4348;
        const STREAM_ABI: u32 = 2;
        let mut hello = [0_u8; 16];
        hello[0..4].copy_from_slice(&MAGIC_HELLO.to_ne_bytes());
        hello[4..8].copy_from_slice(&STREAM_ABI.to_ne_bytes());
        let mut pair = [-1_i32; 2];
        // SAFETY: pair names writable storage for two new descriptors.
        if unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, pair.as_mut_ptr()) } != 0 {
            return (-1, -1);
        }
        // SAFETY: getpid takes no argument and cannot fail.
        let pid = u64::try_from(unsafe { libc::getpid() }).unwrap_or(0);
        hello[8..16].copy_from_slice(&pid.to_ne_bytes());
        let mut vector = libc::iovec {
            iov_base: hello.as_mut_ptr().cast(),
            iov_len: hello.len(),
        };
        let mut control = [0_u8; 64];
        // SAFETY: message addresses live stack storage and carries one correctly sized SCM_RIGHTS record.
        let sent = unsafe {
            let mut message: libc::msghdr = std::mem::zeroed();
            message.msg_iov = &raw mut vector;
            message.msg_iovlen = 1;
            message.msg_control = control.as_mut_ptr().cast();
            message.msg_controllen = control.len() as _;
            let header = libc::CMSG_FIRSTHDR(&raw const message);
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len = libc::CMSG_LEN(size_of::<i32>() as _) as _;
            std::ptr::copy_nonoverlapping(
                (&raw const pair[1]).cast::<u8>(),
                libc::CMSG_DATA(header),
                size_of::<i32>(),
            );
            message.msg_controllen = libc::CMSG_SPACE(size_of::<i32>() as _) as _;
            libc::sendmsg(self.broker_child.as_raw_fd(), &raw const message, 0)
        };
        if sent != hello.len() as isize {
            // SAFETY: neither end is returned on this path.
            unsafe { libc::close(pair[0]) };
            // SAFETY: neither end is returned on this path.
            unsafe { libc::close(pair[1]) };
            return (-1, -1);
        }
        (pair[0], pair[1])
    }

    pub(crate) fn configure(&self, backend: *mut crate::bindings::Backend) -> i32 {
        // SAFETY: backend is owned by the Engine caller and both descriptors remain live.
        unsafe {
            bindings::hl_c_backend_checkpoint_configure(
                backend,
                self.broker_child.as_raw_fd(),
                self.trigger_descriptor.as_raw_fd(),
            )
        }
    }
    /// Creates a broker and trigger pair owned entirely by this package.
    pub fn create() -> std::io::Result<(CheckpointBroker, Self)> {
        // The bridge stubs report an unloaded engine with the same status a genuine
        // platform failure uses, so resolve the library first and keep its reason.
        crate::loader::api()
            .map_err(|error| std::io::Error::other(format!("native engine library unavailable: {error}")))?;
        let mut parent = -1;
        let mut child = -1;
        // SAFETY: both output locations are writable; success transfers two descriptors.
        let pair_status = unsafe { bindings::hl_c_backend_checkpoint_broker_pair(&raw mut parent, &raw mut child) };
        if pair_status != STATUS_OK || parent < 0 || child < 0 {
            return Err(std::io::Error::other("native checkpoint broker creation failed"));
        }
        let mut trigger = -1;
        let mut mapping = std::ptr::null_mut();
        // SAFETY: both output locations are writable; success transfers a descriptor and mapping.
        let trigger_status =
            unsafe { bindings::hl_c_backend_checkpoint_trigger_create(&raw mut trigger, &raw mut mapping) };
        let Some(trigger_mapping) = NonNull::new(mapping) else {
            // SAFETY: pair creation transferred unique ownership above.
            drop(unsafe { OwnedFd::from_raw_fd(parent) });
            // SAFETY: pair creation transferred unique ownership above.
            drop(unsafe { OwnedFd::from_raw_fd(child) });
            return Err(std::io::Error::other("native checkpoint trigger creation failed"));
        };
        if trigger_status != STATUS_OK || trigger < 0 {
            // SAFETY: C produced this mapping; destroy accepts an absent descriptor.
            unsafe { bindings::hl_c_backend_checkpoint_trigger_destroy(mapping, trigger) };
            // SAFETY: pair creation transferred unique ownership above.
            drop(unsafe { OwnedFd::from_raw_fd(parent) });
            // SAFETY: pair creation transferred unique ownership above.
            drop(unsafe { OwnedFd::from_raw_fd(child) });
            return Err(std::io::Error::other("native checkpoint trigger creation failed"));
        }
        // SAFETY: all three descriptors were uniquely transferred on success.
        Ok(unsafe {
            (
                CheckpointBroker(OwnedFd::from_raw_fd(parent)),
                Self {
                    broker_child: OwnedFd::from_raw_fd(child),
                    trigger_descriptor: OwnedFd::from_raw_fd(trigger),
                    trigger_mapping,
                },
            )
        })
    }

    /// Installs duplicates into the native engine's private descriptor table.
    pub fn adopt(&self, isa: u32) -> std::io::Result<()> {
        // SAFETY: C borrows both live descriptors and relocates private duplicates.
        let status = unsafe {
            bindings::hl_c_backend_checkpoint_adopt(
                isa,
                self.broker_child.as_raw_fd(),
                self.trigger_descriptor.as_raw_fd(),
            )
        };
        (status == STATUS_OK)
            .then_some(())
            .ok_or_else(|| std::io::Error::other("native checkpoint transport adoption failed"))
    }

    /// Advances the capture generation observed at guest safepoints.
    #[must_use]
    pub fn bump(&self) -> u32 {
        // SAFETY: the mapping is live until this owner is dropped.
        unsafe { bindings::hl_c_backend_checkpoint_trigger_bump(self.trigger_mapping.as_ptr()) }
    }

    /// Native signal reserved for interrupting guest checkpoint safepoints.
    #[must_use]
    pub fn interrupt_signal(isa: u32) -> i32 {
        // SAFETY: immutable native constant query.
        unsafe { bindings::hl_c_backend_checkpoint_interrupt_signal(isa) }
    }
}

impl Drop for CheckpointTransport {
    fn drop(&mut self) {
        // SAFETY: destroy owns this live mapping; -1 leaves Rust's OwnedFd as the sole descriptor owner.
        unsafe {
            bindings::hl_c_backend_checkpoint_trigger_destroy(self.trigger_mapping.as_ptr(), -1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static ADOPTION: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[cfg(target_os = "linux")]
    fn matching_descriptors(descriptor: i32) -> std::collections::BTreeSet<i32> {
        use std::os::unix::fs::MetadataExt as _;

        let expected = std::fs::metadata(format!("/dev/fd/{descriptor}")).expect("owned descriptor metadata");
        std::fs::read_dir("/dev/fd")
            .expect("descriptor directory")
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().to_str()?.parse::<i32>().ok())
            .filter(|candidate| {
                std::fs::metadata(format!("/dev/fd/{candidate}"))
                    .is_ok_and(|metadata| metadata.dev() == expected.dev() && metadata.ino() == expected.ino())
            })
            .collect()
    }

    /// Open descriptors in this process, as a count rather than a set: the tests below reason about how
    /// many references one announcement leaves behind, not about which numbers they landed on.
    #[cfg(feature = "native-test-hooks")]
    fn open_descriptor_count() -> usize {
        let directory = if cfg!(target_os = "linux") {
            "/proc/self/fd"
        } else {
            "/dev/fd"
        };
        std::fs::read_dir(directory).expect("descriptor directory").count()
    }

    /// The announcing side must still reference the broker end of its channel after the hello is sent,
    /// and must drop that reference once the broker has answered.
    ///
    /// Counted rather than inspected, because the reference is engine-private and has no accessor: each
    /// announcement releases the previous one's reference and takes two of its own, so from a process
    /// that has never announced the table grows 2, 3, 4. Dropping the reference at send time reads 1, 2,
    /// 3; never releasing it reads 2, 4, 6. Run in a fresh child for the same reason the registry test
    /// is: the count is only readable from a process whose announcement history is known.
    #[cfg(feature = "native-test-hooks")]
    #[test]
    fn an_announcement_holds_the_broker_end_until_the_broker_answers() {
        const CHILD: &str = "HL_NATIVE_CHECKPOINT_ANNOUNCEMENT_REFERENCE_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
                .args([
                    "--exact",
                    "checkpoint::tests::an_announcement_holds_the_broker_end_until_the_broker_answers",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(CHILD, "1")
                .output()
                .expect("spawn announcement reference child");
            assert!(
                output.status.success(),
                "announcement reference child failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        let _adoption = ADOPTION.lock().expect("checkpoint adoption lock");
        let (_broker, transport) = CheckpointTransport::create().expect("checkpoint transport");
        let base = open_descriptor_count();
        let first = transport.connect_for_test().expect("first announcement");
        assert_eq!(
            open_descriptor_count() - base,
            2,
            "an announcement must keep its own reference to the broker end while it is in flight"
        );
        let second = transport.connect_for_test().expect("second announcement");
        assert_eq!(
            open_descriptor_count() - base,
            3,
            "the previous announcement's reference must be released, not leaked"
        );
        let third = transport.connect_for_test().expect("third announcement");
        assert_eq!(
            open_descriptor_count() - base,
            4,
            "every announcement releases exactly one reference and takes two"
        );
        drop((first, second, third));
    }

    /// A channel the broker accepted must answer its first request.
    ///
    /// This is the user-visible defect. A restoring member announces itself and reads `proc.<gpid>/meta`
    /// in the same breath, and an announcement whose broker end was collected while it was in flight
    /// reads EOF instead -- "read the broker's reply: this channel ended before one arrived" -- with the
    /// member alive, its request already written, and the socket still connected. Measured on macOS 26.3
    /// (Darwin 25.3.0, arm64) at 5 in 40,000 announcements before this was fixed. A Linux host cannot
    /// express it, so the loop is a control there rather than a probe, which is why the round count
    /// differs by host.
    ///
    /// Run in a fresh child: it announces tens of thousands of channels, and the engine-global channel
    /// cache those announcements leave behind is not state the rest of this suite should inherit.
    #[cfg(feature = "native-test-hooks")]
    #[test]
    fn an_accepted_channel_answers_its_first_request() {
        use std::io::{Read as _, Write as _};

        const CHILD: &str = "HL_NATIVE_CHECKPOINT_FIRST_REQUEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
                .args([
                    "--exact",
                    "checkpoint::tests::an_accepted_channel_answers_its_first_request",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(CHILD, "1")
                .output()
                .expect("spawn first-request child");
            assert!(
                output.status.success(),
                "first-request child failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        let _adoption = ADOPTION.lock().expect("checkpoint adoption lock");
        let (broker, transport) = CheckpointTransport::create().expect("checkpoint transport");
        let rounds = if cfg!(target_os = "macos") { 20_000 } else { 2_000 };
        for round in 0..rounds {
            let mut announced = transport.connect_for_test().expect("announce a channel");
            announced.write_all(&[9_u8; 32]).expect("write the first request");
            let (channel, peer) = broker
                .accept(Duration::from_secs(5))
                .unwrap_or_else(|| panic!("round {round}: the broker never accepted the announcement"));
            let mut request = [0_u8; 32];
            peer.read_exact(&channel, &mut request).unwrap_or_else(|error| {
                panic!("round {round}: the accepted channel ended before its first request arrived: {error}")
            });
            assert_eq!(request, [9_u8; 32]);
            let mut writable = &channel;
            writable.write_all(&[0_u8; 16]).expect("answer the request");
            let mut reply = [0_u8; 16];
            announced.read_exact(&mut reply).expect("read the answer");
        }
    }

    #[test]
    fn transport_resources_are_live_and_generation_advances() {
        let _adoption = ADOPTION.lock().expect("checkpoint adoption lock");
        let (_broker, transport) = CheckpointTransport::create().expect("checkpoint transport");
        assert_eq!(transport.bump(), 1);
        assert_eq!(transport.bump(), 2);
        assert!(CheckpointTransport::interrupt_signal(1) > 0);
        transport.adopt(1).expect("adopt transport descriptors");
    }

    #[test]
    fn transport_generation_rollover_never_publishes_zero() {
        use std::collections::BTreeSet;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU32, Ordering};

        let (_broker, transport) = CheckpointTransport::create().expect("checkpoint transport");
        let transport = Arc::new(transport);
        // SAFETY: the transport uniquely owns a live, properly aligned u32 mapping.
        let generation = unsafe { AtomicU32::from_ptr(transport.trigger_mapping.as_ptr().cast()) };
        generation.store(u32::MAX - 1, Ordering::Release);
        assert_eq!(transport.bump(), u32::MAX);
        assert_eq!(transport.bump(), 1);
        assert_eq!(generation.load(Ordering::Acquire), 1);

        generation.store(u32::MAX - 3, Ordering::Release);
        let workers = (0..8)
            .map(|_| {
                let transport = Arc::clone(&transport);
                std::thread::spawn(move || (0..100).map(|_| transport.bump()).collect::<Vec<_>>())
            })
            .collect::<Vec<_>>();
        let published = workers
            .into_iter()
            .flat_map(|worker| worker.join().expect("generation bump worker"))
            .collect::<Vec<_>>();
        assert!(published.iter().all(|generation| *generation != 0));
        assert_eq!(
            published.iter().copied().collect::<BTreeSet<_>>().len(),
            published.len()
        );
    }

    #[cfg(feature = "native-test-hooks")]
    #[test]
    fn registry_allocation_failure_rejects_transport_without_leaking_descriptors() {
        const CHILD: &str = "HL_NATIVE_CHECKPOINT_REGISTRY_FAILURE_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "checkpoint::tests::registry_allocation_failure_rejects_transport_without_leaking_descriptors",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(CHILD, "1")
                .spawn()
                .unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    assert!(status.success(), "checkpoint registry child failed: {status}");
                    return;
                }
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("checkpoint registry child exceeded 15 seconds");
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }

        let _adoption = ADOPTION.lock().expect("checkpoint adoption lock");
        let directory = if cfg!(target_os = "linux") {
            "/proc/self/fd"
        } else {
            "/dev/fd"
        };
        let before = std::fs::read_dir(directory).expect("descriptor directory").count();
        // SAFETY: this feature-only hook fails exactly the next registry reservation.
        unsafe { crate::bindings::hl_c_backend_checkpoint_test_fail_registry_allocation() };
        assert!(CheckpointTransport::create().is_err());
        let after = std::fs::read_dir(directory).expect("descriptor directory").count();
        assert_eq!(after, before, "failed registration leaked a checkpoint descriptor");
    }

    #[test]
    #[cfg(unix)]
    fn repeated_adoption_replaces_owned_descriptors() {
        let _adoption = ADOPTION.lock().expect("checkpoint adoption lock");
        let (_broker, transport) = CheckpointTransport::create().expect("checkpoint transport");
        transport.adopt(1).expect("initial adoption");
        #[cfg(target_os = "linux")]
        let broker_source = transport.broker_child.as_raw_fd();
        #[cfg(target_os = "linux")]
        let trigger_source = transport.trigger_descriptor.as_raw_fd();
        #[cfg(target_os = "linux")]
        let mut broker_copies = matching_descriptors(broker_source);
        #[cfg(target_os = "linux")]
        let mut trigger_copies = matching_descriptors(trigger_source);
        #[cfg(target_os = "linux")]
        assert!(broker_copies.remove(&broker_source));
        #[cfg(target_os = "linux")]
        assert!(trigger_copies.remove(&trigger_source));
        #[cfg(target_os = "linux")]
        assert_eq!(broker_copies.len(), 1, "one engine-owned broker duplicate");
        #[cfg(target_os = "linux")]
        assert_eq!(trigger_copies.len(), 1, "one engine-owned trigger duplicate");
        for _ in 0..32 {
            transport.adopt(1).expect("replacement adoption");
            drop(transport.broker_child.try_clone().expect("live broker source"));
            drop(transport.trigger_descriptor.try_clone().expect("live trigger source"));
            #[cfg(target_os = "linux")]
            {
                let mut next_broker = matching_descriptors(broker_source);
                let mut next_trigger = matching_descriptors(trigger_source);
                assert!(next_broker.remove(&broker_source));
                assert!(next_trigger.remove(&trigger_source));
                assert_eq!(next_broker.len(), 1, "old broker duplicate leaked");
                assert_eq!(next_trigger.len(), 1, "old trigger duplicate leaked");
                assert!(
                    broker_copies.is_disjoint(&next_broker),
                    "old broker duplicate remained live"
                );
                assert!(
                    trigger_copies.is_disjoint(&next_trigger),
                    "old trigger duplicate remained live"
                );
                broker_copies = next_broker;
                trigger_copies = next_trigger;
            }
        }
    }

    #[test]
    fn accept_without_announcement_times_out() {
        let (broker, _transport) = CheckpointTransport::create().expect("checkpoint transport");
        assert!(broker.accept(Duration::ZERO).is_none());
    }
}
