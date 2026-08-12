//! Optional process allocator counter used by packaged benchmark workers.

#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static THREAD: Cell<u64> = const { Cell::new(0) };
}

fn bump() {
    let _ = THREAD.try_with(|count| count.set(count.get().wrapping_add(1)));
}

pub struct CountingAllocator;

// SAFETY: every operation delegates to `System` unchanged. The thread-local
// counter has no destructor and cannot re-enter allocation.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        bump();
        // SAFETY: `layout` is forwarded unchanged under `GlobalAlloc::alloc`'s contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        bump();
        // SAFETY: `layout` is forwarded unchanged under `GlobalAlloc::alloc_zeroed`'s contract.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        bump();
        // SAFETY: the pointer, layout, and requested size are forwarded unchanged under `GlobalAlloc::realloc`.
        unsafe { System.realloc(pointer, layout, size) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: the pointer and layout are forwarded unchanged under `GlobalAlloc::dealloc`'s contract.
        unsafe { System.dealloc(pointer, layout) }
    }
}
