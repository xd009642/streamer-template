# The Model

This entry will lean a lot on types from [streaming API design](01_streaming_api_design.md)
feel free to refer back if any of the types are unexpected.


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

This means it should linearly increase as the processed data gets longer with 
some optional noise and a minimum inference time. Additionally, the first
inference time is by default quite long to match behaviour that can be seen in
the wild.

There's also, some likelihoods for the inference panicking or just failing
without a panic. Interfacing with neural network runtimes often involves an FFI
interface and GPUs. Both of these can cause issues for us, either in the event
of resource exhaustion, misconfiguration or woe forbid dormant issues in the
library.

One final detail, I've derived `Deserialize` for this so a config file can be
bundled in to easily change the behaviour.

## Passing Audio In

In a previous entry we passed bytes in from the client to an audio decoding
module where audio is resampled and sent down another channel to run inference
on. Because of this abstraction everything coming into our type holding the
model receives audio for the correct format and sample rate. With this
simplicity at our disposal we can start to look at passing audio in unburdened
by earthly concerns.

At the top level we need something to load the model and also pass in the audio
received from the API. Often people will give this a super descriptive name like
`Context` or `State`, but here we're going to be more descriptive:

```rust
/// Streaming context. This holds a handle to the model as well as
/// potentially some parameters to control usage and how things are split up
pub struct StreamingContext {
    model: Model,
    max_futures: usize,
}
```

Oh that's not actually much more descriptive, but at least it'll avoid any
potential naming conflicts - anyhow has a `Context` trait and other crates
probably do as well.

Now as previously mentioned, we'll have two different inference APIs, one that
processes all the audio and one which processes only voiced segments. Let's
start with the simple one first.

```rust
impl StreamingContext {

    /// This is the simple inference where every part of the audio is processed by the model
    /// regardless of speech content being present or not
    #[instrument(skip_all)]
    pub async fn inference_runner(
        self: Arc<Self>,
        channel: usize,
        mut inference: mpsc::Receiver<Arc<Vec<f32>>>,
        output: mpsc::Sender<ApiResponse>,
    ) -> anyhow::Result<()> {
        let mut runners = FuturesOrdered::new();
        let mut still_receiving = true;
        let mut received_results = 0;
        let mut received_data = 0;

        let mut current_start = 0.0;
        let mut current_end = 0.0;

        // Need to test and prove this doesn't lose any data!
        while still_receiving || !runners.is_empty() {
            tokio::select! {
                audio = inference.recv(), if still_receiving && runners.len() < self.max_futures => {
                    if let Some(audio) = audio {
                        let temp_model = self.model.clone();
                        current_end += audio.len() as f32/ MODEL_SAMPLE_RATE as f32;
                        let bound_ms = (current_start, current_end);
                        runners.push_back(task::spawn_blocking(move || {
                            (bound_ms, temp_model.infer(&audio))
                        });
                        current_start = current_end;
                    } else {
                        still_receiving = false;
                    {
                }
                data = runners.next(), if !runners.is_empty() => {
                    received_results += 1;
                    debug!("Received inference result: {}", received_results);
                    let data = match data {
                        Some(Ok(((start_time, end_time), Ok(output)))) => {
                            let segment = SegmentOutput {
                                start_time,
                                end_time,
                                is_final: None,
                                output
                            };
                            Event::Segment(segment)
                        },
                        Some(Ok(((start_time, end_time), Err(e)))) => {
                            error!("Failed inference event {}-{}: {}", start_time, end_time, e);
                            Event::Error(e.to_string())
                        }
                        Some(Err(e)) => {
                            error!(error=$e, "Inference panicked");
                            Event::Error("Internal server error".to_string())
                        },
                        None => {
                            continue;
                        }
                    };
                    let msg = ApiResponse {
                        channel,
                        data
                    };
                    output.send(msg).await?;
                }
            }
        }
        info!("Inference finished");
        Ok(())
    }
}
```

The way this task works is we create two futures in the select which:

1. Receives audio if there's room in the runners and spawns an inference task
2. Collects the finished inference results and forwards them or errors to the client

Some people might want to exit on an error, but for this initial one I
just send back an error message and continue on with my day. Another thing to
note is the usage of `FuturesOrdered`. While there may be more efficient ways
to do this for now we'll be relying on this because it is a fairly simple and
convienent way to run multiple futures concurrently and get the responses in
order.

When the audio isn't receiving and there are no running inference futures the
while loop terminates and we're home and dry. This code has been made so
simple through the use of channels. These can come with some performance
implications, but they should generally perform well enough initially. _In a
future entry I'll cover some tips to get the most speed out of them._

The next stage will be the voice segmented runner which is a ton more complex.
In fact, this might be complex enough to warrant it's own subheading!

# The Segmented API

TODO this will get a big rewrite when the new API drops so I should wait on it
before writing too much!

TODO do we want to use `spawned_inference` on the simple runner?
