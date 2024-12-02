#![cfg(not(tarpaulin_include))]
#![allow(clippy::disallowed_methods)]
use tokio::task;

#[tokio::main]
async fn main() {
    init_tracing_opentelemetry::tracing_subscriber_ext::init_subscribers()
        .expect("Failed to setup otel");

    // So using our disallowed method here just to launch the server. The main worker thread can't
    // run spawned tasks so it causes a forced context switch. So we want to make sure things
    // aren't ran on the main worker.
    task::spawn(streamer_template::launch_server())
        .await
        .unwrap();
}
