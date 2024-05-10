#![cfg(not(tarpaulin_include))]
use std::env;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::{Layer, Registry};

fn setup_logging() -> Result<(), Box<dyn std::error::Error>> {
    let fmt = tracing_subscriber::fmt::Layer::default();

    let filter = match env::var("RUST_LOG") {
        Ok(_) => EnvFilter::from_env("RUST_LOG"),
        _ => EnvFilter::new("streamer_template=info"),
    };

    let subscriber = filter.and_then(fmt).with_subscriber(Registry::default());

    tracing::subscriber::set_global_default(subscriber)?;
    Ok(())
}

fn main() {
    setup_logging().expect("Failed to setup logging");
    // So for launching either axum or actix we don't want to use the macros to make our tokio
    // runtime because actix takes a different approach (multiple single threaded runtimes).
    streamer_template::launch_server();
}
