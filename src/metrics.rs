//! TODO write about metrics methodology:
//!
//! Going for pull because:
//!
//! 1. Works for google and we're lower scale
//! 2. leaves choice up to metrics consumers on how to grab things
//! 3. More dynamic
use metrics::{counter, describe_counter, describe_histogram, histogram, Counter, Histogram, Unit};
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};
use quanta::{Clock, Instant};
use std::time::Duration;
use tokio_metrics::{TaskMetrics, TaskMonitor};

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
    pub route: TaskMonitor,
    pub client_receiver: TaskMonitor,
    pub audio_decoding: TaskMonitor,
    pub inference: TaskMonitor,
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
        .set_buckets_for_metric(
            Matcher::Suffix("seconds".to_string()),
            RtfMetric::duration_buckets(),
        )
        .unwrap()
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
    counter!("idled_count", "task" => system).increment(metrics.total_idled_count);
    counter!("total_poll_count", "task" => system).increment(metrics.total_poll_count);
    counter!("total_fast_poll_count", "task" => system).increment(metrics.total_fast_poll_count);
    counter!("total_slow_poll_count", "task" => system).increment(metrics.total_slow_poll_count);
    counter!("total_short_delay_count", "task" => system)
        .increment(metrics.total_short_delay_count);
    counter!("total_long_delay_count", "task" => system).increment(metrics.total_long_delay_count);
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

pub fn get_panic_counter(system: Subsystem) -> Counter {
    let name = system.name();
    counter!("total_task_panic_count", "task" => name)
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
            update_metrics(Subsystem::AudioDecoding, metric);
        }
        if let Some(metric) = client_interval.next() {
            update_metrics(Subsystem::Client, metric);
        }
        if let Some(metric) = inference_interval.next() {
            update_metrics(Subsystem::Metrics, metric);
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
            0.05, 0.1, 0.2, 0.3, 0.4, 0.5, 0.75, 1.0, 1.1, 1.2, 1.3, 1.4, 1.5, 1.75, 2.0, 4.0, 6.0,
            8.0, 10.0, 15.0,
        ]
    }

    const fn duration_buckets() -> &'static [f64] {
        &[
            0.25, 0.5, 0.75, 1.0, 2.0, 3.0, 4.0, 5.0, 7.5, 10.0, 15.0, 20.0, 30.0, 60.0, 90.0,
            120.0, 300.0,
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
