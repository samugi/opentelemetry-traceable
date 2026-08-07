# stylus

Dynamic, per-function tracing on top of [`opentelemetry-rust`](https://github.com/open-telemetry/opentelemetry-rust).
Annotate any `fn`/`async fn` with `#[traceable]`, then enable or disable tracing for it at
runtime — individually, in bulk, or as an arbitrary subset of tens of thousands of functions —
without recompiling.

## Quick start

Annotate the functions you might one day want traced:

```rust
use stylus::traceable;

#[traceable]
fn process() { /* ... */ }

#[traceable(name = "kafka.fetch")]
async fn fetch() { /* ... */ }

#[traceable(fields("component" = "proxy", "request_id" = id.clone()))]
async fn handle(id: String) { /* ... */ }
```

Nothing traces yet. An **`Instrumentation`** is the only way to turn tracing on — there is no
global or default instrumentation, and no process-wide tracer. Each `Instrumentation` brings its
own `Tracer`, holds its own enabled subset, and builds its own isolated span hierarchy:

```rust
use opentelemetry::trace::TracerProvider as _;
use stylus::instrumentation::Instrumentation;

let provider = /* your own opentelemetry_sdk::trace::SdkTracerProvider */;

let instr = Instrumentation::builder()
    .name("checkout-debug")                     // diagnostic only
    .tracer(provider.tracer("checkout-debug"))  // any opentelemetry Tracer; required
    .build()?;                                  // Err(SlotsExhausted) if 64 are already live

instr.set_enabled_encoded(&encoded)?; // a compact id list — see "Compact subset encoding"
```

Dropping `instr` stops new spans for it and releases its slot for reuse; spans already in flight
finish and export normally.

Every `#[traceable]` function is disabled for every instrumentation by default. When no
instrumentation is tracing a function, a call costs a single atomic load — no span, no
`opentelemetry::Context` work at all. When at least one is, the macro builds one child span per
active instrumentation and attaches a single context for the wrapped call (attach/detach for sync,
`FutureExt::with_context` across `.await` for async), so in-process parent/child nesting is
automatic, including across `.await` points.

Disabling a function only skips *its own* span — an enabled function still nests under the nearest
recording ancestor span **of the same instrumentation**, whether or not everything in between is
enabled.

Registry key (the string a function is listed under, and the id you configure by) defaults to
`module_path!() + "::" + fn_name`, or the `name` argument if given. Note this isn't qualified by a
surrounding `impl` type — two methods with the same name in the same module share a key unless
`name` disambiguates them.

The macro takes only `name` and `fields(...)`. There is deliberately no `tracer` argument: a call
site never names or looks up a tracer, because each instrumentation supplies its own.

## Configuring an instrumentation

An instrumentation is configured exclusively through compact encoded id lists (see
[Compact subset encoding](#compact-subset-encoding) below for how `encoded` is produced) — there's
no name-based way to enable/disable. A name list doesn't scale as a wire format: 100 names out of a
100,000-function registry is 4-6 KB of configuration just to select 0.1% of it, so encoded ids are
the only way in.

```rust
instr.set_enabled_encoded(&encoded)?;    // replace the whole enabled set
instr.enable_encoded(&encoded)?;         // additive
instr.disable_encoded(&encoded)?;        // subtractive
instr.enable_all();                      // every registered function
instr.disable_all();                     // none

instr.set_child_only_encoded(&encoded)?; // replace the child-only set

instr.enabled_names();                   // -> impl Iterator<Item = &'static str>, read-only
instr.child_only_names();                // -> impl Iterator<Item = &'static str>, read-only

stylus::catalog::all_names();            // -> every registered key, regardless of instrumentation
```

`enabled_names` / `child_only_names` are introspection only — configuration always goes in as
encoded ids. `all_names()` lives on the catalog, not on a handle, because the set of linked
`#[traceable]` functions is a property of the binary.

### Running several at once

There's exactly one mechanism, so "several instrumentations in parallel" is just the general case of
what's above rather than a separate feature. Create as many as you need — each with its own enabled
subset, its own child-only set, and its own `Tracer` (potentially a different backend):

```rust
let checkout = Instrumentation::builder()
    .name("checkout-debug")
    .tracer(checkout_provider.tracer("checkout-debug"))
    .build()?;
let db_audit = Instrumentation::builder()
    .name("db-audit")
    .tracer(audit_provider.tracer("db-audit"))
    .build()?;

checkout.set_enabled_encoded(&checkout_subset)?;
db_audit.set_enabled_encoded(&db_subset)?;
```

Each builds a fully independent trace from the same physical call chain: a function enabled for
both produces two spans, one per instrumentation, each parented within its own hierarchy. The
disabled fast path is unchanged — the extra machinery is paid only on calls where at least one
instrumentation is actually active.

`MAX_INSTRUMENTATIONS = 64` bounds how many may be **live at once**, not how many may be created
over the process lifetime: each handle owns one bit per `#[traceable]` site, and `Drop` releases
its slot to a free list for reuse. That matters for hot-reload, where an instrumentation is torn
down and rebuilt whenever its identity (tracer, endpoint) changes. `build()` returns
`Err(SlotsExhausted)` only when 64 are concurrently live.

Slot reuse leaves one narrow, accepted race: `Drop` clears its bit across many sites
non-atomically, and a traced call reads a site's mask before reading the slot table. A thread
descheduled between those two reads, across an *entire* drop and rebuild, could find the new
occupant's tracer behind a bit the old occupant set and emit one span into the wrong
instrumentation. Closing it properly would need epoch-based reclamation plus hot-path validation;
the cost of losing the race is a single mis-attributed span during a reload, which isn't worth that
machinery. See [`docs/multi-instrumentation.md`](docs/multi-instrumentation.md) for the design.

## Limitation: in-process only

Instrumentations never read or write `opentelemetry::Context`'s single "current span" slot — their
spans live exclusively in a `Context` extension envelope, one entry per active slot, so async
propagation stays O(1) no matter how many instrumentations are live. Distributed propagation is
dropped deliberately as a consequence:

- **An incoming `traceparent` is not joined.** A propagator deposits the remote parent in
  `Context`'s span slot, which no instrumentation consults, so the first traced function on a call
  path always roots a fresh trace.
- **A span created outside stylus is never a parent** either — not a web framework's server span,
  not a hand-rolled `tracer.start()`.
- **Outbound requests carry no `traceparent` from stylus**, since propagators inject whatever is in
  that same span slot.

An instrumentation's spans nest only under other `#[traceable]` spans of that *same*
instrumentation. In-process nesting, including across `.await`, works correctly.

## Avoiding orphan spans for functions shared across call paths

A function's enabled bit is per-instrumentation but not per-call-site: it fires for *every* caller,
not just the one you had in mind. That's fine for a function with one call site, but a function
shared across multiple call paths (a common `db`/`cache`/logging-style helper called from several
different flows) will also fire — as a disconnected root span — every time some *other*, non-traced
path calls it, since there's nothing above it in that instrumentation's hierarchy to attach to.

The fix is *child-only mode*: for a given instrumentation, a function in this mode only creates a
span when that instrumentation already has a recording span on the current call path — never a
root, even when enabled.

```rust
instr.set_child_only_encoded(&encoded)?; // same compact id list as the enabled set
```

Child-only is orthogonal to enabled, and scoped to one instrumentation: a function must be enabled
for that instrumentation to trace at all, and being child-only *for that instrumentation*
additionally suppresses it when it would otherwise root. The same function can be child-only for
one instrumentation and root-capable for another.

Crucially it's **not** a source annotation — it's set at runtime, because whether a shared helper
*should* root a trace depends on what you're tracing. Tracing the flow that calls it? Put it in
child-only mode so it stays nested and never orphans on the *other* flows. Tracing the helper's own
subsystem (e.g. "trace the database")? Leave it root-capable so it still produces a trace even when
its immediate caller isn't traced. It only changes *when* a span is created, not whether — still
zero false negatives on the path you enabled, still the same near-zero cost when off.

Deciding which functions to put in child-only mode for a given request is mechanical given a
call graph: a function should be child-only exactly when one of its own callers is also being
traced (so it always has a parent), and root-capable otherwise. That's the rule the agent
workflow below applies.

## Compact subset encoding

`stylus::codec` encodes an arbitrary selection of functions as a **lossless delta-encoded id
list** instead of a name list. The ids are sorted, turned into LEB128 varint deltas, then
base64'd (URL-safe, unpadded). There are **no false positives**: exactly the listed functions
are toggled, nothing else.

```rust
let mut ids = vec![0, 3, 7];
let encoded = stylus::codec::encode(&mut ids);
instr.set_enabled_encoded(&encoded)?;   // replace the whole enabled set
instr.enable_encoded(&encoded)?;        // additive
instr.disable_encoded(&encoded)?;       // subtractive
```

An `id` is a `#[traceable]` function's index in the **sorted set of registry keys**
(`stylus::registry::names_by_id`): dense, 0-based, and reported by `stylus::catalog`. Sorting is
done in place, which is why `encode` takes `&mut [u64]`. Decoding is
`stylus::codec::decode(&str) -> Result<Vec<u64>, stylus::codec::DecodeError>`; a corrupt string
returns `Err(DecodeError)` and leaves state unchanged.

Ids are build-scoped, not instrumentation-scoped: the same encoded string means the same set of
functions for every instrumentation in that binary.

### When ids change

An id depends on nothing but the set of registry keys, so it survives rebuilds, edits anywhere in
the source, moving functions around, and differing profiles — debug and release agree. **Adding or
removing a `#[traceable]` key renumbers the ids after it**, and that's the only thing that
invalidates an encoded string; regenerate the catalog and re-encode.

Deriving ids from keys rather than from registry position is what makes that hold: `linkme` gives
no ordering guarantee and its order does shift between builds. Keeping them dense (rather than
hashing each key into a build-independent `u64`) is what keeps the encoding compact, and sorting by
key clusters a module's functions onto consecutive ids, so module-shaped subsets encode especially
small.

Two call sites can share a key — two methods with the same name in the same module, absent a `name`
override. They share an id and toggle together.

To enable *everything* without enumerating every id from code, call `instr.enable_all()`. For a
config-file-driven setup, encode every id from the catalog instead. "Disable everything" is still
just an empty enabled string.

## Letting an LLM (or a script) configure an arbitrary subset

This is the intended workflow when the thing picking which functions to trace has source
access but isn't running Rust, or is choosing from thousands of candidates and can't
reasonably be handed a plain name list:

1. **Dump the catalog.** From within your instrumented application (an admin endpoint, a debug
   CLI flag, a one-off example — `stylus` has no way to know how *your* app wants to expose
   this, so it just provides the data):
   ```rust
   let json = stylus::catalog::catalog_json();
   ```
   This returns every `#[traceable]` function currently linked into the binary, each with the
   same registry-index id `stylus::codec` uses internally:
   ```json
   {
     "functions": [
       { "name": "my_crate::process", "id": 0 },
       { "name": "kafka.fetch", "id": 1 }
     ]
   }
   ```
   Ids are stable across rebuilds; regenerate this when you add or remove a `#[traceable]`
   function (see [When ids change](#when-ids-change)).

   This is the *node* list. Call-graph edges aren't known to the running binary — the consumer
   derives them from your source, and they're what decide which functions can root a trace and
   which should be child-only so the hierarchy stays intact.
2. **Hand that JSON to the consumer** (an LLM with access to your source, a script, an
   operator) along with the task: "pick whichever of these functions should be traced."
3. **Turn the picks into an encoded id list:**
   ```rust
   let mut ids = chosen_ids;
   let encoded = stylus::codec::encode(&mut ids);
   ```
   Expose that the same way you exposed the catalog, so the consumer can encode its own picks
   without writing Rust — the ids index this binary's registry, so nothing outside it can do the
   encoding on its behalf. (`stylus-demo` wires both up as `catalog` and `encode --ids`
   subcommands.)
4. **Apply it** to the instrumentation that should trace that subset:
   `instr.set_enabled_encoded(&encoded)?`.

### The encoding, if you need to reproduce it without this crate

Simple enough to reimplement in a short script, given a catalog dump's list of chosen `id`s
(each an index into the registry):

1. **Sort** the chosen ids ascending.
2. **Delta**: replace each id with its difference from the previous one (the first is left
   as-is), so you have a list of non-negative deltas.
3. **Varint**: encode each delta as an LEB128 unsigned varint (7 bits per byte, low bit of the
   continuation flag set on all but the last byte).
4. **Wire format**: concatenate the varint bytes, then base64 (URL-safe, unpadded).

## For coding agents: `agents/AGENTS.md` and `agents/SKILL.md`

Two copy-paste templates for any application built on `stylus`, so a coding agent asked
something like "trace the database" or "turn off tracing" can reconfigure it without reading
this whole README.

- `agents/AGENTS.md` → copy to the application repo's root as `AGENTS.md`.
- `agents/SKILL.md` → copy to the application repo as a manually-invoked Claude Code skill, e.g.
  `.claude/skills/configure-tracing/SKILL.md`.

Both drive the application's own catalog and encode entry points (step 1 and 5), read its source
to derive the call graph, then apply the child-only rule above so the resulting hierarchy holds
together. They expect the app to expose those two entry points and ask the user if it doesn't.
See `stylus-demo/AGENTS.md` and `stylus-demo/.claude/skills/configure-tracing/SKILL.md` for a
working copy.

## Crate layout

- `stylus` — runtime: `#[traceable]` re-export, `registry` (the `linkme`-collected
  `TraceSite`/`REGISTRY`, one enabled/child-only bitmask pair per site), `instrumentation`
  (`Instrumentation` + its builder — creating, configuring, and dropping instrumentations, plus
  the `start_spans` hot path), `codec` (lossless delta-encoded id list encode/decode),
  `catalog` (the `{name, id}` dump and `all_names`).
- `stylus-macros` — the `#[traceable]` proc-macro implementation.

## Benchmarks

`stylus/benches/traceable_overhead.rs` (Criterion) runs the same CPU-bound workload (1000
iterations) through a plain function, through `#[traceable]` with nothing tracing it, through
`#[traceable]` with one and then two live instrumentations (real in-process span exporters, not
no-op tracers), and through `#[tracing::instrument]` for comparison. Run with
`cargo bench -p stylus`. Representative local numbers:

| variant                          | time      |
| -------------------------------- | --------- |
| `no_macro`                       | 862 ns    |
| `traceable_disa` (mask == 0)     | 862 ns    |
| `traceable_one_instr_enab`       | 1.245 µs  |
| `traceable_one_instr_disa`       | 871 ns    |
| `traceable_two_instr_enab`       | 1.488 µs  |
| `traceable_two_instr_disa`       | 871 ns    |
| `tracing_instrument_disa`        | 931 ns    |
| `tracing_instrument_enab`        | 1.919 µs  |

With no instrumentation tracing a function, `#[traceable]` costs the same as no macro at all,
within noise — the mask is `0` and the macro dispatch is a single atomic load followed by the raw
body. Enabled adds the real cost of span construction and export, once per active instrumentation.

Dropping the special-cased default instrumentation is a small, honest trade. Previously a
hardcoded `mask == 1` path ran a single span against a process-global tracer at ~1.21 µs, so the
single-instrumentation case now costs ~35 ns more (1.21 → 1.245 µs). In exchange the multi-slot
path got *faster*, because `start_spans` no longer carries the slot-0 branch and its
`slot0_cx`/`env_changed` bookkeeping: one named instrumentation went 1.29 → 1.245 µs and two went
1.58 → 1.488 µs. One uniform path, slightly cheaper as soon as you have more than a single
instrumentation.

## Development

Tooling is managed with [`mise`](https://mise.jdx.dev/) (config at `.config/mise/config.toml`):

```
mise run test   # cargo nextest run --workspace
mise run lint   # cargo fmt --all -- --check && cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Tests must be run via `cargo nextest run`, not plain `cargo test` — several tests mutate
process-global state (the `linkme` registry's per-site bitmasks, which every instrumentation in
the process shares), and rely on nextest's process-per-test isolation instead of a shared-process
`Mutex`. Each crate with tests has a `check_test_runner` test that fails with a clear message if
it detects it's running under plain `cargo test`.

CI (`.github/workflows/ci.yml`) runs `rust-fmt`, `rust-clippy`, `rust-doc`, and `rust-test`
(via `mise run test`) as separate jobs on every push/PR.
