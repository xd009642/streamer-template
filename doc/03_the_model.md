# The Model

As previously mentioned, this series won't integrate a real model. There's
plenty of them around and we care more about the API and engine side than the
model side. But, we still have a dummy model to help us replicate some
behaviour and we still need to discuss it before we can move on.

In real life these models are normally the slowest part of the system and a
bottleneck we have to get around. So with that in mind I'm going to be just
doing a blocking thread sleep. It's not so realistic as it doesn't saturate
any CPU or GPU resources. But it's enough to demonstrate a point.

Here's all the code for the model minus creating it:

```rust
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

#[derive(Debug, Deserialize, Serialize)]
pub struct Output {
    pub count: usize,
}

impl Model {
    #[instrument(skip_all)]
    pub fn infer(&self, data: &[f32]) -> anyhow::Result<Output> {
        let duration = Duration::from_secs_f32(data.len() as f32 / MODEL_SAMPLE_RATE as f32);

        let jitter = self.jitter * (fastrand::f32() * 2.0 - 1.0);
        let mut delay = duration.as_secs_f32() * self.constant_factor + self.delay + jitter;
        if self.warmup_penalty > 0.0 && FIRST_RUN.load(Ordering::Relaxed) {
            info!("First inference");
            FIRST_RUN.store(false, Ordering::Relaxed);
            delay += delay * self.warmup_penalty;
        }
        let delay = Duration::from_secs_f32(delay);

        sleep(delay);
        if fastrand::f32() < self.failure_rate {
            if fastrand::f32() < self.panic_rate {
                panic!("Inference catastrophically failed");
            } else {
                Ok(Output { count: data.len() })
            }
        } else {
            anyhow::bail!("Unexpected inference failure");
        }
    }
}
```

The first thing that probably jumps out is that we have this static `AtomicBool`
at the top called `FIRST_RUN`. A lot of neural network frameworks lazy-load the
model weights or have an optimisation step after some data is processed. Using
Tensorflow, I've seen the first inference be 10-15x slower than subsequent
inferences because of lazy loading.

Our calculated delay is roughly:

$$$
y = mx+c+rand(jitter)
$$$

This means it should linearly increase as the processed data gets longer with some
optional noise and a minimum inference time. We can see that this delay is added

