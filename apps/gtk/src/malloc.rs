//! glibc's allocator, told to give back what the app frees.
//!
//! By default glibc raises the size from which it maps a block on its own to the largest such
//! block freed so far, so after the first few 1 MiB PDF tiles every later one comes out of an
//! arena, and an arena keeps what is freed in it for the next allocation. Reading a 552-page PDF
//! through and closing it left 537 MB resident that way, and 194 MB with the threshold held and
//! a trim after the close (`ACCENT_BENCH_MEMORY`).

/// Map every block of 128 KiB and up on its own, so freeing one unmaps it. That is where glibc
/// starts anyway; setting it is what stops glibc raising it.
///
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

/// Hand the kernel the free pages left between blocks still in use, in a second: after a PDF tab
/// or a window has closed, once the frame that still painted it and its threads have gone too.
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
