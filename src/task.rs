#![allow(clippy::disallowed_methods, clippy::manual_async_fn)]
use metrics::Counter;
use std::future::Future;
use std::pin::Pin;
use std::task::{ready, Context, Poll};
use tokio::task;
use tracing::{Instrument, Span};

/// This type is a thin wrapper around a tokio join handle.
pub struct JoinHandle<T> {
    inner: task::JoinHandle<T>,
    panic_counter: Counter,
}

impl<T> JoinHandle<T> {
    pub fn abort(&self) {
        self.inner.abort()
    }
}

impl<T> Future for JoinHandle<T> {
    type Output = anyhow::Result<T>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let pinned = Pin::new(&mut self.inner);
        let res = ready!(pinned.poll(cx));
        let res = match res {
            Ok(v) => Ok(v),
            Err(e) => {
                self.panic_counter.increment(1);
                Err(anyhow::anyhow!(e))
            }
        };
        Poll::Ready(res)
    }
}

/// This is a wrapper around
/// [`tokio::task::spawn`](https://docs.rs/tokio/latest/tokio/task/fn.spawn.html) with the means
/// added to track panics with a metric. This assumes that there's no special handling needed for a
/// panic (or if there is it'll be fine figuring it out via the anyhow error.
///
/// If you're future is meant to be running longer than the task it's spawned in, this may not be a
/// goo dchoice as it instruments the future in the current span. Instead you should consider
/// creating another slightly different version of this function called something like
/// `spawn_free_running` (name free to be bikeshed)
pub fn spawn<F>(future: F, panic_inc: impl Into<Counter>) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let current = Span::current();
    // Here we spawn the future then move it into an async block and await to keep the same
    // behaviour as spawn (namely without awaiting it will just free-run)
    let inner = task::spawn(future.instrument(current));
    JoinHandle {
        inner,
        panic_counter: panic_inc.into(),
    }
}

/// This is a wrapper around
/// [`tokio::task::spawn_blocking`](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html) with the means
/// added to track panics with a metric. This assumes that there's no special handling needed for a
/// panic (or if there is it'll be fine figuring it out via the anyhow error.
pub fn spawn_blocking<F, R>(f: F, panic_inc: impl Into<Counter>) -> JoinHandle<F::Output>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let inner = task::spawn_blocking(f);
    JoinHandle {
        inner,
        panic_counter: panic_inc.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::*;

    /// This is a very ugly test to see if our panics are registered. For some reason the
    /// metrics-rs crates don't provide a simple way to just introspect the value of a counter so I
    /// just recreate the metrics encoding code and make sure we get a panic and don't get one.
    ///
    /// This just install a global metrics handle so we can't do it in another test in the same
    /// process (this is potential future pain).
    #[tokio::test]
    async fn check_spawn_panic_increments() {
        let encoder = AppMetricsEncoder::new();

        let _ = spawn(async {}, get_panic_counter(Subsystem::AudioDecoding)).await;

        let render = encoder.render();
        assert!(render.contains(r#"total_task_panic_count{task="audio_decoding"} 0"#));

        let _ = spawn(
            async { unimplemented!("ohno") },
            get_panic_counter(Subsystem::AudioDecoding),
        )
        .await;

        let render = encoder.render();
        assert!(render.contains(r#"total_task_panic_count{task="audio_decoding"} 1"#));

        let _ = spawn_blocking(
            || println!("Hello"),
            get_panic_counter(Subsystem::Inference),
        )
        .await;

        let render = encoder.render();
        assert!(render.contains(r#"total_task_panic_count{task="inference"} 0"#));

        let _ = spawn_blocking(
            || unimplemented!("ohno"),
            get_panic_counter(Subsystem::Inference),
        )
        .await;

        let render = encoder.render();
        assert!(render.contains(r#"total_task_panic_count{task="inference"} 1"#));
    }
}
