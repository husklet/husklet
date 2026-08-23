use vte4::prelude::*;

/// Catches a grid allocation which completed before the worker published its PTY.
///
/// Pinned VTE propagates every later allocation to a foreign PTY itself. The
/// attach boundary is the only gap: process startup can finish after allocation,
/// so copy the current non-empty grid once when the worker publishes the PTY.
pub(super) fn synchronise(terminal: &vte4::Terminal, pty: &vte4::Pty) -> bool {
    resize_grid(
        (terminal.column_count() as i32, terminal.row_count() as i32),
        |rows, columns| pty.set_size(rows, columns).is_ok(),
    )
}

fn resize_grid(dimensions: (i32, i32), mut resize: impl FnMut(i32, i32) -> bool) -> bool {
    dimensions.0 > 0 && dimensions.1 > 0 && resize(dimensions.1, dimensions.0)
}

#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use super::*;
    use crate::{gio, glib};
    use std::os::fd::{AsRawFd as _, FromRawFd as _};

    #[test]
    fn empty_grid_is_deferred_and_a_valid_grid_is_oriented_for_the_pty() {
        let mut calls = Vec::new();
        assert!(!resize_grid((0, 24), |rows, columns| {
            calls.push((rows, columns));
            true
        }));
        assert!(calls.is_empty());

        assert!(resize_grid((80, 24), |rows, columns| {
            calls.push((rows, columns));
            true
        }));
        assert_eq!(calls, [(24, 80)]);
    }

    #[test]
    fn foreign_pty_catches_up_at_attach_and_vte_propagates_later_resizes() {
        let ran = crate::test_support::on_the_toolkit_thread(|| {
            let mut master = -1;
            let mut slave = -1;
            // SAFETY: openpty initializes both owned descriptors and retains no pointers.
            assert_eq!(
                unsafe {
                    libc::openpty(
                        &raw mut master,
                        &raw mut slave,
                        std::ptr::null_mut(),
                        std::ptr::null(),
                        std::ptr::null(),
                    )
                },
                0
            );
            // SAFETY: openpty returned two new descriptors owned by this scenario.
            let master = unsafe { std::os::fd::OwnedFd::from_raw_fd(master) };
            // SAFETY: paired descriptor, closed when this scenario ends.
            let slave = unsafe { std::os::fd::OwnedFd::from_raw_fd(slave) };
            let pty = vte4::Pty::foreign_sync(master, gio::Cancellable::NONE).unwrap();
            let terminal = vte4::Terminal::new();
            let window = gtk::Window::new();
            window.set_default_size(740, 360);
            window.set_child(Some(&terminal));
            window.present();
            let allocated = await_dimensions(&terminal, None);

            terminal.set_pty(Some(&pty));
            pty.set_size(1, 1).unwrap();
            assert_eq!(pty_grid(slave.as_raw_fd()), (1, 1));
            assert!(synchronise(&terminal, &pty));
            let initial = pty_grid(slave.as_raw_fd());
            assert_eq!(initial, allocated, "attach did not synchronise immediately");
            assert_ne!(initial, (120, 40), "the hard-coded fallback reached the live PTY");

            window.set_default_size(980, 620);
            let resized = await_grid(&terminal, slave.as_raw_fd(), Some(initial));
            assert_ne!(resized, initial, "VTE did not propagate its later allocation");
            window.close();
        });
        if !ran {
            println!("skipped: no display connection");
        }
    }

    fn await_dimensions(terminal: &vte4::Terminal, previous: Option<(i32, i32)>) -> (i32, i32) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            while glib::MainContext::default().iteration(false) {}
            let dimensions = (terminal.column_count() as i32, terminal.row_count() as i32);
            if dimensions.0 > 0 && dimensions.1 > 0 && Some(dimensions) != previous {
                return dimensions;
            }
            assert!(std::time::Instant::now() < deadline, "terminal={dimensions:?}");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn pty_grid(slave: std::os::fd::RawFd) -> (i32, i32) {
        let mut size = libc::winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: `slave` is live and `size` is writable for the ioctl.
        assert_eq!(unsafe { libc::ioctl(slave, libc::TIOCGWINSZ, &raw mut size) }, 0);
        (i32::from(size.ws_col), i32::from(size.ws_row))
    }

    fn await_grid(terminal: &vte4::Terminal, slave: std::os::fd::RawFd, previous: Option<(i32, i32)>) -> (i32, i32) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            while glib::MainContext::default().iteration(false) {}
            let dimensions = (terminal.column_count() as i32, terminal.row_count() as i32);
            let pty = pty_grid(slave);
            if dimensions.0 > 0 && dimensions.1 > 0 && Some(dimensions) != previous && pty == dimensions {
                return dimensions;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "terminal={dimensions:?}, pty={pty:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}
