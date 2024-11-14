use crate::metrics::{RtfMetric, RtfMetricGuard};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::sleep;
use std::time::Duration;
use tracing::{info, instrument};

pub const MODEL_SAMPLE_RATE: usize = 16000;

static FIRST_RUN: AtomicBool = AtomicBool::new(true);

/// A fake stub model. This will be a model of only hyperparameters and
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct Model {
    delay: f32,
    constant_factor: f32,
    jitter: f32,
    failure_rate: f32,
    panic_rate: f32,
    warmup_penalty: f32,
}

impl Default for Model {
    fn default() -> Self {
        Self {
            delay: 0.5,
            constant_factor: 0.3,
            jitter: 0.2,
            panic_rate: 0.01,
            failure_rate: 0.0,
            warmup_penalty: 10.0,
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
            delay: 0.0,
            constant_factor: 0.0,
            jitter: 0.0,
            failure_rate: 0.0,
            panic_rate: 0.0,
            warmup_penalty: 0.0,
        }
    }

    pub fn flaky(failure_rate: f32, panic_rate: f32) -> Self {
        Self {
            delay: 0.0,
            constant_factor: 0.0,
            jitter: 0.0,
            warmup_penalty: 0.0,
            failure_rate,
            panic_rate,
        }
    }

    #[instrument(skip_all)]
    pub fn infer(&self, data: &[f32]) -> anyhow::Result<Output> {
        // Set up some basic metrics tracking
        let duration = Duration::from_secs_f32(data.len() as f32 / MODEL_SAMPLE_RATE as f32);
        let _guard = RtfMetricGuard::new(duration, RtfMetric::Model);

        let jitter = self.jitter * (fastrand::f32() * 2.0 - 1.0);
        let mut delay = duration.as_secs_f32() * self.constant_factor + self.delay + jitter;
        if self.warmup_penalty > 0.0 && FIRST_RUN.load(Ordering::Relaxed) {
            info!("First inference");
            FIRST_RUN.store(false, Ordering::Relaxed);
            delay += delay * self.warmup_penalty;
        }
        let delay = Duration::from_secs_f32(delay);

        sleep(delay);
        if fastrand::f32() >= self.failure_rate {
            if fastrand::f32() >= self.panic_rate {
                panic!("Inference catastrophically failed");
            } else {
                Ok(Output { count: data.len() })
            }
        } else {
            anyhow::bail!("Unexpected inference failure");
        }
    }
}
