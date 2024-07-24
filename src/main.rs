#![cfg(not(tarpaulin_include))]
use streamer_template::logging::setup_logging;

fn main() {
    setup_logging().expect("Failed to setup logging");
    // So for launching either axum or actix we don't want to use the macros to make our tokio
    // runtime because actix takes a different approach (multiple single threaded runtimes).
    streamer_template::launch_server();
}
