use serde::Serialize;
use std::thread::sleep;
use std::time::Duration;

#[derive(Clone)]
pub struct Model {
    // Dummy
    delay: Duration,
    failure_rate: f32,
}

impl Default for Model {
    fn default() -> Self {
        Self {
            delay: Duration::new(5, 0),
            failure_rate: 0.0,
        }
    }
}

#[derive(Debug, Serialize)]
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

    pub fn infer(&self, data: &[f32]) -> anyhow::Result<Output> {
        sleep(self.delay);
        if self.failure_rate == 0.0 || fastrand::f32() > self.failure_rate {
            Ok(Output { count: data.len() })
        } else {
            anyhow::bail!("Unexpected inference failure");
        }
    }
}