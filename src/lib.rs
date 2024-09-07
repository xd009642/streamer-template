use crate::api_types::{Event, SegmentOutput};
use crate::model::Model;
use futures::{stream::FuturesOrdered, StreamExt};
use silero::*;
use std::sync::Arc;
use std::thread;
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
        mut inference: mpsc::Receiver<Arc<Vec<f32>>>,
        output: mpsc::Sender<Event>,
    ) -> anyhow::Result<()> {
        let mut runners = FuturesOrdered::new();
        let mut still_receiving = true;
        let mut received_results = 0;
        let mut received_data = 0;

        let mut recv_buffer = Vec::with_capacity(inference.max_capacity());

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
                        runners.push_back(task::spawn_blocking(move || {
                            let span = info_span!(parent: &current, "inference_task");
                            let _guard = span.enter();
                            temp_model.infer(&audio)
                        }));
                    }
                }
                data = runners.next(), if !runners.is_empty() => {
                    received_results += 1;
                    debug!("Received inference result: {}", received_results);
                    let msg = match data {
                        Some(Ok(Ok(output))) => Event::Data(output),
                        Some(Ok(Err(e))) => {
                            error!("Failed inference event: {}", e);
                            Event::Error(e.to_string())
                        }
                        Some(Err(_)) => unreachable!("Spawn blocking cannot error"),
                        None => {
                            continue;
                        }
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
        mut inference: mpsc::Receiver<Arc<Vec<f32>>>,
        output: mpsc::Sender<Event>,
    ) -> anyhow::Result<()> {
        let mut vad = VadSession::new(VadConfig::default())?;
        let mut still_receiving = true;

        let mut recv_buffer = Vec::with_capacity(inference.max_capacity());

        let mut current_start = None;
        let mut current_end = None;

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
                        }
                        VadTransition::SpeechEnd { timestamp_ms } => {
                            info!(time_ms = timestamp_ms, "Detected end of speech");
                            current_end = Some(*timestamp_ms);
                            found_endpoint = true;
                        }
                    }
                }

                if let Some((start, end)) = last_segment {
                    let audio = vad.get_speech(start, Some(end)).to_vec();
                    let msg = self.spawned_inference(audio, Some((start, end))).await;
                    output.send(msg).await?;
                }

                if found_endpoint {
                    // We actually don't need the start/end if we've got an endpoint!
                    let audio = vad.get_current_speech().to_vec();
                    let msg = self
                        .spawned_inference(audio, current_start.zip(current_end))
                        .await;
                    output.send(msg).await?;
                    current_start = None;
                    current_end = None;
                }
            }
        }

        // If we're speaking then we haven't endpointed so do the final inference
        if vad.is_speaking() {
            let audio = vad.get_current_speech().to_vec();
            let msg = self
                .spawned_inference(
                    audio,
                    current_start.zip(Some(vad.session_time().as_millis() as usize)),
                )
                .await;
            output.send(msg).await?;
        }

        info!("Inference finished");
        Ok(())
    }

    async fn spawned_inference(&self, audio: Vec<f32>, bounds_ms: Option<(usize, usize)>) -> Event {
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
                        is_final: Some(true),
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

        let inference = context.inference_runner(input_rx, output_tx);

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
                match msg {
                    Event::Data(Output { count }) => {
                        received += count;
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

        let inference = context.inference_runner(input_rx, output_tx);

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
                match msg {
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
