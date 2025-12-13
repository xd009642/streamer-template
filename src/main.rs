#![cfg(not(tarpaulin_include))]
#![allow(clippy::disallowed_methods)]
use std::panic::{set_hook, PanicHookInfo};
use tokio::task;
use tracing::{error, error_span};

fn traced_panic_hook(info: &PanicHookInfo) {
    metrics::counter!("service_panics").increment(1);
    // https://github.com/rust-lang/rust/blob/90743e7298aca107ddaa0c202a4d3604e29bfeb6/library/std/src/panicking.rs#L235-L288
    let location = info.location();

    let msg = match info.payload().downcast_ref::<&'static str>() {
        Some(s) => *s,
        None => match info.payload().downcast_ref::<String>() {
            Some(s) => &s[..],
            None => "Box<dyn Any>",
        },
    };

    let thread = std::thread::current();
    let thread = thread.name().unwrap_or("<unnamed>");
    let backtrace = std::backtrace::Backtrace::capture();

    let _entered = if let Some(location) = location {
        error_span!("panic", %thread, location = %format!("{}:{}:{}", location.file(), location.line(), location.column()))
    } else {
        // very unlikely to hit here, but the guarantees of std could change
        error_span!("panic", %thread)
    }
    .entered();

    if backtrace.status() == std::backtrace::BacktraceStatus::Captured {
        error!("{msg}\n\nStack backtrace:\n{backtrace}");
    } else {
        error!("{msg}");
    }
}

#[tokio::main]
async fn main() {
    let _guard = init_tracing_opentelemetry::tracing_subscriber_ext::init_subscribers()
        .expect("Failed to setup otel");

    metrics::describe_counter!(
        "service_panics",
        "tracks total panics during the service runtime"
    );
    set_hook(Box::new(traced_panic_hook));

    // So using our disallowed method here just to launch the server. The main worker thread can't
    // run spawned tasks so it causes a forced context switch. So we want to make sure things
    // aren't ran on the main worker.
    task::spawn(streamer_template::launch_server())
        .await
        .unwrap();
}
