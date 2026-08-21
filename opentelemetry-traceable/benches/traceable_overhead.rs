//! Compares the cost of the same workload across three shapes: a plain
//! function with no macro, a `#[traceable]` function with tracing
//! disabled, and a `#[traceable]` function with tracing enabled.
//!
//! Also compares against a minimal equivalent built on the
//! `tracing` crate instead: `#[tracing::instrument]`, checked on every call
//! via `tracing_subscriber`'s `DynFilterFn` (which reports `Interest::sometimes()`.
//! The "enabled" case uses `tracing-opentelemetry` with the same `SdkTracerProvider` and
//! `InMemorySpanExporter` the `opentelemetry-traceable` "enabled" benchmark uses.

// `criterion_group!` expands to an undocumented public `benches` fn.
#![allow(missing_docs)]

use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracer, SdkTracerProvider};
use opentelemetry_traceable::instrumentation::Instrumentation;
use opentelemetry_traceable::traceable;
use tracing_subscriber::filter::DynFilterFn;
use tracing_subscriber::prelude::*;

fn workload(n: u64) -> u64 {
    let mut acc: u64 = 0xcbf2_9ce4_8422_2325;
    for i in 0..n {
        acc ^= i;
        acc = acc.wrapping_mul(0x0000_0100_0000_01b3);
    }
    acc
}

fn plain(n: u64) -> u64 {
    workload(n)
}

#[traceable]
fn traceable_fn(n: u64) -> u64 {
    workload(n)
}

/// In order to mimic what opentelemetry-traceable does, we use
/// an AtomicBool as a mock, to be loaded within the DynFilterFn
/// to take the tracing decision.
static TRACING_ENABLED: AtomicBool = AtomicBool::new(false);

fn init_tracing_subscriber(tracer: SdkTracer) {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let filter = DynFilterFn::new(|_metadata, _cx| TRACING_ENABLED.load(Ordering::Relaxed));
        let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);
        let subscriber = tracing_subscriber::registry().with(otel_layer.with_filter(filter));
        tracing::subscriber::set_global_default(subscriber)
            .expect("setting the global tracing subscriber for this bench");
    });
}

#[tracing::instrument]
fn tracing_instrument_fn(n: u64) -> u64 {
    workload(n)
}

const WORKLOAD_ITERATIONS: u64 = 1_000;

fn bench_traceable_overhead(c: &mut Criterion) {
    let mut group = c.benchmark_group("traceable_overhead");

    group.bench_function("no_macro", |b| {
        b.iter(|| plain(black_box(WORKLOAD_ITERATIONS)));
    });

    group.bench_function("traceable_disa", |b| {
        b.iter(|| traceable_fn(black_box(WORKLOAD_ITERATIONS)));
    });

    let exporter1 = InMemorySpanExporter::default();
    let provider1 = SdkTracerProvider::builder()
        .with_simple_exporter(exporter1)
        .build();
    let instr1 = Instrumentation::builder()
        .name("bench-1")
        .tracer(provider1.tracer("bench-1"))
        .build()
        .expect("a free instrumentation slot");

    instr1.enable_all();
    group.bench_function("traceable_one_instr_enab", |b| {
        b.iter(|| traceable_fn(black_box(WORKLOAD_ITERATIONS)));
    });
    instr1.disable_all();
    group.bench_function("traceable_one_instr_disa", |b| {
        b.iter(|| traceable_fn(black_box(WORKLOAD_ITERATIONS)));
    });

    let exporter2 = InMemorySpanExporter::default();
    let provider2 = SdkTracerProvider::builder()
        .with_simple_exporter(exporter2)
        .build();
    let instr2 = Instrumentation::builder()
        .name("bench-2")
        .tracer(provider2.tracer("bench-2"))
        .build()
        .expect("a free instrumentation slot");

    instr1.enable_all();
    instr2.enable_all();
    group.bench_function("traceable_two_instr_enab", |b| {
        b.iter(|| traceable_fn(black_box(WORKLOAD_ITERATIONS)));
    });
    instr1.disable_all();
    instr2.disable_all();
    group.bench_function("traceable_two_instr_disa", |b| {
        b.iter(|| traceable_fn(black_box(WORKLOAD_ITERATIONS)));
    });

    drop(instr1);
    drop(instr2);

    let tracing_exporter = InMemorySpanExporter::default();
    let tracing_provider = SdkTracerProvider::builder()
        .with_simple_exporter(tracing_exporter)
        .build();
    init_tracing_subscriber(tracing_provider.tracer("tracing_bench"));

    TRACING_ENABLED.store(false, Ordering::Relaxed);
    group.bench_function("tracing_instrument_disa", |b| {
        b.iter(|| tracing_instrument_fn(black_box(WORKLOAD_ITERATIONS)));
    });

    TRACING_ENABLED.store(true, Ordering::Relaxed);
    group.bench_function("tracing_instrument_enab", |b| {
        b.iter(|| tracing_instrument_fn(black_box(WORKLOAD_ITERATIONS)));
    });

    group.finish();
}

criterion_group!(benches, bench_traceable_overhead);
criterion_main!(benches);
