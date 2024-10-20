use std::future::Future;
use tokio::task;

// Here we need to pass in some sort of handle to our panic counting metric
// which is shareable. Maybe just an `Arc<AtomicUsize>` or whatever the measured counter type is.

pub fn spawn<F>(future: F) -> impl Future<Output = F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    async move {
        let res = task::spawn(future).await;
        match res {
            Ok(v) => v,
            Err(e) => {
                todo!("Do some metrics stuff")
            }
        }
    }
}

pub fn spawn_blocking<F, R>(f: F) -> impl Future<Output = R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    async move {
        let res = task::spawn_blocking(f).await;
        match res {
            Ok(v) => v,
            Err(e) => {
                todo!("Do some metrics stuff")
            }
        }
    }
}
