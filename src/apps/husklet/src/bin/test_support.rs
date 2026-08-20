//! The one thread in this test binary that owns GTK.
//!
//! GTK belongs to whichever thread entered it, and libtest gives every `#[test]`
//! a thread of its own, so a binary holding two GTK tests has no thread both of
//! them are allowed to draw on. Two tests here need one — the extension page and
//! the extension shelf — so this module keeps a thread, enters GTK on it once,
//! and runs a scenario there for whichever test asks. Entering GTK inside the
//! test's own thread, which is what both of them used to do, is what fails:
//!
//! * With a display, the first entrant claims the default main context and the
//!   second is refused it. `gtk::init` then either panics with "Attempted to
//!   initialize GTK from two different threads" (`gtk4-0.9.7/src/rt.rs:136`,
//!   observed 3/3 at `--test-threads=1`) or answers `Err` and the test skips
//!   itself, chosen by the scheduler.
//! * With no display, GTK 4.22 sets its `gtk_initialized` flag *before* it opens
//!   one — `libgtk-4.so.1` stores it at `gtk_init_check+0x2e4` and calls
//!   `gdk_display_open_default` at `+0x341` — so the first `gtk_init_check`
//!   answers false and every later call short-circuits to true at `+0x1a` with
//!   `gdk_display_get_default()` still null. gtk4-rs takes that true, re-checks
//!   only `gtk_is_initialized`, and hands back `Ok(())`. The test then builds a
//!   widget, `gtk_widget_init` asks `gtk_css_static_style_get_default` for the
//!   style provider's settings, and the null `GtkSettings` is dereferenced.
//!   Measured on `x86_64` Linux with a twelve-line C program: call 1 false and
//!   display null, call 2 true and display null, `gtk_box_new` SIGSEGV.
//!
//! So `gtk::init()` answering `Ok` does not mean GTK can draw, and the display
//! is what has to be asked. Both questions are asked once, here, behind a
//! channel that no second thread can race.

use std::panic::AssertUnwindSafe;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::OnceLock;

/// A scenario to run on the toolkit thread, and where its outcome is sent back.
type Errand = (Box<dyn FnOnce() + Send>, SyncSender<std::thread::Result<()>>);

/// Runs `scenario` on the one thread in this process that owns GTK.
///
/// Answers `false` when GTK cannot draw on this host at all, which is every
/// host with no display connection; the caller says it skipped. A panic inside
/// `scenario` — which is what a failed assertion is — is carried back and raised
/// again on the calling thread, so libtest fails the test that asked rather than
/// aborting the binary from a thread it is not watching.
pub(crate) fn on_the_toolkit_thread(scenario: impl FnOnce() + Send + 'static) -> bool {
    let Some(errands) = toolkit() else {
        return false;
    };
    let (finished, outcome) = sync_channel(1);
    errands
        .send((Box::new(scenario), finished))
        .expect("the toolkit thread outlives every test that asks it for work");
    match outcome.recv().expect("the toolkit thread answers every errand") {
        Ok(()) => true,
        // The payload is re-raised as a message rather than resumed, because
        // libtest attributes a failure to the test whose thread the panic hook
        // ran on. Resuming would leave the assertion printed by the toolkit
        // thread and the test reported as failed with nothing under it.
        Err(panic) => panic!("on the toolkit thread: {}", said(&panic)),
    }
}

/// What a panic payload says, for the few shapes a failed assertion produces.
fn said(panic: &Box<dyn std::any::Any + Send>) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|said| (*said).to_owned())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "the scenario panicked".to_owned())
}

/// Enters GTK on the calling thread, reports whether it can draw, and then serves errands.
///
/// Returns once the errand sender is dropped, or immediately when the host has no display.
fn serve_errands(queue: &Receiver<Errand>, entered: &SyncSender<bool>) {
    // Both halves matter: `Ok` alone is what a second failed entry answers on a
    // display-less host, and a display alone is not enough to have acquired the
    // main context.
    let drawable = gtk::init().is_ok() && gtk::gdk::Display::default().is_some();
    entered.send(drawable).expect("the entry is awaited");
    if !drawable {
        return;
    }
    while let Ok((scenario, finished)) = queue.recv() {
        let outcome = std::panic::catch_unwind(AssertUnwindSafe(scenario));
        let _ = finished.send(outcome);
    }
}

/// The toolkit thread's errand channel, or `None` when GTK has no display here.
fn toolkit() -> Option<&'static SyncSender<Errand>> {
    static TOOLKIT: OnceLock<Option<SyncSender<Errand>>> = OnceLock::new();
    TOOLKIT
        .get_or_init(|| {
            let (errands, queue) = sync_channel::<Errand>(0);
            let (entered, entry) = sync_channel(1);
            std::thread::Builder::new()
                .name("husklet-toolkit".to_owned())
                .spawn(move || serve_errands(&queue, &entered))
                .expect("a thread for the toolkit");
            entry
                .recv()
                .expect("the toolkit thread reports whether it entered GTK")
                .then_some(errands)
        })
        .as_ref()
}
