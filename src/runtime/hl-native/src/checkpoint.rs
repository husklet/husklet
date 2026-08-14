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

impl CheckpointBroker {
    /// Waits for one guest process to connect to the checkpoint broker.
    #[must_use]
    pub fn accept(&self, timeout: Duration) -> Option<(UnixStream, u64)> {
        let timeout_ms = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
        let mut host_pid = 0;
        // SAFETY: the broker descriptor remains owned by self through the call;
        // a nonnegative result transfers one stream descriptor to Rust.
        let channel = unsafe {
            bindings::hl_c_backend_checkpoint_broker_accept(self.0.as_raw_fd(), timeout_ms, &raw mut host_pid)
        };
        (channel >= 0).then(|| {
            // SAFETY: C returned a uniquely owned descriptor.
            (unsafe { UnixStream::from_raw_fd(channel) }, host_pid)
        })
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
