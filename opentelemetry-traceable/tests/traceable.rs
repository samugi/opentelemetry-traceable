//! Integration tests for `#[traceable]` span creation and dynamic enable/disable.
//!
//! Every test drives its own [`Instrumentation`] with its own in-memory
//! exporter, and a fresh instrumentation starts with every bit clear -- so
//! there's no global tracer or default instrumentation to reset between tests.
//!
//! What *is* still shared is the traced functions themselves: their enabled bits
//! live in the process-global `linkme` registry, so a test that enables a
//! function will collect a span from any concurrently-running test that calls
//! it. Hence `cargo nextest run` (process-per-test), enforced by
//! `check_test_runner.rs`.

use std::collections::HashMap;

use opentelemetry::Context;
use opentelemetry::propagation::TextMapPropagator;
use opentelemetry::trace::{
    SpanContext, SpanId, TraceContextExt, TraceFlags, TraceId, TracerProvider as _,
};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use opentelemetry_traceable::instrumentation::{BuildError, Instrumentation};
use opentelemetry_traceable::traceable;

/// Build an instrumentation with its own in-memory exporter/provider. The
/// returned exporter observes only this instrumentation's spans; the tracer
/// keeps its provider alive for the instrumentation's lifetime.
fn instr(name: &'static str) -> (Instrumentation, InMemorySpanExporter) {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let instr = Instrumentation::builder()
        .name(name)
        .tracer(provider.tracer(name))
        .build()
        .expect("a free instrumentation slot");
    (instr, exporter)
}

/// As [`instr`], but the *distributed* kind: it parents to whatever span is
/// current and leaves its own span there. At most one may be live at a time.
fn distributed_instr(name: &'static str) -> (Instrumentation, InMemorySpanExporter) {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let instr = Instrumentation::builder()
        .name(name)
        .tracer(provider.tracer(name))
        .distributed()
        .build()
        .expect("a free instrumentation slot and no other distributed instrumentation");
    (instr, exporter)
}

/// A valid remote span context, standing in for what a propagator extracts from
/// an incoming `traceparent` -- or for a web framework's own server span.
fn remote_parent() -> SpanContext {
    SpanContext::new(
        TraceId::from_bytes([
            0x0b, 0xad, 0xc0, 0xde, 0xde, 0xad, 0xbe, 0xef, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06,
            0x07, 0x08,
        ]),
        SpanId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0x42]),
        TraceFlags::SAMPLED,
        true,
        Default::default(),
    )
}

/// This instrumentation's enabled keys, sorted -- `enabled_names` promises that
/// order, so tests can compare against a plain slice.
fn enabled(instr: &Instrumentation) -> Vec<&'static str> {
    instr.enabled_names().collect()
}

#[traceable]
fn plain(x: u64) -> u64 {
    x + 1
}

#[traceable(name = "custom.span")]
fn named() -> u64 {
    42
}

#[traceable(fields("component" = "proxy", "request_id" = id.clone()))]
fn with_fields(id: String) -> String {
    id
}

#[traceable]
async fn plain_async(x: u64) -> u64 {
    x + 1
}

#[traceable(name = "nesting::parent")]
fn parent() -> u64 {
    child()
}

#[traceable(name = "nesting::child")]
fn child() -> u64 {
    7
}

#[traceable(name = "shared::root")]
fn shared_root() -> u64 {
    shared_leaf()
}

#[traceable(name = "shared::leaf")]
fn shared_leaf() -> u64 {
    5
}

#[traceable(name = "shared::async_root")]
async fn shared_async_root() -> u64 {
    shared_async_leaf().await
}

#[traceable(name = "shared::async_leaf")]
async fn shared_async_leaf() -> u64 {
    5
}

/// Reports whether the ambient `Context`'s own span slot holds a valid span --
/// used to prove which kind writes into it.
#[traceable(name = "probe::ambient")]
fn probe_ambient() -> bool {
    Context::current().span().span_context().is_valid()
}

/// Injects the ambient context into a carrier the way an outbound HTTP client
/// would, from inside a traced body. This is the real propagation contract:
/// a propagator only ever sees `Context`'s span slot.
#[traceable(name = "probe::inject")]
fn probe_inject() -> HashMap<String, String> {
    let mut carrier = HashMap::new();
    TraceContextPropagator::new().inject_context(&Context::current(), &mut carrier);
    carrier
}

struct Widget;

impl Widget {
    #[traceable(name = "widget::render")]
    fn render(&self) -> u64 {
        99
    }
}

// Two same-named methods on different types in one module. A registry key isn't
// qualified by the surrounding `impl`, so both of these carry the *same* key and
// are one selectable thing that toggles together. Deliberate, and pinned by
// `two_sites_sharing_a_key_toggle_together` below.
struct Left;
struct Right;

impl Left {
    #[traceable]
    fn shared_method(&self) -> u64 {
        1
    }
}

impl Right {
    #[traceable]
    fn shared_method(&self) -> u64 {
        2
    }
}

#[test]
fn disabled_by_default_emits_no_span() {
    let (_instr, exporter) = instr("A");

    let result = plain(1);

    assert_eq!(result, 2);
    assert!(exporter.get_finished_spans().unwrap().is_empty());
}

#[test]
fn enabling_emits_a_span_with_default_name() {
    let (instr, exporter) = instr("A");
    instr.enable(&[concat!(module_path!(), "::plain")]).unwrap();

    let result = plain(1);

    assert_eq!(result, 2);
    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].name, "plain");
}

#[test]
fn custom_span_name_and_the_instrumentations_tracer_scope() {
    let (instr, exporter) = instr("test-tracer");
    instr.enable(&["custom.span"]).unwrap();

    let result = named();

    assert_eq!(result, 42);
    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].name, "custom.span");
    // The scope comes from the instrumentation's own tracer -- a call site has
    // no say in it, since the `tracer` macro argument no longer exists.
    assert_eq!(spans[0].instrumentation_scope.name(), "test-tracer");
}

#[test]
fn fields_become_span_attributes() {
    let (instr, exporter) = instr("A");
    instr
        .enable(&[concat!(module_path!(), "::with_fields")])
        .unwrap();

    let result = with_fields("abc".to_string());

    assert_eq!(result, "abc");
    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 1);
    let attrs = &spans[0].attributes;
    assert!(
        attrs
            .iter()
            .any(|kv| kv.key.as_str() == "component" && kv.value.as_str() == "proxy")
    );
    assert!(
        attrs
            .iter()
            .any(|kv| kv.key.as_str() == "request_id" && kv.value.as_str() == "abc")
    );
}

#[tokio::test]
async fn async_function_is_instrumented_when_enabled() {
    let (instr, exporter) = instr("A");
    instr
        .enable(&[concat!(module_path!(), "::plain_async")])
        .unwrap();

    let result = plain_async(1).await;

    assert_eq!(result, 2);
    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].name, "plain_async");
}

#[test]
fn nested_spans_share_a_trace_and_correct_parent() {
    let (instr, exporter) = instr("A");
    instr
        .enable(&["nesting::parent", "nesting::child"])
        .unwrap();

    let result = parent();

    assert_eq!(result, 7);
    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 2);
    let child_span = spans.iter().find(|s| s.name == "nesting::child").unwrap();
    let parent_span = spans.iter().find(|s| s.name == "nesting::parent").unwrap();
    assert_eq!(
        child_span.parent_span_id,
        parent_span.span_context.span_id()
    );
    assert_eq!(
        child_span.span_context.trace_id(),
        parent_span.span_context.trace_id()
    );
}

#[test]
fn disabling_parent_does_not_suppress_enabled_child() {
    let (instr, exporter) = instr("A");
    // Only the child is enabled -- the parent function still runs (and still
    // calls the child) but must not itself be wrapped in a span, and must not
    // prevent the child's span from being recorded.
    instr.enable(&["nesting::child"]).unwrap();

    let result = parent();

    assert_eq!(result, 7);
    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].name, "nesting::child");
}

#[test]
fn method_inside_impl_block_is_instrumented() {
    let (instr, exporter) = instr("A");
    instr.enable(&["widget::render"]).unwrap();

    let result = Widget.render();

    assert_eq!(result, 99);
    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].name, "widget::render");
}

#[test]
fn set_enabled_replaces_the_active_subset() {
    let (instr, exporter) = instr("A");
    instr
        .enable(&[concat!(module_path!(), "::plain"), "custom.span"])
        .unwrap();
    instr.set_enabled(&["custom.span"]).unwrap();

    plain(1);
    named();

    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].name, "custom.span");
}

// Registration happens at compile time (the linkme-collected static lives
// inside each function's body), not on first call -- so every traceable fn
// should show up here even without being invoked in this test.
#[test]
fn registry_discovers_all_traceable_functions_in_this_binary() {
    let names: std::collections::HashSet<_> = opentelemetry_traceable::registry::keys()
        .iter()
        .copied()
        .collect();
    assert!(names.contains("custom.span"));
    assert!(names.contains("nesting::parent"));
    assert!(names.contains("nesting::child"));
    assert!(names.contains("widget::render"));
}

#[test]
fn enabled_names_reflects_the_active_subset() {
    let (instr, _exporter) = instr("A");
    instr
        .enable(&["nesting::parent", "nesting::child"])
        .unwrap();

    // Compared as an ordered slice rather than a set: `enabled_names` sorts, and
    // that reproducibility is a promise now that there's no id order to inherit.
    assert_eq!(enabled(&instr), ["nesting::child", "nesting::parent"]);
}

// A site's enabled state is the *union* across instrumentations, and each one
// owns exactly its own bit. Pinned explicitly because it is the invariant any
// future consolidation of the per-site masks has to preserve: a derived
// "is anyone tracing this site" summary must stay set while *any* instrumentation
// still wants the site, and must not be clobbered by another one disabling it.
#[test]
fn one_instrumentation_disabling_a_site_leaves_the_others_alone() {
    let (a, exporter_a) = instr("A");
    let (b, exporter_b) = instr("B");

    a.enable(&["nesting::child"]).unwrap();
    b.enable(&["nesting::child"]).unwrap();
    assert_eq!(enabled(&a), ["nesting::child"]);
    assert_eq!(enabled(&b), ["nesting::child"]);

    a.disable(&["nesting::child"]).unwrap();

    assert!(
        enabled(&a).is_empty(),
        "A disabled it, so A must see nothing"
    );
    assert_eq!(
        enabled(&b),
        ["nesting::child"],
        "B never disabled it, so B must be untouched"
    );

    child();
    assert!(
        exporter_a.get_finished_spans().unwrap().is_empty(),
        "A must collect no span after disabling"
    );
    assert_eq!(
        exporter_b.get_finished_spans().unwrap().len(),
        1,
        "B must still collect its span"
    );
}

// --- Dynamic reconfiguration -------------------------------------------------
// Hot-reload drives these paths on every `config.yaml` save, so they get explicit
// coverage: repeated reconfiguration must not accumulate stale state, concurrent
// reconfiguration must not lose an update, and rebuilding an instrumentation while
// traced functions are running must stay consistent.

#[test]
fn repeated_reconfiguration_leaves_no_stale_state() {
    let (instr, exporter) = instr("A");

    for _ in 0..50 {
        instr.set_enabled(&["nesting::parent"]).unwrap();
        assert_eq!(enabled(&instr), ["nesting::parent"]);

        instr.set_enabled(&["nesting::child"]).unwrap();
        assert_eq!(enabled(&instr), ["nesting::child"]);

        instr.set_enabled::<&str>(&[]).unwrap();
        assert!(enabled(&instr).is_empty());
    }

    // Nothing traced during the loop, so the final state alone decides what runs.
    instr.set_enabled(&["nesting::child"]).unwrap();
    parent();

    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 1, "only the last configuration should apply");
    assert_eq!(spans[0].name, "nesting::child");
}

#[test]
fn concurrent_reconfiguration_of_two_instrumentations_loses_nothing() {
    let (a, _exporter_a) = instr("A");
    let (b, _exporter_b) = instr("B");

    // Each instrumentation owns its own bit, so hammering both at once must leave
    // both final states intact rather than one clobbering the other.
    std::thread::scope(|scope| {
        scope.spawn(|| {
            for _ in 0..200 {
                a.set_enabled(&["nesting::parent"]).unwrap();
            }
        });
        scope.spawn(|| {
            for _ in 0..200 {
                b.set_enabled(&["nesting::child"]).unwrap();
            }
        });
    });

    assert_eq!(enabled(&a), ["nesting::parent"]);
    assert_eq!(enabled(&b), ["nesting::child"]);
}

#[test]
fn rebuilding_an_instrumentation_under_load_stays_consistent() {
    // Identity changes in `config.yaml` tear an instrumentation down and build a
    // fresh one, recycling its slot -- while traced functions keep running. Each
    // generation must only ever collect spans for keys it actually enabled.
    let stop = std::sync::atomic::AtomicBool::new(false);

    std::thread::scope(|scope| {
        scope.spawn(|| {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                parent();
            }
        });

        for _ in 0..50 {
            let (instr, exporter) = instr("churn");
            instr
                .enable(&["nesting::parent", "nesting::child"])
                .unwrap();
            parent();
            for span in exporter.get_finished_spans().unwrap() {
                assert!(
                    span.name == "nesting::parent" || span.name == "nesting::child",
                    "collected a span for a key this instrumentation never enabled: {}",
                    span.name
                );
            }
            drop(instr);
        }

        stop.store(true, std::sync::atomic::Ordering::Relaxed);
    });
}

#[test]
fn set_enabled_toggles_spans_by_key() {
    let (instr, exporter) = instr("A");

    instr.set_enabled(&["nesting::child"]).unwrap();
    let result = parent();

    assert_eq!(result, 7);
    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].name, "nesting::child");
}

#[test]
fn enable_and_disable_are_additive_and_subtractive() {
    let (instr, exporter) = instr("A");

    instr
        .enable(&["nesting::parent", "nesting::child"])
        .unwrap();
    instr.disable(&["nesting::parent"]).unwrap();
    let result = parent();

    assert_eq!(result, 7);
    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].name, "nesting::child");
}

#[test]
fn a_glob_enables_every_key_beneath_it() {
    let (instr, exporter) = instr("A");
    let selection = instr.set_enabled(&["nesting::*"]).unwrap();

    assert_eq!(selection.keys, ["nesting::child", "nesting::parent"]);
    parent();

    let mut names: Vec<String> = exporter
        .get_finished_spans()
        .unwrap()
        .iter()
        .map(|span| span.name.to_string())
        .collect();
    names.sort();
    assert_eq!(names, ["nesting::child", "nesting::parent"]);
}

#[test]
fn a_bare_glob_selects_the_same_set_as_enable_all() {
    let (via_glob, _exporter) = instr("A");
    via_glob.set_enabled(&["*"]).unwrap();
    let through_glob = enabled(&via_glob);

    assert_eq!(through_glob, opentelemetry_traceable::registry::keys());
}

#[test]
fn an_empty_selector_list_clears_everything() {
    let (instr, exporter) = instr("A");
    instr.enable(&["nesting::*"]).unwrap();

    let cleared = instr.set_enabled::<&str>(&[]).unwrap();

    assert!(cleared.keys.is_empty());
    assert!(enabled(&instr).is_empty());
    parent();
    assert!(exporter.get_finished_spans().unwrap().is_empty());
}

#[test]
fn a_glob_matching_nothing_is_reported_but_applies_the_rest() {
    let (instr, _exporter) = instr("A");

    let selection = instr
        .enable(&["nesting::parent", "no::such::module::*"])
        .unwrap();

    assert_eq!(selection.unmatched_globs, ["no::such::module::*"]);
    assert_eq!(enabled(&instr), ["nesting::parent"]);
}

#[test]
fn an_unknown_exact_key_is_rejected_and_leaves_the_subset_untouched() {
    let (instr, _exporter) = instr("A");
    instr.enable(&["nesting::parent"]).unwrap();

    // The reason resolution happens before any bit is touched: one typo must not
    // partially apply the rest of the list.
    let error = instr
        .set_enabled(&["nesting::child", "nesting::typo"])
        .expect_err("`nesting::typo` is not a registry key");

    assert_eq!(error.keys, ["nesting::typo"]);
    assert_eq!(
        enabled(&instr),
        ["nesting::parent"],
        "a rejected selection must leave the previous subset in place"
    );
}

#[test]
fn two_sites_sharing_a_key_toggle_together() {
    let (instr, exporter) = instr("A");
    let key = concat!(module_path!(), "::shared_method");

    // One selector, two call sites -- the flat registry walk matches both because
    // they carry the same string.
    let selection = instr.set_enabled(&[key]).unwrap();
    assert_eq!(
        selection.keys,
        [key],
        "a shared key is one selectable thing, listed once"
    );

    assert_eq!(Left.shared_method() + Right.shared_method(), 3);

    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 2, "both call sites must have been enabled");
    // The *span* name is the bare fn name; `key` is the module-qualified registry
    // key that selects it. Both sites produce the same span name here too.
    assert!(spans.iter().all(|span| span.name == "shared_method"));
    assert_eq!(
        enabled(&instr),
        [key],
        "and it must still be reported only once"
    );
}

#[test]
fn keys_are_the_sorted_deduped_registry_keys() {
    // This no longer guards identity -- nothing is addressed by position any
    // more, so a different order would change nothing about what a config
    // selects. What it does guard is determinism: neither `keys()` nor the
    // `*_names()` listings may leak the linker's arbitrary `REGISTRY` order,
    // which shifts between builds, out to callers.
    let keys = opentelemetry_traceable::registry::keys();

    let mut expected: Vec<&str> = opentelemetry_traceable::registry::REGISTRY
        .iter()
        .map(|site| site.name)
        .collect();
    expected.sort_unstable();
    expected.dedup();
    assert_eq!(keys, expected);

    assert!(
        keys.windows(2).all(|pair| pair[0] < pair[1]),
        "keys must be strictly increasing, i.e. sorted and de-duplicated"
    );
}

// --- In-process kind: `Context`'s span slot is never read or written --------

#[test]
fn in_process_spans_never_land_in_the_ambient_context_span_slot() {
    let (instr, exporter) = instr("A");
    instr.enable(&["probe::ambient"]).unwrap();

    // The probe reports what it sees in `Context::current().span()` from inside
    // its own traced body. A span *was* created for it (asserted below), but it
    // lives in the extension envelope, so the span slot stays empty -- which is
    // exactly why outbound `traceparent` injection carries nothing from an
    // in-process instrumentation.
    let saw_ambient_span = probe_ambient();

    assert!(
        !saw_ambient_span,
        "an in-process span must not occupy Context's span slot"
    );
    assert_eq!(exporter.get_finished_spans().unwrap().len(), 1);
}

#[test]
fn in_process_spans_are_not_injected_into_an_outbound_carrier() {
    let (instr, exporter) = instr("A");
    instr.enable(&["probe::inject"]).unwrap();

    let carrier = probe_inject();

    assert_eq!(exporter.get_finished_spans().unwrap().len(), 1);
    assert!(
        !carrier.contains_key("traceparent"),
        "a propagator reads Context's span slot, which an in-process span never occupies"
    );
}

#[test]
fn in_process_does_not_join_an_ambient_incoming_parent() {
    let (instr, exporter) = instr("A");
    instr.enable(&["nesting::parent"]).unwrap();

    let remote = remote_parent();
    let guard = Context::current()
        .with_remote_span_context(remote.clone())
        .attach();

    let _ = parent();
    drop(guard);

    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 1);
    assert_ne!(
        spans[0].span_context.trace_id(),
        remote.trace_id(),
        "an in-process instrumentation roots its own trace rather than joining the ambient one"
    );
    assert_eq!(
        spans[0].parent_span_id,
        SpanId::INVALID,
        "the ambient remote parent must not be adopted"
    );
}

// --- Distributed kind: `Context`'s span slot is the parent and the output ----

#[test]
fn distributed_joins_an_incoming_remote_parent() {
    let (instr, exporter) = distributed_instr("edge");
    instr.enable(&["nesting::parent"]).unwrap();
    assert!(instr.is_distributed());

    // Stands in for a propagator-extracted incoming `traceparent`, or a web
    // framework's server span: a valid span context in the ambient span slot.
    let remote = remote_parent();
    let guard = Context::current()
        .with_remote_span_context(remote.clone())
        .attach();

    let _ = parent();
    drop(guard);

    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(
        spans[0].span_context.trace_id(),
        remote.trace_id(),
        "a distributed instrumentation continues the incoming trace"
    );
    assert_eq!(
        spans[0].parent_span_id,
        remote.span_id(),
        "and hangs off the incoming span"
    );
}

#[test]
fn distributed_span_becomes_the_ambient_current_span() {
    let (instr, exporter) = distributed_instr("edge");
    instr.enable(&["probe::ambient"]).unwrap();

    // The mirror of the in-process case above: the span goes *into* the slot a
    // propagator injects from, which is what makes outbound propagation work.
    let saw_ambient_span = probe_ambient();

    assert!(
        saw_ambient_span,
        "a distributed span must be the current span for the wrapped call"
    );
    assert_eq!(exporter.get_finished_spans().unwrap().len(), 1);
}

#[test]
fn distributed_spans_are_injected_into_an_outbound_carrier() {
    let (instr, exporter) = distributed_instr("edge");
    instr.enable(&["probe::inject"]).unwrap();

    let carrier = probe_inject();

    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 1);
    let traceparent = carrier
        .get("traceparent")
        .expect("a distributed span must be injected by an ordinary propagator");
    // `00-<trace_id>-<span_id>-<flags>`: the downstream service continues *this*
    // span, with no propagator code in this crate.
    assert!(
        traceparent.contains(&format!("{:032x}", spans[0].span_context.trace_id())),
        "carried the wrong trace: {traceparent}"
    );
    assert!(
        traceparent.contains(&format!("{:016x}", spans[0].span_context.span_id())),
        "carried the wrong span: {traceparent}"
    );
}

#[test]
fn distributed_nests_through_the_ambient_span_slot() {
    let (instr, exporter) = distributed_instr("edge");
    instr
        .enable(&["nesting::parent", "nesting::child"])
        .unwrap();

    assert_eq!(parent(), 7);

    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 2);
    let child_span = spans.iter().find(|s| s.name == "nesting::child").unwrap();
    let parent_span = spans.iter().find(|s| s.name == "nesting::parent").unwrap();
    assert_eq!(
        child_span.parent_span_id,
        parent_span.span_context.span_id()
    );
    assert_eq!(
        child_span.span_context.trace_id(),
        parent_span.span_context.trace_id()
    );
}

#[tokio::test]
async fn distributed_nesting_survives_await_points() {
    let (instr, exporter) = distributed_instr("edge");
    instr
        .enable(&["shared::async_root", "shared::async_leaf"])
        .unwrap();

    assert_eq!(shared_async_root().await, 5);

    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 2);
    let leaf = spans
        .iter()
        .find(|s| s.name == "shared::async_leaf")
        .unwrap();
    let root = spans
        .iter()
        .find(|s| s.name == "shared::async_root")
        .unwrap();
    assert_eq!(leaf.parent_span_id, root.span_context.span_id());
    assert_eq!(leaf.span_context.trace_id(), root.span_context.trace_id());
}

#[test]
fn only_one_distributed_instrumentation_may_be_live() {
    let (first, _exporter) = distributed_instr("edge");

    let second = Instrumentation::builder()
        .name("second-edge")
        .tracer(SdkTracerProvider::builder().build().tracer("second-edge"))
        .distributed()
        .build();

    assert_eq!(
        second.err(),
        Some(BuildError::DistributedAlreadyLive),
        "two of them would fight over Context's single span slot"
    );
    assert!(first.is_distributed());

    // An in-process one is unaffected -- the limit is on the role, not on slots.
    let (in_process, _exp) = instr("A");
    assert!(!in_process.is_distributed());
}

#[test]
fn dropping_a_distributed_instrumentation_releases_the_role() {
    // Hot-reload rebuilds an instrumentation whenever its tracer or endpoint
    // changes, so the role has to come back with the slot.
    for _ in 0..3 {
        let (instr, exporter) = distributed_instr("edge");
        instr.enable(&["nesting::child"]).unwrap();
        let _ = child();
        assert_eq!(exporter.get_finished_spans().unwrap().len(), 1);
        drop(instr);
    }
}

#[test]
fn a_distributed_and_an_in_process_instrumentation_stay_isolated() {
    let (dist, exp_dist) = distributed_instr("edge");
    let (local, exp_local) = instr("A");

    dist.enable(&["nesting::parent", "nesting::child"]).unwrap();
    local
        .enable(&["nesting::parent", "nesting::child"])
        .unwrap();

    // An incoming trace the distributed one must join and the in-process one
    // must ignore.
    let remote = remote_parent();
    let guard = Context::current()
        .with_remote_span_context(remote.clone())
        .attach();
    let _ = parent();
    drop(guard);

    let spans_dist = exp_dist.get_finished_spans().unwrap();
    let spans_local = exp_local.get_finished_spans().unwrap();
    assert_eq!(spans_dist.len(), 2);
    assert_eq!(spans_local.len(), 2);

    assert!(
        spans_dist
            .iter()
            .all(|s| s.span_context.trace_id() == remote.trace_id()),
        "the distributed instrumentation joins the incoming trace"
    );
    assert!(
        spans_local
            .iter()
            .all(|s| s.span_context.trace_id() != remote.trace_id()),
        "the in-process one is untouched by the distributed span in the ambient slot"
    );

    // The in-process child parents to the in-process parent, not to the
    // distributed span that was current at the time.
    let local_child = spans_local
        .iter()
        .find(|s| s.name == "nesting::child")
        .unwrap();
    let local_parent = spans_local
        .iter()
        .find(|s| s.name == "nesting::parent")
        .unwrap();
    assert_eq!(
        local_child.parent_span_id,
        local_parent.span_context.span_id()
    );
}

// --- Multiple, independently-configured instrumentations -------------------

#[test]
fn two_instrumentations_isolate_their_spans_and_traces() {
    let (a, exp_a) = instr("A");
    let (b, exp_b) = instr("B");

    // Overlapping enabled sets: both trace the parent, only A traces the child.
    a.enable(&["nesting::parent", "nesting::child"]).unwrap();
    b.enable(&["nesting::parent"]).unwrap();

    let _ = parent();

    let spans_a = exp_a.get_finished_spans().unwrap();
    let spans_b = exp_b.get_finished_spans().unwrap();

    let names_a: std::collections::HashSet<_> = spans_a.iter().map(|s| s.name.as_ref()).collect();
    let names_b: std::collections::HashSet<_> = spans_b.iter().map(|s| s.name.as_ref()).collect();
    assert_eq!(names_a, ["nesting::parent", "nesting::child"].into());
    assert_eq!(names_b, ["nesting::parent"].into());

    // No trace ID ever crosses between the two exporters.
    let traces_a: std::collections::HashSet<_> =
        spans_a.iter().map(|s| s.span_context.trace_id()).collect();
    let traces_b: std::collections::HashSet<_> =
        spans_b.iter().map(|s| s.span_context.trace_id()).collect();
    assert!(traces_a.is_disjoint(&traces_b));
}

#[test]
fn each_instrumentation_nests_independently() {
    let (a, exp_a) = instr("A");
    let (b, exp_b) = instr("B");

    // A traces one parent/child pair, B a different one.
    a.enable(&["nesting::parent", "nesting::child"]).unwrap();
    b.enable(&["shared::root", "shared::leaf"]).unwrap();

    let _ = parent();
    let _ = shared_root();

    let sa = exp_a.get_finished_spans().unwrap();
    assert_eq!(sa.len(), 2, "A sees only its own pair");
    let ap = sa.iter().find(|s| s.name == "nesting::parent").unwrap();
    let ac = sa.iter().find(|s| s.name == "nesting::child").unwrap();
    assert_eq!(ac.parent_span_id, ap.span_context.span_id());
    assert_eq!(ac.span_context.trace_id(), ap.span_context.trace_id());

    let sb = exp_b.get_finished_spans().unwrap();
    assert_eq!(sb.len(), 2, "B sees only its own pair");
    let br = sb.iter().find(|s| s.name == "shared::root").unwrap();
    let bl = sb.iter().find(|s| s.name == "shared::leaf").unwrap();
    assert_eq!(bl.parent_span_id, br.span_context.span_id());
    assert_eq!(bl.span_context.trace_id(), br.span_context.trace_id());
}

#[test]
fn the_same_function_traced_by_two_instrumentations_yields_separate_traces() {
    let (a, exp_a) = instr("A");
    let (b, exp_b) = instr("B");

    // The same function enabled in both -- each produces its own span in its
    // own tracer/backend, in its own trace.
    a.enable(&["nesting::parent"]).unwrap();
    b.enable(&["nesting::parent"]).unwrap();

    let _ = parent();

    let sa = exp_a.get_finished_spans().unwrap();
    let sb = exp_b.get_finished_spans().unwrap();
    assert_eq!(sa.len(), 1);
    assert_eq!(sb.len(), 1);
    assert_eq!(sa[0].name, "nesting::parent");
    assert_eq!(sb[0].name, "nesting::parent");
    assert_ne!(
        sa[0].span_context.trace_id(),
        sb[0].span_context.trace_id(),
        "each instrumentation builds a separate trace"
    );
}

#[test]
fn dropping_an_instrumentation_stops_new_spans_and_frees_its_slot() {
    let (a, exp_a) = instr("A");
    a.enable(&["nesting::parent"]).unwrap();
    let _ = parent();
    assert_eq!(exp_a.get_finished_spans().unwrap().len(), 1);

    exp_a.reset();
    drop(a);
    let _ = parent();
    assert!(
        exp_a.get_finished_spans().unwrap().is_empty(),
        "no new spans for a dropped instrumentation's slot"
    );

    // A fresh instrumentation still allocates cleanly and works -- and since the
    // dropped one released its slot, this may well be the very same slot.
    let (c, exp_c) = instr("C");
    c.enable(&["nesting::parent"]).unwrap();
    let _ = parent();
    assert_eq!(exp_c.get_finished_spans().unwrap().len(), 1);
}

#[test]
fn slots_are_reused_so_churn_does_not_exhaust_them() {
    // Build and drop well past MAX_INSTRUMENTATIONS one at a time. With a bump
    // allocator this would fail partway through; with slot reuse it can't.
    let total = opentelemetry_traceable::instrumentation::MAX_INSTRUMENTATIONS * 3;
    for i in 0..total {
        let (one, exporter) = instr("churn");
        one.enable(&["nesting::child"]).unwrap();
        let _ = child();
        assert_eq!(
            exporter.get_finished_spans().unwrap().len(),
            1,
            "instrumentation #{i} of {total} should still trace"
        );
    }
}

#[tokio::test]
async fn instrumentation_works_across_await_points() {
    let (a, exp_a) = instr("A");
    a.enable(&["shared::async_root", "shared::async_leaf"])
        .unwrap();

    let _ = shared_async_root().await;

    let sa = exp_a.get_finished_spans().unwrap();
    assert_eq!(sa.len(), 2);
    let root = sa.iter().find(|s| s.name == "shared::async_root").unwrap();
    let leaf = sa.iter().find(|s| s.name == "shared::async_leaf").unwrap();
    assert_eq!(leaf.parent_span_id, root.span_context.span_id());
    assert_eq!(leaf.span_context.trace_id(), root.span_context.trace_id());
}
