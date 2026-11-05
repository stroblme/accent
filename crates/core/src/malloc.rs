//! Keep freed large blocks from accumulating in glibc arenas.
//!
//! Glibc raises its mmap threshold after large frees, sending later blocks into per-thread arenas
//! that retain memory. Holding the threshold reduced the app's resident memory from 537 MB to
//! 194 MB in a 552-page PDF drill (`ACCENT_BENCH_MEMORY`, with a trim after close), and the peak
//! of a cold `accent-cli index` over 48 notes of 15.5 MB from 1.39 GB to 0.46 GB. Musl, which the
//! uploaded server is built with, has neither call, its allocator mapping large blocks of its own.

/// Fix glibc's mmap threshold at 128 KiB so large frees unmap blocks instead of raising the threshold.
// ponytail: each such block now costs a mapping and its page faults. The cairo renderer copies
// every tile it draws, every frame, which made 1.6 s more system time over the 38 s deep-zoom
// drill; GL took no more. If it shows, leave the threshold alone and trim on an idle timer instead.
pub fn tune() {
    // SAFETY: M_MMAP_THRESHOLD takes a byte count, and any count is safe.
    #[cfg(target_env = "gnu")]
    unsafe {
        libc::mallopt(libc::M_MMAP_THRESHOLD, 128 << 10)
    };
}

/// Return free allocator pages to the kernel.
pub fn trim() {
    // SAFETY: only returns free pages to the kernel.
    #[cfg(target_env = "gnu")]
    unsafe {
        libc::malloc_trim(0)
    };
}
