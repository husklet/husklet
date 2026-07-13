//! Linux-fence ↔ Metal-completion bridge for the compositor's explicit-sync contract.
//!
//! The Wayland explicit-sync protocol (`zwp_linux_explicit_synchronization_v1`, implemented in
//! `dd-compositor`) is a fence contract: the compositor must wait on a client's **acquire** fence before
//! it samples a buffer, and signal a **release** fence only after its own GPU work on that buffer has
//! completed. On the real macOS host that GPU work runs on Metal, so the two Linux `dma_fence`
//! sync_files bridge to Metal as follows:
//!
//!   * **acquire → GPU wait.** The guest's acquire sync_file becomes readable when its fence signals.
//!     [`AcquireWaiter`] polls it (portable; a `poll(2)` on the sync_file). On macOS the presenter can
//!     additionally encode a GPU-side wait so the queue itself stalls — it signals an `MTLSharedEvent`
//!     from the poll edge and issues `MTLCommandQueue::encodeWait(event, value)` before the sampling
//!     command buffer (see [`metal`]).
//!   * **GPU completion → release.** [`CompletionFence`] is an `eventfd` the compositor signals when its
//!     GPU work finishes; its fd is handed back to the client as the `fenced_release` fence. On macOS the
//!     Metal presenter registers `fence.signal()` as its command buffer's `addCompletedHandler`, so the
//!     eventfd fires exactly when the `MTLSharedEvent` reaches the frame's completion value.
//!
//! The primitives here are portable and Linux-tested; the `MTLSharedEvent`/`addCompletedHandler` wiring
//! is a mac-gated presenter concern (documented on [`metal`]).

use std::io;
use std::os::unix::io::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};

/// A release fence the compositor signals when its GPU work on a buffer completes. Backed by an
/// `eventfd`: it starts unsignalled (not readable) and becomes readable once [`signal`](Self::signal) is
/// called, exactly like a `dma_fence` sync_file. The owned fd is handed to the client via
/// `zwp_linux_buffer_release_v1.fenced_release`.
pub struct CompletionFence {
    fd: OwnedFd,
}

impl CompletionFence {
    /// Create an unsignalled completion fence.
    pub fn new() -> io::Result<Self> {
        // EFD_CLOEXEC | EFD_NONBLOCK: a poll-readable, non-blocking counter fd.
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { fd: unsafe { OwnedFd::from_raw_fd(fd) } })
    }

    /// Signal the fence (GPU work complete). Idempotent enough for the contract: the eventfd counter is
    /// incremented, leaving the fd permanently readable. Called from the Metal command buffer's
    /// completion handler on macOS.
    pub fn signal(&self) -> io::Result<()> {
        let v: u64 = 1;
        let n = unsafe { libc::write(self.fd.as_raw_fd(), &v as *const u64 as *const libc::c_void, 8) };
        if n != 8 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Whether the fence has been signalled (readable), without consuming it.
    pub fn is_signalled(&self) -> bool {
        poll_readable(self.fd.as_raw_fd(), 0).unwrap_or(false)
    }

    /// Borrow the underlying fd (e.g. to hand to the release event).
    pub fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd_compat()
    }

    /// Consume the fence into its owned fd (to move into the `fenced_release` event).
    pub fn into_owned_fd(self) -> OwnedFd {
        self.fd
    }
}

/// Wraps a client's acquire fence (a `dma_fence` sync_file, or any pollable fd) so the compositor can
/// block on it before sampling. On macOS the same fd additionally bridges to a GPU-side wait via an
/// `MTLSharedEvent` (see the module docs).
pub struct AcquireWaiter<'a> {
    fd: BorrowedFd<'a>,
}

impl<'a> AcquireWaiter<'a> {
    pub fn new(fd: BorrowedFd<'a>) -> Self {
        Self { fd }
    }

    /// Block until the acquire fence signals, up to `timeout_ms` (`-1` = forever). `Ok(true)` if it
    /// signalled, `Ok(false)` on timeout. This is the CPU wait the compositor performs before it reads
    /// the buffer, guaranteeing it never samples pixels the client's GPU has not finished producing.
    pub fn wait(&self, timeout_ms: i32) -> io::Result<bool> {
        poll_readable(self.fd.as_raw_fd(), timeout_ms)
    }
}

/// `poll(2)` an fd for readability (`POLLIN`), the edge a signalled sync_file / eventfd gives.
fn poll_readable(fd: RawFd, timeout_ms: i32) -> io::Result<bool> {
    let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    let rc = unsafe { libc::poll(&mut pfd as *mut libc::pollfd, 1, timeout_ms) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(rc > 0 && (pfd.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR)) != 0)
}

/// Tiny shim so callers don't need to import `AsFd`.
trait AsFdCompat {
    fn as_fd_compat(&self) -> BorrowedFd<'_>;
}
impl AsFdCompat for OwnedFd {
    fn as_fd_compat(&self) -> BorrowedFd<'_> {
        use std::os::unix::io::AsFd;
        self.as_fd()
    }
}

/// The Metal presenter's integration contract for the mac host (documented here; wired in the
/// `MetalPresenter`, which owns the `MTLCommandQueue`/`MTLSharedEvent`, so this crate's headless build
/// pulls in no Metal objects):
///
/// * **Acquire (wait-before-sample).** Before committing the command buffer that samples a surface's
///   buffer, the presenter calls [`AcquireWaiter::wait`] on the surface's acquire fd (the compositor's
///   `take_acquire_fence`). For a GPU-side stall it maps the fd's signal edge onto an `MTLSharedEvent`
///   value and issues `MTLCommandQueue::encodeWait` on that event/value before the sampling pass, so the
///   Metal queue itself blocks until the guest's fence fires.
/// * **Release (signal-after-completion).** The presenter creates a [`CompletionFence`], registers
///   `fence.signal()` as the sampling command buffer's `addCompletedHandler`, and hands
///   `fence.into_owned_fd()` back to the compositor for `signal_buffer_release(sid, Some(fd))`. The
///   client's `fenced_release` then fires exactly when the `MTLSharedEvent` reaches the frame's
///   completion value — release strictly after GPU completion.
///
/// This module deliberately holds only the portable, Linux-testable fence primitives; the objc2-metal
/// calls named above live in the mac-only presenter so the compositor's fence semantics stay verifiable
/// off-device.
#[doc(hidden)]
pub const METAL_BRIDGE_CONTRACT: () = ();

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::io::AsFd;

    #[test]
    fn completion_fence_starts_unsignalled_and_becomes_readable_after_signal() {
        let fence = CompletionFence::new().unwrap();
        assert!(!fence.is_signalled(), "a fresh completion fence must not be signalled");
        // A waiter on it times out while the GPU work is still pending…
        assert!(!AcquireWaiter::new(fence.as_fd()).wait(0).unwrap());
        fence.signal().unwrap();
        assert!(fence.is_signalled(), "signal() must make the release fence readable");
        // …and unblocks once the GPU completion signals it.
        assert!(AcquireWaiter::new(fence.as_fd()).wait(200).unwrap());
    }

    #[test]
    fn acquire_waiter_blocks_until_the_fence_signals() {
        // An eventfd standing in for a guest acquire sync_file.
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        assert!(fd >= 0);
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        assert!(!AcquireWaiter::new(owned.as_fd()).wait(0).unwrap(), "unsignalled acquire fence is not ready");
        let v: u64 = 1;
        assert_eq!(unsafe { libc::write(owned.as_raw_fd(), &v as *const u64 as *const libc::c_void, 8) }, 8);
        assert!(AcquireWaiter::new(owned.as_fd()).wait(200).unwrap(), "acquire wait returns once signalled");
    }
}
