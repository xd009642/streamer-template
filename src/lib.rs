use crate::api_types::{ApiResponse, Event, SegmentOutput, StartMessage};
use crate::model::Model;
use futures::{stream::FuturesOrdered, StreamExt};
use silero::*;
use std::sync::Arc;
use std::{thread, time::Duration};
use tokio::sync::mpsc;
use tokio::task;
use tracing::{debug, error, info, info_span, instrument, warn, Span};

pub type AudioChannel = Arc<Vec<f32>>;

pub mod api_types;
mod audio;
pub mod axum_server;
pub mod metrics;
pub mod model;

pub async fn launch_server() {
    let ctx = Arc::new(StreamingContext::new());
    info!("Launching server");
    axum_server::run_axum_server(ctx)
        .await
        .expect("Failed to launch server");
    info!("Server exiting");
}

#[derive(Clone)]
pub struct StreamingContext {
    model: Model,
    min_data: usize,
    max_futures: usize,
}

impl Default for StreamingContext {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamingContext {
    pub fn new() -> Self {
        let max_futures = thread::available_parallelism()
            .map(|x| x.get())
            .unwrap_or(4);
        Self {
            model: Model::default(),
            min_data: 512,
            max_futures,
        }
    }

    pub fn new_with_model(model: Model) -> Self {
        let max_futures = thread::available_parallelism()
            .map(|x| x.get())
            .unwrap_or(4);
        Self {
            model,
            min_data: 512,
            max_futures,
        }
    }

    #[instrument(skip_all)]
    pub fn should_run_inference(&self, data: &[f32], is_last: bool) -> bool {
        data.len() >= self.min_data || (is_last && !data.is_empty())
    }

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
                        current_end += audio.len() as f32/16000.0;
                        let bound_ms = (current_start, current_end);
                        runners.push_back(task::spawn_blocking(move || {
                            let span = info_span!(parent: &current, "inference_task");
                            let _guard = span.enter();
                            (bound_ms, temp_model.infer(&audio))
                        }));
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
                            Event::Error(e.to_string())
                        }
                        Some(Err(_)) => unreachable!("Spawn blocking cannot error"),
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

    #[instrument(skip_all)]
    pub async fn segmented_runner(
        self: Arc<Self>,
        settings: StartMessage,
        channel: usize,
        mut inference: mpsc::Receiver<Arc<Vec<f32>>>,
        output: mpsc::Sender<ApiResponse>,
    ) -> anyhow::Result<()> {
        let mut vad = VadSession::new(VadConfig::default())?;
        let mut still_receiving = true;

        let mut recv_buffer = Vec::with_capacity(inference.max_capacity());

        let mut current_start = None;
        let mut current_end = None;
        let mut dur_since_inference = Duration::from_millis(0);
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
                let events = vad.process(&audio)?;

                let mut found_endpoint = false;
                let mut last_segment = None;
                for event in &events {
                    match event {
                        VadTransition::SpeechStart { timestamp_ms } => {
                            info!(time_ms = timestamp_ms, "Detected start of speech");
                            match (current_start, current_end) {
                                (Some(start), Some(end)) if found_endpoint => {
                                    if last_segment.is_some() {
                                        // More than 2x start/end pairs found in a single chunk.
                                        // Something is going wrong!
                                        error!("Found another endpoint but already had a last segment! Losing segment {:?}", last_segment);
                                    }
                                    last_segment = Some((start, end));
                                }
                                (None, _) if found_endpoint => {
                                    error!("Found an endpoint with no start time");
                                }
                                _ => {}
                            }
                            current_start = Some(*timestamp_ms);
                            current_end = None;
                            found_endpoint = false;
                            let msg = ApiResponse {
                                data: Event::Active {
                                    time: *timestamp_ms as f32 / 1000.0,
                                },
                                channel,
                            };
                            if output.send(msg).await.is_err() {
                                error!("Failed to send vad active event");
                                anyhow::bail!("Output channel closed");
                            }
                        }
                        VadTransition::SpeechEnd { timestamp_ms } => {
                            info!(time_ms = timestamp_ms, "Detected end of speech");
                            current_end = Some(*timestamp_ms);
                            found_endpoint = true;
                            let msg = ApiResponse {
                                data: Event::Inactive {
                                    time: *timestamp_ms as f32 / 1000.0,
                                },
                                channel,
                            };
                            if output.send(msg).await.is_err() {
                                error!("Failed to send vad inactive event");
                                anyhow::bail!("Output channel closed");
                            }
                        }
                    }
                }
                let current_vad_dur = vad.current_speech_duration();
                if last_segment.is_none()
                    && settings.interim_results
                    && current_vad_dur > (dur_since_inference + INTERIM_THRESHOLD)
                {
                    dur_since_inference = current_vad_dur;
                    let session_time = vad.session_time();
                    let audio = vad.get_current_speech().to_vec();
                    // So here we could do a bit of faffing to not block on this inference to keep
                    // things running but for now we're going to limit each request to a maximum of
                    // N_CHANNELS concurrent inferences.
                    let data = self
                        .spawned_inference(
                            audio,
                            current_start.zip(Some(session_time.as_millis() as usize)),
                            false,
                        )
                        .await;
                    let msg = ApiResponse { channel, data };
                    output.send(msg).await?;
                }

                if let Some((start, end)) = last_segment {
                    let audio = vad.get_speech(start, Some(end)).to_vec();
                    let data = self
                        .spawned_inference(audio, Some((start, end)), true)
                        .await;
                    let msg = ApiResponse { channel, data };
                    output.send(msg).await?;
                    dur_since_inference = Duration::from_millis(0);
                }

                if found_endpoint {
                    // We actually don't need the start/end if we've got an endpoint!
                    let audio = vad.get_current_speech().to_vec();
                    let data = self
                        .spawned_inference(audio, current_start.zip(current_end), true)
                        .await;
                    let msg = ApiResponse { channel, data };
                    output.send(msg).await?;
                    dur_since_inference = Duration::from_millis(0);
                    current_start = None;
                    current_end = None;
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
            if output.send(msg).await.is_err() {
                error!("Failed to send end of audio inactive event");
                anyhow::bail!("Output channel closed");
            }
            let audio = vad.get_current_speech().to_vec();
            let data = self
                .spawned_inference(
                    audio,
                    current_start.zip(Some(session_time.as_millis() as usize)),
                    true,
                )
                .await;
            let msg = ApiResponse { channel, data };
            output.send(msg).await?;
        }

        info!("Inference finished");
        Ok(())
    }

    async fn spawned_inference(
        &self,
        audio: Vec<f32>,
        bounds_ms: Option<(usize, usize)>,
        is_final: bool,
    ) -> Event {
        let current = Span::current();
        let temp_model = self.model.clone();
        let result = task::spawn_blocking(move || {
            let span = info_span!(parent: &current, "inference_task");
            let _guard = span.enter();
            temp_model.infer(&audio)
        })
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
                Event::Error(e.to_string())
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
            min_data: 10,
            max_futures: 4,
        });

        let inference = context.inference_runner(1, input_rx, output_tx);

        let sender = task::spawn(async move {
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

        let receiver = task::spawn(async move {
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
        let model = Model::flaky(1.0);

        let (input_tx, input_rx) = mpsc::channel(10);
        let (output_tx, mut output_rx) = mpsc::channel(10);

        let context = Arc::new(StreamingContext {
            model,
            min_data: 10,
            max_futures: 4,
        });

        let inference = context.inference_runner(0, input_rx, output_tx);

        let sender = task::spawn(async move {
            for _ in 0..100 {
                let to_send = (0..10).map(|x| x as f32).collect::<Vec<_>>();

                input_tx.send(Arc::new(to_send)).await.unwrap();
            }
            info!("Finished sender task");
        });

        let receiver = task::spawn(async move {
            let mut received_errors = 0;
            while let Some(msg) = output_rx.recv().await {
                assert_eq!(msg.channel, 0);
                match msg.data {
                    Event::Error(_e) => {
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
