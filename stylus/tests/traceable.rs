//! Integration tests for `#[traceable]` span creation and dynamic enable/disable.

use opentelemetry::global;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use stylus::instrumentation::Instrumentation;
use stylus::traceable;

fn setup() -> InMemorySpanExporter {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    global::set_tracer_provider(provider);
    stylus::config::disable_all();
    stylus::config::set_child_only([]);
    exporter
}

/// Encode a subset by name: resolve each name to its registry id via the
/// catalog (the same mapping an application would use), then delta-encode the
/// ids. Panics if a name isn't a known trace site.
fn encode_names(names: &[&str]) -> String {
    let catalog = stylus::catalog::catalog();
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
    stylus::config::encode(&mut ids)
}

#[traceable]
fn plain(x: u64) -> u64 {
    x + 1
}

#[traceable(name = "custom.span", tracer = "test-tracer")]
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

struct Widget;

impl Widget {
    #[traceable(name = "widget::render")]
    fn render(&self) -> u64 {
        99
    }
}

#[test]
fn disabled_by_default_emits_no_span() {
    let exporter = setup();

    let result = plain(1);

    assert_eq!(result, 2);
    assert!(exporter.get_finished_spans().unwrap().is_empty());
}

#[test]
fn enabling_emits_a_span_with_default_name() {
    let exporter = setup();
    stylus::config::enable([concat!(module_path!(), "::plain")]);

    let result = plain(1);

    assert_eq!(result, 2);
    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].name, "plain");
}

#[test]
fn custom_name_and_tracer() {
    let exporter = setup();
    stylus::config::enable(["custom.span"]);

    let result = named();

    assert_eq!(result, 42);
    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].name, "custom.span");
    assert_eq!(spans[0].instrumentation_scope.name(), "test-tracer");
}

#[test]
fn fields_become_span_attributes() {
    let exporter = setup();
    stylus::config::enable([concat!(module_path!(), "::with_fields")]);

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
    let exporter = setup();
    stylus::config::enable([concat!(module_path!(), "::plain_async")]);

    let result = plain_async(1).await;

    assert_eq!(result, 2);
    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].name, "plain_async");
}

#[test]
fn nested_spans_share_a_trace_and_correct_parent() {
    let exporter = setup();
    stylus::config::enable(["nesting::parent", "nesting::child"]);

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
    let exporter = setup();
    // Only the child is enabled -- the parent function still runs (and still
    // calls the child) but must not itself be wrapped in a span, and must not
    // prevent the child's span from being recorded.
    stylus::config::enable(["nesting::child"]);

    let result = parent();

    assert_eq!(result, 7);
    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].name, "nesting::child");
}

#[test]
fn method_inside_impl_block_is_instrumented() {
    let exporter = setup();
    stylus::config::enable(["widget::render"]);

    let result = Widget.render();

    assert_eq!(result, 99);
    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].name, "widget::render");
}

#[test]
fn set_enabled_replaces_the_active_subset() {
    let exporter = setup();
    stylus::config::enable([concat!(module_path!(), "::plain"), "custom.span"]);
    stylus::config::set_enabled(["custom.span"]);

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
    setup();

    let names: std::collections::HashSet<_> = stylus::config::all_names().collect();
    assert!(names.contains("custom.span"));
    assert!(names.contains("nesting::parent"));
    assert!(names.contains("nesting::child"));
    assert!(names.contains("widget::render"));
}

#[test]
fn enabled_names_reflects_the_active_subset() {
    setup();
    stylus::config::enable(["nesting::parent", "nesting::child"]);

    let enabled: std::collections::HashSet<_> = stylus::config::enabled_names().collect();
    assert_eq!(enabled, ["nesting::parent", "nesting::child"].into());
}

#[test]
fn set_enabled_encoded_toggles_spans_via_an_encoded_id_list() {
    let exporter = setup();
    let encoded = encode_names(&["nesting::child"]);

    stylus::config::set_enabled_encoded(&encoded).unwrap();
    let result = parent();

    assert_eq!(result, 7);
    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].name, "nesting::child");
}

#[test]
fn enable_and_disable_encoded_are_additive_and_subtractive() {
    let exporter = setup();
    let both = encode_names(&["nesting::parent", "nesting::child"]);
    let just_parent = encode_names(&["nesting::parent"]);

    stylus::config::enable_encoded(&both).unwrap();
    stylus::config::disable_encoded(&just_parent).unwrap();
    let result = parent();

    assert_eq!(result, 7);
    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].name, "nesting::child");
}

#[test]
fn set_enabled_encoded_rejects_a_corrupt_encoded_id_list() {
    setup();
    assert!(stylus::config::set_enabled_encoded("not a valid list!!").is_err());
}

#[test]
fn catalog_reports_every_traceable_function_with_a_matching_id() {
    setup();
    // Touch every traceable fn once so its local static is linked in.
    let _ = plain(0);
    named();
    let _ = with_fields(String::new());
    parent();
    Widget.render();

    let catalog = stylus::catalog::catalog();

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
        let decoded = stylus::config::decode(&encode_names(&[name])).unwrap();
        assert_eq!(decoded, vec![by_name[name]]);
    }

    // catalog_json() must be well-formed JSON containing the same data.
    let json = stylus::catalog::catalog_json();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(parsed["functions"].as_array().unwrap().len() >= catalog.functions.len());
}

#[test]
fn child_only_creates_no_span_without_an_active_parent() {
    let exporter = setup();
    // Enabled AND put in child-only mode, but called directly with no ambient
    // span -- must stay silent, not become an orphan root.
    stylus::config::enable(["shared::leaf"]);
    stylus::config::set_child_only(["shared::leaf"]);

    let result = shared_leaf();

    assert_eq!(result, 5);
    assert!(exporter.get_finished_spans().unwrap().is_empty());
}

#[test]
fn child_only_nests_correctly_under_an_active_parent() {
    let exporter = setup();
    stylus::config::enable(["shared::root", "shared::leaf"]);
    stylus::config::set_child_only(["shared::leaf"]);

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
    let exporter = setup();
    // Root enabled, leaf's own flag left off -- child-only only relaxes the
    // "needs a parent" requirement, it doesn't bypass the function's own
    // enabled flag.
    stylus::config::enable(["shared::root"]);
    stylus::config::set_child_only(["shared::leaf"]);

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
    let exporter = setup();

    // Not child-only: called directly, it roots its own span.
    stylus::config::enable(["shared::leaf"]);
    assert_eq!(shared_leaf(), 5);
    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].name, "shared::leaf");

    // Flip the same function into child-only mode: now it suppresses itself.
    exporter.reset();
    stylus::config::set_child_only(["shared::leaf"]);
    assert_eq!(shared_leaf(), 5);
    assert!(exporter.get_finished_spans().unwrap().is_empty());
}

#[tokio::test]
async fn child_only_works_for_async_functions_too() {
    let exporter = setup();

    stylus::config::enable(["shared::async_leaf"]);
    stylus::config::set_child_only(["shared::async_leaf"]);
    let result = shared_async_leaf().await;
    assert_eq!(result, 5);
    assert!(
        exporter.get_finished_spans().unwrap().is_empty(),
        "async child-only fn must not orphan without an active parent"
    );

    stylus::config::enable(["shared::async_root", "shared::async_leaf"]);
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
    setup();

    stylus::config::set_child_only(["shared::leaf", "shared::async_leaf"]);
    let names: std::collections::HashSet<_> = stylus::config::child_only_names().collect();

    assert!(names.contains("shared::leaf"));
    assert!(names.contains("shared::async_leaf"));
    assert!(!names.contains("shared::root"));
}

// --- Multiple, independently-configured instrumentations -------------------

/// Build a named instrumentation with its own in-memory exporter/provider.
/// The returned exporter observes only that instrumentation's spans; the
/// tracer keeps its provider alive for the instrumentation's lifetime.
fn named_instr(name: &'static str) -> (Instrumentation, InMemorySpanExporter) {
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

#[test]
fn two_instrumentations_isolate_their_spans_and_traces() {
    setup();
    let (a, exp_a) = named_instr("A");
    let (b, exp_b) = named_instr("B");

    // Overlapping enabled sets: both trace the parent, only A traces the child.
    a.enable(["nesting::parent", "nesting::child"]);
    b.enable(["nesting::parent"]);

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
    setup();
    let (a, exp_a) = named_instr("A");
    let (b, exp_b) = named_instr("B");

    // A traces one parent/child pair, B a different one.
    a.enable(["nesting::parent", "nesting::child"]);
    b.enable(["shared::root", "shared::leaf"]);

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
fn default_and_named_coexist_with_independent_traces() {
    let exp_default = setup();
    let (named, exp_named) = named_instr("N");

    // The same function enabled in both the default instrumentation and a named
    // one -- each produces its own span in its own tracer/backend.
    stylus::config::enable(["nesting::parent"]);
    named.enable(["nesting::parent"]);

    let _ = parent();

    let sd = exp_default.get_finished_spans().unwrap();
    let sn = exp_named.get_finished_spans().unwrap();
    assert_eq!(sd.len(), 1);
    assert_eq!(sn.len(), 1);
    assert_eq!(sd[0].name, "nesting::parent");
    assert_eq!(sn[0].name, "nesting::parent");
    assert_ne!(
        sd[0].span_context.trace_id(),
        sn[0].span_context.trace_id(),
        "default and named instrumentations build separate traces"
    );
}

#[test]
fn child_only_is_per_instrumentation() {
    setup();
    let (a, exp_a) = named_instr("A");
    let (b, exp_b) = named_instr("B");

    // Both enable the shared leaf; A puts it in child-only mode, B leaves it
    // root-capable. Called directly (no parent), A must suppress it, B must not.
    a.enable(["shared::leaf"]);
    a.set_child_only(["shared::leaf"]);
    b.enable(["shared::leaf"]);

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
fn dropping_an_instrumentation_stops_new_spans_and_frees_reuse() {
    setup();
    let (a, exp_a) = named_instr("A");
    a.enable(["nesting::parent"]);
    let _ = parent();
    assert_eq!(exp_a.get_finished_spans().unwrap().len(), 1);

    exp_a.reset();
    drop(a);
    let _ = parent();
    assert!(
        exp_a.get_finished_spans().unwrap().is_empty(),
        "no new spans for a dropped instrumentation's slot"
    );

    // A fresh instrumentation still allocates cleanly and works.
    let (c, exp_c) = named_instr("C");
    c.enable(["nesting::parent"]);
    let _ = parent();
    assert_eq!(exp_c.get_finished_spans().unwrap().len(), 1);
}

#[tokio::test]
async fn named_instrumentation_works_across_await_points() {
    setup();
    let (a, exp_a) = named_instr("A");
    a.enable(["shared::async_root", "shared::async_leaf"]);

    let _ = shared_async_root().await;

    let sa = exp_a.get_finished_spans().unwrap();
    assert_eq!(sa.len(), 2);
    let root = sa.iter().find(|s| s.name == "shared::async_root").unwrap();
    let leaf = sa.iter().find(|s| s.name == "shared::async_leaf").unwrap();
    assert_eq!(leaf.parent_span_id, root.span_context.span_id());
    assert_eq!(leaf.span_context.trace_id(), root.span_context.trace_id());
}
