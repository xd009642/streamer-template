use crate::api_types::*;
use crate::audio::decode_audio;
use crate::StreamingContext;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Extension,
    },
    response::{IntoResponse, Json},
    routing::get,
    Router,
};
use futures::{
    stream::{Stream, StreamExt},
    FutureExt,
};
use serde_json::Value;
use std::error::Error;
use std::sync::Arc;
use tokio::{signal, sync::mpsc, task};
use tokio_stream::wrappers::ReceiverStream;
use tracing::{error, info, warn};

async fn ws_handler(
    ws: WebSocketUpgrade,
    vad_processing: bool,
    Extension(state): Extension<Arc<StreamingContext>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, vad_processing, state))
}

async fn handle_initial_start<S, E>(receiver: &mut S) -> Option<StartMessage>
where
    S: Stream<Item = Result<Message, E>> + Unpin,
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
    Ok(Message::Text(string.into()))
}

/// Actual websocket statemachine (one will be spawned per connection)
///
/// Note we can't instrument this as the websocket API call is the root span and this makes
/// tracing harder RE otel context propagation.
async fn handle_socket(socket: WebSocket, vad_processing: bool, state: Arc<StreamingContext>) {
    let (sender, mut receiver) = socket.split();

    let (client_sender, client_receiver) = mpsc::channel(8);
    let client_receiver = ReceiverStream::new(client_receiver);
    let recv_task = client_receiver
        .map(create_websocket_message)
        .forward(sender)
        .map(|result| {
            if let Err(e) = result {
                error!("error sending websocket msg: {}", e);
            }
        });
    let _ = task::spawn(recv_task);

    let mut start = match handle_initial_start(&mut receiver).await {
        Some(start) => start,
        None => {
            info!("Exiting with processing any messages, no data received");
            return;
        }
    };

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

            let inference_task = async move {
                if vad_processing {
                    context
                        .segmented_runner(start_cloned, channel_id, samples_rx, client_sender_clone)
                        .await
                } else {
                    context
                        .inference_runner(channel_id, samples_rx, client_sender_clone)
                        .await
                }
            };

            let handle = task::spawn(inference_task);
            running_inferences.push(handle);
            senders.push(samples_tx);
        }
        let transcoding_task = task::spawn(decode_audio(start.format, audio_bytes_rx, senders));

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
        .route(
            "/api/v1/simple",
            get(move |ws, app_state| ws_handler(ws, false, app_state)),
        )
        .route(
            "/api/v1/segmented",
            get(move |ws, app_state| ws_handler(ws, true, app_state)),
        )
        .route("/api/v1/health", get(health_check))
        .layer(Extension(app_state))
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
