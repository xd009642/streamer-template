//! TODO write about metrics methodology:
//!
//! Going for pull because:
//!
//! 1. Works for google and we're lower scale
//! 2. leaves choice up to metrics consumers on how to grab things
//! 3. More dynamic
use metrics::{counter, describe_counter, describe_histogram, gauge, histogram, Counter, Histogram, Unit};
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};
use quanta::{Clock, Instant};
use std::sync::LazyLock;
use std::time::Duration;
use tokio_metrics::{RuntimeMetrics, RuntimeMonitor, TaskMetrics, TaskMonitor};

pub static METRICS_HANDLE: LazyLock<AppMetricsEncoder> = LazyLock::new(AppMetricsEncoder::new);

pub struct AppMetricsEncoder {
    pub metrics: StreamingMonitors,
    pub prometheus_handle: PrometheusHandle,
}

impl AppMetricsEncoder {
    pub fn new() -> Self {
        let builder = PrometheusBuilder::new();

        describe_task_metrics();
        let builder = describe_audio_metrics(builder);

        let prometheus_handle = builder.install_recorder().unwrap();
        let metrics = StreamingMonitors::new();
        Self {
            metrics,
            prometheus_handle,
        }
    }

    pub fn render(&self) -> String {
        self.prometheus_handle.render()
    }

    pub fn update(&self) {
        self.metrics.run_collector();
        self.prometheus_handle.run_upkeep();
    }
}

pub struct StreamingMonitors {
    runtime: RuntimeMonitor,
    pub route: TaskMonitor,
    pub client_receiver: TaskMonitor,
    pub audio_decoding: TaskMonitor,
    pub inference: TaskMonitor,
    pub metrics: TaskMonitor,
}

fn describe_audio_metrics(builder: PrometheusBuilder) -> PrometheusBuilder {
    describe_histogram!(
        "media_duration_seconds",
        Unit::Seconds,
        "Duration of audio."
    );
    describe_histogram!(
        "processing_duration_seconds",
        Unit::Seconds,
        "Duration of processing time."
    );
    describe_histogram!("rtf", "Real-Time Factor of the processing");

    builder
        .set_buckets_for_metric(Matcher::Suffix("rtf".to_string()), RtfMetric::rtf_buckets())
        .unwrap()
}

fn describe_task_metrics() {
    describe_counter!(
        "idled_count",
        Unit::Count,
        "The total number of times that tasks idled, waiting to be awoken."
    );
    describe_counter!(
        "total_poll_count",
        Unit::Count,
        "The total number of times tasks were polled."
    );
    describe_counter!(
        "total_fast_poll_count",
        Unit::Count,
        "The total number of times that polling tasks completed swiftly."
    );
    describe_counter!(
        "total_slow_poll_count",
        Unit::Count,
        "The total number of times that polling tasks completed slowly."
    );
    describe_counter!(
        "total_short_delay_count",
        Unit::Count,
        "The total count of tasks with short scheduling delays."
    );
    describe_counter!(
        "total_long_delay_count",
        Unit::Count,
        "The total count of tasks with long scheduling delays."
    );
    describe_counter!(
        "total_task_panic_count",
        Unit::Count,
        "The total count of times the task panicked"
    );
}

fn update_metrics(system: Subsystem, metrics: TaskMetrics) {
    let system = system.name();

    counter!("instrumented_count", "task" => system).increment(metrics.instrumented_count);
    counter!("dropped_count", "task" => system).increment(metrics.dropped_count);
    counter!("first_poll_count", "task" => system).increment(metrics.first_poll_count);
    counter!("total_idled_count", "task" => system).increment(metrics.total_idled_count);
    counter!("total_poll_count", "task" => system).increment(metrics.total_poll_count);
    counter!("total_fast_poll_count", "task" => system).increment(metrics.total_fast_poll_count);
    counter!("total_slow_poll_count", "task" => system).increment(metrics.total_slow_poll_count);
    counter!("total_short_delay_count", "task" => system)
        .increment(metrics.total_short_delay_count);
    counter!("total_long_delay_count", "task" => system).increment(metrics.total_long_delay_count);

    histogram!("total_first_poll_delay", "task" => system).record(metrics.total_first_poll_delay.as_secs_f64());
    histogram!("total_idle_duration", "task" => system).record(metrics.total_idle_duration.as_secs_f64());
    histogram!("total_scheduled_duration", "task" => system).record(metrics.total_scheduled_duration.as_secs_f64());
    histogram!("total_poll_duration", "task" => system).record(metrics.total_poll_duration.as_secs_f64());
    histogram!("total_fast_poll_duration", "task" => system).record(metrics.total_fast_poll_duration.as_secs_f64());
    histogram!("total_slow_poll_duration", "task" => system).record(metrics.total_slow_poll_duration.as_secs_f64());
    histogram!("total_short_delay_duration", "task" => system).record(metrics.total_short_delay_duration.as_secs_f64());
    histogram!("total_long_delay_duration", "task" => system).record(metrics.total_long_delay_duration.as_secs_f64());
    
    gauge!("max_idle_duration", "task" => system).set(metrics.max_idle_duration.as_secs_f64());
}

fn update_runtime_metrics(metrics: RuntimeMetrics) {
    gauge!("workers_count").set(metrics.workers_count as f64);
    gauge!("live_tasks_count").set(metrics.live_tasks_count as f64);
    gauge!("global_queue_depth").set(metrics.global_queue_depth as f64);
    gauge!("max_park_count").set(metrics.max_park_count as f64);
    gauge!("min_park_count").set(metrics.min_park_count as f64);
    gauge!("max_busy_duration").set(metrics.max_busy_duration.as_secs_f64());
    gauge!("min_busy_duration").set(metrics.min_busy_duration.as_secs_f64());

    counter!("total_park_count").increment(metrics.total_park_count);
    counter!("total_busy_duration").increment(metrics.total_busy_duration.as_nanos() as u64);

    histogram!("elapsed").record(metrics.elapsed.as_secs_f64());
}

impl Default for StreamingMonitors {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Copy, Clone, Eq, PartialEq)]
pub enum Subsystem {
    AudioDecoding,
    Client,
    Inference,
    Routing,
    Metrics,
}

impl Subsystem {
    const fn name(&self) -> &'static str {
        match self {
            Self::AudioDecoding => "audio_decoding",
            Self::Client => "client",
            Self::Inference => "inference",
            Self::Routing => "api_routing",
            Self::Metrics => "metrics",
        }
    }
}

#[inline(always)]
fn update_intervals(subsystem: Subsystem, monitor: &TaskMonitor) {
    let mut interval = monitor.intervals();
    if let Some(metric) = interval.next() {
        update_metrics(subsystem, metric);
    }
}

impl StreamingMonitors {
    pub fn new() -> Self {
        let runtime = RuntimeMonitor::new(&tokio::runtime::Handle::current());
        Self {
            runtime,
            route: TaskMonitor::new(),
            client_receiver: TaskMonitor::new(),
            audio_decoding: TaskMonitor::new(),
            inference: TaskMonitor::new(),
            metrics: TaskMonitor::new(),
        }
    }

    pub fn run_collector(&self) {
        let mut interval = self.runtime.intervals();
        if let Some(metrics) = interval.next() {
            update_runtime_metrics(metrics); 
        }
        update_intervals(Subsystem::Routing, &self.route);
        update_intervals(Subsystem::AudioDecoding, &self.audio_decoding);
        update_intervals(Subsystem::Client, &self.client_receiver);
        update_intervals(Subsystem::Inference, &self.inference);
        update_intervals(Subsystem::Metrics, &self.metrics);
    }

    pub fn get_monitor(&self, system: Subsystem) -> TaskMonitor {
        match system {
            Subsystem::Routing => self.route.clone(),
            Subsystem::AudioDecoding => self.audio_decoding.clone(),
            Subsystem::Client => self.client_receiver.clone(),
            Subsystem::Inference => self.inference.clone(),
            Subsystem::Metrics => self.metrics.clone(),
        }
    }
}

pub enum RtfMetric {
    AudioDecoding,
    Vad,
    Model,
}

impl RtfMetric {
    const fn name(&self) -> &'static str {
        match self {
            Self::AudioDecoding => "audio_decoding",
            Self::Model => "model_inference",
            Self::Vad => "vad_processing",
        }
    }

    /// Define the histogram buckets for our RTF. In real life you'll have a vague idea of how
    /// fast your model runs via benchmarking and then just try and pick some values around that
    /// which feel right. It may take some tuning and adapting as things go and may change based on
    /// models or things like what type of GPU you use. I'll just pick some pretty arbitrary
    /// measurements that feel ok.
    ///
    /// This is also a moderately large histogram that covers all our stages despite some of them
    /// being a lot bigger. This is because I'm not too sure about specifying different buckets
    /// based on the metric fields (not just name).
    const fn rtf_buckets() -> &'static [f64] {
        &[
            0.01, 0.05, 0.1, 0.2, 0.3, 0.4, 0.5, 0.75, 1.0, 1.1, 1.2, 1.3, 1.4, 1.5, 1.75, 2.0,
            4.0, 6.0, 8.0, 10.0, 15.0,
        ]
    }
}

pub struct RtfMetricGuard {
    clock: Clock,
    now: Instant,
    audio_duration: Duration,
    rtf: Histogram,
    processing_duration: Histogram,
}

impl RtfMetricGuard {
    pub fn new(audio_duration: Duration, metric: RtfMetric) -> Self {
        let clock = Clock::new();
        let name = metric.name();
        histogram!("media_duration_seconds", "pipeline" => name)
            .record(audio_duration.as_secs_f64());
        let processing_duration =
            histogram!("processing_duration_seconds", "pipeline_stage" => name);
        let rtf = histogram!("rtf", "pipeline" => name);
        let now = clock.now();
        Self {
            clock,
            rtf,
            audio_duration,
            processing_duration,
            now,
        }
    }
}

impl Drop for RtfMetricGuard {
    fn drop(&mut self) {
        let end = self.clock.now();
        let processing_duration = end.duration_since(self.now);
        self.processing_duration
            .record(processing_duration.as_secs_f64());
        let rtf = processing_duration.as_secs_f64() / self.audio_duration.as_secs_f64();
        self.rtf.record(rtf);
    }
}

impl From<Subsystem> for Counter {
    fn from(val: Subsystem) -> Self {
        let name = val.name();
        counter!("total_task_panic_count", "task" => name)
    }
}
