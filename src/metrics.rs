//! TODO write about metrics methodology:
//!
//! Going for pull because:
//!
//! 1. Works for google and we're lower scale
//! 2. leaves choice up to metrics consumers on how to grab things
//! 3. More dynamic
use metrics::{counter, describe_counter, Counter, Unit};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use tokio::sync::Mutex;
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

fn update_metrics(system: &'static str, metrics: TaskMetrics) {
    counter!(system, "task_metric" => "idled_count").increment(metrics.total_idled_count);
    counter!(system, "task_metric" => "total_poll_count").increment(metrics.total_poll_count);
    counter!(system, "task_metric" => "total_fast_poll_count")
        .increment(metrics.total_fast_poll_count);
    counter!(system, "task_metric" => "total_slow_poll_count")
        .increment(metrics.total_slow_poll_count);
    counter!(system, "task_metric" => "total_short_delay_count")
        .increment(metrics.total_short_delay_count);
    counter!(system, "task_metric" => "total_long_delay_count")
        .increment(metrics.total_long_delay_count);
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
    counter!("total_task_panic_count", "system" => name)
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
            update_metrics("api_routing", metric);
        }
        if let Some(metric) = audio_interval.next() {
            update_metrics("audio_decoding", metric);
        }
        if let Some(metric) = client_interval.next() {
            update_metrics("client", metric);
        }
        if let Some(metric) = inference_interval.next() {
            update_metrics("inference", metric);
        }
    }
}
