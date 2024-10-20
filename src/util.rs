use tokio::task;
use std::future::Future;


pub fn spawn<F>(future: F) -> JoinHandle<F::Output> ⓘ
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    todo!()
}

pub fn spawn_blocking<F, R>(f: F) -> JoinHandle<R> ⓘ
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    todo!()
}
