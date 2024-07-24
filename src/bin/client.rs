use anyhow::Context;
use clap::Parser;
use futures::{SinkExt, StreamExt};
use hound::WavReader;
use std::path::PathBuf;
use std::time::Duration;
use streamer_template::{api_types::*, logging::setup_logging};
use tokio::time::{sleep, timeout};
use tokio_tungstenite::tungstenite::Message;
use tracing::{error, info, trace};

#[derive(Clone, Debug, Parser)]
struct Cli {
    #[clap(short, long)]
    /// Input audio file to stream
    input: PathBuf,
    #[clap(long, default_value = "256")]
    /// Size of audio chunks to send to the server
    chunk_size: usize,
    #[clap(long)]
    /// Trace ID for tracing the API request
    trace_id: Option<String>,
    #[clap(short, long)]
    /// Address of the streaming server
    addr: String,
    #[clap(long)]
    /// Attempts to simulate real time streaming by adding a pause between sending proportional to
    /// sample rate
    real_time: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    setup_logging().expect("Failed to setup logging");
    // Lets just start by loading the whole file, doing the messages and then sending them all in
    // one go.
    let args = Cli::parse();

    info!("Connecting to: {}", args.addr);

    let (ws, _) = match timeout(
        Duration::from_secs(5),
        tokio_tungstenite::connect_async(&args.addr),
    )
    .await
    {
        Ok(ws) => ws?,
        Err(_e) => {
            error!("Timed out trying to connect to socket");
            anyhow::bail!("Timed out trying to connect");
        }
    };

    info!("Connected to server, sending packets");

    let (mut ws_tx, mut ws_rx) = ws.split();

    let reader = WavReader::open(&args.input)?;

    let chunk_size = args.chunk_size;
    let real_time = args.real_time;
    let sender: tokio::task::JoinHandle<anyhow::Result<()>> = tokio::task::spawn(async move {
        let spec = reader.spec();
        let mut samples = reader.into_samples::<i16>();

        let start = RequestMessage::Start(StartMessage {
            trace_id: args.trace_id.clone(),
            channels: spec.channels as usize,
            sample_rate: spec.sample_rate as usize,
        });
        let delay = if real_time {
            let n_samples = (chunk_size as f32 / (spec.bits_per_sample as f32 / 8.0)).ceil();
            let duration = n_samples / spec.sample_rate as f32;
            Duration::from_secs_f32(duration)
        } else {
            Duration::from_secs(0)
        };
        let start = serde_json::to_string(&start).unwrap();
        ws_tx.send(Message::Text(start)).await?;

        let mut buffer = vec![];
        for sample in samples {
            buffer.extend(sample?.to_le_bytes());
            if buffer.len() >= chunk_size {
                trace!("Sending: {} bytes", buffer.len());
                ws_tx.send(Message::Binary(buffer)).await?;
                buffer = vec![];
                sleep(delay).await;
            }
        }

        let stop = RequestMessage::Stop(StopMessage { disconnect: true });
        let stop = serde_json::to_string(&stop).unwrap();
        ws_tx.send(Message::Text(stop)).await?;
        Ok(())
    });

    while let Some(res) = ws_rx.next().await {
        let res = res?;
        if let Message::Text(res) = res {
            info!("Got message: {}", res);
        }
    }

    let res = sender.await.unwrap();
    res.context("Sending task")?;

    Ok(())
}
