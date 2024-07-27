#![cfg(not(tarpaulin_include))]

#[tokio::main]
async fn main() {
    init_tracing_opentelemetry::tracing_subscriber_ext::init_subscribers().expect("Failed to setup otel");
    // So for launching either axum or actix we don't want to use the macros to make our tokio
    // runtime because actix takes a different approach (multiple single threaded runtimes).
    streamer_template::launch_server().await;
}
