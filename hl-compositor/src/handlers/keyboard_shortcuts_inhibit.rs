//! `zwp_keyboard_shortcuts_inhibit_manager_v1` — a surface asks the compositor to stop intercepting its
//! own keyboard shortcuts so ALL keys reach the client (VMs, remote-desktop viewers, terminal
//! multiplexers, emulators that need Ctrl+Alt+F-keys / Super). Composed from the vendored Smithay
//! `keyboard_shortcuts_inhibit` module.
//!
//! ## Host policy (honour the inhibit)
//! dd's compositor reserves essentially no global keyboard shortcuts on its single Cocoa window (system
//! chords are handled by macOS above dd, not intercepted here), so honouring an inhibit request is both
//! safe and correct: [`Self::new_inhibitor`] immediately [`activate`](smithay::wayland::keyboard_shortcuts_inhibit::KeyboardShortcutsInhibitor::activate)s
//! the inhibitor, which emits `active` to the client and flips the seat's
//! [`keyboard_shortcuts_inhibited`](smithay::wayland::keyboard_shortcuts_inhibit::KeyboardShortcutsInhibitorSeat)
//! flag. A production policy could gate activation on focus/user confirmation; unconditional activation
//! is the sensible default when the compositor owns no conflicting chords.

use smithay::wayland::keyboard_shortcuts_inhibit::{
    KeyboardShortcutsInhibitHandler, KeyboardShortcutsInhibitState, KeyboardShortcutsInhibitor,
};

use crate::DdState;

impl KeyboardShortcutsInhibitHandler for DdState {
    fn keyboard_shortcuts_inhibit_state(&mut self) -> &mut KeyboardShortcutsInhibitState {
        &mut self.keyboard_shortcuts_inhibit
    }

    /// A client requested shortcut inhibition for a (surface, seat). dd owns no conflicting global
    /// chords, so activate it immediately — the client receives `active` and gets every key.
    fn new_inhibitor(&mut self, inhibitor: KeyboardShortcutsInhibitor) {
        inhibitor.activate();
    }
}
