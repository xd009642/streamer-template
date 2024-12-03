# The Model

This entry will lean a lot on types from [streaming API design](01_streaming_api_design.md)
feel free to refer back if any of the types are unexpected.


As previously mentioned, this series won't integrate a real model. There's
plenty of them around and we care more about the API and engine side than the
model side. However, we still have a dummy model to help us replicate some
behaviour, and we need to discuss it before moving on.

In reality, these models are normally the slowest part of the system and a
bottleneck we have to get around. So with that in mind, I'll
do a blocking thread sleep. It's an unrealistic workload as it doesn't saturate
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

/// A fake stub model. This will be a model of only hyperparameters
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
            anyhow::bail!("Unexpected inference failure");
        } else {
            if fastrand::f32() < self.panic_rate {
                panic!("Inference catastrophically failed");
            } else {
                Ok(Output { count: data.len() })
            }
        }
    }
}
```

The first thing that probably jumps out is that we have this static `AtomicBool`
at the top called `FIRST_RUN`. Most neural network frameworks lazy-load the
model weights or have an optimisation step after some data is processed. Using
Tensorflow, I've seen the first inference be 10-15x slower than subsequent
inferences because of lazy loading.

Our calculated delay is roughly:

$$
y = mx+c+rand(jitter)
$$

This means it should linearly increase as the processed data gets longer with 
some optional noise and a minimum inference time. Additionally, the first
inference time is by default quite long to match behaviour observed in
the wild.

There's also, some likelihood of the inference panicking or failing
without a panic. Interfacing with neural network runtimes often involves an FFI
interface and GPUs. Both of these can cause issues for us, either in the event
of resource exhaustion, misconfiguration or woe forbid a dormant issue in the
library.

One final detail, I've derived `Deserialize` for this so a config file can be
used easily changing the behaviour.

## Passing Audio In

In a previous entry, we passed bytes in from the client to an audio decoding
module where audio is resampled and sent down another channel to run inference
on. Because of this abstraction, everything coming into our type holding the
model receives audio for the correct format and sample rate. With this
simplicity at our disposal, we can start to look at passing audio in unburdened
by earthly concerns.

At the top level, we need something to load the model and also pass in the audio
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
potential naming conflicts - `anyhow` has a `Context` trait and other crates
do as well.

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
                    }
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
                            Event::Error {
                                message: e.to_string()
                            }
                        }
                        Some(Err(e)) => {
                            error!(error=$e, "Inference panicked");
                            Event::Error {
                                message: "Internal server error".to_string()
                            }
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

Some people might want to exit on an error, but for this initial implementation, we'll
send back an error message and continue processing. Another thing to
note is the usage of `FuturesOrdered`. While there may be more efficient ways
to do this for now we'll be relying on this because it is a fairly simple and
convenient way to run multiple futures concurrently and get the responses in
order. Additionally, `max_futures` is only done on a per-request basis, if we
wanted it as a global limit we'd have to look to something like a semaphore.

When the audio isn't receiving and there are no running inference futures the
while loop terminates and we're home and dry. This code has been made so
simple through the use of channels. These can come with some performance
implications, but they should generally perform well enough initially. _In a
future entry I'll cover some tips to get the most speed out of them._

The next stage will be the voice-segmented runner which is a ton more complex.
This might be complex enough to warrant its own subheading!

## The Segmented API

The VAD segmented API will have more places we can call inference, we'll be
calling it on the end of an utterance and also if interim results are desired
at regular intervals. Additionally, with how the VAD library works we'll
potentially have an active speech segment at the end and need to run a final
inference on it after we get a stop request.

We also won't be running inferences in parallel because we won't run at
a fixed rate. With this in mind, I'll be creating a new method called
`spawned_inference` which the segmented runner can call every time it wants to
perform an inference.

```rust
async fn spawned_inference(
    &self,
    audio: Vec<f32>,
    bounds_ms: Option<(usize, usize)>,
    is_final: bool,
) -> Event {
    let temp_model = self.model.clone();
    let result = task::spawn_blocking(move || temp_model.infer(&audio)).await;

    match result {
        Ok(Ok(output)) => {
            if let Some((start, end)) = bounds_ms {
                let start_time = start as f32 / 1000.0;
                let end_time = end as f32 / 1000.0;
                let seg = SegmentOutput {
                    start_time,
                    end_time,
                    is_final: Some(is_final),
                    output,
                };
                Event::Segment(seg)
            } else {
                Event::Data(output)
            }
        }
        Ok(Err(e)) => {
            error!("Failed inference event: {}", e);
            Event::Error {
                message: e.to_string(),
            }
        }
        Err(e) => {
            error!(error=%e, "Inference panicked");
            Event::Error {
                message: "Internal server error".to_string()
            }
        }
    }
}
```

There is potential here to use a `Duration` instead of `usize` for the time
tracking. But as we work it out from samples and our resolution will be
limited by the interim response interval a `usize` works well enough and saves
some type conversion effort.

We can see this is fairly similar to the simple runner, and while we could work
to refactor our simple runner to use this method it wouldn't be desirable. A
future typically only progresses when `.await` is called on it. And for
futures in an `FuturesOrdered` that will be when we poll the `FuturesUnordered`.
Whereas, the futures returned by `spawn` and `spawn_blocking` will start running
before being polled. Because of this refactoring would add a delay to when our
first task is spawned and add latency into the system.

For now, let's do an initial pass at the implementation this first version won't
be fully featured. Initially, we'll skip:

1. Events (speech start/end emitting)
2. Partial Inferences

Additionally, for the VAD we'll be using an opinionated version of
[silero](https://github.com/snakers4/silero-vad). This is a project open-sourced
by my employer, Emotech and can be found [here](https://github.com/emotechlab/silero-rs).

The Silero crate will hold onto the audio buffer, you pass it slices and it
will push it onto an internal queue. Active speech can be accessed with 
`VadSession::get_current_speech` and each process can return a `Vec` of events
containing the speech starts and ends. The ending speech has the samples contained
within so they can be removed from the internal buffer as well. 

With that short introduction to our new dependency here's the initial code:

```rust
pub async fn segmented_runner(
    self: Arc<Self>,
    _settings: StartMessage,
    channel: usize,
    mut inference: mpsc::Receiver<Vec<f32>>,
    output: mpsc::Sender<ApiResponse>,
) -> anyhow::Result<()> {
    let mut vad = VadSession::new(VadConfig::default())?;
    let mut still_receiving = true;

    // Need to test and prove this doesn't lose any data!
    while let Some(audio) = inference.recv().await {
        let mut events = vad.process(&audio)?;

        for event in events.drain(..) {
            match event {
                VadTransition::SpeechStart { timestamp_ms } => {
                    todo!()
                }
                VadTransition::SpeechEnd {
                    start_timestamp_ms,
                    end_timestamp_ms,
                    samples,
                } => {
                    info!(time_ms = end_timestamp_ms, "Detected end of speech");
                    let data = self
                        .spawned_inference(
                            samples,
                            Some((start_timestamp_ms, end_timestamp_ms)),
                            true,
                        )
                        .await;
                    let msg = ApiResponse { channel, data };
                    output
                        .send(msg)
                        .await
                        .context("Failed to send inference result")?;
                }
            }
        }
    }

    // If we're speaking then we haven't endpointed so do the final inference
    if vad.is_speaking() {
        let audio = vad.get_current_speech().to_vec();
        info!(session_time=?vad.session_time(), current_duration=?vad.current_speech_duration(), "vad state");
        let current_start =
            (vad.session_time() - vad.current_speech_duration()).as_millis() as usize;
        let current_end = session_time.as_millis() as usize;
        let data = self
            .spawned_inference(audio, Some((current_start, current_end)), true)
            .await;
        let msg = ApiResponse { channel, data };
        output
            .send(msg)
            .await
            .context("Failed to send final inference")?;
    }

    info!("Inference finished");
    Ok(())
}
```

So how do we go about adding events? Simple just emit them when we get our speech start/end! So
our match becomes:

```rust
match event {
    VadTransition::SpeechStart { timestamp_ms } => {
        info!(time_ms = timestamp_ms, "Detected start of speech");
        let msg = ApiResponse {
            data: Event::Active {
                time: timestamp_ms as f32 / 1000.0,
            },
            channel,
        };
        output
            .send(msg)
            .await
            .context("Failed to send vad active event")?;
    }
    VadTransition::SpeechEnd {
        start_timestamp_ms,
        end_timestamp_ms,
        samples,
    } => {
        info!(time_ms = end_timestamp_ms, "Detected end of speech");
        let msg = ApiResponse {
            data: Event::Inactive {
                time: end_timestamp_ms as f32 / 1000.0,
            },
            channel,
        };
        // We'll send the inactive message first because it should be faster to
        // send
        output
            .send(msg)
            .await
            .context("Failed to send vad inactive event")?;

        let data = self
            .spawned_inference(
                samples,
                Some((start_timestamp_ms, end_timestamp_ms)),
                true,
            )
            .await;
        let msg = ApiResponse { channel, data };
        output
            .send(msg)
            .await
            .context("Failed to send inference result")?;
    }
}
```

Also, we can't forget the final inference of any audio that's still not finished speaking. 

```rust
    // If we're speaking then we haven't endpointed so do the final inference
    if vad.is_speaking() {
        let session_time = vad.session_time();
        let msg = ApiResponse {
            data: Event::Inactive {
                time: session_time.as_secs_f32(),
            },
            channel,
        };
        output
            .send(msg)
            .await
            .context("Failed to send end of audio inactive event")?;
        let audio = vad.get_current_speech().to_vec();
        info!(session_time=?vad.session_time(), current_duration=?vad.current_speech_duration(), "vad state");
        let current_start =
            (vad.session_time() - vad.current_speech_duration()).as_millis() as usize;
        let current_end = session_time.as_millis() as usize;
        let data = self
            .spawned_inference(audio, Some((current_start, current_end)), true)
            .await;
        let msg = ApiResponse { channel, data };
        output
            .send(msg)
            .await
            .context("Failed to send final inference")?;
    }
```

Wow, this event stuff is easy. Okay, now onto the interim results and we'll be
done with this code. We'll add two variables at the top of the function:

```rust
let mut last_inference_time = Duration::from_millis(0);
// So we're not allowing this to be configured via API. Instead we're setting it to the
// equivalent of every 500ms.
const INTERIM_THRESHOLD: Duration = Duration::from_millis(500);
```

We want to track when we last did an interfence so our interim result doesn't
come too early or too late. We also define our constant interim duration - this
could be part of the API but we don't want to put extra work into making sure
users don't crash our system with insane values. Remember the model is expected
to be computationally intense and will take up some amount of compute so setting
unreasonably low times to run it is an easy way to bring down a service!

We'll store the value from `VadTransition::SpeechEnd::end_timestamp_ms` in our
`last_inference_time` then after the code that processes our VAD events we want
to put the partial processing:

```rust
for event in events.drain(..) {
    // You've seen this code above!
}

let session_time = vad.session_time();
if vad.is_speaking() && (session_time - last_inference_time) >= INTERIM_THRESHOLD {
    last_inference_time = vad.session_time();
    info!(session_time=?vad.session_time(), current_duration=?vad.current_speech_duration(), "vad state");
    let current_start =
        (vad.session_time() - vad.current_speech_duration()).as_millis() as usize;
    let current_end = session_time.as_millis() as usize;
    let data = self
        .spawned_inference(
            vad.get_current_speech().to_vec(),
            Some((current_start, current_end)),
            false,
        )
        .await;
    let msg = ApiResponse { channel, data };
    output
        .send(msg)
        .await
        .context("Failed to send partial inference")?;
}
```

And that's all there is to it. When all this is put together the function is
quite long but I've kept it together instead of making some smaller functions
that are only used in one place. This might change in future though.

## Conclusion

Well here we are, we've got a fake model and inferencing code to call it for our
planned APIs. Now we need to cover the Axum API we'll make and then we'll
have a fully working system and can start to dive into more depth on various
parts of the system.
