//! TODO write about metrics methodology:
//!
//! Going for pull because:
//!
//! 1. Works for google and we're lower scale
//! 2. leaves choice up to metrics consumers on how to grab things
//! 3. More dynamic
use tokio_metrics::TaskMonitor;

#[derive(Clone)]
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
}
