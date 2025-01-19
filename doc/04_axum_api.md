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
you're using as the main branch potentially has a number of breaking changes that
stop the examples from working with the last released version.

At time of writing the latest released Axum is 0.8.1 so this will be added to
the `Cargo.toml` in the dependencies section:

```
axum = { version = "0.8.1", features = ["tracing", "ws"] }
```

With that preamble out of the way let's get started with defining our function
to launch the server. Initially, we'll just start with a simple health-check,
but in future we might want to change the health status if the service needs
restarting. We'll also add a shutdown signal for graceful shutdown so if we get
a SIGTERM, the server will stop receiving connections and close after the last
request is finished. I'll also pass in our model's `StreamingContext` because we
know we'll need that in future!

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

Make service router is a separate function for one important reason -
testability. This means our tests can easily create a router and test routes etc
without binding to a port.

The health check API and all of our APIs designed to be used by users will be
in a versioned API hence the `/api/v1` prefix to the health check. Versioned APIs
are great because they allow us to make breaking changes to our API and just
increase the version. We can then potentially keep a backwards compatible endpoint
and give users time to migrate any code relying on our service before we remove
legacy APIs.

Now we add a `launch_server` function in the `lib.rs` which the main function will
call to launch everything. It will load a config file with the model settings,
create the context and then launch the axum server.

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

This is all pretty simple stuff so far and you can find it in the Axum
getting started documentation. The next part is where we start to up the
complexity because it's time to add in the websocket handler.

## Websockets

Websockets are a streaming protocol built on top of TCP that allows for
bidirectional streaming. A websocket resembles a raw TCP socket more than HTTP
as it allows streaming between client and server and before HTTP/2 it was the
only way to do streaming where the client streams data into the server. If it's
only the server streaming SSE (Server-Sent Events), can be used with HTTP/1.1.
Because of this we need to upgrade the HTTP connection to a websocket connection:

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
called on the context but everything else should be reused. In future we might
want to separate it for better tracking of metrics per-endpoint, but as a first
implementation we always want to strive for simplicity.

Well now, take a breath because now we're finally set to finally write the last
parts of the API defined in part 2. With this addition there will be a working
API that can be called and audio streams in and responses and events out. Before
we start coding lets refamiliarise ourselves with the steps we want to follow:

1. Receive a start message from the client
2. Use this to set up the audio decoding and inference tasks
3. Receive audio and forward it into the decoding
4. Connect audio decoding output to inference tasks (one per channel)
5. Forward inference output to client
6. Handle stop requests and either keep connection or disconnect
7. Wait for another start or data to resume processing

Well waiting for a start message might be a moderately sized block of code
that's repeated. Splitting this into it's own function used by `handle_socket`
already makes sense. Time to implement it as follows: 

```rust
use futures::stream::StreamExt;

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
```

I've popped some [`tracing`](https://crates.io/crates/tracing) logs here just
to make the process we go through when handshaking clear. This function will
ignore any invalid messages and just receive messages from the websocket until
it gets a start message or the client disconnects. For invalid start messages
we may in future want to return an error to the user but we'll assume that's
unnecessary for now.

The next steps will be to setup the receiver task to forward message to the
client from the inference task, and then wait for the first message. I'll
make a small function for encoding the messages just to make the chained calls
look tidier as well.

```rust
fn create_websocket_message(event: ApiResponse) -> Result<Message, axum::Error> {
    let string = serde_json::to_string(&event).unwrap();
    Ok(Message::Text(string))
}

async fn handle_socket(
    socket: WebSocket,
    vad_processing: bool,
    state: Arc<StreamingContext>,
) {
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
        todo!("need to start processing things!");
    }
}
```

The eagle-eyed among us may have realised I've preemptively put a label on the
loop. This is because at the start of the loop we'll be setting up the audio
decoding tasks and inference tasks. Then we will loop through the stream of 
messages until we get a stop. After we get the stop a new start or more audio
data will resume processing but we want the stop to force an end of processing
of the existing audio. For that inference and decoding the audio is finished,
so if they batch up in anyway they have to know the last audio is potentially
a partial batch.

Setting up the inference tasks:

```rust
// Don't forget me from forwarding from inference to client
let (client_sender, client_receiver) = mpsc::channel(8);
// ...
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
        };

        let handle = task::spawn(inference_task);
        running_inferences.push(handle);
        senders.push(samples_tx);
    }
    
    // Transcoding and message processing 
    todo!("The rest of the owl");

    // Clean up inference task
    for handle in running_inferences.drain(..) {
        match handle.await {
            Ok(Err(e)) => error!("Inference failed: {}", e),
            Err(e) => error!("Inference task panicked: {}", e),
            Ok(Ok(_)) => {}
        }
    }
}
```

The transcoding task is also fairly simple, we just want to spawn the audio
decoder with our channels passed in. 

```rust
let transcoding_task = task::spawn(
    decode_audio(start.format, audio_bytes_rx, senders).in_current_span()
);
```

In our current function the last stage is to read the messages from the
websocket and send down the appropriate channel. There's also a small
check after we join the running inferences so I'll include a bit of
the code from a previous sample again:

```rust
'outer: loop {

    // Code from setting up the inference and transcoding tasks

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
```

When we receive binary data this is audio so we send into our transcoding task.
If that sending has failed it means one of two things:

1. Transcoding failed with an error
2. We've had a single utterance request and the utterances have finished

In this case we break out of the message handling and when we await the other
tasks we'll see if anything went wrong.

For the text messages these should either be start or stop, these come when our
current stream ends and a new one is starting or the connection will be closed.

If we exited the websocket message receiving without receiving anything then the
client will have disconnected before we did anything and we want to exit same
with a disconnection request hence the little if with a final break at the end.

With this we have a working API and can stream audio into a model and get
results back.
