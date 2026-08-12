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

use opentelemetry::Context;
use opentelemetry::trace::{
    SpanContext, SpanId, TraceContextExt, TraceFlags, TraceId, TracerProvider as _,
};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use opentelemetry_traceable::instrumentation::Instrumentation;
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

/// Encode a subset by name: resolve each name to its registry id via the
/// catalog (the same mapping an application would use), then delta-encode the
/// ids. Panics if a name isn't a known trace site.
fn encode_names(names: &[&str]) -> String {
    let catalog = opentelemetry_traceable::catalog::catalog();
    let mut ids: Vec<u64> = names
        .iter()
        .map(|name| {
            catalog
                .functions
                .iter()
                .find(|f| f.name == *name)
                .unwrap_or_else(|| panic!("no traceable function named `{name}`"))
                .id
        })
        .collect();
    opentelemetry_traceable::codec::encode(&mut ids)
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
/// used to prove instrumentations never write into it.
#[traceable(name = "probe::ambient")]
fn probe_ambient() -> bool {
    Context::current().span().span_context().is_valid()
}

struct Widget;

impl Widget {
    #[traceable(name = "widget::render")]
    fn render(&self) -> u64 {
        99
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
    instr
        .enable_encoded(&encode_names(&[concat!(module_path!(), "::plain")]))
        .unwrap();

    let result = plain(1);

    assert_eq!(result, 2);
    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].name, "plain");
}

#[test]
fn custom_span_name_and_the_instrumentations_tracer_scope() {
    let (instr, exporter) = instr("test-tracer");
    instr
        .enable_encoded(&encode_names(&["custom.span"]))
        .unwrap();

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
        .enable_encoded(&encode_names(&[concat!(module_path!(), "::with_fields")]))
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
        .enable_encoded(&encode_names(&[concat!(module_path!(), "::plain_async")]))
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
        .enable_encoded(&encode_names(&["nesting::parent", "nesting::child"]))
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
    instr
        .enable_encoded(&encode_names(&["nesting::child"]))
        .unwrap();

    let result = parent();

    assert_eq!(result, 7);
    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].name, "nesting::child");
}

#[test]
fn method_inside_impl_block_is_instrumented() {
    let (instr, exporter) = instr("A");
    instr
        .enable_encoded(&encode_names(&["widget::render"]))
        .unwrap();

    let result = Widget.render();

    assert_eq!(result, 99);
    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].name, "widget::render");
}

#[test]
fn set_enabled_encoded_replaces_the_active_subset() {
    let (instr, exporter) = instr("A");
    instr
        .enable_encoded(&encode_names(&[
            concat!(module_path!(), "::plain"),
            "custom.span",
        ]))
        .unwrap();
    instr
        .set_enabled_encoded(&encode_names(&["custom.span"]))
        .unwrap();

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
    let names: std::collections::HashSet<_> =
        opentelemetry_traceable::catalog::all_names().collect();
    assert!(names.contains("custom.span"));
    assert!(names.contains("nesting::parent"));
    assert!(names.contains("nesting::child"));
    assert!(names.contains("widget::render"));
}

#[test]
fn enabled_names_reflects_the_active_subset() {
    let (instr, _exporter) = instr("A");
    instr
        .enable_encoded(&encode_names(&["nesting::parent", "nesting::child"]))
        .unwrap();

    let enabled: std::collections::HashSet<_> = instr.enabled_names().collect();
    assert_eq!(enabled, ["nesting::parent", "nesting::child"].into());
}

#[test]
fn set_enabled_encoded_toggles_spans_via_an_encoded_id_list() {
    let (instr, exporter) = instr("A");
    let encoded = encode_names(&["nesting::child"]);

    instr.set_enabled_encoded(&encoded).unwrap();
    let result = parent();

    assert_eq!(result, 7);
    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].name, "nesting::child");
}

#[test]
fn enable_and_disable_encoded_are_additive_and_subtractive() {
    let (instr, exporter) = instr("A");
    let both = encode_names(&["nesting::parent", "nesting::child"]);
    let just_parent = encode_names(&["nesting::parent"]);

    instr.enable_encoded(&both).unwrap();
    instr.disable_encoded(&just_parent).unwrap();
    let result = parent();

    assert_eq!(result, 7);
    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].name, "nesting::child");
}

#[test]
fn set_enabled_encoded_rejects_a_corrupt_encoded_id_list() {
    let (instr, _exporter) = instr("A");
    assert!(instr.set_enabled_encoded("not a valid list!!").is_err());
}

#[test]
fn catalog_reports_every_traceable_function_with_a_matching_id() {
    // Touch every traceable fn once so its local static is linked in.
    let _ = plain(0);
    named();
    let _ = with_fields(String::new());
    parent();
    Widget.render();

    let catalog = opentelemetry_traceable::catalog::catalog();

    // Ids are dense registry indices: `0..n`, each reported exactly once.
    let mut ids: Vec<u64> = catalog.functions.iter().map(|f| f.id).collect();
    ids.sort_unstable();
    let n = ids.len() as u64;
    assert_eq!(ids, (0..n).collect::<Vec<_>>(), "ids must be a dense 0..n");

    // The catalog's name->id mapping is what an encoded subset is built from,
    // so encoding a name via the catalog and applying it must enable exactly
    // that function.
    let by_name: std::collections::HashMap<_, _> =
        catalog.functions.iter().map(|f| (f.name, f.id)).collect();
    for name in [
        "custom.span",
        "nesting::parent",
        "nesting::child",
        "widget::render",
    ] {
        assert!(by_name.contains_key(name), "{name} missing from catalog");
        let decoded = opentelemetry_traceable::codec::decode(&encode_names(&[name])).unwrap();
        assert_eq!(decoded, vec![by_name[name]]);
    }

    // catalog_json() must be well-formed JSON containing the same data.
    let json = opentelemetry_traceable::catalog::catalog_json();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(parsed["functions"].as_array().unwrap().len() >= catalog.functions.len());
}

#[test]
fn ids_depend_only_on_the_sorted_set_of_registry_keys() {
    // An id is an index into the sorted set of registry keys -- never a raw
    // `REGISTRY` position, and with no dependence on source location. That's
    // what makes an encoded subset survive a rebuild: nothing about linker
    // layout, an unrelated edit, moving a function within its file, or the build
    // profile can perturb it. Only adding/removing a key can.
    let names = opentelemetry_traceable::registry::names_by_id();

    let mut expected: Vec<&str> = opentelemetry_traceable::catalog::all_names().collect();
    expected.sort_unstable();
    expected.dedup();
    assert_eq!(
        names, expected,
        "ids must follow the sorted, de-duplicated key order"
    );

    // Ids are dense 0..n: the catalog is exactly the sorted key order, position
    // for position, with each entry's id equal to its index.
    let catalog = opentelemetry_traceable::catalog::catalog();
    assert_eq!(catalog.functions.len(), names.len());
    for (id, name) in names.iter().enumerate() {
        let entry = &catalog.functions[id];
        assert_eq!(entry.id, id as u64);
        assert_eq!(entry.name, *name);
    }
}

// --- In-process only: `Context`'s span slot is never read or written ---------

#[test]
fn spans_never_land_in_the_ambient_context_span_slot() {
    let (instr, exporter) = instr("A");
    instr
        .enable_encoded(&encode_names(&["probe::ambient"]))
        .unwrap();

    // The probe reports what it sees in `Context::current().span()` from inside
    // its own traced body. A span *was* created for it (asserted below), but it
    // lives in the extension envelope, so the span slot stays empty -- which is
    // exactly why outbound `traceparent` injection carries nothing from opentelemetry-traceable.
    let saw_ambient_span = probe_ambient();

    assert!(
        !saw_ambient_span,
        "an instrumentation's span must not occupy Context's span slot"
    );
    assert_eq!(exporter.get_finished_spans().unwrap().len(), 1);
}

#[test]
fn does_not_join_an_ambient_incoming_parent() {
    let (instr, exporter) = instr("A");
    instr
        .enable_encoded(&encode_names(&["nesting::parent"]))
        .unwrap();

    // Stands in for a propagator-extracted incoming `traceparent`, or a web
    // framework's server span: a valid span context in the ambient span slot.
    let remote = SpanContext::new(
        TraceId::from_bytes([
            0x0b, 0xad, 0xc0, 0xde, 0xde, 0xad, 0xbe, 0xef, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06,
            0x07, 0x08,
        ]),
        SpanId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0x42]),
        TraceFlags::SAMPLED,
        true,
        Default::default(),
    );
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
        "instrumentations root their own trace rather than joining the ambient one"
    );
    assert_eq!(
        spans[0].parent_span_id,
        SpanId::INVALID,
        "the ambient remote parent must not be adopted"
    );
}

// --- Child-only mode -------------------------------------------------------

#[test]
fn child_only_creates_no_span_without_an_active_parent() {
    let (instr, exporter) = instr("A");
    // Enabled AND put in child-only mode, but called directly with no active
    // parent for this instrumentation -- must stay silent, not orphan a root.
    instr
        .enable_encoded(&encode_names(&["shared::leaf"]))
        .unwrap();
    instr
        .set_child_only_encoded(&encode_names(&["shared::leaf"]))
        .unwrap();

    let result = shared_leaf();

    assert_eq!(result, 5);
    assert!(exporter.get_finished_spans().unwrap().is_empty());
}

#[test]
fn child_only_nests_correctly_under_an_active_parent() {
    let (instr, exporter) = instr("A");
    instr
        .enable_encoded(&encode_names(&["shared::root", "shared::leaf"]))
        .unwrap();
    instr
        .set_child_only_encoded(&encode_names(&["shared::leaf"]))
        .unwrap();

    let result = shared_root();

    assert_eq!(result, 5);
    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 2);
    let leaf_span = spans.iter().find(|s| s.name == "shared::leaf").unwrap();
    let root_span = spans.iter().find(|s| s.name == "shared::root").unwrap();
    assert_eq!(leaf_span.parent_span_id, root_span.span_context.span_id());
}

#[test]
fn child_only_mode_still_respects_the_enabled_flag() {
    let (instr, exporter) = instr("A");
    // Root enabled, leaf's own flag left off -- child-only only relaxes the
    // "needs a parent" requirement, it doesn't bypass the function's own
    // enabled flag.
    instr
        .enable_encoded(&encode_names(&["shared::root"]))
        .unwrap();
    instr
        .set_child_only_encoded(&encode_names(&["shared::leaf"]))
        .unwrap();

    let result = shared_root();

    assert_eq!(result, 5);
    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].name, "shared::root");
}

#[test]
fn child_only_is_a_runtime_mode_the_same_fn_can_root_or_not() {
    // The same enabled function roots a trace when *not* in child-only mode,
    // and stays silent (no orphan) when it *is* -- the whole point of making
    // child-only a runtime decision rather than a source annotation.
    let (instr, exporter) = instr("A");

    // Not child-only: called directly, it roots its own span.
    instr
        .enable_encoded(&encode_names(&["shared::leaf"]))
        .unwrap();
    assert_eq!(shared_leaf(), 5);
    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].name, "shared::leaf");

    // Flip the same function into child-only mode: now it suppresses itself.
    exporter.reset();
    instr
        .set_child_only_encoded(&encode_names(&["shared::leaf"]))
        .unwrap();
    assert_eq!(shared_leaf(), 5);
    assert!(exporter.get_finished_spans().unwrap().is_empty());
}

#[tokio::test]
async fn child_only_works_for_async_functions_too() {
    let (instr, exporter) = instr("A");

    instr
        .enable_encoded(&encode_names(&["shared::async_leaf"]))
        .unwrap();
    instr
        .set_child_only_encoded(&encode_names(&["shared::async_leaf"]))
        .unwrap();
    let result = shared_async_leaf().await;
    assert_eq!(result, 5);
    assert!(
        exporter.get_finished_spans().unwrap().is_empty(),
        "async child-only fn must not orphan without an active parent"
    );

    instr
        .enable_encoded(&encode_names(&["shared::async_root", "shared::async_leaf"]))
        .unwrap();
    let result = shared_async_root().await;
    assert_eq!(result, 5);
    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 2);
    let leaf_span = spans
        .iter()
        .find(|s| s.name == "shared::async_leaf")
        .unwrap();
    let root_span = spans
        .iter()
        .find(|s| s.name == "shared::async_root")
        .unwrap();
    assert_eq!(leaf_span.parent_span_id, root_span.span_context.span_id());
}

#[test]
fn child_only_names_reflects_the_configured_set() {
    let (instr, _exporter) = instr("A");

    instr
        .set_child_only_encoded(&encode_names(&["shared::leaf", "shared::async_leaf"]))
        .unwrap();
    let names: std::collections::HashSet<_> = instr.child_only_names().collect();

    assert!(names.contains("shared::leaf"));
    assert!(names.contains("shared::async_leaf"));
    assert!(!names.contains("shared::root"));
}

// --- Multiple, independently-configured instrumentations -------------------

#[test]
fn two_instrumentations_isolate_their_spans_and_traces() {
    let (a, exp_a) = instr("A");
    let (b, exp_b) = instr("B");

    // Overlapping enabled sets: both trace the parent, only A traces the child.
    a.enable_encoded(&encode_names(&["nesting::parent", "nesting::child"]))
        .unwrap();
    b.enable_encoded(&encode_names(&["nesting::parent"]))
        .unwrap();

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
    a.enable_encoded(&encode_names(&["nesting::parent", "nesting::child"]))
        .unwrap();
    b.enable_encoded(&encode_names(&["shared::root", "shared::leaf"]))
        .unwrap();

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
    a.enable_encoded(&encode_names(&["nesting::parent"]))
        .unwrap();
    b.enable_encoded(&encode_names(&["nesting::parent"]))
        .unwrap();

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
fn child_only_is_per_instrumentation() {
    let (a, exp_a) = instr("A");
    let (b, exp_b) = instr("B");

    // Both enable the shared leaf; A puts it in child-only mode, B leaves it
    // root-capable. Called directly (no parent), A must suppress it, B must not.
    a.enable_encoded(&encode_names(&["shared::leaf"])).unwrap();
    a.set_child_only_encoded(&encode_names(&["shared::leaf"]))
        .unwrap();
    b.enable_encoded(&encode_names(&["shared::leaf"])).unwrap();

    let _ = shared_leaf();

    assert!(
        exp_a.get_finished_spans().unwrap().is_empty(),
        "A: child-only with no parent stays silent"
    );
    let sb = exp_b.get_finished_spans().unwrap();
    assert_eq!(sb.len(), 1, "B: root-capable, so it roots its own span");
    assert_eq!(sb[0].name, "shared::leaf");
}

#[test]
fn dropping_an_instrumentation_stops_new_spans_and_frees_its_slot() {
    let (a, exp_a) = instr("A");
    a.enable_encoded(&encode_names(&["nesting::parent"]))
        .unwrap();
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
    c.enable_encoded(&encode_names(&["nesting::parent"]))
        .unwrap();
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
        one.enable_encoded(&encode_names(&["nesting::child"]))
            .unwrap();
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
    a.enable_encoded(&encode_names(&["shared::async_root", "shared::async_leaf"]))
        .unwrap();

    let _ = shared_async_root().await;

    let sa = exp_a.get_finished_spans().unwrap();
    assert_eq!(sa.len(), 2);
    let root = sa.iter().find(|s| s.name == "shared::async_root").unwrap();
    let leaf = sa.iter().find(|s| s.name == "shared::async_leaf").unwrap();
    assert_eq!(leaf.parent_span_id, root.span_context.span_id());
    assert_eq!(leaf.span_context.trace_id(), root.span_context.trace_id());
}
