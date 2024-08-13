//! TODO write about metrics methodology:
//!
//! Going for pull because:
//!
//! 1. Works for google and we're lower scale
//! 2. leaves choice up to metrics consumers on how to grab things
//! 3. More dynamic
use measured::text::BufferedTextEncoder;
use measured::{CounterVec, FixedCardinalityLabel, LabelGroup, MetricGroup};
use tokio::sync::Mutex;
use tokio_metrics::{TaskMetrics, TaskMonitor};

pub struct AppMetricsEncoder {
    pub(crate) encoder: Mutex<BufferedTextEncoder>,
    pub metrics: StreamingMonitors,
}

impl AppMetricsEncoder {
    pub fn new(metrics: StreamingMonitors) -> Self {
        Self {
            encoder: Mutex::default(),
            metrics,
        }
    }
}

pub struct StreamingMonitors {
    pub route: TaskMonitor,
    pub client_receiver: TaskMonitor,
    pub audio_decoding: TaskMonitor,
    pub inference: TaskMonitor,
    metrics_group: TaskMetricGroup,
}

#[derive(FixedCardinalityLabel, Copy, Clone)]
enum TaskMetricCounter {
    IdledCount,
    TotalPollCount,
    TotalFastPollCount,
    TotalSlowPollCount,
    TotalShortDelayCount,
    TotalLongDelayCount,
}

#[derive(LabelGroup)]
#[label(set = TaskLabelGroupSet)]
struct TaskLabelGroup {
    task_metric: TaskMetricCounter,
}
#[derive(MetricGroup)]
#[metric(new())]
struct TaskMetricGroup {
    /// API Route based task counters
    route_counters: CounterVec<TaskLabelGroupSet>,
    /// Audio decoding based task counters
    audio_counters: CounterVec<TaskLabelGroupSet>,
    /// Client receiving task counters
    client_counters: CounterVec<TaskLabelGroupSet>,
    /// Model inference task counters
    inference_counters: CounterVec<TaskLabelGroupSet>,
}

fn update_metrics(counters: &CounterVec<TaskLabelGroupSet>, metrics: TaskMetrics) {
    counters.inc_by(
        TaskLabelGroup {
            task_metric: TaskMetricCounter::IdledCount,
        },
        metrics.total_idled_count,
    );
    counters.inc_by(
        TaskLabelGroup {
            task_metric: TaskMetricCounter::TotalPollCount,
        },
        metrics.total_poll_count,
    );
    counters.inc_by(
        TaskLabelGroup {
            task_metric: TaskMetricCounter::TotalFastPollCount,
        },
        metrics.total_fast_poll_count,
    );
    counters.inc_by(
        TaskLabelGroup {
            task_metric: TaskMetricCounter::TotalSlowPollCount,
        },
        metrics.total_slow_poll_count,
    );
    counters.inc_by(
        TaskLabelGroup {
            task_metric: TaskMetricCounter::TotalShortDelayCount,
        },
        metrics.total_short_delay_count,
    );
    counters.inc_by(
        TaskLabelGroup {
            task_metric: TaskMetricCounter::TotalLongDelayCount,
        },
        metrics.total_long_delay_count,
    );
}

impl StreamingMonitors {
    pub fn new() -> Self {
        Self {
            route: TaskMonitor::new(),
            client_receiver: TaskMonitor::new(),
            audio_decoding: TaskMonitor::new(),
            inference: TaskMonitor::new(),
            metrics_group: TaskMetricGroup::new(),
        }
    }

    pub fn encode(&self, enc: &mut BufferedTextEncoder) {
        // err type is Infallible
        let _ = self.metrics_group.collect_group_into(enc);
    }

    pub fn run_collector(&self) {
        let mut route_interval = self.route.intervals();
        let mut audio_interval = self.audio_decoding.intervals();
        let mut client_interval = self.client_receiver.intervals();
        let mut inference_interval = self.inference.intervals();

        if let Some(metric) = route_interval.next() {
            update_metrics(&self.metrics_group.route_counters, metric);
        }
        if let Some(metric) = audio_interval.next() {
            update_metrics(&self.metrics_group.audio_counters, metric);
        }
        if let Some(metric) = client_interval.next() {
            update_metrics(&self.metrics_group.client_counters, metric);
        }
        if let Some(metric) = inference_interval.next() {
            update_metrics(&self.metrics_group.inference_counters, metric);
        }
    }
}
