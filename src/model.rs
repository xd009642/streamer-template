use crate::metrics::{RtfMetric, RtfMetricGuard};
use serde::{Deserialize, Serialize};
use std::thread::sleep;
use std::time::Duration;
use tracing::instrument;

pub const MODEL_SAMPLE_RATE: usize = 16000;

#[derive(Clone)]
pub struct Model {
    // Dummy
    delay: Duration,
    failure_rate: f32,
}

impl Default for Model {
    fn default() -> Self {
        Self {
            delay: Duration::new(1, 0),
            failure_rate: 0.0,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Output {
    pub count: usize,
}

impl Model {
    pub fn speedy() -> Self {
        Self {
            delay: Duration::new(0, 0),
            failure_rate: 0.0,
        }
    }

    pub fn flaky(failure_rate: f32) -> Self {
        Self {
            delay: Duration::new(0, 0),
            failure_rate,
        }
    }

    #[instrument(skip_all)]
    pub fn infer(&self, data: &[f32]) -> anyhow::Result<Output> {
        // Set up some basic metrics tracking
        let duration = Duration::from_secs_f32(data.len() as f32 / 16000.0);
        let _guard = RtfMetricGuard::new(duration, RtfMetric::Model);

        sleep(self.delay);
        if self.failure_rate == 0.0 || fastrand::f32() > self.failure_rate {
            Ok(Output { count: data.len() })
        } else {
            anyhow::bail!("Unexpected inference failure");
        }
    }
}
