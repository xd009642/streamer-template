//! TODO write about metrics methodology:
//!
//! Going for pull because:
//!
//! 1. Works for google and we're lower scale
//! 2. leaves choice up to metrics consumers on how to grab things
//! 3. More dynamic
use metrics::{counter, describe_counter, histogram, Counter, Histogram, Unit};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use quanta::{Clock, Instant};
use std::time::Duration;
use tokio_metrics::{TaskMetrics, TaskMonitor};

pub struct AppMetricsEncoder {
    pub metrics: StreamingMonitors,
    pub prometheus_handle: PrometheusHandle,
}

impl AppMetricsEncoder {
    pub fn new(metrics: StreamingMonitors) -> Self {
        let builder = PrometheusBuilder::new();

        describe_task_metrics();

        let prometheus_handle = builder.install_recorder().unwrap();
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
    Audio,
    Client,
    Inference,
    Routing,
    Metrics,
}

impl Subsystem {
    const fn name(&self) -> &'static str {
        match self {
            Self::Audio => "audio_decoding",
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

pub enum RtfMetric {
    Audio,
    Vad,
    Model,
}

impl RtfMetric {
    const fn name(&self) -> &'static str {
        match self {
            Self::Audio => "audio_decoding",
            Self::Model => "model_inference",
            Self::Vad => "vad_processing",
        }
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
        histogram!("media_duration", "pipeline" => name).record(audio_duration.as_secs_f64());
        let processing_duration = histogram!("processing_duration", "pipeline" => name);
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
