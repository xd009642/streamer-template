#![allow(clippy::disallowed_methods, clippy::manual_async_fn)]
use metrics::Counter;
use std::future::Future;
use tokio::task;
use tracing::{Instrument, Span};

/// This is a wrapper around
/// [`tokio::task::spawn`](https://docs.rs/tokio/latest/tokio/task/fn.spawn.html) with the means
/// added to track panics with a metric. This assumes that there's no special handling needed for a
/// panic (or if there is it'll be fine figuring it out via the anyhow error.
pub fn spawn<F>(future: F, panic_inc: Counter) -> impl Future<Output = anyhow::Result<F::Output>>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let current = Span::current();
    // Here we spawn the future then move it into an async block and await to keep the same
    // behaviour as spawn (namely without awaiting it will just free-run)
    let future = task::spawn(future).instrument(current);
    async move {
        let res = future.await;
        match res {
            Ok(v) => Ok(v),
            Err(e) => {
                panic_inc.increment(1);
                Err(anyhow::anyhow!(e))
            }
        }
    }
}

/// This is a wrapper around
/// [`tokio::task::spawn_blocking`](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html) with the means
/// added to track panics with a metric. This assumes that there's no special handling needed for a
/// panic (or if there is it'll be fine figuring it out via the anyhow error.
pub fn spawn_blocking<F, R>(f: F, panic_inc: Counter) -> impl Future<Output = anyhow::Result<R>>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let current = Span::current();
    let future = task::spawn_blocking(f).instrument(current);
    async move {
        let res = future.await;
        match res {
            Ok(v) => Ok(v),
            Err(e) => {
                panic_inc.increment(1);
                Err(anyhow::anyhow!(e))
            }
        }
    }
}
