use std::env;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::{Layer, Registry};

pub fn setup_logging() -> Result<(), Box<dyn std::error::Error>> {
    let fmt = tracing_subscriber::fmt::Layer::default();

    let filter = match env::var("RUST_LOG") {
        Ok(_) => EnvFilter::from_env("RUST_LOG"),
        _ => EnvFilter::new("client=info,streamer_template=info"),
    };

    let subscriber = filter.and_then(fmt).with_subscriber(Registry::default());

    tracing::subscriber::set_global_default(subscriber)?;
    Ok(())
}
