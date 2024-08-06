use anyhow::Context;
use clap::Parser;
use futures::{SinkExt, StreamExt};
use hound::WavReader;
use opentelemetry::trace::TracerProvider as TracerProviderTrait;
use opentelemetry::{global, KeyValue};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::{trace as sdktrace, trace::Tracer, Resource};
use opentelemetry_semantic_conventions::resource::SERVICE_NAME;
use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use streamer_template::api_types::*;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::tungstenite::Message;
use tracing::{error, info, instrument, trace, Instrument, Span};
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::{Layer, Registry};

#[derive(Clone, Debug, Parser)]
struct Cli {
    #[clap(short, long)]
    /// Input audio file to stream
    input: PathBuf,
    #[clap(long, default_value = "256")]
    /// Size of audio chunks to send to the server
    chunk_size: usize,
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
    let _guard = setup_logging().expect("Failed to setup logging");
    // Lets just start by loading the whole file, doing the messages and then sending them all in
    // one go.
    let args = Cli::parse();

    run_client(args).await?;

    // if you don't do this your trace likely won't get sent in time before the exporter is
    // terminated.
    global::shutdown_tracer_provider();

    Ok(())
}

fn get_otel_span_id(span: Span) -> Option<String> {
    let context = span.context();
    let map = global::get_text_map_propagator(|prop| {
        let mut map = HashMap::new();
        prop.inject_context(&context, &mut map);
        map
    });

    info!("Got trace propagation: {:?}", map);
    map.get("traceparent").cloned()
}

#[instrument]
async fn run_client(args: Cli) -> anyhow::Result<()> {
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

    let start_instant = Instant::now();

    let chunk_size = args.chunk_size;
    let real_time = args.real_time;
    let sender: tokio::task::JoinHandle<anyhow::Result<()>> = tokio::task::spawn(
        async move {
            let trace_id = get_otel_span_id(Span::current());

            let spec = reader.spec();
            let samples = reader.into_samples::<i16>();

            let start = RequestMessage::Start(StartMessage {
                trace_id,
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
        }
        .in_current_span(),
    );

    let mut first_instant = None;

    while let Some(res) = ws_rx.next().await {
        let res = res?;
        if let Message::Text(res) = res {
            if first_instant.is_none() {
                first_instant = Some(Instant::now().duration_since(start_instant));
            }
            info!("Got message: {}", res);
        }
    }

    let finished = Instant::now().duration_since(start_instant);
    match first_instant {
        Some(s) => info!("First message: {}s. Total time: {}s", s.as_secs_f32(), finished.as_secs_f32()),
        None => info!("No messages. Total time: {}s", finished.as_secs_f32())
    }


    let res = sender.await.unwrap();
    res.context("Sending task")?;

    Ok(())
}

pub fn setup_logging() -> anyhow::Result<Tracer> {
    let filter = match env::var("RUST_LOG") {
        Ok(_) => EnvFilter::from_env("RUST_LOG"),
        _ => EnvFilter::new("client=info,streamer_template=info"),
    };

    let service_name = "streaming-client";
    let builder = opentelemetry_otlp::new_exporter().tonic();
    let builder = match env::var("TRACER_ENDPOINT") {
        Ok(s) => {
            info!("Setting otel endpoint to {}", s);
            builder.with_endpoint(s)
        }
        _ => builder,
    };
    println!("Builder: {:?}", builder);

    let trace_provider = opentelemetry_otlp::new_pipeline()
        .tracing()
        .with_exporter(builder)
        .with_trace_config(
            sdktrace::Config::default().with_resource(Resource::new(vec![KeyValue::new(
                SERVICE_NAME,
                service_name,
            )])),
        )
        .install_batch(opentelemetry_sdk::runtime::Tokio)?;

    global::set_text_map_propagator(TraceContextPropagator::new());

    let provider = trace_provider.provider().context("No trace provider")?;
    let tracer = provider.tracer(service_name.to_string());

    let opentelemetry = tracing_opentelemetry::layer().with_tracer(tracer.clone());
    let fmt = tracing_subscriber::fmt::Layer::default();
    let subscriber = filter
        .and_then(fmt)
        .and_then(opentelemetry)
        .with_subscriber(Registry::default());

    tracing::subscriber::set_global_default(subscriber)?;
    global::set_tracer_provider(provider);
    Ok(tracer)
}
