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

One thing to keep in mind, unlike a normal REST API where we can measure things
like response time easily here things aren't so easy. For the more common use
case of the VAD (Voice Activity Detection) segmented audio the first response
will be some point after the first voiced frame appears. Additionally, if the
user hasn't requested interim results the response for a complete utterance will
be some latency after the last voiced frame appears. Because of this things
like response time become harder to measure and the normal metrics people use
to monitor performance start to become less strongly correlated to performance
or harder to generate.

## Remove This section

We'll also aim
to collect the following metrics:

1. Tokio task metrics for performance engineering purposes
2. Any task panics which suggest critical failures in the code
3. Runtime performance metrics for the pipeline stages themselves
4. Probably some counters or gauges to let us ascertain load

## The Metrics Ecosystem

For the metrics implementation we'll be using [metrics-rs/metrics](https://github.com/metrics-rs/metrics)
and it's related ecosystem. This provides a number of handy macros for
registering and updating metrics and avoid us having to pass handles
around too much.

Usage isn't too hard, we can use macros like so to describe a metric and add
some documentation to the metric:

```rust
describe_counter!("request_count", Unit::Count, "number of requests");
```

And then incrementing the counter we do:

```rust
counter!("request_count").increment(1);
```

Personally, I'm not too much of a fan of stringly typed things, so for metric
areas you'll either see me using the strings in one small concentrated area or
if it's for code that's meant to be called outside of the metrics module creating
an enum like:

```rust
#[derive(Copy, Clone, Eq)]
pub enum MetricArea {
    AudioDecoding,
    Model,
}

impl MetricArea {
    fn metric_name(&self) -> &'static str {
        match self {
            Self::AudioDecoding => "audio_decoding",
            Self::Model => "model",
        }
    }
}
```

To save rewriting a few very similar enum impls, I'll put a
comment above the enum like `// marker enum`.

I also don't want to be pushing metrics out but rather have something call a
`/metrics` endpoint. This avoids some of the annoying log messages about no
such endpoint when a metrics collector isn't around and makes configuration
easier. To add this to our Axum server we'll create a type called
`AppMetricsEncoder` like so:

```rust
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};

pub struct AppMetricsEncoder {
    pub prometheus_handle: PrometheusHandle,
}

impl AppMetricsEncoder {
    pub fn new() -> Self {
        let builder = PrometheusBuilder::new();

        let prometheus_handle = builder.install_recorder().unwrap();
        Self {
            prometheus_handle,
        }
    }

    pub fn render(&self) -> String {
        self.prometheus_handle.render()
    }

    pub fn update(&self) {
        self.prometheus_handle.run_upkeep();
    }
}
```

Adding the `/metrics` endpoint then looks like:

```rust
pub fn make_service_router(app_state: Arc<StreamingContext>) -> Router {
    let metrics_encoder = Arc::new(AppMetricsEncoder::new());
    let collector_metrics = metrics_encoder.clone();
    // Keep the metrics upkeep going in a background task
    let _ = tokio::task::spawn(
        async move {
            loop {
                collector_metrics.update();
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    );
    Router::new()
        .route(
            "/api/v1/simple",
            get({
                move |ws, app_state| {
                    ws_handler(ws, false, app_state)
                }
            }),
        )
        .route(
            "/api/v1/segmented",
            get({
                move |ws, app_state| {
                    ws_handler(ws, true, app_state)
                }
            }),
        )
        .route("/api/v1/health", get(health_check))
        .route("/metrics", get(get_metrics))
        .layer(Extension(metrics_encoder))
        .layer(Extension(app_state))
}
```

If we call `/metrics` now we'll get an empty prometheus response, which is
progress but we've still got work to do. Now, without further ado lets pick an
area we want metrics for and go about implementing them!

## Tokio Metrics

The tokio team have been working on
[`tokio_metrics`](https://docs.rs/tokio-metrics/latest/tokio_metrics/)
to collect metrics for a task. As we expect our work will have parts which
are more CPU bound such as inference there's always the chance we might
unwittingly block the executor and reduce throughput.

There's also a great part of the docs called [Why are my tasks slow](https://docs.rs/tokio-metrics/latest/tokio_metrics/struct.TaskMonitor.html#why-are-my-tasks-slow)
which explains all the metrics and how they can be interpreted.

However, there are 18 metrics and information overload is a thing that
exists. So initially, we'll be limiting the metrics to just:

* `idled_count` - total number of idled tasks
* `total_poll_count` - total number of times a task was polled
* `total_fast_poll_count` - number of polls that were fast
* `total_slow_poll_count` - number of polls that were slow
* `total_short_delay_count` - number of tasks with short scheduling delays
* `total_long_delay_count` - number of tasks with long scheduling delays

Now `total_poll_count` should be equivalent to `total_fast_poll_count + total_slow_poll_count`
making it a little redundant. But one extra metric doesn't hurt and it can
be a quick sanity check my end that the metric implementation is correct.

One thing is for sure, none of these metrics on their own necessarily mean
latency is impacted as they often work together. For example, an increased
`total_long_delay_count` could result from fewer task polls. But understanding
what the runtime is doing is often a useful step in diagnosing performance
issues.

We need to make a `TaskMonitor` for each task and keep it around for the
duration of the program. To keep the monitors around we'll make a struct and
dump them all in there. Additionally, the monitors will have to be polled
in a background thread and the values extracted and put into our metrics. 
Our initial struct looks like:

```rust
pub struct StreamingMonitors {
    pub route: TaskMonitor,
    pub client_receiver: TaskMonitor,
    pub audio_decoding: TaskMonitor,
    pub inference: TaskMonitor,
}

impl StreamingMonitors {
    pub fn new() -> Self {
        Self {
            route: TaskMonitor::new(),
            client_receiver: TaskMonitor::new(),
            audio_decoding: TaskMonitor::new(),
            inference: TaskMonitor::new(),
        }
    }

    pub fn run_collector(&self) {
        let mut route_interval = self.route.intervals();
        let mut audio_interval = self.audio_decoding.intervals();
        let mut client_interval = self.client_receiver.intervals();
        let mut inference_interval = self.inference.intervals();

        if let Some(metric) = route_interval.next() {
            update_metrics(Subsystem::Routing, metric);
        }
        if let Some(metric) = audio_interval.next() {
            update_metrics(Subsystem::Audio, metric);
        }
        if let Some(metric) = client_interval.next() {
            update_metrics(Subsystem::Client, metric);
        }
        if let Some(metric) = inference_interval.next() {
            update_metrics(Subsystem::Metrics, metric);
        }
    }
}

fn update_metrics(system: Subsystem, metrics: TaskMetrics) {
    let system = system.name();
    counter!("idled_count", "task" => system).increment(metrics.total_idled_count);
    counter!("total_poll_count", "task" => system).increment(metrics.total_poll_count);
    counter!("total_fast_poll_count", "task" => system).increment(metrics.total_fast_poll_count);
    counter!("total_slow_poll_count", "task" => system).increment(metrics.total_slow_poll_count);
    counter!("total_short_delay_count", "task" => system)
        .increment(metrics.total_short_delay_count);
    counter!("total_long_delay_count", "task" => system).increment(metrics.total_long_delay_count);
}
```

Integrating this into our function to setup the Axum router we end up with this
code:

```rust
pub fn make_service_router(app_state: Arc<StreamingContext>) -> Router {
    let streaming_monitor = StreamingMonitors::new();
    let metrics_encoder = Arc::new(AppMetricsEncoder::new(streaming_monitor));
    let collector_metrics = metrics_encoder.clone();
    let _ = tokio::task::spawn(
        async move {
            loop {
                collector_metrics.update();
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    );
    Router::new()
        .route(
            "/api/v1/simple",
            get({
                move |ws, app_state, metrics_enc: Extension<Arc<AppMetricsEncoder>>| {
                    let route = metrics_enc.metrics.route.clone();
                    TaskMonitor::instrument(&route, ws_handler(ws, false, app_state, metrics_enc))
                }
            }),
        )
        .route(
            "/api/v1/segmented",
            get({
                move |ws, app_state, metrics_enc: Extension<Arc<AppMetricsEncoder>>| {
                    let route = metrics_enc.metrics.route.clone();
                    TaskMonitor::instrument(&route, ws_handler(ws, true, app_state, metrics_enc))
                }
            }),
        )
        .route("/api/v1/health", get(health_check))
        .route("/metrics", get(get_metrics))
        .layer(Extension(metrics_encoder))
        .layer(Extension(app_state))
}
```

We now also pass the metrics encoder into the `ws_handler` function so we can
instrument the various tasks we care about.

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
