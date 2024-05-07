#![cfg(not(tarpaulin_include))]

fn main() {
    // So for launching either axum or actix we don't want to use the macros to make our tokio
    // runtime because actix takes a different approach (multiple single threaded runtimes).
    println!("Hello, world!");
}
