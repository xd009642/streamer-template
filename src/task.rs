#![allow(clippy::disallowed_methods, clippy::manual_async_fn)]
use std::future::Future;
use tokio::task;

// Here we need to pass in some sort of handle to our panic counting metric
// which is shareable. Maybe just an `Arc<AtomicUsize>` or whatever the measured counter type is.

pub fn spawn<F>(future: F, panic_inc: impl Fn()->()) -> impl Future<Output = anyhow::Result<F::Output>>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    async move {
        let res = task::spawn(future).await;
        match res {
            Ok(v) => Ok(v),
            Err(e) => {
                panic_inc();
                todo!("Do some metrics stuff")
            }
        }
    }
}

pub fn spawn_blocking<F, R>(f: F, panic_inc: impl Fn()->()) -> impl Future<Output = anyhow::Result<R>>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    async move {
        let res = task::spawn_blocking(f).await;
        match res {
            Ok(v) => Ok(v),
            Err(e) => {
                panic_inc();
                todo!("Do some metrics stuff")
            }
        }
    }
}
