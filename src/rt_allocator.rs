use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};

thread_local! {
    static REALTIME_CALLBACK: Cell<bool> = const { Cell::new(false) };
}

static CALLBACK_ALLOCATIONS: AtomicU64 = AtomicU64::new(0);

pub(crate) struct DetectingAllocator;

unsafe impl GlobalAlloc for DetectingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record_callback_allocation();
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record_callback_allocation();
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        record_callback_allocation();
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

fn record_callback_allocation() {
    REALTIME_CALLBACK.with(|active| {
        if active.get() {
            CALLBACK_ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
    });
}

pub(crate) struct RealtimeCallbackGuard {
    previous: bool,
}

impl RealtimeCallbackGuard {
    pub(crate) fn enter() -> Self {
        let previous = REALTIME_CALLBACK.with(|active| active.replace(true));
        Self { previous }
    }
}

impl Drop for RealtimeCallbackGuard {
    fn drop(&mut self) {
        REALTIME_CALLBACK.with(|active| active.set(self.previous));
    }
}

#[allow(dead_code)]
pub(crate) fn callback_allocation_count() -> u64 {
    CALLBACK_ALLOCATIONS.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_allocator_counts_realtime_callback_allocations() {
        let before = callback_allocation_count();
        {
            let _guard = RealtimeCallbackGuard::enter();
            let value = Box::new([0_u8; 64]);
            std::hint::black_box(value);
        }
        assert!(callback_allocation_count() > before);
    }
}
