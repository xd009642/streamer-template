#![deny(clippy::disallowed_methods)]
use crate::api_types::{ApiResponse, Event, SegmentOutput, StartMessage};
use crate::metrics::{get_panic_counter, RtfMetric, RtfMetricGuard, Subsystem};
use crate::model::Model;
use anyhow::Context;
use futures::{stream::FuturesOrdered, StreamExt};
use serde::Deserialize;
use silero::*;
use std::sync::Arc;
use std::{thread, time::Duration};
use tokio::{fs, sync::mpsc};
use tracing::{debug, error, info, info_span, instrument, Span};

pub type AudioChannel = Arc<Vec<f32>>;

pub mod api_types;
mod audio;
pub mod axum_server;
pub mod metrics;
pub mod model;
pub mod task;

pub use crate::model::MODEL_SAMPLE_RATE;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    model: Model,
}

pub async fn launch_server() {
    let config = fs::read("config.json")
        .await
        .expect("Couldn't read server config");
    let config = serde_json::from_slice(&config).expect("Couldn't deserialize config");
    info!(config=?config, "service config loaded");
    let ctx = Arc::new(StreamingContext::new_with_config(config));
    info!("Launching server");
    axum_server::run_axum_server(ctx)
        .await
        .expect("Failed to launch server");
    info!("Server exiting");
}

/// Streaming context. This holds a handle to the model as well as
/// potentially some parameters to control usage and how things are split up
pub struct StreamingContext {
    model: Model,
    max_futures: usize,
}

impl Default for StreamingContext {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamingContext {
    pub fn new() -> Self {
        Self::new_with_config(Config::default())
    }

    /// Creates a new context with default parameters
    pub fn new_with_config(config: Config) -> Self {
        let max_futures = thread::available_parallelism()
            .map(|x| x.get())
            .unwrap_or(4);
        Self {
            model: config.model,
            max_futures,
        }
    }

    /// Creates a new one with a given model
    pub fn new_with_model(model: Model) -> Self {
        let max_futures = thread::available_parallelism()
            .map(|x| x.get())
            .unwrap_or(4);
        Self { model, max_futures }
    }

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

        let mut recv_buffer = Vec::with_capacity(inference.max_capacity());

        let mut current_start = 0.0;
        let mut current_end = 0.0;

        // Need to test and prove this doesn't lose any data!
        while still_receiving || !runners.is_empty() {
            tokio::select! {
                msg_len = inference.recv_many(&mut recv_buffer, inference.max_capacity()), if still_receiving && runners.len() < self.max_futures => {
                    if msg_len == 0 {
                        info!("No longer receiving any messages");
                        still_receiving = false;
                    }
                    else {
                        received_data += msg_len;
                        let mut audio = vec![];
                        for samples in recv_buffer.drain(..) {
                            audio.extend_from_slice(&samples);
                        }
                        debug!(received_data=received_data, batch_size=msg_len, "Adding to inference runner task");
                        let temp_model = self.model.clone();
                        let current = Span::current();
                        current_end += audio.len() as f32/ MODEL_SAMPLE_RATE as f32;
                        let bound_ms = (current_start, current_end);
                        runners.push_back(task::spawn_blocking(move || {
                            let span = info_span!(parent: &current, "inference_task");
                            let _guard = span.enter();
                            (bound_ms, temp_model.infer(&audio))
                        }, get_panic_counter(Subsystem::Inference)));
                        current_start = current_end;
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
                            error!(error=%e, "Inference panicked");
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

    /// Splits apart an audio file by speech content and runs the model just on the parts
    /// containing speech.
    #[instrument(skip_all)]
    pub async fn segmented_runner(
        self: Arc<Self>,
        _settings: StartMessage,
        channel: usize,
        mut inference: mpsc::Receiver<Arc<Vec<f32>>>,
        output: mpsc::Sender<ApiResponse>,
    ) -> anyhow::Result<()> {
        let mut vad = VadSession::new(VadConfig::default())?;
        let mut still_receiving = true;

        let mut recv_buffer = Vec::with_capacity(inference.max_capacity());

        let mut last_inference_time = Duration::from_millis(0);
        // So we're not allowing this to be configured via API. Instead we're setting it to the
        // equivalent of every 500ms.
        const INTERIM_THRESHOLD: Duration = Duration::from_millis(500);

        // Need to test and prove this doesn't lose any data!
        while still_receiving {
            let msg_len = inference
                .recv_many(&mut recv_buffer, inference.max_capacity())
                .await;
            if msg_len == 0 {
                info!("No longer receiving any messages");
                still_receiving = false;
            } else {
                let mut audio = vec![];
                for samples in recv_buffer.drain(..) {
                    audio.extend_from_slice(&samples);
                }
                let duration =
                    Duration::from_secs_f32(audio.len() as f32 / MODEL_SAMPLE_RATE as f32);
                let guard = RtfMetricGuard::new(duration, RtfMetric::Vad);
                let mut events = vad.process(&audio)?;
                std::mem::drop(guard);

                for event in events.drain(..) {
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
                            last_inference_time = Duration::from_millis(end_timestamp_ms as u64);
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
                }

                // Okay here if the vad is speaking then we're in a segment and we should look at
                // the time of the last inference and our interim threshold to see if we should do
                // another response. Otherwise, we have nothing to do! Because ending segment
                // inferences are dealt with when processing the vad events
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
            }
        }

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

        info!("Inference finished");
        Ok(())
    }

    /// Spawns an inference and generates a response object.
    async fn spawned_inference(
        &self,
        audio: Vec<f32>,
        bounds_ms: Option<(usize, usize)>,
        is_final: bool,
    ) -> Event {
        let current = Span::current();
        let temp_model = self.model.clone();
        let result = task::spawn_blocking(
            move || {
                let span = info_span!(parent: &current, "inference_task");
                let _guard = span.enter();
                temp_model.infer(&audio)
            },
            get_panic_counter(Subsystem::Inference),
        )
        .await;
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
            Err(_) => unreachable!("Spawn blocking cannot error"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Output;
    use tracing_test::traced_test;

    #[tokio::test]
    #[traced_test]
    async fn doesnt_lose_data() {
        let model = Model::speedy();

        let (input_tx, input_rx) = mpsc::channel(10);
        let (output_tx, mut output_rx) = mpsc::channel(10);

        let context = Arc::new(StreamingContext {
            model,
            max_futures: 4,
        });

        let inference = context.inference_runner(1, input_rx, output_tx);

        let sender = tokio::task::spawn(async move {
            let mut bytes_sent = 0;
            for _ in 0..100 {
                let data = fastrand::u8(5..);
                bytes_sent += data as usize;
                let to_send = (0..data).map(|x| x as f32).collect::<Vec<_>>();

                input_tx.send(Arc::new(to_send)).await.unwrap();
            }
            info!("Finished sender task");
            bytes_sent
        });

        let receiver = tokio::task::spawn(async move {
            let mut received = 0;
            while let Some(msg) = output_rx.recv().await {
                assert_eq!(msg.channel, 1);
                match msg.data {
                    Event::Data(Output { count }) => {
                        received += count;
                    }
                    Event::Segment(SegmentOutput { output, .. }) => {
                        received += output.count;
                    }
                    e => panic!("Unexpected: {:?}", e),
                }
            }
            info!("Finished receiver task");
            received
        });

        let (bytes_sent, count_received, run) = tokio::join!(sender, receiver, inference);

        run.unwrap();
        assert_eq!(bytes_sent.unwrap(), count_received.unwrap());
        assert!(logs_contain("Adding to inference runner task"));
        assert!(logs_contain("Inference finished"));
    }

    #[tokio::test]
    #[traced_test]
    async fn broken_model() {
        let model = Model::flaky(1.0, 0.0);

        let (input_tx, input_rx) = mpsc::channel(10);
        let (output_tx, mut output_rx) = mpsc::channel(10);

        let context = Arc::new(StreamingContext {
            model,
            max_futures: 4,
        });

        let inference = context.inference_runner(0, input_rx, output_tx);

        let sender = tokio::task::spawn(async move {
            for _ in 0..100 {
                let to_send = (0..10).map(|x| x as f32).collect::<Vec<_>>();

                input_tx.send(Arc::new(to_send)).await.unwrap();
            }
            info!("Finished sender task");
        });

        let receiver = tokio::task::spawn(async move {
            let mut received_errors = 0;
            while let Some(msg) = output_rx.recv().await {
                assert_eq!(msg.channel, 0);
                match msg.data {
                    Event::Error { .. } => {
                        received_errors += 1;
                    }
                    _ => {
                        panic!("Didn't expect actual messages back!");
                    }
                }
            }
            info!("Finished receiver task");
            received_errors
        });

        let (_, count_received, run) = tokio::join!(sender, receiver, inference);

        run.unwrap();
        assert!(count_received.unwrap() > 1);
        assert!(logs_contain("Adding to inference runner task"));
        assert!(logs_contain("Failed inference event"));
    }
}
