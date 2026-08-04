#![allow(unsafe_code)]

use std::sync::Weak;

use super::native::Reactor;

type Token = (u64, i32, bool, bool);

impl Reactor {
    pub(super) fn run(source: Weak<Self>) {
        loop {
            let Some(shared) = source.upgrade() else { return };
            let mut tokens = Self::snapshot(&shared);
            let mut polls = Self::pollset(&shared, &tokens);
            drop(shared);
            // SAFETY: polls is initialized writable storage. The wake pipe interrupts
            // this blocking call whenever ownership or the socket set changes.
            if unsafe { libc::poll(polls.as_mut_ptr(), polls.len() as _, -1) } <= 0 {
                continue;
            }
            if polls[0].revents & libc::POLLIN != 0 {
                Self::drain(&source)
            }
            let Some(shared) = source.upgrade() else { return };
            Self::disarm(&shared, &tokens, &polls);
            let observers = shared.observers.lock().unwrap_or_else(|error| error.into_inner());
            let ready = tokens
                .drain(..)
                .zip(polls.iter().skip(1))
                .filter(|(_, poll)| poll.revents != 0)
                .filter_map(|((token, _, _, _), _)| observers.get(&token).and_then(Weak::upgrade))
                .collect::<Vec<_>>();
            drop(observers);
            for observer in ready {
                observer.readiness_changed()
            }
        }
    }

    fn snapshot(shared: &Self) -> Vec<Token> {
        shared
            .sockets
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .map(|(token, entry)| {
                (
                    *token,
                    entry.descriptor,
                    entry.wants_read,
                    entry.connecting || entry.wants_write,
                )
            })
            .collect()
    }

    fn pollset(shared: &Self, tokens: &[Token]) -> Vec<libc::pollfd> {
        let mut polls = Vec::with_capacity(tokens.len() + 1);
        polls.push(libc::pollfd {
            fd: shared.wake_read,
            events: libc::POLLIN,
            revents: 0,
        });
        polls.extend(tokens.iter().map(|(_, descriptor, read, write)| libc::pollfd {
            fd: *descriptor,
            events: (if *read { libc::POLLIN | libc::POLLPRI } else { 0 })
                | libc::POLLERR
                | libc::POLLHUP
                | if *write { libc::POLLOUT } else { 0 },
            revents: 0,
        }));
        polls
    }

    fn disarm(shared: &Self, tokens: &[Token], polls: &[libc::pollfd]) {
        let mut sockets = shared.sockets.lock().unwrap_or_else(|error| error.into_inner());
        for ((token, _, _, _), poll) in tokens.iter().zip(polls.iter().skip(1)) {
            let Some(entry) = sockets.get_mut(token) else { continue };
            if poll.revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP) != 0 {
                entry.wants_read = false;
            }
            if poll.revents & (libc::POLLOUT | libc::POLLERR | libc::POLLHUP) != 0 {
                entry.connecting = false;
                entry.wants_write = false;
            }
        }
    }

    fn drain(source: &Weak<Self>) {
        let Some(shared) = source.upgrade() else { return };
        let mut bytes = [0_u8; 64];
        // SAFETY: bytes is writable and wake_read remains owned while shared is live.
        unsafe {
            libc::read(shared.wake_read, bytes.as_mut_ptr().cast(), bytes.len());
        }
    }
}
