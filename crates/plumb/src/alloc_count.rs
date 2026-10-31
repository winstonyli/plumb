//! Test-only: counts this thread's allocations, so a test can pin a path to none.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static COUNT: Cell<u64> = const { Cell::new(0) };
}

struct Counting;

fn bump() {
    COUNT.with(|c| c.set(c.get() + 1));
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        bump();
        unsafe { System.alloc(l) }
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        bump();
        unsafe { System.alloc_zeroed(l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        bump();
        unsafe { System.realloc(p, l, n) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// Runs `f`; gives its result and how many allocations this thread made meanwhile.
pub fn allocations<R>(f: impl FnOnce() -> R) -> (R, u64) {
    let before = COUNT.with(Cell::get);
    let r = f();
    (r, COUNT.with(Cell::get) - before)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_counter_sees_an_allocation_and_a_reused_buffer_makes_none() {
        let (s, n) = allocations(|| String::from("grown"));
        assert!(n >= 1, "{n}");
        let mut buf = s;
        let ((), n) = allocations(|| {
            buf.clear();
            buf.push_str("again");
        });
        assert_eq!(n, 0);
    }
}
