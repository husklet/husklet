use std::time::Duration;

use hl_log::{tag, Level};
use objc2_quartz_core::CAMetalLayer;

pub(super) const DRAWABLES: usize = 3;
const SLOW_ACQUIRE: Duration = Duration::from_millis(2);

#[link(name = "objc")]
extern "C" {
    fn objc_msgSend();
    fn sel_registerName(name: *const std::ffi::c_char) -> *const std::ffi::c_void;
}

fn supports(layer: &CAMetalLayer, selector: &'static std::ffi::CStr) -> bool {
    type Responds = unsafe extern "C" fn(
        *const CAMetalLayer,
        *const std::ffi::c_void,
        *const std::ffi::c_void,
    ) -> bool;
    let responds = unsafe { sel_registerName(c"respondsToSelector:".as_ptr()) };
    let candidate = unsafe { sel_registerName(selector.as_ptr()) };
    let call: Responds = unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { call(layer, responds, candidate) }
}

pub(super) fn configure(layer: &CAMetalLayer) {
    unsafe {
        if supports(layer, c"setMaximumDrawableCount:") {
            layer.setMaximumDrawableCount(DRAWABLES);
        }
        if supports(layer, c"setAllowsNextDrawableTimeout:") {
            layer.setAllowsNextDrawableTimeout(true);
        }
        if supports(layer, c"setDisplaySyncEnabled:") {
            layer.setDisplaySyncEnabled(true);
        }
    }
}

pub(super) fn can_acquire(queue_depth: usize) -> bool {
    queue_depth < DRAWABLES
}

pub(super) fn record_acquire(elapsed: Duration, queue_depth: usize, acquired: bool) {
    let level = if !acquired || elapsed >= SLOW_ACQUIRE {
        Level::Warn
    } else {
        Level::Trace
    };
    hl_log::hl_log!(
        tag::PRESENT,
        level,
        "drawable_acquire elapsed_us={} queued={} capacity={} acquired={}",
        elapsed.as_micros(),
        queue_depth,
        DRAWABLES,
        acquired
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drawable_queue_is_bounded_for_triple_buffering() {
        assert_eq!(DRAWABLES, 3);
        assert!(can_acquire(0));
        assert!(can_acquire(2));
        assert!(!can_acquire(3));
        assert!(!can_acquire(usize::MAX));
    }

    #[test]
    fn fourth_frame_waits_for_a_native_completion() {
        let mut queued = 0;
        for _ in 0..DRAWABLES {
            assert!(can_acquire(queued));
            queued += 1;
        }
        assert!(!can_acquire(queued));
        queued -= 1;
        assert!(can_acquire(queued));
    }

    #[test]
    fn slow_threshold_is_below_one_refresh_period() {
        assert!(SLOW_ACQUIRE < Duration::from_millis(8));
    }
}
