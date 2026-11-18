//! A Tokio runtime beside the GTK main loop. Database work runs there; the
//! UI awaits the result inside `glib::spawn_future_local`, so nothing blocks
//! a frame.

use std::future::Future;
use std::sync::OnceLock;

fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("atlas-db")
            .enable_all()
            .build()
            .expect("tokio runtime")
    })
}

/// Runs `future` on the database runtime and waits for it from the UI.
pub async fn run<F>(future: F) -> F::Output
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    match runtime().spawn(future).await {
        Ok(value) => value,
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    }
}
