#![cfg(not(tarpaulin_include))]
use std::env;
use tokio::task;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::{Layer, Registry};

#[tokio::main]
async fn main() {
    if let Err(e) = setup_logging() {
        eprintln!("No logs will be emitted: {}", e);
    }
    // So using our disallowed method here just to launch the server. The main worker thread can't
    // run spawned tasks so it causes a forced context switch. So we want to make sure things
    // aren't ran on the main worker.
    task::spawn(streamer_template::launch_server())
        .await
        .unwrap();
}

pub fn setup_logging() -> anyhow::Result<()> {
    let filter = match env::var("RUST_LOG") {
        Ok(_) => EnvFilter::from_env("RUST_LOG"),
        _ => EnvFilter::new("streamer_template=info"),
    };

    let fmt = tracing_subscriber::fmt::Layer::default();
    let subscriber = filter.and_then(fmt).with_subscriber(Registry::default());

    tracing::subscriber::set_global_default(subscriber)?;
    Ok(())
}
