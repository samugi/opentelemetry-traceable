//! Integration tests for `#[traceable]` span creation and dynamic enable/disable.

use opentelemetry::global;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use stylus::traceable;

fn setup() -> InMemorySpanExporter {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    global::set_tracer_provider(provider);
    stylus::config::disable_all();
    exporter
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

#[traceable(name = "child_only::root")]
fn child_only_root() -> u64 {
    child_only_leaf()
}

#[traceable(name = "child_only::leaf", child_only)]
fn child_only_leaf() -> u64 {
    5
}

#[traceable(name = "child_only::async_root")]
async fn child_only_async_root() -> u64 {
    child_only_async_leaf().await
}

#[traceable(name = "child_only::async_leaf", child_only)]
async fn child_only_async_leaf() -> u64 {
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
fn set_enabled_encoded_toggles_spans_via_a_blob() {
    let exporter = setup();
    let blob = stylus::subset::encode(["nesting::child"], 0.01);

    stylus::config::set_enabled_encoded(&blob).unwrap();
    let result = parent();

    assert_eq!(result, 7);
    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].name, "nesting::child");
}

#[test]
fn enable_and_disable_encoded_are_additive_and_subtractive() {
    let exporter = setup();
    let both = stylus::subset::encode(["nesting::parent", "nesting::child"], 0.01);
    let just_parent = stylus::subset::encode(["nesting::parent"], 0.01);

    stylus::config::enable_encoded(&both).unwrap();
    stylus::config::disable_encoded(&just_parent).unwrap();
    let result = parent();

    assert_eq!(result, 7);
    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].name, "nesting::child");
}

#[test]
fn set_enabled_encoded_rejects_a_corrupt_blob() {
    setup();
    assert!(stylus::config::set_enabled_encoded("not a valid blob!!").is_err());
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
    let by_name: std::collections::HashMap<_, _> =
        catalog.functions.iter().map(|f| (f.name, f.id)).collect();

    assert_eq!(catalog.hash, "fnv1a64");
    for name in [
        "custom.span",
        "nesting::parent",
        "nesting::child",
        "widget::render",
    ] {
        assert_eq!(by_name[name], stylus::subset::id_of(name));
    }

    // catalog_json() must be well-formed JSON containing the same data.
    let json = stylus::catalog::catalog_json();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(parsed["functions"].as_array().unwrap().len() >= catalog.functions.len());
}

#[test]
fn child_only_creates_no_span_without_an_active_parent() {
    let exporter = setup();
    // Enabled, but called directly with no ambient span -- must stay silent,
    // not become an orphan root.
    stylus::config::enable(["child_only::leaf"]);

    let result = child_only_leaf();

    assert_eq!(result, 5);
    assert!(exporter.get_finished_spans().unwrap().is_empty());
}

#[test]
fn child_only_nests_correctly_under_an_active_parent() {
    let exporter = setup();
    stylus::config::enable(["child_only::root", "child_only::leaf"]);

    let result = child_only_root();

    assert_eq!(result, 5);
    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 2);
    let leaf_span = spans.iter().find(|s| s.name == "child_only::leaf").unwrap();
    let root_span = spans.iter().find(|s| s.name == "child_only::root").unwrap();
    assert_eq!(leaf_span.parent_span_id, root_span.span_context.span_id());
}

#[test]
fn child_only_stays_disabled_even_with_an_active_parent() {
    let exporter = setup();
    // Root enabled, leaf's own flag left off -- child_only only relaxes the
    // "needs a parent" requirement, it doesn't bypass the function's own
    // enabled flag.
    stylus::config::enable(["child_only::root"]);

    let result = child_only_root();

    assert_eq!(result, 5);
    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].name, "child_only::root");
}

#[tokio::test]
async fn child_only_works_for_async_functions_too() {
    let exporter = setup();

    stylus::config::enable(["child_only::async_leaf"]);
    let result = child_only_async_leaf().await;
    assert_eq!(result, 5);
    assert!(
        exporter.get_finished_spans().unwrap().is_empty(),
        "async child_only fn must not orphan without an active parent"
    );

    stylus::config::enable(["child_only::async_root", "child_only::async_leaf"]);
    let result = child_only_async_root().await;
    assert_eq!(result, 5);
    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 2);
    let leaf_span = spans
        .iter()
        .find(|s| s.name == "child_only::async_leaf")
        .unwrap();
    let root_span = spans
        .iter()
        .find(|s| s.name == "child_only::async_root")
        .unwrap();
    assert_eq!(leaf_span.parent_span_id, root_span.span_context.span_id());
}

#[test]
fn catalog_reports_child_only_correctly() {
    setup();

    let catalog = stylus::catalog::catalog();
    let by_name: std::collections::HashMap<_, _> = catalog
        .functions
        .iter()
        .map(|f| (f.name, f.child_only))
        .collect();

    assert!(by_name["child_only::leaf"]);
    assert!(!by_name["child_only::root"]);
}
