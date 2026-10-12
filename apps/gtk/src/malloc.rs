//! Keep freed PDF tiles from accumulating in glibc arenas.
//!
//! Glibc raises its mmap threshold after large frees, sending later tiles into arenas that
//! retain memory. Holding the threshold and trimming after close reduced resident memory
//! from 537 MB to 194 MB in a 552-page PDF drill (`ACCENT_BENCH_MEMORY`).

/// Fix glibc's mmap threshold at 128 KiB so large frees unmap blocks instead of raising the threshold.
// ponytail: each such block now costs a mapping and its page faults. The cairo renderer copies
// every tile it draws, every frame, which made 1.6 s more system time over the 38 s deep-zoom
// drill; GL took no more. If it shows, leave the threshold alone and trim on an idle timer instead.
pub fn tune() {
    // SAFETY: M_MMAP_THRESHOLD (-3) takes a byte count, and any count is safe.
    #[cfg(target_env = "gnu")]
    unsafe {
        mallopt(-3, 128 << 10)
    };
}

/// Trim freed allocator pages after a one-second delay for closed views and workers to finish.
pub fn trim_soon() {
    #[cfg(target_env = "gnu")]
    gtk::glib::timeout_add_seconds_local_once(1, || {
        // SAFETY: only returns free pages to the kernel.
        unsafe { malloc_trim(0) };
    });
}

#[cfg(target_env = "gnu")]
unsafe extern "C" {
    fn mallopt(param: i32, value: i32) -> i32;
    fn malloc_trim(pad: usize) -> i32;
}
