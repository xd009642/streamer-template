use crate::api_types::*;
use crate::audio::decode_audio;
use crate::AudioChannel;
use crate::{OutputEvent, StreamingContext};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Extension,
    },
    response::IntoResponse,
    routing::get,
    Router,
};
use bytes::Bytes;
use futures::{sink::SinkExt, stream::StreamExt};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::mpsc;
use tower_http::trace::{DefaultMakeSpan, TraceLayer};
use tracing::info;

async fn ws_handler(
    ws: WebSocketUpgrade,
    Extension(state): Extension<Arc<StreamingContext>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// Actual websocket statemachine (one will be spawned per connection)
async fn handle_socket(socket: WebSocket, state: Arc<StreamingContext>) {
    let (mut sender, mut receiver) = socket.split();
}

pub fn make_service_router(app_state: Arc<StreamingContext>) -> Router {
    Router::new()
        .route("/ws", get(ws_handler))
        .layer(Extension(app_state))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::default().include_headers(true)),
        )
}

#[tokio::main]
pub async fn run_axum_server(app_state: Arc<StreamingContext>) {
    let app = make_service_router(app_state);

    // run it with hyper
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000")
        .await
        .unwrap();
    info!("listening on {}", listener.local_addr().unwrap());
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .unwrap();
}
