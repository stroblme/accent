//! Glibc's allocator tuned at start (`accent_core::malloc`), and freed pages handed back once a
//! closed view lets go of them.

pub use accent_core::malloc::tune;

/// Trim freed allocator pages after a one-second delay for closed views and workers to finish.
pub fn trim_soon() {
    #[cfg(target_env = "gnu")]
    gtk::glib::timeout_add_seconds_local_once(1, accent_core::malloc::trim);
}
