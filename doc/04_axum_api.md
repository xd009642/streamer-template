# The Axum API

For the API in this project we're going to be using
[Axum](https://github.com/tokio-rs/axum), this is largely because of my own
familiarity with Axum. Although, there is a small benefit where Axum and
[Tonic](https://github.com/hyperium/tonic) use the same request router so
multiplexing between REST and gRPC becomes easier in the future if we want to
do it.

One important tip when you're looking at Axum, unlike actix-web and other 
frameworks there isn't a website with a tutorial and examples so you have to
rely on the docs and Github. When you go on Github view the tag for the version
you're using as the main branch usually has a number of breaking changes that
stop the examples from working with the last released version.

At time of writing the latest released Axum is 0.7.5 so this will be added to
the `Cargo.toml` in the dependencies section:

```
axum = { version = "0.7.8", features = ["tracing", "ws"] }
```

With that preamble out of the way let's get started with defining our function
to launch the server. Initially, we'll just start with a simple health-check.
I'll also pass in our model's `StreamingContext` because we know we'll need 
that in future!

```rust
use tokio::signal;
use axum::{extract::Extension, response::Json, routing::get, Router};

async fn health_check() -> Json<Value> {
    Json(serde_json::json!({"status": "healthy"}))
}

fn make_service_router(state: Arc<StreamingContext>) -> Router {
    Router::new()
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
```

The health check API and all of our APIs designed to be used by users will be
in a versioned API hence the `/api/v1` prefix to the health check. Now we add
a `launch_server` function in the `lib.rs` which the main function will call to
launch everything. It will load a config file with the model settings, create the
context and then launch the axum server.

```rust
pub async fn launch_server() {
    let config = fs::read("config.json")
        .await
        .expect("Couldn't read server config");
    let config = serde_json::from_slice(&config).expect("Couldn't deserialize config");
    info!(config=?config, "service config loaded");
    let ctx = Arc::new(StreamingContext::new_with_config(config));
    info!("Launching server");
    axum_server::run_axum_server(ctx)
        .await
        .expect("Failed to launch server");
    info!("Server exiting");
}
```

If we run our server we can now call the health check and see it all working:

```
$ curl localhost:8080/api/v1/health
{"status":"healthy"}
```

This is all pretty simple stuff so far and you should find it in the Axum
getting started documentation. Time to add in the websocket handler.

## Websockets

Websockets are a streaming protocol built on top of TCP that allows for
bidirectional streaming. Because of this we need to upgrade the HTTP connection
to a websocket connection:

```rust
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Extension,
    },
    response::IntoResponse,
};

async fn ws_handler(
    ws: WebSocketUpgrade,
    vad_processing: bool,
    Extension(state): Extension<Arc<StreamingContext>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| {
        handle_socket(socket, vad_processing, state, metrics)
    })
}

async fn handle_socket(
    socket: WebSocket,
    vad_processing: bool,
    state: Arc<StreamingContext>,
) {
    todo!()
}
```

Adding this to the router we get:

```rust
fn make_service_router(state: Arc<StreamingContext>) -> Router {
    Router::new()
        .route(
            "/api/v1/simple",
            get({
                move |ws, app_state| {
                    ws_handler(ws, false, app_state, metrics_enc)
                }
            }),
        )
        .route(
            "/api/v1/segmented",
            get({
                move |ws, app_state| {
                    ws_handler(ws, true, app_state, metrics_enc)
                }
            }),
        )
        .route("/api/v1/health", get(health_check))
        .layer(Extension(app_state))
}
```

I'm reusing the sample function for both the simple chunked API and the VAD
segmented API, this is mainly because the only difference should be the method
called on the context but everything else should be reused.
