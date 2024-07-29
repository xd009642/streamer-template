use crate::api_types::*;
use crate::audio::decode_audio;
use crate::{OutputEvent, StreamingContext};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Extension,
    },
    response::{IntoResponse, Json},
    routing::get,
    Router,
};
use axum_tracing_opentelemetry::middleware::{OtelAxumLayer, OtelInResponseLayer};
use futures::{stream::StreamExt, FutureExt};
use opentelemetry::global;
use serde_json::Value;
use std::error::Error;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{error, info, instrument, warn, Instrument, Span};
use tracing_opentelemetry::OpenTelemetrySpanExt;

async fn ws_handler(
    ws: WebSocketUpgrade,
    Extension(state): Extension<Arc<StreamingContext>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
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

fn create_websocket_message(output: OutputEvent) -> Result<Message, axum::Error> {
    let event = Event::from(output);
    let string = serde_json::to_string(&event).unwrap();
    Ok(Message::Text(string))
}

/// Actual websocket statemachine (one will be spawned per connection)
#[instrument(skip_all)]
async fn handle_socket(socket: WebSocket, state: Arc<StreamingContext>) {
    let (sender, mut receiver) = socket.split();

    let (client_sender, client_receiver) = mpsc::channel(8);
    let client_receiver = ReceiverStream::new(client_receiver);
    tokio::task::spawn(
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
        for _i in 0..start.channels {
            let client_sender_clone = client_sender.clone();
            let (samples_tx, samples_rx) = mpsc::channel(8);
            let context = state.clone();
            let handle = tokio::task::spawn(
                async move {
                    context
                        .inference_runner(samples_rx, client_sender_clone)
                        .await
                }
                .in_current_span(),
            );
            running_inferences.push(handle);
            senders.push(samples_tx);
        }
        let transcoding_task = tokio::task::spawn(
            decode_audio(start.sample_rate, audio_bytes_rx, senders).in_current_span(),
        );

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

pub fn make_service_router(app_state: Arc<StreamingContext>) -> Router {
    Router::new()
        .route("/api/v1/stream", get(ws_handler))
        .route("/api/v1/health", get(health_check))
        .layer(Extension(app_state))
        .layer(OtelInResponseLayer::default())
        .layer(OtelAxumLayer::default())
}

pub async fn run_axum_server(app_state: Arc<StreamingContext>) -> anyhow::Result<()> {
    let app = make_service_router(app_state);

    // run it with hyper
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
    info!("listening on {}", listener.local_addr().unwrap());
    axum::serve(listener, app).await?;
    Ok(())
}
