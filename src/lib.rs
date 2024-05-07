use crate::model::{Model, Output};
use bytes::{Bytes, BytesMut};
use futures::{stream::FuturesOrdered, StreamExt};
use std::sync::Arc;
use std::thread;
use tokio::sync::mpsc;
use tokio::task;
use tracing::{debug, error, info, warn};

pub mod model;
#[cfg(feature = "actix-web")]
mod actix_server;
#[cfg(feature = "axum")]
mod axum_server;

pub enum InputEvent {
    Start,
    Data(Bytes),
    Stop,
}

pub enum OutputEvent {
    Response(Output),
    ModelError(String),
}

pub struct StreamingContext {
    model: Model,
    min_data: usize,
    max_futures: usize,
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

    pub fn should_run_inference(&self, data: &[u8], is_last: bool) -> bool {
        data.len() >= self.min_data || (is_last && !data.is_empty())
    }

    pub async fn inference_runner(
        self: Arc<Self>,
        mut inference: mpsc::Receiver<Bytes>,
        output: mpsc::Sender<OutputEvent>,
    ) -> anyhow::Result<()> {
        let mut runners = FuturesOrdered::new();
        let mut still_receiving = true;
        let mut received_results = 0;
        let mut received_data = 0;
        // Need to test and prove this doesn't lose any data!
        while still_receiving || !runners.is_empty() {
            tokio::select! {
                msg = inference.recv(), if still_receiving && runners.len() < self.max_futures => {
                    match msg {
                        None => {
                            info!("No longer receiving any messages");
                            still_receiving = false;
                        }
                        Some(msg) => {
                            received_data += 1;
                            debug!("Adding to inference runner task: {}", received_data);
                            let temp_model = self.model.clone();
                            runners.push_back(task::spawn_blocking(move || {
                                temp_model.infer(msg)
                            }));
                        }
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

    pub async fn simple(
        self: Arc<Self>,
        mut input: mpsc::Receiver<InputEvent>,
        output: mpsc::Sender<OutputEvent>,
    ) -> anyhow::Result<()> {
        let mut data_store = BytesMut::new();
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
                        data_store.extend(&bytes);
                    } else {
                        warn!("Data sent when in stop mode. Discarding");
                    }
                }
            }

            // Check if we want to do an inference
            if self.should_run_inference(&data_store, false) {
                tx.send(Bytes::from(data_store)).await?;
                data_store = BytesMut::new();
            }
        }

        // Check if we want to do an inference
        if self.should_run_inference(&data_store, true) {
            tx.send(Bytes::from(data_store)).await?;
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
                let to_send = Bytes::from((0..data).collect::<Vec<_>>());

                input_tx.send(InputEvent::Data(to_send)).await.unwrap();
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
                let to_send = Bytes::from((0..data).collect::<Vec<_>>());

                input_tx.send(InputEvent::Data(to_send)).await.unwrap();
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
                let to_send = Bytes::from((0..10).collect::<Vec<_>>());

                input_tx.send(InputEvent::Data(to_send)).await.unwrap();
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
                    },
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
