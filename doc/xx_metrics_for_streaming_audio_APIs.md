# Adding Metrics

Traditionally, people have aimed to hit some "3 pillars of observability". The
three pillars are logs, metrics and distributed traces. There's some criticism
of these ideals as they can be hard to work with, limited in what they let you
see and not great at helping you fix customer experience. Large amounts of
conceptually separate and unlinked data is harder to work with that a snapshot
of the state of the system at the moment things go wrong.

But with that in mind, metrics can still have some usage. In benchmarking, and
some critical alerts. So here we'll mainly be focusing on setting up a
`/metrics` API which can be called to grab prometheus metrics. 

Now, without further ado lets pick an area we want metrics for and go about
implementing them!

## Remove This section

We'll also aim
to collect the following metrics:

1. Tokio task metrics for performance engineering purposes
2. Any task panics which suggest critical failures in the code
3. Runtime performance metrics for the pipeline stages themselves
4. Probably some counters or gauges to let us ascertain load

For each section I'll 

## Panics

Our current code uses a lot of `tokio::task::spawn` and
`tokio::task::spawn_blocking` to keep data streaming through the pipeline
stages and avoiding blocking the runtime. However, any time someone uses
one of these functions we want them to be checking the outer error to make
sure the spawned fucntion or future hasn't panicked. A panic would indicate
something's gone wrong and either means our API should change it's health
status or some engineer should be alerted that there's an issue in the code.

Reviewing every usage of these functions to spot out missed panics is a bit
onerous and occasionally something might slip through the cracks. So let's
create our own version of them and pass in a `metrics::Counter` which will
count the times we've panicked in this task.

```rust
use metrics::Counter;
use std::future::Future;
use tokio::task;
use tracing::{Instrument, Span};

pub fn spawn<F>(future: F, panic_inc: Counter) -> impl Future<Output = anyhow::Result<F::Output>>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let current = Span::current();
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
```

Tokio spawns will keep on running on a separate worker without being manually
awaited and don't cancel on drop. We want to maintain the same behaviour to
avoid confusion so we run the spawn commands and then move them into an `async`
block to be awaited and the counter incremented. 

This does mean if a task panics but we never await the task and drop it the
metric won't increment. But there will be a compiler warning about an unused
`Future` so these parts of the code will stand out.

Notice this also instruments our spawns in the current span which we want so
our tracing context all lines up.

This was relatively easy. But how can we make sure other people working on the
code use our spawns instead of the tokio one? Well, Clippy actually has some
nice functionality for this. 

In the project I'll create a `clippy.toml` with the following contents:

```toml
disallowed-methods = [
    "tokio::task::spawn_blocking",
    "tokio::task::spawn"
]
```

And then at the top of `src/task.rs` where I've added this code I add the
following clippy attribute:

```rust
#![allow(clippy::disallowed_methods, clippy::manual_async_fn)]
```

This means clippy will let us use the tokio spawns in this module and no
where else. It will also silent the warning lint about us returning an
`impl Future` type instead of writing an `async` function.

## Tokio Metrics

The tokio team have been working on
[`tokio_metrics`](https://docs.rs/tokio-metrics/latest/tokio_metrics/)
to collect metrics for a task. As we expect our work will have parts which
are more CPU bound such as inference there's always the chance we might
unwittingly block the executor and reduce throughput. 

There's also a great part of the docs called [Why are my tasks slow](https://docs.rs/tokio-metrics/latest/tokio_metrics/struct.TaskMonitor.html#why-are-my-tasks-slow)
which explains all the metrics and how they can be interpreted. 
