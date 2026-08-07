//! Socket kind probing and switch/inet socket replacement.

#![allow(unsafe_code)]

use hl_runtime::RuntimeNetworkError;

use super::{Entry, Native};

impl Native {
    pub(super) fn is_icmp(&self, token: u64) -> bool {
        self.shared
            .sockets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&token)
            .is_some_and(|entry| entry.icmp)
    }

    pub(super) fn socket_type(&self, token: u64) -> Result<i32, RuntimeNetworkError> {
        let descriptor = {
            let sockets = self
                .shared
                .sockets
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let entry = sockets.get(&token).ok_or(RuntimeNetworkError::Invalid)?;
            if let Some(kind) = entry.kind {
                return Ok(kind);
            }
            entry.descriptor
        };
        let kind = Self::descriptor_type(descriptor)?;
        let mut sockets = self
            .shared
            .sockets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = sockets.get_mut(&token).ok_or(RuntimeNetworkError::Invalid)?;
        entry.kind = Some(kind);
        Ok(kind)
    }

    pub(super) fn descriptor_type(descriptor: i32) -> Result<i32, RuntimeNetworkError> {
        let mut kind = 0_i32;
        let mut kind_length = size_of::<i32>() as libc::socklen_t;
        // SAFETY: kind is writable and the table retains the live descriptor.
        if unsafe {
            libc::getsockopt(
                descriptor,
                libc::SOL_SOCKET,
                libc::SO_TYPE,
                (&raw mut kind).cast(),
                &raw mut kind_length,
            )
        } == 0
        {
            Ok(kind)
        } else {
            Err(Self::runtime_error())
        }
    }

    pub(super) fn switch_socket(&self, token: u64, expected: i32) -> Result<i32, RuntimeNetworkError> {
        let mut sockets = self
            .shared
            .sockets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = sockets.get_mut(&token).ok_or(RuntimeNetworkError::Invalid)?;
        if entry.switched {
            return Ok(entry.descriptor);
        }
        let kind = match entry.kind {
            Some(kind) => kind,
            None => Self::descriptor_type(entry.descriptor)?,
        };
        if kind != expected {
            return Err(RuntimeNetworkError::OperationNotSupported);
        }
        entry.kind = Some(kind);
        Self::install_replacement(entry, libc::AF_UNIX, expected, 0, true)?;
        entry.switched = true;
        let descriptor = entry.descriptor;
        drop(sockets);
        self.wake();
        Ok(descriptor)
    }

    pub(super) fn reset_switch_socket(&self, token: u64, expected: i32) -> Result<(), RuntimeNetworkError> {
        self.replace_socket(token, expected, libc::AF_UNIX, true)
    }

    pub(super) fn restore_inet_socket(&self, token: u64, expected: i32) -> Result<(), RuntimeNetworkError> {
        let mut sockets = self
            .shared
            .sockets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = sockets.get_mut(&token).ok_or(RuntimeNetworkError::Invalid)?;
        if !entry.switched {
            return Err(RuntimeNetworkError::Invalid);
        }
        let family = entry.original_family.unwrap_or(libc::AF_INET);
        let protocol = entry.original_protocol.unwrap_or(0);
        Self::install_replacement(entry, family, expected, protocol, false)?;
        entry.guest_local = None;
        entry.guest_peer = None;
        entry.switch_path = None;
        entry.switch_interface = None;
        entry.datagram_peer = None;
        entry.switched = false;
        drop(sockets);
        self.wake();
        Ok(())
    }

    pub(super) fn replace_socket(
        &self,
        token: u64,
        expected: i32,
        family: i32,
        switched: bool,
    ) -> Result<(), RuntimeNetworkError> {
        let mut sockets = self
            .shared
            .sockets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = sockets.get_mut(&token).ok_or(RuntimeNetworkError::Invalid)?;
        if !entry.switched {
            return Err(RuntimeNetworkError::Invalid);
        }
        Self::install_replacement(entry, family, expected, 0, switched)?;
        if !switched {
            entry.guest_local = None;
            entry.guest_peer = None;
            entry.switch_path = None;
            entry.switch_interface = None;
            entry.datagram_peer = None;
        }
        entry.switched = switched;
        drop(sockets);
        self.wake();
        Ok(())
    }

    pub(super) fn install_replacement(
        entry: &mut Entry,
        family: i32,
        kind: i32,
        protocol: i32,
        switch_transport: bool,
    ) -> Result<(), RuntimeNetworkError> {
        // SAFETY: socket returns a newly owned descriptor or a negative errno result.
        let replacement = unsafe { libc::socket(family, kind, protocol) };
        if replacement < 0 {
            return Err(Self::runtime_error());
        }
        // Snapshot table-owned state before replacement. Options are typed and
        // bounded at the Linux ABI boundary; no unbounded host buffer is retained.
        // SAFETY: entry.descriptor is owned by the locked socket table, so it stays open
        // across this call; F_GETFL takes no pointer argument and only reads fd flags.
        let flags = unsafe { libc::fcntl(entry.descriptor, libc::F_GETFL) };
        // SAFETY: same live table-owned descriptor under the same lock; F_GETFD is a
        // pointer-free query that cannot alias or free anything.
        let descriptor_flags = unsafe { libc::fcntl(entry.descriptor, libc::F_GETFD) };
        // Configure the unshared replacement completely before dup2. Failure
        // therefore leaves the original descriptor and projected state intact.
        // SAFETY: replacement is the descriptor just returned by socket() above and is
        // solely owned here; F_SETFL/F_SETFD pass only integer flags, no pointers.
        unsafe {
            if flags >= 0 {
                libc::fcntl(replacement, libc::F_SETFL, flags);
            }
            if descriptor_flags >= 0 {
                libc::fcntl(replacement, libc::F_SETFD, descriptor_flags);
            }
        }
        for ((level, option), value) in &entry.options {
            if (!switch_transport || Self::switch_option(*level, *option))
                && let Err(error) = super::super::socket_option::set(replacement, *level, *option, value.clone())
            {
                if switch_transport {
                    continue;
                }
                // SAFETY: replacement is still solely owned here.
                unsafe { libc::close(replacement) };
                return Err(error);
            }
        }
        // SAFETY: dup2 atomically replaces the table descriptor. replacement
        // remains solely owned here and is closed on both outcomes.
        let replaced = unsafe { libc::dup2(replacement, entry.descriptor) };
        unsafe { libc::close(replacement) };
        if replaced < 0 {
            return Err(Self::runtime_error());
        }
        Ok(())
    }

    pub(super) const fn switch_option(level: i32, option: i32) -> bool {
        level == 1 && matches!(option, 2 | 6 | 9 | 10 | 13 | 15 | 20 | 21)
    }

    pub(super) fn set_socket_option(
        &self,
        token: u64,
        level: i32,
        option: i32,
        value: hl_linux::GuestSocketOption,
    ) -> Result<(), RuntimeNetworkError> {
        let mut sockets = self
            .shared
            .sockets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = sockets.get_mut(&token).ok_or(RuntimeNetworkError::Invalid)?;
        super::super::socket_option::set(entry.descriptor, level, option, value.clone())?;
        if (level, option) == (1, 27) {
            entry.options.remove(&(1, 26));
        } else {
            entry.options.insert((level, option), value);
        }
        Ok(())
    }

    pub(super) fn duplicate_descriptor(&self, token: u64) -> Result<i32, RuntimeNetworkError> {
        let sockets = self
            .shared
            .sockets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let descriptor = sockets.get(&token).ok_or(RuntimeNetworkError::Invalid)?.descriptor;
        // SAFETY: descriptor remains live under the table lock and dup returns independent ownership.
        let duplicate = unsafe { libc::dup(descriptor) };
        if duplicate < 0 {
            Err(Self::runtime_error())
        } else {
            Ok(duplicate)
        }
    }
}
