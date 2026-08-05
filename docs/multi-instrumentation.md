# Design: instrumentations

> Status: **implemented**. This document is the design record for
> `stylus::instrumentation`. It captures the problem, the approach, the
> alternatives that were rejected and *why*, and the measured performance
> outcome.
>
> It has two layers of history. The feature originally *added* named
> instrumentations alongside a permanently-reserved default one; a later pass
> **removed** that default, so an instrumentation is now the only way to trace
> anything. Both decisions are recorded below, since the second only makes sense
> against the first.

## Problem

Originally `stylus` was single-config: each `#[traceable]` function's
`TraceSite` carried one `enabled: AtomicBool` + one `child_only: AtomicBool`,
and the macro's enabled path always reached for the single process-wide
`opentelemetry::global::tracer(name)` and nested under the single ambient
`Context::current()`. You could not run two independent tracing configurations
at once — different enabled subsets, isolated span hierarchies,
different tracers/backends.

The goal: create **instrumentations** at runtime, each with its own enabled
subset, its own isolated span tree built from the *same* underlying call flow,
and its own `Tracer` (possibly a different backend). Hard constraint at the
time: **no measurable performance regression** for existing/default usage.

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

## Design

### 1. Per-site state → two `AtomicU64` bitmasks

`TraceSite.enabled: AtomicBool` / `child_only: AtomicBool` became
`enabled_mask: AtomicU64` / `child_only_mask: AtomicU64`. Bit `N` = "instrumentation
slot `N` wants this function traced / child-only". `MAX_INSTRUMENTATIONS = 64`.

`AtomicU64::load` + `== 0` on a 64-bit target is one `MOV` + one `TEST` —
identical cost to the old `AtomicBool` fast path.

### 2. Two-way macro dispatch

`stylus-macros` `expand()` dispatches on the loaded `enabled_mask`:

- **`== 0`** → run the original body, no machinery. One atomic load.
- **anything else** → delegate to
  `stylus::instrumentation::start_spans(mask, child_only_mask, span_name, attrs)`.

This *used* to be a three-way dispatch, with a middle `== 1` case that was
byte-identical to pre-multi-instrumentation codegen (`global::tracer(name)` +
`Context::current_with_span` + `.attach()`/`with_context`) specifically so
existing callers provably paid nothing new. Removing the default instrumentation
removed the thing that branch existed to serve — there is no longer a
"default-only" mask, and no global tracer to look up — so it collapsed into the
general path. See [Removing the default instrumentation](#removing-the-default-instrumentation)
for what that cost.

### 3. Isolated per-instrumentation hierarchies

Every instrumentation rides a sparse envelope:
`MultiInstrumentState(SmallVec<[(u8, Context); 4]>)` stored via
`Context::with_value`. Sparse (only active slots, ~1–2), so per-call work is
proportional to what's active, not the 64-slot cap. It stores the **full**
per-slot `Context` (cheap `Arc`-bump clones), not a bare `SpanContext` (which
would force `with_remote_span_context`, wrongly marking the parent
`is_remote: true`).

`start_spans` reads the current envelope (cheap `Context::get`), and for each
set bit resolves that slot's parent from its envelope entry, applies per-slot
`child_only` (skip if the slot's bit is set and its parent isn't recording),
starts a child span with **that slot's tracer**, and finally builds one new
`Context` carrying every active slot's new tip, attached once for the whole
call. If every active slot is suppressed it returns `None` and the body runs raw.

Tracers are type-erased behind a small `DynTracer` object-safe trait
(`start_in(builder, parent) -> Context`) so an arbitrary concrete `Tracer` is
stored per slot without naming its `Span` type — the span is erased into the
returned `Context` via `with_span`.

### 4. `Instrumentation` handle + slot allocator

`Instrumentation::builder().name(..).tracer(some_tracer).build()` allocates a
slot and registers the tracer. The handle exposes the encoded-subset API scoped
to its slot: `enable_all`, `disable_all`, `set_enabled_encoded`,
`enable_encoded`, `disable_encoded`, `set_child_only_encoded`, plus
`enabled_names`/`child_only_names` for read-only introspection.

- **Slot allocation: bump `0..64`, then recycle a free list.** `Drop` releases
  the slot after clearing its bits and removing its tracer, so
  `MAX_INSTRUMENTATIONS` bounds *concurrently live* instrumentations rather than
  lifetime creations. `SlotsExhausted` therefore only fires with 64 alive at once.
- **Slot table:** `ArcSwap<[Option<Arc<dyn DynTracer>>; 64]>`. Hot-path read =
  one atomic pointer load, no lock; rare writes (build/drop) swap the whole
  table.
- **`Drop`:** clears its bit from every site's masks (stops new spans), drops the
  tracer, then releases the slot. In-flight spans are independent `Arc`-owned
  objects and finish/export normally.
- **Ordering:** masks use `Relaxed` (same eventually-consistent config semantics
  as the old `AtomicBool`).

#### Why slot reuse, and the race it accepts

Reuse was originally rejected for an ABA hazard, and the hazard is real: `Drop`
clears a slot's bit across many sites *non-atomically*, and a `#[traceable]` call
reads the site's mask (in the macro) before reading the slot table (in
`start_spans`). A thread descheduled between those two reads, across an entire
drop *and* rebuild, would find the new occupant's tracer behind a bit the old
occupant set, and emit **one span into the wrong instrumentation**.

It was accepted anyway, because bump-allocation's cap turned out to be the worse
problem once the default instrumentation was removed. Previously the "main"
tracing config lived in permanently-reserved slot 0 and consumed nothing; an
application hot-reloading its exporter identity just swapped the OTel global
provider. With no global to swap, that same edit now tears down and rebuilds an
instrumentation, taking a slot each time — so an interactive
edit-config-and-watch workflow walks toward 64 and then fails until restart.

The trade, stated plainly:

- **Cost of losing the race:** one mis-attributed span, during a reload only.
  Nothing is unsafe; nothing corrupts; steady-state tracing never touches it.
- **Cost of closing it properly:** epoch-based reclamation (or hazard pointers)
  before a slot returns to the free list, plus generation validation on the hot
  multi-slot path. A generation counter cannot live in the mask — it's one bit
  per slot, with nowhere to put one — so validation could only ever narrow the
  window after the fact, not close it.
- **Cost of the cap:** a hard `SlotsExhausted` failure in the workflow the
  library is most used for.

One reload-scoped mis-attributed span is cheaper than either alternative, and
unlike the cap it degrades gracefully.

## Removing the default instrumentation

The default slot was never *just* "a permanently allocated instrumentation." It
was the only one wired into `opentelemetry::Context`'s single `.span` field, and
that asymmetry was its entire remaining purpose:

- **Inbound:** it parented off `Context::current()`, so a propagator-extracted
  remote parent (`extract_with_context` → `cx.with_remote_span_context(sc)`,
  which lands in `.span`) became the parent — the process joined the incoming
  distributed trace. It likewise nested under any span made outside stylus.
- **Outbound:** because its span occupied `.span`,
  `TraceContextPropagator::inject_context` (which reads exactly `cx.span()`)
  emitted it into the `traceparent` header.

Named instrumentations had neither: their parent was
`env_entry.cloned().unwrap_or_default()`, and `Context::default()` is empty, so
they ignored `.span` entirely.

Collapsing everything into one uniform mechanism therefore meant choosing what
happens to those two capabilities. Three options were weighed:

1. **Opt-in flag on at most one instrumentation** — preserve today's behavior
   behind `.propagating(true)`, keeping the asymmetry but making it explicit and
   configurable rather than hardcoded to slot 0.
2. **Drop propagation entirely** — every instrumentation is envelope-only and
   always roots its own trace.
3. **Every instrumentation reads `.span`** — all of them join an inbound trace.
   Rejected: they would then share a trace id with the incoming span and with
   each other, destroying the isolation guarantee (and its
   `traces_a.is_disjoint(&traces_b)` test).

**Option 2 was chosen.** The result is one code path with no special cases: no
`DEFAULT_SLOT`, no slot-0 branch in `start_spans`, no `slot0_cx`/`env_changed`
bookkeeping, no `global::tracer` lookup, and no `tracer = "..."` macro argument
(a call site has nothing to name once every instrumentation brings its own
tracer). `stylus::config` was deleted outright.

### The resulting documented limitation

Instrumentations are **in-process only**. Their spans live exclusively in the
`Context` extension envelope and never occupy the `.span` slot, so:

- an incoming `traceparent` is not joined — the first traced function on a call
  path always roots a fresh trace;
- a span created outside stylus (a web framework's server span, a hand-rolled
  `tracer.start()`) is never a parent;
- outbound `traceparent` headers carry nothing from stylus.

An instrumentation's spans nest only under other `#[traceable]` spans of that
same instrumentation. In-process nesting itself is unaffected and works across
`.await` thread-hops, since the envelope rides inside the propagated `Context`.

Restoring cross-process participation later means reintroducing option 1's
opt-in flag; making *every* instrumentation propagate downstream would still need
a custom multi-header propagator, which remains out of scope.

## Measured result

Same 1000-iteration CPU workload, `criterion`, `with_simple_exporter`:

| variant                                | time     | notes                                          |
| -------------------------------------- | -------- | ---------------------------------------------- |
| `no_macro`                             | 862 ns   | baseline                                       |
| `traceable_disa` (`mask == 0`)         | 862 ns   | **= baseline**; one atomic load, lost in noise |
| `traceable_one_instr_enab`             | 1.245 µs | one active slot                                |
| `traceable_one_instr_disa`             | 871 ns   | live instrumentation, function not enabled     |
| `traceable_two_instr_enab`             | 1.488 µs | two active slots (~2× span work)               |
| `traceable_two_instr_disa`             | 871 ns   |                                                |
| `tracing_instrument_disa`              | 931 ns   | `#[tracing::instrument]` + `DynFilterFn` gate  |
| `tracing_instrument_enab`              | 1.919 µs | same real span construction + export           |

The disabled path remains statistically indistinguishable from the no-macro
baseline.

Against the pre-removal numbers, dropping the specialized default path was
roughly a wash, and slightly favorable overall:

| path                        | before   | after    | delta   |
| --------------------------- | -------- | -------- | ------- |
| single instrumentation      | 1.21 µs¹ | 1.245 µs | +35 ns  |
| one named instrumentation   | 1.29 µs  | 1.245 µs | −45 ns  |
| two named instrumentations  | 1.58 µs  | 1.488 µs | −92 ns  |

¹ the old `mask == 1` default-only path, with its inlined global-tracer lookup.

So the one case that got slower is the single-instrumentation path, by ~35 ns
(~3%), which is what the removed special case used to buy. Every multi-slot case
got *faster*, because `start_spans` shed the slot-0 branch and the
`slot0_cx`/`env_changed` bookkeeping that existed only to let the default slot
skip the envelope. Trading ~3% on one path for the deletion of an entire special
case — and for a uniform mental model — was judged worth it.

## Stable trace-site ids

Ids were originally a site's position in the `linkme`-collected `REGISTRY`. That
turned out to be unusable: `linkme` guarantees no ordering, and in practice the
order shifted on almost any rebuild — editing a file containing no `#[traceable]`
function at all was enough, and debug and release never agreed with each other.
Since a stale encoded value still decodes (out-of-range ids are skipped,
in-range ones resolve to whatever now sits at that index), every such rebuild
silently retargeted tracing with no error anywhere.

An id is now an index into the **sorted set of registry keys**
(`registry::names_by_id`). That depends on nothing but the keys themselves, so it
is invariant under rebuilds, edits anywhere in the source, moving functions
within a file, and the build profile.

Two alternatives were rejected:

- **Hashing each key into a build-independent `u64`.** Stable, but it scatters
  ids across the whole range, and the codec's compactness comes from delta-encoding
  a *dense* index — every encoded string would balloon.
- **Sorting by `(name, file, line)`.** Was implemented briefly, using `file!()`/
  `line!()` from the macro to break ties between sites sharing a key. Rejected: it
  reintroduces a source-location dependency, so two same-key sites in one file
  swap ids when reordered. Sorting keys alone removes the tie-break entirely.

Consequences worth noting:

- Sites sharing a registry key (two methods with the same name in one module,
  absent a `name` override) now share one id and toggle together. Previously they
  had distinct ids that nothing could tell apart. `apply_encoded` walks a grouped
  view so one id flips every site carrying that key.
- Sorting by key clusters a module's functions onto consecutive ids, so
  module-shaped subsets encode smaller than before.
- Adding or removing a key still renumbers the ids after it. That's inherent to a
  dense index and is the only case that requires re-encoding.

## Files

- `stylus/src/registry.rs` — `TraceSite` masks, and the sorted-key id ordering
  (`names_by_id` / `name_by_id` / `id_of_name`).
- `stylus/src/instrumentation.rs` — `Instrumentation`, `InstrumentationBuilder`,
  constants, `ArcSwap` slot table, free-list allocator, shared bit-op helpers,
  `DynTracer`, `MultiInstrumentState`, `start_spans`.
- `stylus/src/catalog.rs` — the `{name, id}` node dump, plus `all_names`.
- `stylus/src/codec.rs` — `encode`/`decode`/`DecodeError` for subset strings.
- `stylus-macros/src/lib.rs` — two-way `expand()` dispatch.
- `stylus/tests/traceable.rs` — isolation/nesting/coexistence/child-only/drop
  tests, slot-reuse churn, and the in-process-only assertions.
- `stylus/benches/traceable_overhead.rs` — per-instrumentation-count cases.
