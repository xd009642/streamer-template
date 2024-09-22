use crate::api_types::*;
use crate::audio::decode_audio;
use crate::metrics::*;
use crate::StreamingContext;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Extension,
    },
    response::{IntoResponse, Json, Response},
    routing::get,
    Router,
};
use axum_tracing_opentelemetry::middleware::{OtelAxumLayer, OtelInResponseLayer};
use futures::{stream::StreamExt, FutureExt};
use opentelemetry::global;
use serde_json::Value;
use std::error::Error;
use std::sync::Arc;
use std::time::Duration;
use tokio::{signal, sync::mpsc};
use tokio_metrics::TaskMonitor;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{error, info, warn, Instrument, Span};
use tracing_opentelemetry::OpenTelemetrySpanExt;

async fn ws_handler(
    ws: WebSocketUpgrade,
    vad_processing: bool,
    Extension(state): Extension<Arc<StreamingContext>>,
    Extension(metrics): Extension<Arc<AppMetricsEncoder>>,
) -> impl IntoResponse {
    let current = Span::current();
    ws.on_upgrade(move |socket| {
        handle_socket(socket, vad_processing, state, metrics).instrument(current)
    })
}

async fn handle_initial_start<S, E>(receiver: &mut S) -> Option<StartMessage>
where
    S: StreamExt<Item = Result<Message, E>> + Unpin,
    E: Error,
{
    let mut start = None;

    while let Some(Ok(msg)) = receiver.next().await {
        if let Ok(text) = msg.into_text() {
            match serde_json::from_str::<RequestMessage>(&text) {
                Ok(RequestMessage::Start(start_msg)) => {
                    info!(start=?start, "Initialising streamer");
                    start = Some(start_msg);
                    break;
                }
                Ok(RequestMessage::Stop(_)) => {
                    warn!("Unexpected stop received as first message");
                }
                Err(e) => {
                    error!(json=%text, error=%e, "invalid json");
                }
            }
        }
    }
    start
}

fn create_websocket_message(event: ApiResponse) -> Result<Message, axum::Error> {
    let string = serde_json::to_string(&event).unwrap();
    Ok(Message::Text(string))
}

/// Actual websocket statemachine (one will be spawned per connection)
///
/// Note we can't instrument this as the websocket API call is the root span and this makes
/// tracing harder RE otel context propagation.
async fn handle_socket(
    socket: WebSocket,
    vad_processing: bool,
    state: Arc<StreamingContext>,
    metrics_enc: Arc<AppMetricsEncoder>,
) {
    let monitors = &metrics_enc.metrics;
    let (sender, mut receiver) = socket.split();

    let (client_sender, client_receiver) = mpsc::channel(8);
    let client_receiver = ReceiverStream::new(client_receiver);
    let recv_task = TaskMonitor::instrument(
        &monitors.client_receiver,
        client_receiver
            .map(create_websocket_message)
            .forward(sender)
            .map(|result| {
                if let Err(e) = result {
                    error!("error sending websocket msg: {}", e);
                }
            })
            .in_current_span(),
    );
    tokio::task::spawn(recv_task);

    let mut start = match handle_initial_start(&mut receiver).await {
        Some(start) => start,
        None => {
            info!("Exiting with processing any messages, no data received");
            return;
        }
    };

    // Okay so we're in a root span so this will work but it wouldn't work outside of a root span
    // necessarily!
    let current = Span::current();
    let parent = global::get_text_map_propagator(|prop| prop.extract(&start));
    current.set_parent(parent);

    'outer: loop {
        info!("Setting up inference loop");
        let (audio_bytes_tx, audio_bytes_rx) = mpsc::channel(8);
        let mut running_inferences = vec![];
        let mut senders = vec![];
        for channel_id in 0..start.format.channels {
            let client_sender_clone = client_sender.clone();
            let (samples_tx, samples_rx) = mpsc::channel(8);
            let context = state.clone();
            let start_cloned = start.clone();

            let inference_task = TaskMonitor::instrument(
                &monitors.inference,
                async move {
                    if vad_processing {
                        context
                            .segmented_runner(
                                start_cloned,
                                channel_id,
                                samples_rx,
                                client_sender_clone,
                            )
                            .await
                    } else {
                        context
                            .inference_runner(channel_id, samples_rx, client_sender_clone)
                            .await
                    }
                }
                .in_current_span(),
            );

            let handle = tokio::task::spawn(inference_task);
            running_inferences.push(handle);
            senders.push(samples_tx);
        }
        let transcoding_task = tokio::task::spawn(TaskMonitor::instrument(
            &monitors.audio_decoding,
            decode_audio(start.format, audio_bytes_rx, senders).in_current_span(),
        ));

        let mut got_messages = false;
        let mut disconnect = false;
        while let Some(Ok(msg)) = receiver.next().await {
            match msg {
                Message::Binary(audio) => {
                    got_messages = true;
                    if let Err(e) = audio_bytes_tx.send(audio.into()).await {
                        warn!("Transcoding channel closed, this may indicate that inference has finished: {}", e);
                        break;
                    }
                }
                Message::Text(text) => match serde_json::from_str::<RequestMessage>(&text) {
                    Ok(RequestMessage::Start(start_msg)) => {
                        got_messages = true;
                        info!(start=?start, "Reinitialising streamer");
                        start = start_msg;
                        break;
                    }
                    Ok(RequestMessage::Stop(msg)) => {
                        got_messages = true;
                        info!("Stopping current stream, {:?}", msg);
                        disconnect = msg.disconnect;
                        break;
                    }
                    Err(e) => {
                        error!(json=%text, error=%e, "invalid json");
                    }
                },
                Message::Close(_frame) => {
                    info!("Finished streaming request");
                    break 'outer;
                }
                _ => {} // We don't care about ping and pong
            }
        }

        std::mem::drop(audio_bytes_tx);
        for handle in running_inferences.drain(..) {
            match handle.await {
                Ok(Err(e)) => error!("Inference failed: {}", e),
                Err(e) => error!("Inference task panicked: {}", e),
                Ok(Ok(_)) => {}
            }
        }
        if let Err(e) = transcoding_task.await.unwrap() {
            error!("Failed from transcoding task: {}", e);
        }
        if !got_messages || disconnect {
            break;
        }
    }
}

async fn health_check() -> Json<Value> {
    Json(serde_json::json!({"status": "healthy"}))
}

async fn get_metrics(Extension(metrics_ext): Extension<Arc<AppMetricsEncoder>>) -> Response {
    let mut encoder = metrics_ext.encoder.lock().await;
    metrics_ext.metrics.encode(&mut encoder);
    Response::new(encoder.finish().into())
}

pub fn make_service_router(app_state: Arc<StreamingContext>) -> Router {
    let streaming_monitor = StreamingMonitors::new();
    let metrics_encoder = Arc::new(AppMetricsEncoder::new(streaming_monitor));
    let collector_metrics = metrics_encoder.clone();
    tokio::task::spawn(async move {
        loop {
            collector_metrics.metrics.run_collector();
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });
    Router::new()
        .route(
            "/api/v1/simple",
            get({
                move |ws, app_state, metrics_enc: Extension<Arc<AppMetricsEncoder>>| {
                    let route = metrics_enc.metrics.route.clone();
                    TaskMonitor::instrument(&route, ws_handler(ws, false, app_state, metrics_enc))
                }
            }),
        )
        .route(
            "/api/v1/segmented",
            get({
                move |ws, app_state, metrics_enc: Extension<Arc<AppMetricsEncoder>>| {
                    let route = metrics_enc.metrics.route.clone();
                    TaskMonitor::instrument(&route, ws_handler(ws, true, app_state, metrics_enc))
                }
            }),
        )
        .route("/api/v1/health", get(health_check))
        .route("/metrics", get(get_metrics))
        .layer(Extension(metrics_encoder))
        .layer(Extension(app_state))
        .layer(OtelInResponseLayer)
        .layer(OtelAxumLayer::default())
}

pub async fn run_axum_server(app_state: Arc<StreamingContext>) -> anyhow::Result<()> {
    let app = make_service_router(app_state);

    // run it with hyper
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    info!("listening on {}", listener.local_addr().unwrap());
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
