# Design: multiple parallel instrumentations

> Status: **implemented**. This document is the design record for the
> multi-instrumentation feature (`stylus::instrumentation`). It captures the
> problem, the approach, the alternatives that were rejected and *why*, and the
> measured performance outcome.

## Problem

Originally `stylus` was single-config: each `#[traceable]` function's
`TraceSite` carried one `enabled: AtomicBool` + one `child_only: AtomicBool`,
and the macro's enabled path always reached for the single process-wide
`opentelemetry::global::tracer(name)` and nested under the single ambient
`Context::current()`. You could not run two independent tracing configurations
at once — different enabled subsets, isolated span hierarchies,
different tracers/backends.

The goal: create **named instrumentations** at runtime, each with its own
enabled subset, its own isolated span tree built from the *same* underlying call
flow, and its own `Tracer` (possibly a different backend). Hard constraint:
**no measurable performance regression** for existing/default usage.

## Answering the central question: "many contexts in a map?"

The natural intuition is "give each instrumentation its own `Context` and hold
them in a map." That intuition is *directionally* right but has four hard
constraints, each verified by reading the vendored `opentelemetry` 0.32 source:

1. Each instrumentation genuinely *does* get its own isolated `Context` lineage
   (own span, own parent chain, own tracer). That part we build.
2. **Whatever holds "current parent per active instrumentation" must be
   propagated with the logical call.** It has to survive async `.await`
   thread-hops, and two concurrent calls traced by the same instrumentation must
   not clobber each other's current-parent. A plain global/static map breaks
   under concurrency; a thread-local breaks under `tokio` work-stealing. The
   only correct carriers are OTel's `Context` (propagated via `.attach()` /
   `FutureExt::with_context`) or a task-local. So "the map" cannot be
   free-floating — it rides *inside a propagated `Context`*.
3. **You cannot have N independent ambient OTel contexts.** `opentelemetry`
   keeps exactly one thread-local current-context stack; `Context::current()`
   returns one thing. So "N separate ambient contexts" is impossible — the
   currently-active tips must be *bundled* into one propagation envelope. This
   isn't a compromise; it's the only correct shape.
4. **Hiding N spans in `Context`'s cheap `.span` slot is impossible.** A span
   stored via `with_span` is triple-type-erased (`SynchronizedSpan` →
   `BoxedSpan` → `Box<dyn ObjectSafeSpan>`), and the `Span` trait has no
   `as_any`/downcast. You can never recover a concrete custom span (holding N
   sub-spans) back out. So a recoverable, typed per-instrumentation payload
   *must* use `Context`'s generic extension slot (`with_value`).

### Where the feared regression comes from, and why it's avoided

`Context::with_value` clones the entire extension `HashMap` on every call
(copy-on-write of the whole map), unlike `.with_span` which is special-cased to
a cheap `Arc` refcount bump. If *every* enabled call paid `with_value`, that
would regress today's callers. The fix is **three-way macro dispatch**:
existing/default callers hit byte-identical code and never touch `with_value` at
all — so their cost is provably unchanged. The `with_value` envelope is paid
**only** on calls where a *named* instrumentation is actually active — new
functionality with no prior baseline to regress against, and even there the one
small map-clone is dwarfed by the span-creation cost that path already pays.

## Design

### 1. Per-site state → two `AtomicU64` bitmasks

`TraceSite.enabled: AtomicBool` / `child_only: AtomicBool` became
`enabled_mask: AtomicU64` / `child_only_mask: AtomicU64`. Bit `N` = "instrumentation
slot `N` wants this function traced / child-only". `MAX_INSTRUMENTATIONS = 64`,
`DEFAULT_SLOT = 0`. Bit 0 is permanently the global `stylus::config`
instrumentation — never allocated/freed, always alive.

`AtomicU64::load` + `== 0` on a 64-bit target is one `MOV` + one `TEST` —
identical cost to the old `AtomicBool` fast path.

### 2. Three-way macro dispatch (the no-regression guarantee)

`stylus-macros` `expand()` dispatches on the loaded `enabled_mask`:

- **`== 0`** → run the original body, no machinery. One atomic load. Identical to
  before.
- **`== 1`** (only `DEFAULT_SLOT`; the case for *every existing caller*) →
  **byte-identical to the previous generated code**: `global::tracer(name)` /
  `Context::current_with_span` / `.attach()` (sync) or `FutureExt::with_context`
  (async), still honoring the default slot's `child_only`. No `with_value`, no
  slot-table lookup.
- **any named bit set** → the multi-slot path, delegated to
  `stylus::instrumentation::start_spans(...)`. Only this branch touches the
  `with_value` envelope.

The fast branches *preserve* the envelope for free: `current_with_span` and
`with_context` clone `entries` as an `Arc` bump, so a named instrumentation's
envelope rides untouched through an intervening default-only frame — mixed
nesting across instrumentations composes correctly with no extra cost.

### 3. Isolated per-instrumentation hierarchies

- **Default slot (0)** keeps using the ambient `Context.span` field — exactly
  the previous mechanism, and it remains the single span injected into outbound
  `traceparent` headers.
- **Named slots** ride a sparse envelope:
  `MultiInstrumentState(SmallVec<[(u8, Context); 4]>)` stored via
  `Context::with_value`. Sparse (only active slots, ~1–2), so per-call work is
  proportional to what's active, not the 64-slot cap. It stores the **full**
  per-slot `Context` (cheap `Arc`-bump clones), not a bare `SpanContext` (which
  would force `with_remote_span_context`, wrongly marking the parent
  `is_remote: true`).

`start_spans` reads the current envelope (cheap `Context::get`), and for each
set bit resolves that slot's parent (slot 0 → ambient span; named → its envelope
entry or root), applies per-slot `child_only` (skip if the slot's bit is set and
its parent isn't recording), starts a child span with **that slot's tracer**, and
finally builds one new `Context` (slot 0's span in `.span`, named slots in one
`with_value`) attached once for the whole call. If every active slot is
suppressed it returns `None` and the body runs raw.

Tracers are type-erased behind a small `DynTracer` object-safe trait
(`start_in(builder, parent) -> Context`) so an arbitrary concrete `Tracer` is
stored per slot without naming its `Span` type — the span is erased into the
returned `Context` via `with_span`. `opentelemetry::global::BoxedTracer`
satisfies this blanket impl too, so slot 0 and named slots use the same code.

### 4. `Instrumentation` handle + slot allocator

`Instrumentation::builder().name(..).tracer(some_tracer).build()` allocates a
slot and registers the tracer. The handle exposes the same shape as
`stylus::config` (`enable`/`disable`/`set_enabled`/`*_encoded`/`set_child_only`/
`enable_child_only`/…) scoped to its slot, via bit-op helpers shared with
`config.rs` (which uses `DEFAULT_SLOT`).

- **Slot allocation: bump-allocate 1..=63, no reuse.** Reuse would create an ABA
  hazard: `Drop` clears a slot's bit across many sites non-atomically, so
  reusing the number could mis-attribute a straggling span to the next occupant.
  `SlotsExhausted` after 64 cumulative instrumentations ever created. Given the
  intended scale (tens, occasional churn) this is very unlikely to bind.
  Slot-reuse-with-generation-counters is a possible follow-up.
- **Slot table:** `ArcSwap<[Option<Arc<dyn DynTracer>>; 64]>`. Hot-path read =
  one atomic pointer load, no lock; rare writes (build/drop) swap the whole
  table.
- **`Drop`:** clears its bit from every site's masks (stops new spans), then
  drops the tracer. In-flight spans are independent `Arc`-owned objects and
  finish/export normally.
- **Ordering:** masks use `Relaxed` (same eventually-consistent config semantics
  as the old `AtomicBool`; no reuse ⇒ no need for stronger ordering).

### 5. Backward compatibility

Every existing `stylus::config` free function is now a thin wrapper over the
shared bit-ops at `DEFAULT_SLOT`. External signatures/behavior are unchanged —
the demo, existing tests, and any caller keep working untouched. Purely additive.

## Documented limitation

`Context` has one `.span` field, so only the default instrumentation's span
propagates downstream via `traceparent`. Named instrumentations are
**in-process only**. Making every named instrumentation propagate downstream
would need a custom multi-header propagator — out of scope.

## Measured result (no regression)

Same 1000-iteration CPU workload, `criterion`, `with_simple_exporter`:

| variant                                  | time     | notes                                           |
| ---------------------------------------- | -------- | ----------------------------------------------- |
| `no_macro`                               | ~884 ns  | baseline                                        |
| `traceable_disabled` (`mask == 0`)       | ~884 ns  | **= baseline**; one atomic load, lost in noise  |
| `traceable_enabled` (`mask == 1`)        | ~1.21 µs | default-only path, **byte-identical to before** |
| `traceable_one_named_instrumentation`    | ~1.29 µs | multi path, 1 active slot (+~80 ns over `== 1`) |
| `traceable_two_named_instrumentations`   | ~1.58 µs | multi path, 2 active slots (~2× span work)      |

The disabled and default-enabled paths are statistically indistinguishable from
before, satisfying the no-regression constraint. The multi path's overhead
(`start_spans` + one `with_value`) is small relative to span creation and is
paid only when a named instrumentation is actually active, scaling with the
number of active slots as expected.

## Files

- `stylus/src/registry.rs` — `TraceSite` masks.
- `stylus/src/instrumentation.rs` — `Instrumentation`, `InstrumentationBuilder`,
  constants, `ArcSwap` slot table, bump allocator, shared bit-op helpers,
  `DynTracer`, `MultiInstrumentState`, `start_spans`.
- `stylus/src/config.rs` — thin wrappers over the shared bit-ops at
  `DEFAULT_SLOT`.
- `stylus-macros/src/lib.rs` — three-way `expand()` dispatch.
- `stylus/tests/traceable.rs` — isolation/nesting/coexistence/child-only/drop
  tests; all prior tests pass unmodified.
- `stylus/benches/traceable_overhead.rs` — named-instrumentation cases.
