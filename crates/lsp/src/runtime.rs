//! Process-wide async runtime shared by language servers and MCP. Two workers handle I/O and JSON
//! while GTK and RPC callers wait outside the runtime.

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
