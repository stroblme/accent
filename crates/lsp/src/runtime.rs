//! The one async runtime in the process.
//!
//! Language servers are the first thing accent talks to asynchronously and the MCP server
//! (Phase 7) is the second; both share this runtime rather than each starting one. Two workers:
//! the work is framing and JSON, and the callers are the GTK main loop and `serve`'s request
//! threads, which poll from outside.

use std::sync::OnceLock;

use tokio::runtime::{Builder, Runtime};

/// The process-wide runtime, started on first use.
pub fn runtime() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("accent-async")
            .build()
            .expect("tokio runtime")
    })
}
