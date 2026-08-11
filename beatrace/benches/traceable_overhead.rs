//! Compares the cost of the same workload across three shapes: a plain
//! function with no macro at all, a `#[traceable]` function with tracing
//! disabled (the near-zero-cost claim this crate is built around), and a
//! `#[traceable]` function with tracing enabled (real span creation +
//! export, so it's not an unrealistically cheap no-op tracer).
//!
//! Also compares against a minimal hand-rolled equivalent built on the
//! `tracing` crate instead: `#[tracing::instrument]` gated by a single
//! global `AtomicBool`, checked on every call via `tracing_subscriber`'s
//! `DynFilterFn` (which reports `Interest::sometimes()` rather than letting
//! `tracing` cache a fixed answer per callsite -- the same "recheck every
//! call" semantics `beatrace` relies on). The "enabled" case is wired through
//! `tracing-opentelemetry` to the exact same `SdkTracerProvider` and
//! `InMemorySpanExporter` the `beatrace` "enabled" benchmark uses, so both
//! pay for real span construction and export, not a no-op stub -- an
//! apples-to-apples comparison rather than a hand-wavy one.

// `criterion_group!` expands to an undocumented public `benches` fn.
#![allow(missing_docs)]

use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracer, SdkTracerProvider};
use beatrace::instrumentation::Instrumentation;
use beatrace::traceable;
use tracing_subscriber::filter::DynFilterFn;
use tracing_subscriber::prelude::*;

/// Some reasonable, non-trivial CPU-bound work -- an FNV-1a-style mixing
/// loop -- so the benchmark reflects overhead relative to a real function
/// body, not just the cost of an empty one.
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

/// Gate for the `tracing`-based equivalent below -- read on every call via
/// `DynFilterFn`, mirroring the per-site `AtomicBool` `beatrace` checks on
/// every call in `registry::TraceSite`.
static TRACING_GATE: AtomicBool = AtomicBool::new(false);

/// Sets the global default `tracing` subscriber once: `tracing_subscriber`'s
/// `registry()` layered with `tracing-opentelemetry`'s bridge (real span
/// export via `tracer`, the same kind of `SdkTracer` the `beatrace` benchmark
/// uses), filtered by a `DynFilterFn` reading `TRACING_GATE`. `DynFilterFn`'s
/// callsite interest is `sometimes()` (it can't assume the closure's answer
/// is fixed), so the gate is re-evaluated on every call rather than cached
/// after the first check -- same dynamic-enable semantics as `beatrace`.
fn init_tracing_subscriber(tracer: SdkTracer) {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let filter = DynFilterFn::new(|_metadata, _cx| TRACING_GATE.load(Ordering::Relaxed));
        let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);
        let subscriber = tracing_subscriber::registry().with(otel_layer.with_filter(filter));
        tracing::subscriber::set_global_default(subscriber)
            .expect("setting the global tracing subscriber for this bench");
    });
}

// Default `#[instrument]` behavior also records every argument as a span
// field (here, `n`) -- `#[traceable]` only does that if `fields(...)` is
// given explicitly. Left as the default (rather than `skip_all`) since it's
// how most real `#[instrument]` call sites are written; the number below
// includes that extra formatting cost, not just the enable/disable check.
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

    // No instrumentation exists yet, so every site's mask is 0 -- the
    // single-atomic-load fast path.
    group.bench_function("traceable_disa", |b| {
        b.iter(|| traceable_fn(black_box(WORKLOAD_ITERATIONS)));
    });

    // Real (if in-process) exporters throughout, so the "enabled" numbers
    // reflect actual span construction + export cost rather than a no-op
    // tracer stub.
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

    // A second instrumentation over the same function: `start_spans` now builds
    // two spans per call, one per active slot, from two different tracers.
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

    // The `tracing` comparison gets its own provider/exporter, so it pays for
    // the same real span construction + export work the numbers above do.
    let tracing_exporter = InMemorySpanExporter::default();
    let tracing_provider = SdkTracerProvider::builder()
        .with_simple_exporter(tracing_exporter)
        .build();
    init_tracing_subscriber(tracing_provider.tracer("tracing_bench"));

    TRACING_GATE.store(false, Ordering::Relaxed);
    group.bench_function("tracing_instrument_disa", |b| {
        b.iter(|| tracing_instrument_fn(black_box(WORKLOAD_ITERATIONS)));
    });

    TRACING_GATE.store(true, Ordering::Relaxed);
    group.bench_function("tracing_instrument_enab", |b| {
        b.iter(|| tracing_instrument_fn(black_box(WORKLOAD_ITERATIONS)));
    });

    group.finish();
}

criterion_group!(benches, bench_traceable_overhead);
criterion_main!(benches);
