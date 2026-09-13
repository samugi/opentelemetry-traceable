//! Basic example for opentelemetry-traceable

use std::thread;
use std::time::Duration;

use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::{self, trace::SdkTracerProvider};
use opentelemetry_traceable::instrumentation::Instrumentation;
use opentelemetry_traceable::traceable;

#[traceable]
fn foo() {
    thread::sleep(Duration::from_secs(1));
}

#[traceable]
fn bar() {
    thread::sleep(Duration::from_secs(1));
}

#[traceable(name = "renamed-baz", fields("component" = "proxy", "request_id" = 123))]
fn baz() {
    thread::sleep(Duration::from_secs(1));
}

#[traceable]
fn quz() {
    thread::sleep(Duration::from_secs(1));
}

fn configure_tracing() -> (Instrumentation, SdkTracerProvider) {
    let exporter = opentelemetry_stdout::SpanExporter::default();

    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_simple_exporter(exporter)
        .build();

    let instr = Instrumentation::builder()
        .tracer(provider.tracer("my-tracer"))
        .build()
        .unwrap();

    instr.set_enabled(&["*::foo", "renamed-baz"]).unwrap();
    (instr, provider)
}

#[tokio::main]
async fn main() {
    let (_instr, _provider_guard) = configure_tracing();

    loop {
        foo(); // traced
        bar(); // skipped
        baz(); // traced with span name: renamed-baz
        quz(); // skipped
    }
}
