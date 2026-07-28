//! Compares the cost of the same workload across three shapes: a plain
//! function with no macro at all, a `#[traceable]` function with tracing
//! disabled (the near-zero-cost claim this crate is built around), and a
//! `#[traceable]` function with tracing enabled (real span creation +
//! export, so it's not an unrealistically cheap no-op tracer).

// `criterion_group!` expands to an undocumented public `benches` fn.
#![allow(missing_docs)]

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use stylus::traceable;

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

const WORKLOAD_ITERATIONS: u64 = 1_000;

fn bench_traceable_overhead(c: &mut Criterion) {
    let mut group = c.benchmark_group("traceable_overhead");

    group.bench_function("no_macro", |b| {
        b.iter(|| plain(black_box(WORKLOAD_ITERATIONS)));
    });

    stylus::config::disable_all();
    group.bench_function("traceable_disabled", |b| {
        b.iter(|| traceable_fn(black_box(WORKLOAD_ITERATIONS)));
    });

    // A real (if in-process) exporter, so the "enabled" number reflects
    // actual span construction + export cost rather than a no-op global
    // tracer stub.
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter)
        .build();
    opentelemetry::global::set_tracer_provider(provider);

    stylus::config::enable_all();
    stylus::config::set_child_only([]);
    group.bench_function("traceable_enabled", |b| {
        b.iter(|| traceable_fn(black_box(WORKLOAD_ITERATIONS)));
    });

    // Enabled + child-only, called with no active parent: exercises the
    // child-only gate (extra atomic load + ambient-context check) on the path
    // where it suppresses the span rather than creating one.
    let all_names: Vec<&str> = stylus::config::all_names().collect();
    stylus::config::set_child_only(all_names);
    group.bench_function("traceable_enabled_child_only_suppressed", |b| {
        b.iter(|| traceable_fn(black_box(WORKLOAD_ITERATIONS)));
    });

    group.finish();
}

criterion_group!(benches, bench_traceable_overhead);
criterion_main!(benches);
