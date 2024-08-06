use crate::model::{Model, Output};
use futures::{stream::FuturesOrdered, StreamExt};
use std::sync::Arc;
use std::thread;
use tokio::sync::mpsc;
use tokio::task;
use tracing::{debug, error, info, info_span, instrument, warn, Span};

pub type AudioChannel = Arc<Vec<f32>>;

pub mod api_types;
mod audio;
mod axum_server;
pub mod model;

pub async fn launch_server() {
    let ctx = Arc::new(StreamingContext::new());
    info!("Launching server");
    axum_server::run_axum_server(ctx)
        .await
        .expect("Failed to launch server");
    info!("Server exiting");
}

pub enum InputEvent {
    Start,
    Data(Arc<Vec<f32>>),
    Stop,
}

pub enum OutputEvent {
    Response(Output),
    ModelError(String),
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
        output: mpsc::Sender<OutputEvent>,
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
                        Some(Ok(Ok(output))) => OutputEvent::Response(output),
                        Some(Ok(Err(e))) => {
                            error!("Failed inference event: {}", e);
                            OutputEvent::ModelError(e.to_string())
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
    pub async fn simple(
        self: Arc<Self>,
        mut input: mpsc::Receiver<InputEvent>,
        output: mpsc::Sender<OutputEvent>,
    ) -> anyhow::Result<()> {
        let mut data_store = Vec::new();
        let mut is_running = false;

        let (tx, rx) = mpsc::channel(self.max_futures);
        let other_self = self.clone();
        let inf_task = task::spawn(async move {
            if let Err(e) = other_self.inference_runner(rx, output).await {
                error!("Inference failed: {}", e);
            }
        });

        while let Some(msg) = input.recv().await {
            match msg {
                InputEvent::Start => {
                    debug!("Received start message");
                    is_running = true;
                }
                InputEvent::Stop => {
                    debug!("Received stop message");
                    is_running = false;
                }
                InputEvent::Data(bytes) => {
                    debug!("Receiving data len: {}", bytes.len());
                    if is_running {
                        // Here we should probably actually do a filtering of the data and push it
                        data_store.extend(bytes.iter());
                    } else {
                        warn!("Data sent when in stop mode. Discarding");
                    }
                }
            }

            // Check if we want to do an inference
            if self.should_run_inference(&data_store, false) {
                tx.send(data_store.into()).await?;
                data_store = Vec::new();
            }
        }

        // Check if we want to do an inference
        if self.should_run_inference(&data_store, true) {
            tx.send(data_store.into()).await?;
        } else {
            // Do any final message stuff!
        }
        std::mem::drop(tx);

        let end = inf_task.await;
        info!("Finished inference: {:?}", end);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

        let inference = context.simple(input_rx, output_tx);

        let sender = task::spawn(async move {
            let mut bytes_sent = 0;
            input_tx.send(InputEvent::Start).await.unwrap();
            for _ in 0..100 {
                let data = fastrand::u8(5..);
                bytes_sent += data as usize;
                let to_send = (0..data).map(|x| x as f32).collect::<Vec<_>>();

                input_tx
                    .send(InputEvent::Data(Arc::new(to_send)))
                    .await
                    .unwrap();
            }
            input_tx.send(InputEvent::Stop).await.unwrap();
            info!("Finished sender task");
            bytes_sent
        });

        let receiver = task::spawn(async move {
            let mut received = 0;
            while let Some(msg) = output_rx.recv().await {
                match msg {
                    OutputEvent::Response(Output { count }) => {
                        received += count;
                    }
                    OutputEvent::ModelError(e) => panic!("{}", e),
                }
            }
            info!("Finished receiver task");
            received
        });

        let (bytes_sent, count_received, run) = tokio::join!(sender, receiver, inference);

        run.unwrap();
        assert_eq!(bytes_sent.unwrap(), count_received.unwrap());
        assert!(logs_contain("Received start message"));
        assert!(logs_contain("Received stop message"));
        assert!(logs_contain("Inference finished"));
    }

    #[tokio::test]
    #[traced_test]
    async fn no_start() {
        let (input_tx, input_rx) = mpsc::channel(10);
        let (output_tx, mut output_rx) = mpsc::channel(10);

        let context = Arc::new(StreamingContext::new());

        let inference = context.simple(input_rx, output_tx);

        let sender = task::spawn(async move {
            let mut bytes_sent = 0;
            for _ in 0..100 {
                let data = fastrand::u8(5..);
                bytes_sent += data as usize;
                let to_send = (0..data).map(|x| x as f32).collect::<Vec<_>>();

                input_tx
                    .send(InputEvent::Data(Arc::new(to_send)))
                    .await
                    .unwrap();
            }
            info!("Finished sender task");
            bytes_sent
        });

        let receiver = task::spawn(async move {
            let mut received = 0;
            while let Some(msg) = output_rx.recv().await {
                match msg {
                    OutputEvent::Response(Output { count }) => {
                        received += count;
                    }
                    OutputEvent::ModelError(e) => panic!("{}", e),
                }
            }
            info!("Finished receiver task");
            received
        });

        let (bytes_sent, count_received, run) = tokio::join!(sender, receiver, inference);

        run.unwrap();
        assert_eq!(count_received.unwrap(), 0);
        assert!(bytes_sent.unwrap() > 0);
        assert!(!logs_contain("Received start message"));
        assert!(logs_contain("Data sent when in stop mode. Discarding"));
        assert!(logs_contain("No longer receiving any messages"));
        assert!(!logs_contain("Adding to inference runner task"));
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

        let inference = context.simple(input_rx, output_tx);

        let sender = task::spawn(async move {
            input_tx.send(InputEvent::Start).await.unwrap();
            for _ in 0..100 {
                let to_send = (0..10).map(|x| x as f32).collect::<Vec<_>>();

                input_tx
                    .send(InputEvent::Data(Arc::new(to_send)))
                    .await
                    .unwrap();
            }
            info!("Finished sender task");
        });

        let receiver = task::spawn(async move {
            let mut received_errors = 0;
            while let Some(msg) = output_rx.recv().await {
                match msg {
                    OutputEvent::Response(Output { count }) => {
                        panic!("Didn't expect actual messages back!");
                    }
                    OutputEvent::ModelError(e) => {
                        received_errors += 1;
                    }
                }
            }
            info!("Finished receiver task");
            received_errors
        });

        let (_, count_received, run) = tokio::join!(sender, receiver, inference);

        run.unwrap();
        assert_eq!(count_received.unwrap(), 100);
        assert!(logs_contain("Received start message"));
        assert!(logs_contain("Adding to inference runner task"));
        assert!(logs_contain("Failed inference event"));
    }
}
