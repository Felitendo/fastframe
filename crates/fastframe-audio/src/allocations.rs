//! A test-only global allocator that counts the allocations each thread
//! makes, so a test can check that what the audio callback does each time
//! allocates nothing.
//!
//! `unsafe` is allowed here because implementing [`GlobalAlloc`] is an
//! `unsafe impl`; every call forwards to [`System`] unchanged, and only a
//! thread-local counter is added, so the allocator's contract is
//! [`System`]'s own. It is compiled only into the crate's own tests.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct Counting;

thread_local! {
    // A `const` `Cell<usize>` has no destructor, so reaching it never
    // allocates, which would recurse into the allocator.
    static COUNT: Cell<usize> = const { Cell::new(0) };
}

// SAFETY: every method forwards to `System` with the same arguments, so
// `System`'s guarantees hold; counting touches no allocated memory.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        COUNT.with(|count| count.set(count.get() + 1));
        // SAFETY: the caller's contract for `alloc`, passed on unchanged.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        COUNT.with(|count| count.set(count.get() + 1));
        // SAFETY: as for `alloc`.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        COUNT.with(|count| count.set(count.get() + 1));
        // SAFETY: `ptr` and `layout` came from this allocator, so from
        // `System`, as the caller's contract for `realloc` requires.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from `System` with `layout`.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

/// How many times `work` allocated or grew memory on this thread.
pub(crate) fn during(work: impl FnOnce()) -> usize {
    let before = COUNT.with(Cell::get);
    work();
    COUNT.with(Cell::get) - before
}
