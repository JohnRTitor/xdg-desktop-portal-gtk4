//! Scoped allocation counting.
//!
//! This mirrors the benchmarking method used for Cloudflare's 1.1.1.1 DNS cache
//! work, where each layout change was validated against a custom
//! [`GlobalAlloc`] shim rather than by inspection: the shim records the number
//! and total size of allocations, so a reduction is *measured* instead of
//! asserted.
//!
//! Counting is opt-in through [`AllocScope`]. Outside a scope the shim costs one
//! thread-local read per allocation, so the rest of the test suite is unaffected.
//! Scopes are per-thread and are not reentrant; a nested [`AllocScope::start`]
//! resets the outer scope's counters.
//!
//! Installed as the crate's `#[global_allocator]` only under `cfg(test)`, so it
//! never reaches the shipped binary or the integration-test binaries.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    marker::PhantomData,
};

/// The allocator used by the crate's unit-test binary.
///
/// Not installed outside `cfg(test)`; see `lib.rs`.
pub struct CountingAlloc;

// `const` initialisation means no lazy allocation and no destructor
// registration, so these are safe to touch from inside `alloc`/`dealloc`.
thread_local! {
    static ARMED: Cell<bool> = const { Cell::new(false) };
    static COUNT: Cell<u64> = const { Cell::new(0) };
    static BYTES: Cell<u64> = const { Cell::new(0) };
}

fn record(size: usize) {
    let Some(armed) = ARMED.try_with(Cell::get).ok() else {
        return;
    };
    if !armed {
        return;
    }
    let _ = COUNT.try_with(|c| c.set(c.get() + 1));
    let _ = BYTES.try_with(|b| b.set(b.get() + size as u64));
}

// SAFETY: every method forwards to `System`, which upholds the `GlobalAlloc`
// contract. The shim adds no layout behaviour of its own.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            record(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            record(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_ptr = unsafe { System.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() {
            // Only the growth is counted, so a `Vec` that settles at its final
            // capacity contributes its real footprint rather than the sum of
            // every intermediate size it passed through.
            record(new_size.saturating_sub(layout.size()));
        }
        new_ptr
    }
}

/// Allocation totals observed inside a scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Snapshot {
    /// Number of allocations made in the scope.
    pub count: u64,
    /// Total bytes requested by those allocations.
    pub bytes: u64,
}

/// RAII scope that counts allocations made on the current thread.
///
/// Dropping the scope without calling [`AllocScope::finish`] still disarms it,
/// so an early return or a panic cannot leave counting enabled for later tests.
///
/// Counting is per-thread, so work handed to another thread is invisible. Finish
/// the scope on the thread that started it -- the type is `!Send` so that
/// mistake is a compile error rather than a silently empty result.
///
/// ```ignore
/// let snap = crate::alloc_probe::AllocScope::start().finish();
/// assert!(snap.count <= 66);
/// ```
#[must_use = "the scope only counts while it is alive; bind it to keep it running"]
pub struct AllocScope {
    finished: bool,
    // Makes the scope `!Send`/`!Sync`: counters are per-thread, so finishing a
    // scope on another thread would silently report the wrong numbers.
    _not_send: PhantomData<*const ()>,
}

impl AllocScope {
    /// Arms counting on the current thread and resets the counters.
    pub fn start() -> Self {
        let _ = COUNT.try_with(|c| c.set(0));
        let _ = BYTES.try_with(|b| b.set(0));
        let _ = ARMED.try_with(|a| a.set(true));
        Self {
            finished: false,
            _not_send: PhantomData,
        }
    }

    /// Reads the totals without disarming, for mid-scope sampling.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            count: COUNT.try_with(Cell::get).unwrap_or(0),
            bytes: BYTES.try_with(Cell::get).unwrap_or(0),
        }
    }

    /// Disarms counting and returns the totals.
    pub fn finish(mut self) -> Snapshot {
        self.finished = true;
        let snap = self.snapshot();
        let _ = ARMED.try_with(|a| a.set(false));
        snap
    }
}

impl Drop for AllocScope {
    fn drop(&mut self) {
        if !self.finished {
            let _ = ARMED.try_with(|a| a.set(false));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two-sided guard: arming must not allocate, and an allocation made inside
    /// a scope must be counted. The positive half is load-bearing -- if `record`
    /// stopped being called, every allocation assertion in the crate would pass
    /// vacuously.
    #[test]
    fn scope_counts_allocation_and_ignores_outside_work() {
        let before_scope = Vec::<u8>::with_capacity(64);
        std::hint::black_box(&before_scope);

        let scope = AllocScope::start();
        let inside_scope = Vec::<u8>::with_capacity(4096);
        let snap = scope.finish();
        std::hint::black_box(&inside_scope);

        assert_eq!(snap.count, 1, "exactly one allocation for the buffer");
        assert!(snap.bytes >= 4096, "the buffer's bytes must be counted");

        // Work after the scope must not be attributed to it.
        let after = Vec::<u8>::with_capacity(8192);
        std::hint::black_box(&after);
        let snap = AllocScope::start().finish();
        assert_eq!(
            snap.count, 0,
            "arming and disarming alone must not allocate"
        );
    }

    #[test]
    fn dropping_scope_disarms() {
        {
            let _scope = AllocScope::start();
        }
        let snap = AllocScope::start().finish();
        assert_eq!(snap.count, 0);
    }

    /// `realloc` is accounted as its growth only, so a `Vec` that settles at its
    /// final capacity contributes that capacity once, not once per doubling.
    #[test]
    fn realloc_counts_only_growth() {
        let scope = AllocScope::start();
        let mut v: Vec<u64> = Vec::with_capacity(1);
        for i in 0..64 {
            v.push(i);
        }
        let snap = scope.finish();
        assert!(snap.count >= 1, "growth must be counted: {snap:?}");
        // One initial allocation plus a handful of doublings, never 64.
        assert!(
            snap.count < 16,
            "growth must not be counted per element: {snap:?}"
        );
    }
}
