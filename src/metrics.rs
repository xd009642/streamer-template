//! TODO write about metrics methodology:
//!
//! Going for pull because:
//!
//! 1. Works for google and we're lower scale
//! 2. leaves choice up to metrics consumers on how to grab things
//! 3. More dynamic
use measured::label::StaticLabelSet;
use measured::metric::name::MetricName;
use measured::metric::MetricFamilyEncoding;
use measured::metric::{
    counter::{write_counter, CounterState},
    group::Encoding,
    MetricEncoding,
};
use measured::text::BufferedTextEncoder;
use measured::{CounterVec, FixedCardinalityLabel, LabelGroup, MetricGroup};
use tokio_metrics::TaskMonitor;

#[derive(Clone)]
pub struct StreamingMonitors {
    pub route: TaskMonitor,
    pub client_receiver: TaskMonitor,
    pub audio_decoding: TaskMonitor,
    pub inference: TaskMonitor,
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

impl StreamingMonitors {
    pub fn new() -> Self {
        Self {
            route: TaskMonitor::new(),
            client_receiver: TaskMonitor::new(),
            audio_decoding: TaskMonitor::new(),
            inference: TaskMonitor::new(),
        }
    }

    pub fn collect(&self, enc: &mut BufferedTextEncoder) {
        let metrics = TaskMetricGroup::new();
        let route_interval = self.route.intervals().next().unwrap();
        metrics.route_counters.inc_by(
            TaskLabelGroup {
                task_metric: TaskMetricCounter::IdledCount,
            },
            route_interval.total_idled_count,
        );
        metrics.route_counters.inc_by(
            TaskLabelGroup {
                task_metric: TaskMetricCounter::TotalPollCount,
            },
            route_interval.total_poll_count,
        );
        metrics.route_counters.inc_by(
            TaskLabelGroup {
                task_metric: TaskMetricCounter::TotalFastPollCount,
            },
            route_interval.total_fast_poll_count,
        );
        metrics.route_counters.inc_by(
            TaskLabelGroup {
                task_metric: TaskMetricCounter::TotalSlowPollCount,
            },
            route_interval.total_slow_poll_count,
        );
        metrics.route_counters.inc_by(
            TaskLabelGroup {
                task_metric: TaskMetricCounter::TotalShortDelayCount,
            },
            route_interval.total_short_delay_count,
        );
        metrics.route_counters.inc_by(
            TaskLabelGroup {
                task_metric: TaskMetricCounter::TotalLongDelayCount,
            },
            route_interval.total_long_delay_count,
        );

        let audio_interval = self.audio_decoding.intervals().next().unwrap();
        metrics.audio_counters.inc_by(
            TaskLabelGroup {
                task_metric: TaskMetricCounter::IdledCount,
            },
            audio_interval.total_idled_count,
        );
        metrics.audio_counters.inc_by(
            TaskLabelGroup {
                task_metric: TaskMetricCounter::TotalPollCount,
            },
            audio_interval.total_poll_count,
        );
        metrics.audio_counters.inc_by(
            TaskLabelGroup {
                task_metric: TaskMetricCounter::TotalFastPollCount,
            },
            audio_interval.total_fast_poll_count,
        );
        metrics.audio_counters.inc_by(
            TaskLabelGroup {
                task_metric: TaskMetricCounter::TotalSlowPollCount,
            },
            audio_interval.total_slow_poll_count,
        );
        metrics.audio_counters.inc_by(
            TaskLabelGroup {
                task_metric: TaskMetricCounter::TotalShortDelayCount,
            },
            audio_interval.total_short_delay_count,
        );
        metrics.audio_counters.inc_by(
            TaskLabelGroup {
                task_metric: TaskMetricCounter::TotalLongDelayCount,
            },
            audio_interval.total_long_delay_count,
        );

        // err type is Infallible
        let _ = metrics.collect_group_into(enc);
    }
}
